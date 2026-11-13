use super::*;

pub(super) fn ref_state(object: Option<&str>) -> dist::RefState<'_> {
    object.map_or(dist::RefState::Absent, dist::RefState::Present)
}

pub(super) fn package_state(identity: Digest, object: String) -> dist::OperationState {
    dist::OperationState::Package {
        identity: Some(identity),
        object: Some(object),
    }
}

pub(super) fn stable_ref_observation(
    previewed: Option<&String>,
    current: Option<String>,
    release: &str,
) -> Option<String> {
    match current {
        Some(object) if object == release => previewed.cloned(),
        other => other,
    }
}

pub(super) fn admitted_local_progress(
    plan: &ReleasePlan,
    head: &str,
    version: &dist::ProductVersion,
    status: &str,
) -> bool {
    plan.source_commit == plan.release_commit
        && head == plan.source_commit
        && version == &plan.requested_version
        && status.is_empty()
}

#[cfg(unix)]
pub(super) fn make_executable(path: &Path) -> Result<(), dist::EffectError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|error| dist::EffectError::Environment(error.to_string()))
}

#[cfg(not(unix))]
pub(super) fn make_executable(_: &Path) -> Result<(), dist::EffectError> {
    Err(dist::EffectError::Environment(
        "release probe installation requires Unix executable modes".to_owned(),
    ))
}

pub(super) fn object_exists(root: &Path, object: &str) -> Result<bool, dist::EffectError> {
    let output = Command::new("git")
        .args(["cat-file", "-e", &format!("{object}^{{tree}}")])
        .current_dir(root)
        .output()
        .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
    Ok(output.status.success())
}

pub(super) fn run(root: &Path, program: &str, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| format!("cannot launch {program}: {error}"))?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("{program} stdout is not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("{program} stderr is not UTF-8: {error}"))?;
    if output.status.success() {
        Ok(stdout.trim().to_owned())
    } else {
        let detail = [stdout.trim(), stderr.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        Err(format!(
            "{program} {} exited {:?}: {detail}",
            arguments.join(" "),
            output.status.code()
        ))
    }
}

pub(crate) fn execute(
    root: &Path,
    journal_root: &Path,
    plan: &ReleasePlan,
    applied: &Digest,
    ambient: &Ambient,
) -> Result<(), Outcome<wire::intent::RecognitionAnswer>> {
    let mut journal = FileReleaseJournal::open(journal_root, plan).map_err(journal_fault)?;
    let mut effects = SystemEffects::new(root, journal_root, ambient).map_err(effect_outcome)?;
    dist::ReleaseExecutor::execute(plan, applied, &mut journal, &mut effects)
        .map(|_| ())
        .map_err(execute_outcome)
}

pub(crate) fn verify_completed(
    journal_root: &Path,
    key: &Digest,
    manifest: &Digest,
    notes: &Digest,
    targets: &[wire::body::PublishedTarget],
) -> Result<(), String> {
    let path = journal_root.join(format!("{key}.json"));
    let bytes =
        fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let document: JournalDocument = wire::decode(&bytes).map_err(|error| error.to_string())?;
    let canonical = wire::encode(&document).map_err(|error| error.to_string())?;
    if bytes != canonical || document.schema != JOURNAL_SCHEMA || document.plan.key != *key {
        return Err("external release journal identity or encoding moved".to_owned());
    }
    dist::verify_plan(&document.plan).map_err(|error| error.to_string())?;
    if document.plan.manifest != *manifest
        || document.plan.notes != *notes
        || document.plan.targets != targets
    {
        return Err("external release journal disagrees with the resident command".to_owned());
    }
    validate_document(&document)?;
    let accepted = document
        .accepted
        .iter()
        .map(|row| row.operation)
        .collect::<Vec<_>>();
    if accepted.as_slice() != document.plan.operations.as_slice() {
        return Err("external release acceptance chain is incomplete or reordered".to_owned());
    }
    dist::ReleaseExecutor::verify_completion(&document.plan, &document.receipts)
        .map_err(|error| error.to_string())
}

pub(super) fn execute_outcome(
    error: dist::ExecuteError,
) -> Outcome<wire::intent::RecognitionAnswer> {
    match error {
        dist::ExecuteError::Effect(effect) => effect_outcome(effect),
        dist::ExecuteError::Journal(detail) => journal_fault(detail),
        other => release_refusal(
            "release::external-plan-moved",
            "external-release-plan",
            "the exact previewed repository, release, evidence, and operations",
            other.to_string(),
            "preview the release again and apply only its new digest",
        ),
    }
}

pub(super) fn effect_outcome(error: dist::EffectError) -> Outcome<wire::intent::RecognitionAnswer> {
    match error {
        dist::EffectError::Moved(detail) => release_refusal(
            "release::external-plan-moved",
            "external-release-plan",
            "the exact previewed release evidence",
            detail,
            "restore the previewed evidence or generate and apply a new plan",
        ),
        dist::EffectError::Environment(detail) => {
            crate::environment_fault("release::environment-failed", detail)
        }
        dist::EffectError::Interrupted => release_refusal(
            "release::operator-interrupted",
            "release-operation",
            "resume the same plan digest",
            "operator interruption".to_owned(),
            "repeat apply with the same digest; durable receipts prevent duplicate effects",
        ),
        dist::EffectError::GateRefused(detail) => release_refusal(
            "release::gate-refused",
            "correctness-gate",
            "one clean full-gate receipt",
            detail,
            "repair the gate finding, rebuild evidence, and preview again",
        ),
        dist::EffectError::RemoteRefused(detail) => release_refusal(
            "release::remote-refused",
            "external-release-authority",
            "one absent or already-converged immutable release ref",
            detail,
            "inspect the remote authority, then resume only if it still names this release",
        ),
    }
}

pub(super) fn release_refusal(
    reason: &str,
    witness: &str,
    expected: &str,
    observed: String,
    detail: &str,
) -> Outcome<wire::intent::RecognitionAnswer> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some(witness.to_owned()),
            Some(expected.to_owned()),
            Some(observed),
        )
        .with_detail(detail),
    ))
}

pub(super) fn journal_fault(detail: String) -> Outcome<wire::intent::RecognitionAnswer> {
    crate::environment_fault("release::operation-journal-failed", detail)
}
