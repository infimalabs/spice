use std::fmt::Write as _;

use super::{
    CORE_REPORT_VERSION, ConfigInventoryEvidence, CoreArmInput, CoreArmReport, CoreEvidence,
    CoreInputs, CoreObservation, CoreReport, CoreStanding, MergeIntegrityEvidence,
    MergeTreeCompletion, PlanningPhaseEvidence, StagingEvidence,
};

/// Evaluate one complete immutable core-family battery.
#[must_use]
pub fn evaluate_core(inputs: &CoreInputs) -> CoreReport {
    let arms = inputs.arms().iter().map(evaluate_core_arm).collect();
    CoreReport {
        version: CORE_REPORT_VERSION,
        tree: inputs.tree().clone(),
        arms,
    }
}

pub(crate) fn evaluate_core_arm(input: &CoreArmInput) -> CoreArmReport {
    let (standing, diagnostic) = match input.observation() {
        CoreObservation::Measured { evidence } => {
            let (standing, diagnostic) = evaluate_evidence(evidence);
            (standing, diagnostic)
        }
        CoreObservation::Unavailable { reason } => (
            CoreStanding::Unavailable,
            format!("{} unavailable: {reason}", input.id()),
        ),
        CoreObservation::Unsupported { reason, recovery } => (
            CoreStanding::Unsupported,
            format!("{} unsupported: {reason}; recovery: {recovery}", input.id()),
        ),
        CoreObservation::Unmeasured { reason } => (
            CoreStanding::Unmeasured,
            format!("{} unmeasured: {reason}", input.id()),
        ),
        CoreObservation::TimedOut { milliseconds } => (
            CoreStanding::TimedOut,
            format!("{} timed out after {milliseconds}ms", input.id()),
        ),
        CoreObservation::Skipped { reason } => (
            CoreStanding::Skipped,
            format!("{} explicitly skipped: {reason}", input.id()),
        ),
    };
    CoreArmReport {
        id: input.id(),
        tree: input.tree().clone(),
        standing,
        diagnostic,
        observation: input.observation().clone(),
    }
}

fn evaluate_evidence(evidence: &CoreEvidence) -> (CoreStanding, String) {
    match evidence {
        CoreEvidence::MergeIntegrity(value) => evaluate_merge(value),
        CoreEvidence::PlanningPhase(value) => evaluate_planning_phase(value),
        CoreEvidence::RepoShape(value) => findings(
            "repo shape",
            "repo shape: ok",
            "repository shape violations",
            value.findings(),
        ),
        CoreEvidence::Staging(value) => evaluate_staging(value),
        CoreEvidence::RepoDocs(value) => evaluate_docs(value.measurements()),
        CoreEvidence::ConfigKeyValidity(value) => findings(
            "config key validity",
            "config key validity: ok",
            "invalid layered configuration",
            value.findings(),
        ),
        CoreEvidence::ConfigFalseDisable(value) => {
            evaluate_inventory("false-disable governance", value)
        }
        CoreEvidence::ConfigTrackedTrust(value) => {
            evaluate_inventory("tracked-file trust governance", value)
        }
    }
}

