//! Acquisition of the complete four-arm structural metric family.
//!
//! The family is acquired in full or not at all, and "in full" means four arms
//! that each carry one explicit state per declared metric. An arm whose input
//! this build cannot reach reports [`EvidenceState::Unavailable`] naming the
//! exact missing input, because the alternative — omitting the arm, or refusing
//! the whole family — spends four declared measurements to say one thing about
//! whichever of them is least ready.

use std::collections::BTreeSet;
use std::path::Path;

use super::baseline::{BaselineCapture, CommittedTexts, FileShapeAuthorship, selects};
use super::{
    ArmDeclaration, ComplexityRoutine, EvidenceError, EvidenceState, MarkdownAnchorInventory,
    MetricEvidence, MetricFinding, MetricId, MetricKey, MetricMeasurement, MetricStateBinding,
    ParseError, ParserEvidence, ParserId, ResolvedMagicMetric, STRUCTURAL_ARMS,
    StructuralArmEvidence, admit_magic_source, measure_complexity, measure_file_shape,
    resolve_magic_metric, resolve_metric_rule, scan_magic_regressions, scan_markdown_links,
    scan_rust_doc_markdown_citations,
};
use crate::core::ActorCapture;
use crate::input::Contract;
use crate::jurisdiction::{Entry, Jurisdiction, Shape};
use crate::manifest::GateId;
use crate::policy::{PathPattern, PolicyContext};
use crate::state::{BoundDecision, JudgedLatches, LatchObservation};
use crate::tree::SelectedTree;

mod bounds;
use bounds::{bounded_row, routine_evidence};

/// One immutable selected tree and the path inventory the arms read from it.
///
/// The inventory is captured once and shared, rather than walked per arm: the
/// Markdown arm resolves link targets against the whole tracked set, so two
/// walks could disagree about what a link points at.
///
/// The committed baselines are borrowed rather than read here, because they are
/// the one input no selected tree holds: magic deltas and authored structural
/// debt compare the selection against the live repository's `HEAD`, which the
/// capture that materialized this tree is the last thing allowed to observe.
///
/// The acting actor and durable latches are borrowed for the same reason.
/// Latch state selects base versus the one policy-compiled elastic limit;
/// actor identity remains on the transaction observation only for shared
/// flex-claim arbitration after an over-base measurement.
#[derive(Debug)]
pub struct MetricSource<'a> {
    root: &'a Path,
    jurisdiction: Jurisdiction,
    contract: &'a Contract,
    baselines: &'a BaselineCapture,
    actor: &'a ActorCapture,
    latches: &'a JudgedLatches,
    reached: Option<BTreeSet<String>>,
}

impl<'a> MetricSource<'a> {
    /// Capture the tracked structural jurisdiction over one selected tree.
    ///
    /// # Errors
    ///
    /// Refuses a walk that cannot complete over the selected tree.
    // Every argument past the tree is an observation of the live repository or
    // the run's state authority that no selected tree can supply, so there is
    // nothing here to group into a narrower type that would not just be this
    // list under one name.
    pub fn capture(
        tree: &'a SelectedTree,
        contract: &'a Contract,
        baselines: &'a BaselineCapture,
        actor: &'a ActorCapture,
        latches: &'a JudgedLatches,
    ) -> Result<Self, MetricAcquisitionError> {
        Self::capture_inner(tree, contract, baselines, actor, latches, None)
    }

    /// Capture one selected arm's authored path reach while retaining the
    /// complete tracked inventory needed for cross-file resolution.
    pub(crate) fn capture_reached(
        tree: &'a SelectedTree,
        contract: &'a Contract,
        baselines: &'a BaselineCapture,
        actor: &'a ActorCapture,
        latches: &'a JudgedLatches,
        reached: &[String],
    ) -> Result<Self, MetricAcquisitionError> {
        Self::capture_inner(tree, contract, baselines, actor, latches, Some(reached))
    }

    fn capture_inner(
        tree: &'a SelectedTree,
        contract: &'a Contract,
        baselines: &'a BaselineCapture,
        actor: &'a ActorCapture,
        latches: &'a JudgedLatches,
        reached: Option<&[String]>,
    ) -> Result<Self, MetricAcquisitionError> {
        let jurisdiction = Jurisdiction::walk(tree.root(), contract.policy().ignore())
            .map_err(|error| MetricAcquisitionError::Source(format!("metric walk: {error}")))?;
        Ok(Self {
            root: tree.root(),
            jurisdiction,
            contract,
            baselines,
            actor,
            latches,
            reached: reached.map(|paths| paths.iter().cloned().collect()),
        })
    }

