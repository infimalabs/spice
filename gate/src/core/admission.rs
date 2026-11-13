use super::{
    ConfigInventoryEvidence, CoreEvidence, CoreFinding, CoreInputError, CoreObservation,
    DocMeasurement, GateId, MergeIntegrityEvidence, MergeTreeCompletion,
};

pub(super) fn validate_observation(
    id: GateId,
    observation: &CoreObservation,
) -> Result<(), CoreInputError> {
    match observation {
        CoreObservation::Measured { evidence } if evidence.id() != id => Err(CoreInputError::new(
            format!("{id} received evidence for {}", evidence.id()),
        )),
        CoreObservation::Measured { evidence } => validate_evidence(evidence),
        CoreObservation::Unavailable { reason } | CoreObservation::Unmeasured { reason }
            if reason.trim().is_empty() =>
        {
            Err(CoreInputError::new(format!(
                "{id} observation reason must be non-empty"
            )))
        }
        CoreObservation::Skipped { reason } if reason.trim().is_empty() => Err(
            CoreInputError::new(format!("{id} skip reason must be non-empty")),
        ),
        CoreObservation::Skipped { .. }
            if !matches!(id, GateId::PlanningPhase | GateId::RepoShape) =>
        {
            Err(CoreInputError::new(format!(
                "{id} has no compiled applicability skip"
            )))
        }
        CoreObservation::Unsupported { reason, recovery }
            if reason.trim().is_empty() || recovery.trim().is_empty() =>
        {
            Err(CoreInputError::new(format!(
                "{id} unsupported observation requires reason and recovery"
            )))
        }
        CoreObservation::TimedOut { milliseconds: 0 } => Err(CoreInputError::new(format!(
            "{id} timeout must be greater than zero milliseconds"
        ))),
        _ => Ok(()),
    }
}

fn validate_evidence(evidence: &CoreEvidence) -> Result<(), CoreInputError> {
    match evidence {
        CoreEvidence::MergeIntegrity(value) => validate_merge(value),
        CoreEvidence::PlanningPhase(_) => Ok(()),
        CoreEvidence::RepoShape(value) => validate_findings(value.findings()),
        CoreEvidence::Staging(value) => validate_paths(value.paths()),
        CoreEvidence::RepoDocs(value) => validate_docs(value.measurements()),
        CoreEvidence::ConfigKeyValidity(value) => validate_findings(value.findings()),
        CoreEvidence::ConfigFalseDisable(value) | CoreEvidence::ConfigTrackedTrust(value) => {
            validate_inventory(value)
        }
    }
}

fn validate_merge(evidence: &MergeIntegrityEvidence) -> Result<(), CoreInputError> {
    match evidence {
        MergeIntegrityEvidence::NoMerge => Ok(()),
        MergeIntegrityEvidence::StagedContent {
            head_tree,
            staged_tree,
        } => {
            validate_git_tree(head_tree)?;
            validate_git_tree(staged_tree)?;
            if head_tree == staged_tree {
                return Err(CoreInputError::new(
                    "staged-content evidence requires distinct HEAD and staged trees",
                ));
            }
            Ok(())
        }
        MergeIntegrityEvidence::Compared {
            head_tree,
            expected_tree,
            completion,
            conflicted_paths,
        } => {
            validate_git_tree(head_tree)?;
            validate_git_tree(expected_tree)?;
            validate_paths(conflicted_paths)?;
            if *completion == MergeTreeCompletion::Clean && !conflicted_paths.is_empty() {
                return Err(CoreInputError::new(
                    "a clean merge-tree completion cannot name conflicted paths",
                ));
            }
            Ok(())
        }
    }
}

pub(super) fn validate_git_tree(tree: &str) -> Result<(), CoreInputError> {
    let valid_length = matches!(tree.len(), 40 | 64);
    let valid_bytes = tree
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    if valid_length && valid_bytes {
        Ok(())
    } else {
        Err(CoreInputError::new(format!(
            "Git tree {tree:?} is not a canonical object ID"
        )))
    }
}

fn validate_paths(paths: &[String]) -> Result<(), CoreInputError> {
    for path in paths {
        validate_repo_path(path)?;
    }
    require_strict_order(paths, "repository paths")
}

fn validate_findings(findings: &[CoreFinding]) -> Result<(), CoreInputError> {
    for finding in findings {
        if finding.code.trim().is_empty() || finding.detail.trim().is_empty() {
            return Err(CoreInputError::new(
                "core finding code and detail must be non-empty",
            ));
        }
        if let Some(path) = &finding.path {
            validate_repo_path(path)?;
        }
    }
    require_strict_order(findings, "core findings")
}

fn validate_docs(measurements: &[DocMeasurement]) -> Result<(), CoreInputError> {
    for measurement in measurements {
        validate_repo_path(&measurement.path)?;
    }
    if measurements
        .windows(2)
        .any(|pair| pair[0].path >= pair[1].path)
    {
        return Err(CoreInputError::new(
            "document measurements must be in strict path order",
        ));
    }
    Ok(())
}

fn validate_inventory(evidence: &ConfigInventoryEvidence) -> Result<(), CoreInputError> {
    for path in evidence.declared.iter().chain(&evidence.observed) {
        if path.is_empty() || path.iter().any(|part| part.trim().is_empty()) {
            return Err(CoreInputError::new(
                "configuration paths must contain non-empty segments",
            ));
        }
    }
    require_strict_order(&evidence.declared, "declared configuration paths")?;
    require_strict_order(&evidence.observed, "observed configuration paths")?;
    validate_findings(&evidence.violations)
}

fn require_strict_order<T: Ord>(values: &[T], label: &str) -> Result<(), CoreInputError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        Err(CoreInputError::new(format!(
            "{label} must be in strict canonical order"
        )))
    } else {
        Ok(())
    }
}

pub(super) fn validate_repo_path(path: &str) -> Result<(), CoreInputError> {
    let invalid = path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains(['\\', '\0'])
        || path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."));
    if invalid {
        Err(CoreInputError::new(format!(
            "repository path {path:?} is not canonical"
        )))
    } else {
        Ok(())
    }
}
