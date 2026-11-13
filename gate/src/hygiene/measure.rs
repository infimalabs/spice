use std::collections::BTreeSet;

use super::*;

/// Measure a complete unordered observation set into fixed arm order.
///
/// # Errors
///
/// Refuses foreign, duplicate, missing, or inconsistent observations. No
/// partial report is returned.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn measure_hygiene(
    observations: impl IntoIterator<Item = HygieneObservation>,
) -> Result<HygieneReport, HygieneError> {
    let mut remaining: Vec<_> = observations.into_iter().collect();
    let mut seen = BTreeSet::new();
    for observation in &remaining {
        if declaration(observation.gate_id).is_none() {
            return Err(HygieneError::ForeignGate(observation.gate_id));
        }
        if !seen.insert(observation.gate_id) {
            return Err(HygieneError::DuplicateGate(observation.gate_id));
        }
    }
    let mut arms = Vec::with_capacity(HYGIENE_ARMS.len());
    for arm in HYGIENE_ARMS {
        let Some(index) = remaining.iter().position(|item| item.gate_id == arm.id) else {
            return Err(HygieneError::MissingGate(arm.id));
        };
        arms.push(measure_arm(arm, remaining.swap_remove(index))?);
    }
    debug_assert!(remaining.is_empty());
    Ok(HygieneReport {
        version: HYGIENE_REPORT_VERSION,
        arms,
    })
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn measure_hygiene_arm(
    observation: HygieneObservation,
) -> Result<HygieneMeasurement, HygieneError> {
    let declaration =
        declaration(observation.gate_id).ok_or(HygieneError::ForeignGate(observation.gate_id))?;
    measure_arm(declaration, observation)
}

fn declaration(gate: GateId) -> Option<HygieneDeclaration> {
    HYGIENE_ARMS.into_iter().find(|arm| arm.id == gate)
}

fn measure_arm(
    declaration: HygieneDeclaration,
    mut observation: HygieneObservation,
) -> Result<HygieneMeasurement, HygieneError> {
    let gate = declaration.id;
    if observation.reason.is_empty() || observation.reason.contains(['\n', '\r']) {
        return Err(HygieneError::InvalidReason(gate));
    }
    canonicalize_evidence(&mut observation.evidence);
    canonicalize_findings(&mut observation.findings);
    validate_findings(gate, observation.outcome, &observation.findings)?;
    validate_evidence(declaration, observation.outcome, &observation.evidence)?;
    validate_policy_decision(
        gate,
        observation.outcome,
        observation.findings.len(),
        &observation.evidence,
    )?;
    validate_provider_evidence(declaration, &observation.evidence, &observation.findings)?;
    let jurisdiction = declaration.jurisdiction;
    Ok(HygieneMeasurement {
        gate_id: gate,
        outcome: observation.outcome,
        reason: observation.reason,
        native_engine: format!(
            "{}@{NATIVE_ANALYZER_VERSION}",
            declaration.analyzer.component()
        ),
        configuration: declaration
            .configuration
            .iter()
            .map(|item| format!("{}:{}", item.source, item.field))
            .collect(),
        roots: owned(jurisdiction.roots),
        includes: owned(jurisdiction.includes),
        excludes: owned(jurisdiction.excludes),
        applicability: jurisdiction.applicability.to_owned(),
        traversals: declaration.traversals.to_vec(),
        provider_contract: declaration.provider_contract.map(str::to_owned),
        evidence: observation.evidence,
        findings: observation.findings,
    })
}

pub(super) fn validate_measurement(measurement: &HygieneMeasurement) -> Result<(), HygieneError> {
    let declaration =
        declaration(measurement.gate_id).ok_or(HygieneError::ForeignGate(measurement.gate_id))?;
    let observed = HygieneObservation {
        gate_id: measurement.gate_id,
        outcome: measurement.outcome,
        reason: measurement.reason.clone(),
        evidence: measurement.evidence.clone(),
        findings: measurement.findings.clone(),
    };
    let expected = measure_arm(declaration, observed)?;
    if expected != *measurement {
        return Err(HygieneError::InvalidEvidence {
            gate: measurement.gate_id,
            evidence: "retained measurement disagrees with its compiled declaration".to_owned(),
        });
    }
    Ok(())
}

fn owned(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

pub(super) fn canonicalize_findings(findings: &mut [HygieneFinding]) {
    for finding in findings.iter_mut() {
        finding.detail.canonicalize();
    }
    findings.sort();
}

fn canonicalize_evidence(evidence: &mut [AnalysisEvidence]) {
    for item in evidence.iter_mut() {
        canonicalize_evidence_item(item);
    }
    evidence.sort();
}

fn canonicalize_evidence_item(evidence: &mut AnalysisEvidence) {
    match evidence {
        AnalysisEvidence::LanguageApplicability {
            project_markers, ..
        } => sort_dedup(project_markers),
        // The reach ranking is left as the analyzer emitted it. Sorting these
        // roots imposes an order collection did not fix, while the ranking is
        // ordered by the measurement itself, widest reach first, and sorting it
        // by path would discard the one thing it was built to say.
        AnalysisEvidence::GraphConstruction {
            production_roots,
            test_roots,
            ..
        } => {
            sort_dedup(production_roots);
            sort_dedup(test_roots);
        }
        AnalysisEvidence::TestClassification { helpers, .. } => {
            sort_dedup(helpers);
        }
        AnalysisEvidence::PrivateBoundaryResolution { packages, .. } => sort_dedup(packages),
        AnalysisEvidence::PathSuppression { suppressed } => {
            suppressed.sort();
            suppressed.dedup();
        }
        AnalysisEvidence::SymbolExtraction { .. }
        | AnalysisEvidence::PolicyDecision { .. }
        | AnalysisEvidence::Analyzer(_)
        | AnalysisEvidence::Deadline { .. }
        | AnalysisEvidence::Parse { .. }
        | AnalysisEvidence::Measurement { .. }
        | AnalysisEvidence::Exclusion(_) => {}
    }
}

fn sort_dedup(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn validate_findings(
    gate: GateId,
    outcome: HygieneOutcome,
    findings: &[HygieneFinding],
) -> Result<(), HygieneError> {
    let valid_presence = match outcome {
        HygieneOutcome::Findings => !findings.is_empty(),
        HygieneOutcome::Passed => true,
        _ => findings.is_empty(),
    };
    if !valid_presence {
        return invalid_findings(
            gate,
            "only completed outcomes may carry findings and findings is nonempty",
        );
    }
    if findings.windows(2).any(|pair| pair[0] == pair[1]) {
        return invalid_findings(gate, "duplicate structured finding");
    }
    for finding in findings {
        if invalid_path(&finding.path)
            || finding.line == Some(0)
            || finding.detail.gate() != gate
            || detail_has_empty_identity(&finding.detail)
            || finding_semantics_are_invalid(finding)
        {
            return invalid_findings(gate, "malformed or gate-mismatched finding identity");
        }
    }
    Ok(())
}

fn invalid_path(path: &str) -> bool {
    path.is_empty() || path.starts_with('/') || path.split('/').any(|part| part == "..")
}

fn detail_has_empty_identity(detail: &HygieneFindingDetail) -> bool {
    match detail {
        HygieneFindingDetail::ModuleReachability {
            provider, subject, ..
        } => provider.is_empty() || subject.is_empty(),
        HygieneFindingDetail::SymbolReachability {
            provider,
            module,
            symbol,
            symbol_kind,
            ..
        } => [provider, module, symbol, symbol_kind]
            .into_iter()
            .any(|item| item.is_empty()),
        HygieneFindingDetail::PythonUnused {
            module,
            symbol,
            declaration_kind,
            reason,
            ..
        } => [module, symbol, declaration_kind, reason]
            .into_iter()
            .any(|item| item.is_empty()),
        HygieneFindingDetail::AssertionFree { test_name } => test_name.is_empty(),
        HygieneFindingDetail::PrivateCoupling {
            test_name,
            coupling_kind,
            target,
        } => [test_name, coupling_kind, target]
            .into_iter()
            .any(|item| item.is_empty()),
    }
}

fn finding_semantics_are_invalid(finding: &HygieneFinding) -> bool {
    match &finding.detail {
        HygieneFindingDetail::ModuleReachability { .. } => finding.line.is_some(),
        HygieneFindingDetail::SymbolReachability {
            provider,
            symbol_kind,
            ..
        } => {
            provider == "python" && !matches!(symbol_kind.as_str(), "function" | "class" | "method")
        }
        HygieneFindingDetail::PythonUnused {
            declaration_kind,
            status,
            reason,
            test_references,
            ..
        } => {
            finding.line.is_none()
                || !matches!(declaration_kind.as_str(), "function" | "class")
                || !python_unused_identity_matches(*status, reason, test_references)
        }
        HygieneFindingDetail::AssertionFree { .. }
        | HygieneFindingDetail::PrivateCoupling { .. } => finding.line.is_none(),
    }
}

fn python_unused_identity_matches(
    status: PythonUnusedStatus,
    reason: &str,
    test_references: &[String],
) -> bool {
    match status {
        PythonUnusedStatus::CandidateUnused => {
            reason == "no production or test references" && test_references.is_empty()
        }
        PythonUnusedStatus::TestOnly => {
            reason == "references only in tests" && !test_references.is_empty()
        }
    }
}

fn invalid_findings<T>(gate: GateId, evidence: &str) -> Result<T, HygieneError> {
    Err(HygieneError::InvalidFindings {
        gate,
        evidence: evidence.to_owned(),
    })
}

fn validate_evidence(
    declaration: HygieneDeclaration,
    outcome: HygieneOutcome,
    evidence: &[AnalysisEvidence],
) -> Result<(), HygieneError> {
    let gate = declaration.id;
    if evidence.is_empty() || duplicate_evidence_kinds(evidence) {
        return invalid_evidence(
            gate,
            "evidence must be nonempty with one row per stage kind",
        );
    }
    validate_evidence_shape(gate, evidence)?;
    validate_single_terminal_cause(gate, outcome, evidence)?;
    match outcome {
        HygieneOutcome::Passed | HygieneOutcome::Findings => {
            validate_completed(declaration, evidence)
        }
        HygieneOutcome::NotApplicable => require_inapplicable(gate, evidence),
        HygieneOutcome::Excluded => require_exclusion(gate, evidence),
        HygieneOutcome::Unavailable => {
            require_analyzer_state(gate, evidence, AnalyzerEvidence::is_unavailable)
        }
        HygieneOutcome::Unsupported => {
            require_analyzer_state(gate, evidence, AnalyzerEvidence::is_unsupported)
        }
        HygieneOutcome::Unmeasured => {
            require_stage_state(gate, evidence, EvidenceState::Unmeasured)
        }
        HygieneOutcome::Timeout => require_stage_state(gate, evidence, EvidenceState::TimedOut),
        HygieneOutcome::ParseFailure => {
            require_stage_state(gate, evidence, EvidenceState::ParseFailed)
        }
        HygieneOutcome::Failed => require_stage_state(gate, evidence, EvidenceState::Failed),
    }
}

fn validate_single_terminal_cause(
    gate: GateId,
    outcome: HygieneOutcome,
    evidence: &[AnalysisEvidence],
) -> Result<(), HygieneError> {
    let observed = terminal_causes(evidence);
    let expected = match outcome {
        HygieneOutcome::Unavailable => [true, false, false, false, false, false],
        HygieneOutcome::Unsupported => [false, true, false, false, false, false],
        HygieneOutcome::Unmeasured => [false, false, true, false, false, false],
        HygieneOutcome::Timeout => [false, false, false, true, false, false],
        HygieneOutcome::ParseFailure => [false, false, false, false, true, false],
        HygieneOutcome::Failed => [false, false, false, false, false, true],
        HygieneOutcome::Passed
        | HygieneOutcome::Findings
        | HygieneOutcome::NotApplicable
        | HygieneOutcome::Excluded => [false; 6],
    };
    if observed == expected {
        Ok(())
    } else {
        invalid_evidence(gate, "outcome has contradictory terminal evidence")
    }
}

fn terminal_causes(evidence: &[AnalysisEvidence]) -> [bool; 6] {
    let unavailable = evidence.iter().any(|item| {
        matches!(
            item,
            AnalysisEvidence::Analyzer(analyzer) if analyzer.is_unavailable()
        )
    });
    let unsupported = evidence.iter().any(|item| {
        matches!(
            item,
            AnalysisEvidence::Analyzer(analyzer) if analyzer.is_unsupported()
        )
    });
    let unmeasured = has_stage_state(evidence, EvidenceState::Unmeasured);
    let timeout = has_deadline_state(evidence, EvidenceState::TimedOut);
    let parse = has_parse_state(evidence, EvidenceState::ParseFailed);
    let failed = has_stage_state(evidence, EvidenceState::Failed);
    [unavailable, unsupported, unmeasured, timeout, parse, failed]
}

fn duplicate_evidence_kinds(evidence: &[AnalysisEvidence]) -> bool {
    let mut ranks = BTreeSet::new();
    let mut analyzers = BTreeSet::new();
    for item in evidence {
        if let AnalysisEvidence::Analyzer(analyzer) = item {
            if !analyzers.insert(analyzer.name()) {
                return true;
            }
        } else if !ranks.insert(evidence_rank(item)) {
            return true;
        }
    }
    false
}

pub(super) const fn evidence_rank(evidence: &AnalysisEvidence) -> u8 {
    match evidence {
        AnalysisEvidence::LanguageApplicability { .. } => 0,
        AnalysisEvidence::GraphConstruction { .. } => 1,
        AnalysisEvidence::SymbolExtraction { .. } => 2,
        AnalysisEvidence::TestClassification { .. } => 3,
        AnalysisEvidence::PrivateBoundaryResolution { .. } => 4,
        AnalysisEvidence::PolicyDecision { .. } => 5,
        AnalysisEvidence::Analyzer(_) => 6,
        AnalysisEvidence::Deadline { .. } => 7,
        AnalysisEvidence::Parse { .. } => 8,
        AnalysisEvidence::Measurement { .. } => 9,
        AnalysisEvidence::PathSuppression { .. } => 10,
        AnalysisEvidence::Exclusion(_) => 11,
    }
}

fn validate_evidence_shape(
    gate: GateId,
    evidence: &[AnalysisEvidence],
) -> Result<(), HygieneError> {
    for item in evidence {
        let malformed = match item {
            AnalysisEvidence::LanguageApplicability {
                language,
                provenance,
                ..
            } => language.is_empty() || !provenance.is_complete(),
            AnalysisEvidence::GraphConstruction {
                production_roots,
                test_roots,
                reach,
                ..
            } => {
                production_roots.iter().any(|path| invalid_path(path))
                    || test_roots.iter().any(|path| invalid_path(path))
                    // A test module importing a module directly also reaches
                    // it, so more importers than reachers is not a narrower
                    // count of one graph: it is two graphs reported as one.
                    // A module no test reaches is left out rather than ranked
                    // at zero, so a zero row is a rank nothing measured.
                    || reach.iter().any(|entry| {
                        invalid_path(&entry.path)
                            || entry.imported_by > entry.reached_by
                            || entry.reached_by == 0
                    })
                    // The order is the measurement. Nothing downstream re-sorts
                    // this ranking, precisely so the widest reach stays first,
                    // which leaves a ranking that arrives out of order saying
                    // the opposite of what it was walked to say.
                    || reach.windows(2).any(|pair| {
                        let [wider, narrower] = pair else {
                            return false;
                        };
                        wider.reached_by < narrower.reached_by
                            || (wider.reached_by == narrower.reached_by
                                && wider.path > narrower.path)
                    })
            }
            AnalysisEvidence::Analyzer(item) => item.name().is_empty(),
            AnalysisEvidence::Deadline {
                limit_ms, state, ..
            } => {
                *limit_ms == 0
                    || !matches!(state, EvidenceState::Completed | EvidenceState::TimedOut)
            }
            AnalysisEvidence::Parse {
                state,
                failed_path,
                diagnostic,
                ..
            } => invalid_parse(*state, failed_path, diagnostic),
            AnalysisEvidence::Measurement { state, stage } => {
                stage.is_empty()
                    || !matches!(
                        state,
                        EvidenceState::Completed
                            | EvidenceState::Unmeasured
                            | EvidenceState::Failed
                    )
            }
            AnalysisEvidence::Exclusion(item) => {
                !item.provenance.is_complete() || item.reason.is_empty()
            }
            AnalysisEvidence::PathSuppression { suppressed } => {
                suppressed.is_empty()
                    || suppressed
                        .iter()
                        .any(|item| !item.is_complete() || invalid_path(&item.path))
            }
            AnalysisEvidence::PrivateBoundaryResolution { packages, .. } => {
                packages.iter().any(String::is_empty)
            }
            AnalysisEvidence::PolicyDecision { field, .. } => field.is_empty(),
            AnalysisEvidence::SymbolExtraction { .. }
            | AnalysisEvidence::TestClassification { .. } => false,
        };
        if malformed {
            return invalid_evidence(gate, "malformed typed stage evidence");
        }
    }
    Ok(())
}

fn invalid_parse(state: EvidenceState, path: &Option<String>, diagnostic: &Option<String>) -> bool {
    match state {
        EvidenceState::Completed => path.is_some() || diagnostic.is_some(),
        EvidenceState::ParseFailed => {
            path.as_deref().is_none_or(invalid_path)
                || diagnostic.as_deref().is_none_or(str::is_empty)
        }
        _ => true,
    }
}

fn validate_completed(
    declaration: HygieneDeclaration,
    evidence: &[AnalysisEvidence],
) -> Result<(), HygieneError> {
    let gate = declaration.id;
    if !has_applicable_language(evidence)
        || !has_native_analyzer(declaration, evidence)
        || !has_completed_deadline(evidence)
        || !has_completed_parse(evidence)
        || !has_stage_state(evidence, EvidenceState::Completed)
    {
        return invalid_evidence(
            gate,
            "completed outcomes require applicable, identified, parsed evidence",
        );
    }
    for traversal in declaration.traversals {
        if !has_traversal_evidence(*traversal, evidence) {
            return invalid_evidence(gate, "completed outcome omitted a declared traversal");
        }
    }
    Ok(())
}

pub(super) fn has_applicable_language(evidence: &[AnalysisEvidence]) -> bool {
    evidence.iter().any(|item| {
        matches!(
            item,
            AnalysisEvidence::LanguageApplicability {
                selected_files,
                applicable: true,
                ..
            } if *selected_files > 0
        )
    })
}

pub(super) fn has_native_analyzer(
    declaration: HygieneDeclaration,
    evidence: &[AnalysisEvidence],
) -> bool {
    evidence.iter().any(|item| {
        matches!(
            item,
            AnalysisEvidence::Analyzer(analyzer)
                if analyzer.is_native_for(declaration)
        )
    })
}

pub(super) fn has_completed_deadline(evidence: &[AnalysisEvidence]) -> bool {
    evidence.iter().any(|item| {
        matches!(
            item,
            AnalysisEvidence::Deadline {
                limit_ms,
                elapsed_ms,
                state: EvidenceState::Completed,
            } if elapsed_ms <= limit_ms
        )
    })
}

pub(super) fn has_completed_parse(evidence: &[AnalysisEvidence]) -> bool {
    evidence.iter().any(|item| {
        matches!(
            item,
            AnalysisEvidence::Parse {
                state: EvidenceState::Completed,
                failed_path: None,
                diagnostic: None,
                ..
            }
        )
    })
}

fn has_stage_state(evidence: &[AnalysisEvidence], expected: EvidenceState) -> bool {
    evidence.iter().any(|item| {
        matches!(
            item,
            AnalysisEvidence::Measurement { state, .. }
                if *state == expected
        )
    })
}

fn has_deadline_state(evidence: &[AnalysisEvidence], expected: EvidenceState) -> bool {
    evidence.iter().any(|item| {
        matches!(
            item,
            AnalysisEvidence::Deadline { state, .. } if *state == expected
        )
    })
}

fn has_parse_state(evidence: &[AnalysisEvidence], expected: EvidenceState) -> bool {
    evidence.iter().any(|item| {
        matches!(
            item,
            AnalysisEvidence::Parse { state, .. } if *state == expected
        )
    })
}

pub(super) fn has_traversal_evidence(
    traversal: HygieneTraversal,
    evidence: &[AnalysisEvidence],
) -> bool {
    match traversal {
        HygieneTraversal::ImportGraph => evidence.iter().any(|item| {
            matches!(
                item,
                AnalysisEvidence::GraphConstruction {
                    production_roots,
                    test_roots,
                    nodes,
                    ..
                } if !production_roots.is_empty()
                    && !test_roots.is_empty()
                    && *nodes > 0
            )
        }),
        HygieneTraversal::SymbolGraph | HygieneTraversal::PythonDeclarations => {
            evidence.iter().any(|item| {
                matches!(
                    item,
                    AnalysisEvidence::SymbolExtraction { declarations, .. }
                        if *declarations > 0
                )
            })
        }
        HygieneTraversal::TestAssertions => evidence.iter().any(|item| {
            matches!(
                item,
                AnalysisEvidence::TestClassification {
                    files,
                    tests,
                    assertions,
                    ..
                } if *files > 0 && *tests > 0 && assertions <= tests
            )
        }),
        HygieneTraversal::PrivateBoundaries => evidence
            .iter()
            .any(|item| matches!(item, AnalysisEvidence::PrivateBoundaryResolution { .. })),
    }
}

fn require_inapplicable(gate: GateId, evidence: &[AnalysisEvidence]) -> Result<(), HygieneError> {
    if evidence.iter().any(|item| {
        matches!(
            item,
            AnalysisEvidence::LanguageApplicability {
                applicable: false,
                ..
            }
        )
    }) {
        Ok(())
    } else {
        invalid_evidence(gate, "not-applicable requires false applicability")
    }
}

fn require_exclusion(gate: GateId, evidence: &[AnalysisEvidence]) -> Result<(), HygieneError> {
    if evidence
        .iter()
        .any(|item| matches!(item, AnalysisEvidence::Exclusion(_)))
    {
        Ok(())
    } else {
        invalid_evidence(gate, "excluded requires provenance evidence")
    }
}

fn require_analyzer_state(
    gate: GateId,
    evidence: &[AnalysisEvidence],
    predicate: fn(&AnalyzerEvidence) -> bool,
) -> Result<(), HygieneError> {
    if evidence.iter().any(|item| {
        matches!(
            item,
            AnalysisEvidence::Analyzer(analyzer) if predicate(analyzer)
        )
    }) {
        Ok(())
    } else {
        invalid_evidence(gate, "outcome lacks matching analyzer evidence")
    }
}

fn require_stage_state(
    gate: GateId,
    evidence: &[AnalysisEvidence],
    expected: EvidenceState,
) -> Result<(), HygieneError> {
    let matched = match expected {
        EvidenceState::TimedOut => has_deadline_state(evidence, EvidenceState::TimedOut),
        EvidenceState::ParseFailed => has_parse_state(evidence, EvidenceState::ParseFailed),
        state => has_stage_state(evidence, state),
    };
    if matched {
        Ok(())
    } else {
        invalid_evidence(gate, "outcome lacks matching terminal evidence")
    }
}

fn invalid_evidence<T>(gate: GateId, evidence: &str) -> Result<T, HygieneError> {
    Err(HygieneError::InvalidEvidence {
        gate,
        evidence: evidence.to_owned(),
    })
}

fn validate_policy_decision(
    gate: GateId,
    outcome: HygieneOutcome,
    findings: usize,
    evidence: &[AnalysisEvidence],
) -> Result<(), HygieneError> {
    if !matches!(outcome, HygieneOutcome::Passed | HygieneOutcome::Findings) {
        return Ok(());
    }
    let decision = evidence.iter().find_map(|item| match item {
        AnalysisEvidence::PolicyDecision {
            field,
            limit,
            observed,
        } => Some((field, *limit, *observed)),
        _ => None,
    });
    let Some((field, limit, observed)) = decision else {
        return invalid_evidence(gate, "completed outcome omitted policy decision");
    };
    if observed as usize != findings {
        return invalid_evidence(gate, "policy count differs from findings");
    }
    if !decision_field_matches(gate, field) {
        return invalid_evidence(gate, "policy decision field does not match arm");
    }
    let expected_findings = observed > limit;
    if expected_findings != (outcome == HygieneOutcome::Findings) {
        return invalid_evidence(gate, "outcome contradicts configured finding limit");
    }
    Ok(())
}

fn decision_field_matches(gate: GateId, field: &str) -> bool {
    match gate {
        GateId::Reachability => field == "policy.debt.reachability_test_only",
        GateId::AssertionFreeTests => field == "policy.debt.assertion_free_tests",
        GateId::SymbolReachability | GateId::PythonUnused | GateId::PrivateInternals => {
            field == "compiled:zero-findings"
        }
        _ => false,
    }
}

fn validate_provider_evidence(
    declaration: HygieneDeclaration,
    evidence: &[AnalysisEvidence],
    findings: &[HygieneFinding],
) -> Result<(), HygieneError> {
    let mut providers = BTreeSet::new();
    for finding in findings {
        match &finding.detail {
            HygieneFindingDetail::ModuleReachability { provider, .. }
            | HygieneFindingDetail::SymbolReachability { provider, .. }
                if provider != "python" =>
            {
                providers.insert(provider.as_str());
            }
            _ => {}
        }
    }
    if providers.is_empty() {
        return Ok(());
    }
    if declaration.provider_contract.is_none() {
        return invalid_evidence(declaration.id, "arm has no external-provider contract");
    }
    for provider in providers {
        if !evidence.iter().any(|item| {
            matches!(
                item,
                AnalysisEvidence::Analyzer(analyzer)
                    if analyzer.is_supported_provider(provider)
            )
        }) {
            return invalid_evidence(
                declaration.id,
                "provider finding lacks an exact identified provider",
            );
        }
    }
    Ok(())
}