    /// Immutable selected-tree root every arm reads.
    #[must_use]
    pub const fn root(&self) -> &Path {
        self.root
    }

    /// Every regular file the walk reached, admitted or not, in path order.
    ///
    /// Refused paths belong here even though nothing measures them. The selected
    /// tree holds tracked content only, so this is the tracked inventory a link
    /// target resolves against, and a link pointing into an ignored directory
    /// still names a real tracked file whose case can be wrong.
    fn tracked(&self) -> Vec<String> {
        self.jurisdiction
            .entries()
            .iter()
            .filter(|entry| matches!(entry.shape(), Shape::File))
            .map(|entry| entry.path().to_owned())
            .collect()
    }

    /// Every regular file a metric may measure, in path order.
    fn measurable(&self) -> Vec<&str> {
        self.jurisdiction
            .measurable()
            .map(Entry::path)
            .filter(|path| {
                self.reached
                    .as_ref()
                    .is_none_or(|reached| reached.contains(*path))
            })
            .collect()
    }
}

/// Acquire all four structural arms from one captured source.
///
/// # Errors
///
/// Returns the evidence refusal that prevented one complete four-arm vector. A
/// file that cannot be read or parsed is not one of these: it is that file's
/// own state, carried in the arm alongside the files that did measure.
pub fn acquire_metrics(
    source: &MetricSource<'_>,
) -> Result<Vec<StructuralArmEvidence>, MetricAcquisitionError> {
    acquire_metrics_with_bounds(source).evidence
}

/// Acquire the family while retaining the exact bounds its measured rows used.
///
/// The public acquisition surface returns evidence alone. The battery capture
/// additionally needs the decisions the file-shape and routine-complexity rows
/// were judged against so it can publish them in the run context without
/// resolving policy a second time. Keeping both products of this one traversal
/// together is what makes that report evidence rather than a reconstruction.
pub(crate) fn acquire_metrics_with_bounds(source: &MetricSource<'_>) -> MetricAcquisition {
    let mut bounds = Vec::new();
    let mut observations = Vec::new();
    let evidence = acquire_metric_evidence(source, &mut bounds, &mut observations);
    MetricAcquisition {
        evidence,
        bounds,
        observations,
    }
}

/// Acquire exactly one structural arm and the bounds that arm decided.
pub(crate) fn acquire_metric_arm_with_bounds(
    source: &MetricSource<'_>,
    gate_id: GateId,
) -> MetricArmAcquisition {
    let mut bounds = Vec::new();
    let mut observations = Vec::new();
    let evidence = STRUCTURAL_ARMS
        .into_iter()
        .find(|declaration| declaration.id() == gate_id)
        .ok_or(EvidenceError::ForeignGate(gate_id).into())
        .and_then(|declaration| {
            acquire_arm_evidence(source, declaration, &mut bounds, &mut observations)
        });
    MetricArmAcquisition {
        evidence,
        bounds,
        observations,
    }
}

/// Both products of one structural-family acquisition.
pub(crate) struct MetricAcquisition {
    evidence: Result<Vec<StructuralArmEvidence>, MetricAcquisitionError>,
    bounds: Vec<BoundDecision>,
    observations: Vec<LatchObservation>,
}

impl MetricAcquisition {
    pub(crate) fn into_parts(
        self,
    ) -> (
        Result<Vec<StructuralArmEvidence>, MetricAcquisitionError>,
        Vec<BoundDecision>,
        Vec<LatchObservation>,
    ) {
        (self.evidence, self.bounds, self.observations)
    }
}

/// Both products of one selected structural-arm acquisition.
pub(crate) struct MetricArmAcquisition {
    evidence: Result<StructuralArmEvidence, MetricAcquisitionError>,
    bounds: Vec<BoundDecision>,
    observations: Vec<LatchObservation>,
}

impl MetricArmAcquisition {
    pub(crate) fn into_parts(
        self,
    ) -> (
        Result<StructuralArmEvidence, MetricAcquisitionError>,
        Vec<BoundDecision>,
        Vec<LatchObservation>,
    ) {
        (self.evidence, self.bounds, self.observations)
    }
}