fn evaluate_merge(evidence: &MergeIntegrityEvidence) -> (CoreStanding, String) {
    match evidence {
        MergeIntegrityEvidence::NoMerge => passed("merge integrity: no merge in progress"),
        MergeIntegrityEvidence::StagedContent {
            head_tree,
            staged_tree,
        } => passed(format!(
            "merge integrity: staged tree {staged_tree} differs from HEAD {head_tree}"
        )),
        MergeIntegrityEvidence::Compared {
            head_tree,
            expected_tree,
            completion: MergeTreeCompletion::Clean,
            conflicted_paths: _,
        } if expected_tree == head_tree => passed(format!(
            "merge integrity: merge-tree result equals HEAD {head_tree}"
        )),
        MergeIntegrityEvidence::Compared {
            head_tree,
            expected_tree,
            completion,
            conflicted_paths,
        } => {
            let mut diagnostic = format!(
                "empty merge refused: MERGE_HEAD exists, but the staged tree still equals HEAD \
                 ({head_tree}) while git merge-tree computes {expected_tree}. Committing now would \
                 discard integrated content."
            );
            match (*completion, conflicted_paths.as_slice()) {
                (MergeTreeCompletion::Clean, _) => diagnostic.push_str(
                    " Recover without removing MERGE_HEAD by installing the computed merge tree, \
                     verify staged content, then retry git commit.",
                ),
                (MergeTreeCompletion::Conflicted, []) => diagnostic.push_str(
                    " Cannot print a safe restart recipe because git merge-tree did not name the \
                     conflicted paths. Re-run the merge manually and resolve its conflicts before \
                     committing.",
                ),
                (MergeTreeCompletion::Conflicted, paths) => {
                    let _ = write!(
                        diagnostic,
                        " The discarded merge is conflicted at: {}. Restart the merge, resolve \
                         each conflict, stage each resolution, then retry git commit with \
                         MERGE_HEAD intact.",
                        paths.join(", ")
                    );
                }
            }
            failed(diagnostic)
        }
    }
}
fn evaluate_staging(evidence: &StagingEvidence) -> (CoreStanding, String) {
    if evidence.paths().is_empty() {
        return passed("staging integrity: no partial files");
    }
    failed(format!(
        "partially staged files; stage the whole file or stash the rest:\n{}",
        evidence
            .paths()
            .iter()
            .map(|path| format!("  {path}"))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

/// Whether an active claim in `phase` forbids task-owned implementation history.
///
/// This is the one commit-authority deny-list, read from the typed phase:
/// `draft`, `plan` and `design` refuse commits, captures, and completions that
/// would publish repository work; `todo` and `review` keep their native
/// permission.
#[must_use]
pub fn lacks_commit_authority(phase: &str) -> bool {
    phase
        .parse::<wire::Phase>()
        .is_ok_and(|phase| phase.forbids_commits())
}

fn evaluate_planning_phase(evidence: &PlanningPhaseEvidence) -> (CoreStanding, String) {
    match evidence {
        PlanningPhaseEvidence::NoActiveClaim => passed("planning phase: no active claim"),
        PlanningPhaseEvidence::Active { task, phase } if lacks_commit_authority(phase.as_str()) => {
            failed(format!(
                "git commit refused: active claimed task {task} is in {} phase",
                phase.as_str()
            ))
        }
        PlanningPhaseEvidence::Active { task, phase } => passed(format!(
            "planning phase: active claimed task {task} is in {} phase",
            phase.as_str()
        )),
    }
}

fn findings(
    label: &str,
    pass_diagnostic: &str,
    failure_header: &str,
    values: &[super::CoreFinding],
) -> (CoreStanding, String) {
    if values.is_empty() {
        return passed(pass_diagnostic);
    }
    let mut diagnostic = failure_header.to_owned();
    for finding in values {
        let path = finding
            .path()
            .map_or_else(String::new, |path| format!(" {path}"));
        let _ = write!(
            diagnostic,
            "\n  {label}.{}{}: {}",
            finding.code(),
            path,
            finding.detail()
        );
    }
    failed(diagnostic)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DocumentStanding<'a> {
    Inherited(&'a crate::LatchInheritance),
    Regressed,
}

fn document_standing(measurement: &super::DocMeasurement) -> DocumentStanding<'_> {
    measurement
        .inherited()
        .map_or(DocumentStanding::Regressed, DocumentStanding::Inherited)
}

fn evaluate_docs(values: &[super::DocMeasurement]) -> (CoreStanding, String) {
    let over: Vec<_> = values
        .iter()
        .filter_map(|measurement| {
            measurement
                .limit()
                .filter(|limit| measurement.characters() > *limit)
                .map(|limit| (measurement, limit))
        })
        .collect();
    if over.is_empty() {
        return passed(format!(
            "repo-truth docs: {} document measurements within bounds",
            values.len()
        ));
    }
    let redirected = over
        .iter()
        .all(|(measurement, _)| measurement.peer_claim() || measurement.covered().is_some());
    let mut diagnostic = if redirected {
        "repo-truth docs exceed the character cap in peer-held repair seams; do not parallel-edit them:"
            .to_owned()
    } else {
        "repo-truth docs exceed the character cap:".to_owned()
    };
    for (measurement, limit) in over {
        let _ = write!(
            diagnostic,
            "\n  {}: {} characters (cap {}); over by {}; standing: ",
            measurement.path(),
            measurement.characters(),
            limit,
            measurement.characters() - limit,
        );
        match document_standing(measurement) {
            DocumentStanding::Inherited(inherited) => {
                let _ = write!(diagnostic, "{inherited}");
            }
            DocumentStanding::Regressed => diagnostic.push_str("new regression"),
        }
        if let Some(task) = measurement.covered() {
            let _ = write!(diagnostic, "; held by sweep task {task}");
        }
    }
    diagnostic.push_str(
        "\n  repair: review each whole file before editing it. Remove duplicated doctrine and \
         consolidate each rule into its canonical section or document. Move or split only \
         cohesive subjects when that improves ownership and navigation, and update every \
         affected index and link. Do not merely golf the triggering contribution; the selected \
         tree must return every listed document under its cap.",
    );
    if redirected {
        passed(diagnostic)
    } else {
        failed(diagnostic)
    }
}

fn evaluate_inventory(label: &str, evidence: &ConfigInventoryEvidence) -> (CoreStanding, String) {
    let missing: Vec<_> = evidence
        .declared()
        .iter()
        .filter(|path| !evidence.observed().contains(path))
        .collect();
    let unexpected: Vec<_> = evidence
        .observed()
        .iter()
        .filter(|path| !evidence.declared().contains(path))
        .collect();
    if missing.is_empty() && unexpected.is_empty() && evidence.violations().is_empty() {
        return passed(format!("{label}: compiled inventory matches declarations"));
    }
    let render_paths = |paths: &[&Vec<String>]| {
        if paths.is_empty() {
            "<none>".to_owned()
        } else {
            paths
                .iter()
                .map(|path| path.join("."))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let mut diagnostic = format!(
        "{label}: shared consumer inventory drift; missing={}; undeclared={}",
        render_paths(&missing),
        render_paths(&unexpected)
    );
    for violation in evidence.violations() {
        let _ = write!(diagnostic, "; {}: {}", violation.code(), violation.detail());
    }
    failed(diagnostic)
}

fn passed(diagnostic: impl Into<String>) -> (CoreStanding, String) {
    (CoreStanding::Passed, diagnostic.into())
}

fn failed(diagnostic: impl Into<String>) -> (CoreStanding, String) {
    (CoreStanding::Failed, diagnostic.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_active_non_implementation_claims_refuse_commits() {
        let task: wire::TaskHandle = "PHASE-1kRKw9Qjx".parse().expect("fixture task handle");

        for phase in [wire::Phase::Draft, wire::Phase::Plan, wire::Phase::Design] {
            let (standing, diagnostic) = evaluate_planning_phase(&PlanningPhaseEvidence::Active {
                task: task.clone(),
                phase,
            });
            assert_eq!(standing, CoreStanding::Failed, "{phase}");
            assert!(diagnostic.contains(task.as_str()), "{diagnostic}");
        }
        for phase in [wire::Phase::Todo, wire::Phase::Review] {
            let (standing, _) = evaluate_planning_phase(&PlanningPhaseEvidence::Active {
                task: task.clone(),
                phase,
            });
            assert_eq!(standing, CoreStanding::Passed, "{phase}");
        }
        let (standing, _) = evaluate_planning_phase(&PlanningPhaseEvidence::NoActiveClaim);
        assert_eq!(standing, CoreStanding::Passed);
    }

    #[test]
    fn document_standing_distinguishes_regressions_from_inherited_debt() {
        let regressed =
            super::super::DocMeasurement::new("DOCTRINE.md", 15_001, 15_000, false).unwrap();
        assert_eq!(document_standing(&regressed), DocumentStanding::Regressed);

        let inheritance = serde_json::from_str("{}").expect("unknown-origin inheritance decodes");
        let inherited = regressed.clone().with_inheritance(Some(inheritance));
        assert!(matches!(
            document_standing(&inherited),
            DocumentStanding::Inherited(_)
        ));
        for measurement in [regressed, inherited] {
            assert_eq!(
                evaluate_docs(std::slice::from_ref(&measurement)).0,
                CoreStanding::Failed,
            );
        }
    }
}