fn acquire_metric_evidence(
    source: &MetricSource<'_>,
    bounds: &mut Vec<BoundDecision>,
    observations: &mut Vec<LatchObservation>,
) -> Result<Vec<StructuralArmEvidence>, MetricAcquisitionError> {
    let mut arms = Vec::with_capacity(STRUCTURAL_ARMS.len());
    for declaration in STRUCTURAL_ARMS {
        arms.push(acquire_arm_evidence(
            source,
            declaration,
            bounds,
            observations,
        )?);
    }
    Ok(arms)
}

fn acquire_arm_evidence(
    source: &MetricSource<'_>,
    declaration: ArmDeclaration,
    bounds: &mut Vec<BoundDecision>,
    observations: &mut Vec<LatchObservation>,
) -> Result<StructuralArmEvidence, MetricAcquisitionError> {
    let metrics = match declaration.id() {
        GateId::Complexity => complexity_metrics(source, declaration, bounds, observations)?,
        GateId::FileShape => file_shape_metrics(source, declaration, bounds, observations)?,
        GateId::MagicNumbers => magic_metrics(source, declaration)?,
        GateId::MarkdownLinks => markdown_metrics(source)?,
        _ => unavailable(declaration, unreached_input(declaration.id()))?,
    };
    StructuralArmEvidence::new(declaration.id(), metrics).map_err(Into::into)
}

/// The exact input an arm needs and this build cannot reach.
///
/// Every structural arm now acquires, so all four say that rather than naming a
/// missing input. Reaching one of them would mean the dispatch above stopped
/// covering an arm it names, and the row says so instead of reporting an
/// unavailability that has no cause. The last is for a gate declaring no
/// structural arm at all, which `STRUCTURAL_ARMS` holds none of.
const fn unreached_input(gate_id: GateId) -> &'static str {
    match gate_id {
        GateId::Complexity => "the complexity arm is acquired rather than reported unreached",
        GateId::FileShape => "the file-shape arm is acquired rather than reported unreached",
        GateId::MagicNumbers => "the magic-number arm is acquired rather than reported unreached",
        GateId::MarkdownLinks => "the Markdown arm is acquired rather than reported unreached",
        _ => "this gate declares no structural arm",
    }
}

/// One whole-arm row per declared metric, each naming the same missing input.
///
/// The row count follows the declaration rather than a fixed one, because
/// `StructuralArmEvidence` requires every declared metric present, and the two
/// paired arms additionally require both of theirs to carry identical keys.
fn unavailable(
    declaration: ArmDeclaration,
    reason: &str,
) -> Result<Vec<MetricEvidence>, MetricAcquisitionError> {
    declared_rows(
        declaration,
        &MetricKey::Arm,
        &EvidenceState::Unavailable {
            reason: reason.to_owned(),
        },
    )
}

/// Every metric an arm declares, carrying one shared state under one key.
///
/// Both callers need the same guarantee for different reasons: a whole-arm row
/// set has to cover the declaration, and a per-path row set has to cover it
/// with identical keys. Building both here is what makes a paired arm's
/// keysets equal by construction rather than by two call sites agreeing.
fn declared_rows(
    declaration: ArmDeclaration,
    key: &MetricKey,
    state: &EvidenceState,
) -> Result<Vec<MetricEvidence>, MetricAcquisitionError> {
    declaration
        .metrics()
        .iter()
        .map(|metric| {
            MetricEvidence::new(*metric, key.clone(), state.clone())
                .map_err(MetricAcquisitionError::Evidence)
        })
        .collect()
}

/// Measure every selected source file's shape against its own elastic bound.
///
/// The file class is tested here for the reason the other two acquiring arms
/// test theirs: a row per unselected path would bury the sources the arm is
/// about. What is additionally selected out is a repository-truth document,
/// even when its suffix is in the class. Those answer to a character bound on
/// the repository-document arm, and a document measured here as well would be
/// held to two limits at once and could fail its file-shape one while passing
/// the bound that arm actually holds it to.
fn file_shape_metrics(
    source: &MetricSource<'_>,
    declaration: ArmDeclaration,
    bounds: &mut Vec<BoundDecision>,
    observations: &mut Vec<LatchObservation>,
) -> Result<Vec<MetricEvidence>, MetricAcquisitionError> {
    match source.baselines {
        BaselineCapture::Reached(_) => {}
        BaselineCapture::Unreachable(reason) => {
            return unavailable(
                declaration,
                &format!("committed file-shape parents could not be read: {reason}"),
            );
        }
    };
    let policy = source.contract.rule_policy();
    let suffixes = policy.document().file_classes().file_shape();
    let generated = source
        .contract
        .shape()
        .generated_paths()
        .iter()
        .map(|pattern| {
            PathPattern::tracked(pattern).map_err(|error| {
                MetricAcquisitionError::Source(format!("policy.generated_paths: {error}"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut metrics = Vec::new();
    for path in source.measurable() {
        if !selects(path, suffixes) || generated.iter().any(|pattern| pattern.matches(path)) {
            continue;
        }
        let document = policy.selects_repository_document(path).map_err(|error| {
            MetricAcquisitionError::Source(format!(
                "repository-document selection for {path:?}: {error}"
            ))
        })?;
        if document {
            continue;
        }
        let key = MetricKey::path(path).map_err(MetricAcquisitionError::Evidence)?;
        metrics.extend(file_shape_rows(
            source,
            declaration,
            path,
            &key,
            bounds,
            observations,
        )?);
    }
    if metrics.is_empty() {
        return declared_rows(
            declaration,
            &MetricKey::Arm,
            &EvidenceState::NotApplicable {
                reason: "the selected tree admits no declared source file class".to_owned(),
            },
        );
    }
    Ok(metrics)
}

/// One selected file's paired rows: its measured shape, or why it has none.
///
/// A file that cannot be read or decoded reports that once and both rows carry
/// it, because the two metrics answer for the same bytes: a lines row that
/// measured beside a bytes row that refused would claim this file was half
/// observed, and the arm would report a keyset its declaration forbids.
///
/// A file a pending merge inherits verbatim from a parent or from Git's exact
/// clean merge tree reports both rows the same way, and for the same reason it
/// takes no bound decision: this run is not where that shape was authored, so
/// measuring it would hold the merging lane to a limit and hand it the flex
/// slice for a file it never wrote. A conflict or one changed byte matches
/// neither source and remains merge-authored.
fn file_shape_rows(
    source: &MetricSource<'_>,
    declaration: ArmDeclaration,
    path: &str,
    key: &MetricKey,
    bounds: &mut Vec<BoundDecision>,
    observations: &mut Vec<LatchObservation>,
) -> Result<Vec<MetricEvidence>, MetricAcquisitionError> {
    let bytes = match std::fs::read(source.root().join(path)) {
        Ok(bytes) => bytes,
        Err(error) => {
            return declared_rows(
                declaration,
                key,
                &EvidenceState::Unavailable {
                    reason: format!("selected source file could not be read: {error}"),
                },
            );
        }
    };
    let inherited = match source.baselines.file_shape_authorship(path, &bytes) {
        FileShapeAuthorship::Parent => Some(format!(
            "{path:?} is byte-identical to a parent of this merge, so its shape was \
             authored by the commit that already answered for it"
        )),
        FileShapeAuthorship::CleanMerge => Some(format!(
            "{path:?} is byte-identical to Git's clean merge result, so its shape is \
             inherited synthesis rather than work authored by the merge receiver"
        )),
        FileShapeAuthorship::MergeAuthored => None,
    };
    if let Some(reason) = inherited {
        return declared_rows(declaration, key, &EvidenceState::NotApplicable { reason });
    }
    let shape = match measure_file_shape(path, &bytes) {
        Ok(shape) => shape,
        Err(error) => return declared_rows(declaration, key, &error.into_evidence_state()),
    };
    let committed_shape = source
        .baselines
        .baseline_bytes(path, &bytes)
        .filter(|committed| *committed != bytes)
        .and_then(|committed| measure_file_shape(path, committed).ok());
    let mut rows = Vec::with_capacity(2);
    for (metric, value, committed) in [
        (
            MetricId::FileLines,
            shape.lines(),
            committed_shape.as_ref().map(|shape| shape.lines()),
        ),
        (
            MetricId::FileBytes,
            shape.bytes(),
            committed_shape.as_ref().map(|shape| shape.bytes()),
        ),
    ] {
        let (row, observed) = bounded_row(source, path, key, metric, value, committed)?;
        rows.push(row);
        if let Some((decision, observation)) = observed {
            bounds.push(decision);
            observations.push(observation);
        }
    }
    Ok(rows)
}

/// Measure Markdown links against the tracked path and anchor inventory.
///
/// The file class is tested here rather than left to the scanner even though the
/// scanner tests it too: offering it every measurable path would turn each of
/// this repository's non-Markdown files into its own `NotApplicable` row. The
/// arm would still fold to the same state, because a measurement outranks a row
/// that had nothing to measure, but every one of those rows would be retained
/// and reported, burying the Markdown the arm is about.
fn markdown_metrics(
    source: &MetricSource<'_>,
) -> Result<Vec<MetricEvidence>, MetricAcquisitionError> {
    let tracked = source.tracked();
    let anchors = markdown_anchor_inventory(source.root(), &tracked)?;
    let mut metrics = Vec::new();
    let mut markdown_sources = 0_u64;
    for path in source.measurable() {
        if !path.to_ascii_lowercase().ends_with(".md") {
            continue;
        }
        markdown_sources = markdown_sources.saturating_add(1);
        let state = markdown_state(source.root(), path, &tracked, &anchors);
        let key = MetricKey::path(path).map_err(MetricAcquisitionError::Evidence)?;
        metrics.push(
            MetricEvidence::new(MetricId::MarkdownLinkInvalidTargets, key, state)
                .map_err(MetricAcquisitionError::Evidence)?,
        );
    }
    if markdown_sources == 0 {
        metrics.push(not_applicable_markdown_metric(
            MetricId::MarkdownLinkInvalidTargets,
            "the selected tree admits no Markdown source",
        )?);
    }

    let mut cited_rust_sources = 0_u64;
    for path in source.measurable() {
        if !path.to_ascii_lowercase().ends_with(".rs") {
            continue;
        }
        let Some(state) = rust_doc_markdown_state(source.root(), path, &tracked) else {
            continue;
        };
        cited_rust_sources = cited_rust_sources.saturating_add(1);
        let key = MetricKey::path(path).map_err(MetricAcquisitionError::Evidence)?;
        metrics.push(
            MetricEvidence::new(MetricId::RustDocMarkdownTargetAbsences, key, state)
                .map_err(MetricAcquisitionError::Evidence)?,
        );
    }
    if cited_rust_sources == 0 {
        metrics.push(not_applicable_markdown_metric(
            MetricId::RustDocMarkdownTargetAbsences,
            "the selected tree admits no Rust doc-comment Markdown citation",
        )?);
    }
    Ok(metrics)
}

/// The row one Markdown metric owes when its own source class is absent.
fn not_applicable_markdown_metric(
    metric: MetricId,
    reason: &str,
) -> Result<MetricEvidence, MetricAcquisitionError> {
    let state = EvidenceState::NotApplicable {
        reason: reason.to_owned(),
    };
    MetricEvidence::new(metric, MetricKey::Arm, state).map_err(MetricAcquisitionError::Evidence)
}

fn markdown_anchor_inventory(
    root: &Path,
    tracked: &[String],
) -> Result<MarkdownAnchorInventory, MetricAcquisitionError> {
    let mut inventory = MarkdownAnchorInventory::default();
    for path in tracked {
        if !path.to_ascii_lowercase().ends_with(".md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(path)) else {
            continue;
        };
        inventory.insert(path, &text).map_err(|error| {
            MetricAcquisitionError::Source(format!(
                "could not index tracked Markdown anchors for {path:?}: {error}"
            ))
        })?;
    }
    Ok(inventory)
}

/// One Markdown source's own state: its finding count, or why it has none.
///
/// A file that cannot be read is reported as unavailable rather than skipped or
/// counted zero, because a zero here is indistinguishable from a clean file and
/// would let an unreadable source pass as one.
fn markdown_state(
    root: &Path,
    path: &str,
    tracked: &[String],
    anchors: &MarkdownAnchorInventory,
) -> EvidenceState {
    let text = match std::fs::read_to_string(root.join(path)) {
        Ok(text) => text,
        Err(error) => {
            return EvidenceState::Unavailable {
                reason: format!("selected Markdown source could not be read: {error}"),
            };
        }
    };
    match scan_markdown_links(path, &text, tracked, anchors) {
        Ok(findings) => EvidenceState::measured(
            MetricMeasurement::new(
                findings.len() as u64,
                ParserEvidence::Native {
                    parser: ParserId::MarkdownTrackedTargetsV1,
                },
            )
            .with_findings(
                findings
                    .iter()
                    .map(|finding| MetricFinding {
                        path: finding.source_path().to_owned(),
                        line: finding.line(),
                        // The link as written, not where it resolved to. A
                        // reader searching the file searches for what is in it.
                        target: finding.raw_target().to_owned(),
                        expected: Some(finding.expected().to_owned()),
                    })
                    .collect(),
            ),
        ),
        Err(error) => error.into_evidence_state(),
    }
}

/// One Rust source containing doc-comment citations, or why it was unreadable.
///
/// A clean Rust source with no citations contributes no row; the arm emits one
/// whole-metric non-applicability row only if no selected Rust source cites a
/// Markdown path. An unreadable source must contribute its refusal because the
/// acquisition cannot know whether it contained a citation.
fn rust_doc_markdown_state(root: &Path, path: &str, tracked: &[String]) -> Option<EvidenceState> {
    let text = match std::fs::read_to_string(root.join(path)) {
        Ok(text) => text,
        Err(error) => {
            return Some(EvidenceState::Unavailable {
                reason: format!(
                    "selected Rust source could not be read for doc citations: {error}"
                ),
            });
        }
    };
    match scan_rust_doc_markdown_citations(path, &text, tracked) {
        Ok(scan) if scan.citations() == 0 => None,
        Ok(scan) => Some(EvidenceState::measured(
            MetricMeasurement::new(
                scan.findings().len() as u64,
                ParserEvidence::Native {
                    parser: ParserId::MarkdownTrackedTargetsV1,
                },
            )
            .with_findings(
                scan.findings()
                    .iter()
                    .map(|finding| MetricFinding {
                        path: finding.source_path().to_owned(),
                        line: finding.line(),
                        target: finding.target_path().to_owned(),
                        // An absence has no correct spelling to offer: the
                        // target is untracked, not misspelled.
                        expected: None,
                    })
                    .collect(),
            ),
        )),
        Err(error) => Some(error.into_evidence_state()),
    }
}

/// Measure new comparison-adjacent literals against each path's committed text.
///
/// The file class is tested here for the same reason the Markdown arm tests its
/// own: offering the scanner every measurable path would bury the arm under one
/// `NotApplicable` row per non-source file. What is deliberately *not* filtered
/// out is a declared class this parser cannot read exactly, because that path is
/// in scope and unmeasured, and the arm has to say so.
fn magic_metrics(
    source: &MetricSource<'_>,
    declaration: ArmDeclaration,
) -> Result<Vec<MetricEvidence>, MetricAcquisitionError> {
    let baselines = match source.baselines {
        BaselineCapture::Reached(baselines) => baselines,
        BaselineCapture::Unreachable(reason) => {
            return unavailable(
                declaration,
                &format!("committed magic-number baselines could not be read: {reason}"),
            );
        }
    };
    let suffixes = source
        .contract
        .rule_policy()
        .document()
        .file_classes()
        .magic();
    let mut metrics = Vec::new();
    for path in source.measurable() {
        if !selects(path, suffixes) {
            continue;
        }
        let state = magic_state(source, path, baselines);
        let key = MetricKey::path(path).map_err(MetricAcquisitionError::Evidence)?;
        metrics.push(
            MetricEvidence::new(MetricId::MagicNumberRegressions, key, state)
                .map_err(MetricAcquisitionError::Evidence)?,
        );
    }
    if metrics.is_empty() {
        return not_applicable_magic();
    }
    Ok(metrics)
}

/// Measure every routine in the selected complexity scope.
///
/// The two metrics are acquired together rather than in two passes because they
/// are read from one parse of one file: measuring them apart would let a routine
/// contribute its length to one metric and, after a re-parse, not exist for the
/// other. The arm requires both metrics to carry identical keys, and one parse
/// is what makes that true rather than checked.
///
/// A path whose class has no vendored grammar carries that refusal as its own
/// pair of rows, so a language the policy selects and this build cannot parse is
/// named instead of measuring as a file with no routines, which is what a silent
/// skip and a clean parse of an empty file are indistinguishable from.
fn complexity_metrics(
    source: &MetricSource<'_>,
    declaration: ArmDeclaration,
    bounds: &mut Vec<BoundDecision>,
    observations: &mut Vec<LatchObservation>,
) -> Result<Vec<MetricEvidence>, MetricAcquisitionError> {
    if let BaselineCapture::Unreachable(reason) = source.baselines {
        return unavailable(
            declaration,
            &format!("committed complexity parents could not be read: {reason}"),
        );
    }
    let suffixes = source
        .contract
        .rule_policy()
        .document()
        .file_classes()
        .complexity();
    let mut metrics = Vec::new();
    for path in source.measurable() {
        if !selects(path, suffixes) {
            continue;
        }
        complexity_rows(source, path, &mut metrics, bounds, observations)?;
    }
    if metrics.is_empty() {
        return not_applicable_complexity();
    }
    Ok(metrics)
}

/// One selected file's rows and the exact bounds each measured routine used.
///
/// A refused routine contributes its paired unavailable rows and no decision;
/// a measured routine contributes both. Appending here keeps evidence and
/// decisions in the same parse traversal and the same canonical metric order.
/// A merge-inherited file contributes neither because this commit authored no
/// routine in it and therefore owns no complexity or bound decision for it. A
/// changed routine whose committed value was already over base is held to base,
/// while an unchanged committed breach retains the flex it previously earned.
fn complexity_rows(
    source: &MetricSource<'_>,
    path: &str,
    metrics: &mut Vec<MetricEvidence>,
    bounds: &mut Vec<BoundDecision>,
    observations: &mut Vec<LatchObservation>,
) -> Result<(), MetricAcquisitionError> {
    let current = match std::fs::read(source.root().join(path)) {
        Ok(current) => current,
        Err(error) => {
            let state = EvidenceState::Unavailable {
                reason: format!("selected complexity source could not be read: {error}"),
            };
            return whole_file_complexity(path, &state, metrics);
        }
    };
    if source.baselines.merge_inherits(path, &current) {
        return Ok(());
    }
    let committed = committed_complexity(source, path, &current);
    let routines = match read_complexity(path, &current) {
        Ok(routines) => routines,
        Err(state) => return whole_file_complexity(path, &state, metrics),
    };
    for routine in routines {
        let prior = committed
            .iter()
            .find(|candidate| candidate.function() == routine.function());
        for (metric, value, committed) in [
            (
                MetricId::RoutineCcn,
                routine.ccn(),
                prior.map(ComplexityRoutine::ccn),
            ),
            (
                MetricId::RoutineLength,
                routine.length(),
                prior.map(ComplexityRoutine::length),
            ),
        ] {
            let (evidence, observed) =
                routine_evidence(source, path, routine.function(), metric, value, committed)?;
            metrics.push(evidence);
            if let Some((decision, observation)) = observed {
                bounds.push(decision);
                observations.push(observation);
            }
        }
    }
    Ok(())
}

/// Parse the committed version of one current complexity source when reachable.
///
/// A missing or unparseable committed version supplies no prior metric value, so no
/// inferred latch can be formed. The current parse reports its own refusal
/// separately.
fn committed_complexity(
    source: &MetricSource<'_>,
    path: &str,
    current: &[u8],
) -> Vec<ComplexityRoutine> {
    source
        .baselines
        .baseline_bytes(path, current)
        .filter(|committed| *committed != current)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(|text| measure_complexity(path, text).ok())
        .unwrap_or_default()
}

/// Parse one selected file, or the state explaining why it was not measured.
fn read_complexity(path: &str, bytes: &[u8]) -> Result<Vec<ComplexityRoutine>, EvidenceState> {
    let text = std::str::from_utf8(bytes).map_err(|error| EvidenceState::Unavailable {
        reason: format!("selected complexity source could not be read: {error}"),
    })?;
    measure_complexity(path, text).map_err(ParseError::into_evidence_state)
}

/// A file the parser refused speaks once per metric, on one shared key.
///
/// The key names the file and no routine, because the refusal is about the file:
/// naming a routine would invent an identity the parse never produced. Both
/// metrics carry it, since the arm admits no row present in one and absent from
/// the other.
fn whole_file_complexity(
    path: &str,
    state: &EvidenceState,
    metrics: &mut Vec<MetricEvidence>,
) -> Result<(), MetricAcquisitionError> {
    let key =
        MetricKey::function(path, WHOLE_FILE_ROUTINE).map_err(MetricAcquisitionError::Evidence)?;
    for metric in [MetricId::RoutineCcn, MetricId::RoutineLength] {
        metrics.push(
            MetricEvidence::new(metric, key.clone(), state.clone())
                .map_err(MetricAcquisitionError::Evidence)?,
        );
    }
    Ok(())
}

/// The function position of a row that speaks for a file rather than a routine.
const WHOLE_FILE_ROUTINE: &str = "<file>";

/// The rows a tree with no named routine in a supported source still owes its arm.
fn not_applicable_complexity() -> Result<Vec<MetricEvidence>, MetricAcquisitionError> {
    let mut metrics = Vec::new();
    for metric in [MetricId::RoutineCcn, MetricId::RoutineLength] {
        let state = EvidenceState::NotApplicable {
            reason:
                "the selected tree contains no named routine in a supported complexity source class"
                    .to_owned(),
        };
        metrics.push(
            MetricEvidence::new(metric, MetricKey::Arm, state)
                .map_err(MetricAcquisitionError::Evidence)?,
        );
    }
    Ok(metrics)
}

/// The one row a tree with no declared magic source still owes its arm.
fn not_applicable_magic() -> Result<Vec<MetricEvidence>, MetricAcquisitionError> {
    let state = EvidenceState::NotApplicable {
        reason: "the selected tree admits no declared magic-number source class".to_owned(),
    };
    let evidence = MetricEvidence::new(MetricId::MagicNumberRegressions, MetricKey::Arm, state)
        .map_err(MetricAcquisitionError::Evidence)?;
    Ok(vec![evidence])
}

/// One source file's own state: its regression count, or why it has none.
///
/// Two different obstructions can reach one row — a class this build cannot
/// read, and a path `HEAD` does not carry — and this is where the order between
/// them is chosen. **The class is asked first.** Which classes are readable is a
/// property of the compiled parser and holds of the path either way, while
/// having no baseline is a property of one commit pair and stops being true at
/// the next commit, after which the same file reports the class refusal anyway.
/// Asking absence first would report the obstruction that expires and withhold
/// the one that does not, withhold it precisely for newly added files, and those
/// are how an unreadable class enters a tree in the first place. It is also the
/// only order under which the arm can say what `magic_metrics` keeps these paths
/// in scope to say: inapplicability may speak for an arm only when it speaks for
/// every row, so a tree adding its first `.py` beside readable sources would fold
/// to `Passed` with an unreadable class sitting in scope unmeasured, and a tree
/// whose whole magic surface is newly added would fold to `NotApplicable` and
/// mint skip provenance over it. Of the two, only the class refusal outranks a
/// measurement.
///
/// A path no parent or pending merge base carries is measured against explicit
/// empty text. That is the only committed content a newly added file can have
/// replaced, and it keeps new supported sources inside the same zero-regression
/// policy as changed sources: a file born without comparison-adjacent literals
/// passes, while one born with them reports its exact path and count.
///
/// The current text is read before the baseline is asked for, because which
/// parent answers depends on it: a merge is measured against the parent holding
/// this file verbatim when one does. A file that cannot be read has no current
/// side either, so it reports that rather than an absence it never established.
fn magic_state(source: &MetricSource<'_>, path: &str, baselines: &CommittedTexts) -> EvidenceState {
    if let Err(unreadable) = admit_magic_source(path) {
        return unreadable.into_evidence_state();
    }
    let current = match std::fs::read_to_string(source.root().join(path)) {
        Ok(text) => text,
        Err(error) => {
            return EvidenceState::Unavailable {
                reason: format!("selected magic-number source could not be read: {error}"),
            };
        }
    };
    let baseline = baselines.baseline(path, current.as_bytes()).unwrap_or("");
    let resolved = match resolve_magic_metric(
        source.contract.rule_policy(),
        &PolicyContext::new().with_path(path),
    ) {
        Ok(resolved) => resolved,
        Err(error) => {
            return EvidenceState::Unavailable {
                reason: format!("magic threshold did not resolve for {path:?}: {error}"),
            };
        }
    };
    measured_magic(path, &current, baseline, &resolved)
}

/// The delta itself, once both sides of the comparison are in hand.
fn measured_magic(
    path: &str,
    current: &str,
    baseline: &str,
    resolved: &ResolvedMagicMetric,
) -> EvidenceState {
    match scan_magic_regressions(path, current, Some(baseline), resolved.threshold()) {
        Ok(findings) => EvidenceState::measured(resolved.measurement(
            findings.len() as u64,
            ParserEvidence::Native {
                parser: ParserId::CGrammarMagicDeltaV1,
            },
        )),
        Err(error) => error.into_evidence_state(),
    }
}

/// Why one complete structural family could not be assembled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetricAcquisitionError {
    /// The selected tree could not be walked.
    Source(String),
    /// Assembled evidence violated its compiled declaration.
    Evidence(EvidenceError),
}

impl From<EvidenceError> for MetricAcquisitionError {
    fn from(error: EvidenceError) -> Self {
        Self::Evidence(error)
    }
}

impl std::fmt::Display for MetricAcquisitionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(reason) => formatter.write_str(reason),
            Self::Evidence(error) => write!(formatter, "structural metric evidence: {error}"),
        }
    }
}

impl std::error::Error for MetricAcquisitionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source(_) => None,
            Self::Evidence(error) => Some(error),
        }
    }
}
