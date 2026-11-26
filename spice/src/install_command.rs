//! Native rim for the digest-bound Spice installer.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use spice::ambient::Ambient;
use spice::install::{InstallError, InstallPreview, InstallReceipt, InstallStatus, RECORD_PATH};
use spice::repository::Repository;
use spice::repository::StateError;
use wire::{Digest, Outcome, Refusal, Reply, ReplyPosition};

use crate::{environment_fault, protocol_fault, store_fault};

pub(crate) enum Answer {
    Plan(Outcome<PlanPreview>),
    Receipt(Outcome<InstallReceipt>),
    Child(ExitStatus),
}

/// An install preview with the exact command that applies its digest.
///
/// A package preview's plan comes from the verified staged executable, which
/// is gone once the preview returns; replaying bare `install --apply` would
/// ask the installed appliance to derive a different plan from its own
/// executable. The command therefore reopens the original package source.
/// The command is display only: the digest still comes from the preview, and
/// apply derives and checks the plan afresh.
pub(crate) struct PlanPreview {
    preview: InstallPreview,
    apply: String,
}

impl PlanPreview {
    fn source(preview: InstallPreview, program: &str) -> Self {
        let apply = source_apply_command(program, preview.digest());
        Self { preview, apply }
    }

    fn package(preview: InstallPreview, program: &str, package: &str) -> Self {
        let apply = package_apply_command(program, package, preview.digest());
        Self { preview, apply }
    }
}

impl serde::Serialize for PlanPreview {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.preview.serialize(serializer)
    }
}

impl fmt::Display for PlanPreview {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.preview.write_text(formatter, &self.apply)
    }
}

pub(crate) fn run(matches: &clap::ArgMatches, ambient: &Ambient) -> Answer {
    let request = if matches.get_flag("unapply") {
        Request::Unapply
    } else if let Some(digest) = matches.get_one::<String>("apply") {
        Request::Apply(digest.clone())
    } else {
        Request::Plan
    };
    let working = match std::env::current_dir() {
        Ok(working) => working,
        Err(error) => {
            return request.fault("install::working-directory-unavailable", error.to_string());
        }
    };
    let repository = match Repository::discover(&working) {
        Ok(repository) => repository,
        Err(error) => {
            return request.fault(spice::repository::ResolveError::REASON, error.to_string());
        }
    };
    if let Some(package) = matches.get_one::<String>("package") {
        return run_package(matches, ambient, &request, &repository, package);
    }
    match request {
        Request::Plan => {
            let executable = match executable(ambient) {
                Ok(executable) => executable,
                Err(outcome) => return Answer::Plan(outcome),
            };
            let lifecycle = match crate::resident::keeper::install_standing(&repository) {
                Ok(lifecycle) => lifecycle,
                Err(outcome) => return Answer::Plan(cast_outcome(outcome)),
            };
            Answer::Plan(result(
                spice::install::preview_with_lifecycle(&repository, Some(&executable), lifecycle)
                    .map(|preview| PlanPreview::source(preview, &invoked_program())),
            ))
        }
        Request::Apply(spelling) => {
            let digest = match spelling.parse::<Digest>() {
                Ok(digest) => digest,
                Err(error) => {
                    return Answer::Receipt(Outcome::Refusal(Refusal::bare(
                        wire::RefusalDiagnostic::new(
                            "install::digest-refused".to_owned(),
                            Some("install-plan".to_owned()),
                            Some("canonical install plan digest".to_owned()),
                            Some(spelling),
                        )
                        .with_detail(error.to_string()),
                    )));
                }
            };
            let executable = match executable(ambient) {
                Ok(executable) => executable,
                Err(outcome) => return Answer::Receipt(outcome),
            };
            Answer::Receipt(apply_with_keeper(
                &repository,
                &executable,
                &digest,
                ambient,
            ))
        }
        Request::Unapply => Answer::Receipt(unapply_with_keeper(&repository, ambient)),
    }
}

fn run_package(
    matches: &clap::ArgMatches,
    ambient: &Ambient,
    request: &Request,
    repository: &Repository,
    package: &str,
) -> Answer {
    if matches!(request, Request::Unapply) {
        return request.package_failure(crate::install_package::Error::input(
            "a package source cannot be combined with --unapply; the installed receipt owns reversal",
        ));
    }
    let candidate = match executable(ambient) {
        Ok(candidate) => candidate,
        Err(outcome) => return Answer::Plan(outcome),
    };
    let apply = match request {
        Request::Apply(digest) => Some(digest.as_str()),
        Request::Plan => None,
        Request::Unapply => unreachable!("package unapply returned above"),
    };
    let json = matches.get_flag("json");
    if apply.is_none() && !json {
        return match crate::install_package::preview(repository, Path::new(package), &candidate) {
            Ok(outcome) => Answer::Plan(package_preview(package, outcome)),
            Err(error) => request.package_failure(error),
        };
    }
    match crate::install_package::run(repository, Path::new(package), &candidate, apply, json) {
        Ok(status) => Answer::Child(status),
        Err(error) => request.package_failure(error),
    }
}

fn apply_with_keeper(
    repository: &Repository,
    executable: &Path,
    expected: &Digest,
    ambient: &Ambient,
) -> Outcome<InstallReceipt> {
    if let Err(outcome) = crate::resident::keeper::admit_owner(repository) {
        return cast_outcome(outcome);
    }
    let lifecycle = match crate::resident::keeper::install_standing(repository) {
        Ok(lifecycle) => lifecycle,
        Err(outcome) => return cast_outcome(outcome),
    };
    let prepared =
        match spice::install::prepare_with_lifecycle(repository, Some(executable), lifecycle) {
            Ok(prepared) => prepared,
            Err(error) => return failure(error),
        };
    if prepared.digest() != expected {
        return failure(InstallError::DigestMismatch {
            expected: expected.clone(),
            observed: prepared.digest().clone(),
        });
    }
    let lifecycle = prepared.plan().lifecycle_owner().cloned();
    let converged = prepared.converged();
    let owned = if converged {
        false
    } else if let Some(expected) = &lifecycle {
        match crate::resident::keeper::quiesce_for_install(repository, expected, ambient) {
            Ok(owned) => owned,
            Err(outcome) => return cast_outcome(outcome),
        }
    } else {
        false
    };
    let applied = spice::install::apply_prepared(repository, Some(executable), prepared);
    match owner_step(applied.as_ref().map(InstallReceipt::status), owned) {
        OwnerStep::Establish => match ensure_keeper(repository) {
            Ok(()) => result(spice::install::complete_lifecycle(repository)),
            Err(outcome) => cast_outcome(outcome),
        },
        OwnerStep::Restore => match ensure_keeper(repository) {
            Ok(()) => result(applied),
            Err(owner) => both_failed(result(applied), owner),
        },
        OwnerStep::Leave => result(applied),
    }
}

/// What a digest-bound apply owes the repository's native lifecycle owner
/// once publication has run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnerStep {
    /// Touch nothing: the owner keeps whatever standing it had.
    Leave,
    /// Register and start the owner under the installed executable, then
    /// settle the receipt.
    Establish,
    /// Publication failed after the prior owner was quiesced: start it again
    /// so the repository keeps the service it had.
    Restore,
}

/// A publication that stopped at the lifecycle boundary is established; a
/// converged or non-Windows plan settles complete and performs no owner
/// effect, preserving an explicit `down` or `down --keeper`; a failed
/// publication restores only an owner this apply stopped.
fn owner_step(applied: Result<InstallStatus, &InstallError>, owned: bool) -> OwnerStep {
    match applied {
        Ok(InstallStatus::Establishing) => OwnerStep::Establish,
        Ok(_) => OwnerStep::Leave,
        Err(_) if owned => OwnerStep::Restore,
        Err(_) => OwnerStep::Leave,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReversalOwnerStep {
    Leave,
    Remove,
    Refuse,
}

/// Package reversal owns only the package-selected default. An explicit
/// keeper is a user choice outside the package receipt.
fn reversal_owner_step(
    standing: Option<&spice::install::LifecycleOwnerStanding>,
) -> ReversalOwnerStep {
    match standing {
        None => ReversalOwnerStep::Leave,
        Some(spice::install::LifecycleOwnerStanding::Default) => ReversalOwnerStep::Remove,
        Some(spice::install::LifecycleOwnerStanding::Configured(_)) => ReversalOwnerStep::Refuse,
    }
}

fn unapply_with_keeper(repository: &Repository, ambient: &Ambient) -> Outcome<InstallReceipt> {
    if let Err(outcome) = crate::resident::keeper::admit_owner(repository) {
        return cast_outcome(outcome);
    }
    let inspected = match spice::install::inspect(repository) {
        Ok(inspected) => inspected,
        Err(error) => return failure(error),
    };
    let lifecycle = match crate::resident::keeper::install_standing(repository) {
        Ok(lifecycle) => lifecycle,
        Err(outcome) => return cast_outcome(outcome),
    };
    let owner_step = reversal_owner_step(lifecycle.as_ref());
    if owner_step == ReversalOwnerStep::Refuse {
        return Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "install::configured-keeper-preserved".to_owned(),
                Some("lifecycle-owner".to_owned()),
                Some("package-default or absent".to_owned()),
                Some("configured".to_owned()),
            )
            .with_detail(
                "package reversal never removes an explicitly configured keeper; run \
                 `spice down --keeper`, then retry `spice install --unapply`",
            ),
        ));
    }
    let owned = if inspected
        .as_ref()
        .is_some_and(|(receipt, _)| receipt.status() != InstallStatus::Applying)
        && let Some(expected) = &lifecycle
    {
        match crate::resident::keeper::quiesce_for_install(repository, expected, ambient) {
            Ok(owned) => owned,
            Err(outcome) => return cast_outcome(outcome),
        }
    } else {
        false
    };
    let unapplied = spice::install::unapply_retained(repository);
    if unapplied.is_err() {
        if owned && let Err(owner) = ensure_keeper(repository) {
            return both_failed(result(unapplied), owner);
        }
        return result(unapplied);
    }
    if owner_step == ReversalOwnerStep::Remove
        && let Err(outcome) = crate::resident::keeper::finish_unapply(repository)
    {
        return cast_outcome(outcome);
    }
    if let Err(error) = spice::install::retire_unapplied(repository) {
        return failure(error);
    }
    result(unapplied)
}

/// Report a publication failure together with the owner restoration failure
/// that followed it, so neither boundary is discarded.
fn both_failed<B: serde::Serialize>(publication: Outcome<B>, owner: Outcome<()>) -> Outcome<B> {
    environment_fault(
        "install::publication-and-owner-failed",
        format!(
            "{}; restoring the lifecycle owner also failed: {}",
            describe(&publication),
            describe(&owner)
        ),
    )
}

fn describe<B: serde::Serialize>(outcome: &Outcome<B>) -> String {
    match outcome {
        Outcome::Reply(_) => "the command replied".to_owned(),
        Outcome::Refusal(refusal) => match refusal.diagnostic().and_then(|d| d.detail()) {
            Some(detail) => format!("{} ({detail})", refusal.reason()),
            None => refusal.reason().to_owned(),
        },
        Outcome::Fault(fault) => format!("{} ({})", fault.reason, fault.detail),
    }
}

fn ensure_keeper(repository: &Repository) -> Result<(), Outcome<()>> {
    let executable = spice::install::installed_binary(repository);
    let installed = crate::resident::keeper::owner_installed(repository)?;
    let mut command = Command::new(&executable);
    if installed {
        command.args(["--json", "up"]);
    } else {
        command.arg(crate::resident::keeper::PACKAGE_DEFAULT_ARGUMENT);
    }
    let status = command
        .current_dir(repository.worktree_root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| {
            environment_fault(
                "install::keeper-resume-failed",
                format!("launch {}: {error}", executable.display()),
            )
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(environment_fault(
            "install::keeper-resume-failed",
            format!("{} exited with {status}", executable.display()),
        ))
    }
}

fn cast_outcome<B>(outcome: Outcome<()>) -> Outcome<B> {
    match outcome {
        Outcome::Reply(_) => protocol_fault(
            "install::keeper-outcome-invalid",
            "lifecycle preparation returned a reply where only failure was possible",
        ),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

enum Request {
    Plan,
    Apply(String),
    Unapply,
}

impl Request {
    fn fault(&self, reason: &str, detail: String) -> Answer {
        match self {
            Self::Plan => Answer::Plan(environment_fault(reason, detail)),
            Self::Apply(_) | Self::Unapply => Answer::Receipt(environment_fault(reason, detail)),
        }
    }

    fn package_failure(&self, error: crate::install_package::Error) -> Answer {
        match self {
            Self::Plan => Answer::Plan(package_failure(&error)),
            Self::Apply(_) | Self::Unapply => Answer::Receipt(package_failure(&error)),
        }
    }
}

fn package_failure<B>(error: &crate::install_package::Error) -> Outcome<B> {
    use crate::install_package::Error;
    match error {
        Error::Environment { .. } | Error::Installed(_) => {
            environment_fault("install::package-environment-failed", error.to_string())
        }
        Error::Unreadable { .. } => {
            protocol_fault("install::package-answer-unreadable", error.to_string())
        }
        Error::Input(_) => Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "install::package-refused".to_owned(),
                Some("sealed-package".to_owned()),
                Some("one exact release candidate plus its sealed evidence".to_owned()),
                Some(error.to_string()),
            )
            .with_detail(
                "select the explicit local root of canonical release evidence, invoke its matching downloaded candidate, and retry; no repository authority changed",
            ),
        )),
    }
}

fn executable<B>(ambient: &Ambient) -> Result<PathBuf, Outcome<B>> {
    #[cfg(debug_assertions)]
    if let Some(executable) = ambient.get(spice::install::TEST_EXECUTABLE) {
        return Ok(PathBuf::from(executable));
    }
    #[cfg(not(debug_assertions))]
    let _ = ambient;
    std::env::current_exe()
        .map_err(|error| environment_fault("install::executable-unresolved", error.to_string()))
}

/// A sealed package's preview as this front-end renders it: the child's
/// structured preview with the command that applies it.
fn package_preview(package: &str, outcome: Outcome<InstallPreview>) -> Outcome<PlanPreview> {
    match outcome {
        Outcome::Reply(Reply { position, body }) => Outcome::Reply(Reply {
            position,
            body: PlanPreview::package(body, &invoked_program(), package),
        }),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

fn result<B>(result: Result<B, InstallError>) -> Outcome<B> {
    match result {
        Ok(body) => Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body,
        }),
        Err(error) => failure(error),
    }
}

pub(crate) fn failure<B>(error: InstallError) -> Outcome<B> {
    match &error {
        InstallError::PathShape(_)
        | InstallError::PathOutsideRoot(_)
        | InstallError::ParentChanged(_)
        | InstallError::GenerationChanged(_)
        | InstallError::BinaryNotRegular(_)
        | InstallError::BinaryEmpty(_)
        | InstallError::OccupantUnrestorable { .. }
        | InstallError::HookPathUnrepresentable { .. }
        | InstallError::SymbolsRefused { .. }
        | InstallError::ReferenceStorage { .. }
        | InstallError::BinaryContextChanged
        | InstallError::DigestMismatch { .. }
        | InstallError::ForeignRecord { .. }
        | InstallError::CanvasRefused { .. }
        | InstallError::RecordMissing
        | InstallError::ApplyInProgress
        | InstallError::UnapplyInProgress
        | InstallError::Drift { .. } => install_refusal(
            &error,
            "install-plan",
            "the exact current Rust install plan and asset state",
        ),
        InstallError::State(
            StateError::Linked { .. }
            | StateError::NotDirectory { .. }
            | StateError::NotRegularFile { .. },
        ) => install_refusal(
            &error,
            "install-state",
            &format!(
                "unlinked current-release Rust state under {}",
                wire::STATE_DIRECTORY
            ),
        ),
        InstallError::Repository(_)
        | InstallError::State(StateError::Filesystem { .. })
        | InstallError::Io { .. }
        | InstallError::GitNonUtf8(_) => {
            environment_fault("install::environment-failed", error.to_string())
        }
        InstallError::Encode(_) | InstallError::Decode(_) | InstallError::InvalidRecord(_) => {
            store_fault("install::state-failed", error.to_string())
        }
    }
}

fn install_refusal<B>(error: &InstallError, witness: &str, expected: &str) -> Outcome<B> {
    let observed = error.to_string();
    let detail = format!("{observed}; {}", install_refusal_remedy(error));
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            install_refusal_reason(error).to_owned(),
            Some(witness.to_owned()),
            Some(expected.to_owned()),
            Some(observed),
        )
        .with_detail(detail),
    ))
}

fn install_refusal_reason(error: &InstallError) -> &'static str {
    match error {
        InstallError::DigestMismatch { .. } => "install::digest-moved",
        InstallError::HookPathUnrepresentable { .. } => "install::hook-path-unrepresentable",
        InstallError::SymbolsRefused { .. } => "install::symbols-refused",
        InstallError::OccupantUnrestorable { .. } => "install::occupant-unrestorable",
        InstallError::ReferenceStorage { .. } => "install::ref-storage-unsupported",
        InstallError::ForeignRecord { .. } => "install::release-owned-state",
        InstallError::CanvasRefused { .. } => "install::canvas-refused",
        InstallError::RecordMissing => "install::record-missing",
        InstallError::ApplyInProgress | InstallError::UnapplyInProgress => {
            "install::transition-incomplete"
        }
        InstallError::Drift { .. } => "install::drift",
        InstallError::PathOutsideRoot(_) => "install::path-outside-root",
        InstallError::ParentChanged(_) => "install::parent-changed",
        InstallError::GenerationChanged(_) => "install::generation-changed",
        InstallError::State(_) => "install::state-refused",
        _ => "install::input-refused",
    }
}

fn install_refusal_remedy(error: &InstallError) -> String {
    if let Some(remedy) = state_remedy(error) {
        return remedy;
    }
    match error {
        InstallError::PathShape(path) => format!(
            "inspect the install target with `{}` and restore it to an unlinked regular file or directory before rerunning this executable's `install` preview",
            inspect_path(path),
        ),
        InstallError::PathOutsideRoot(path) => format!(
            "inspect the escaping install path with `{}` and repair the named symlink or record path so it stays within its permitted Rust root",
            inspect_path(path),
        ),
        InstallError::ParentChanged(path) => format!(
            "inspect the changed destination parent with `{}`; rerun this executable's `install` preview and use only its newly printed digest",
            inspect_path(path),
        ),
        InstallError::GenerationChanged(path) => format!(
            "inspect the concurrently changed install receipt with `{}`; keep the completed direction and rerun this executable's `install` preview before retrying",
            inspect_path(path),
        ),
        InstallError::BinaryNotRegular(path) | InstallError::BinaryEmpty(path) => format!(
            "inspect the selected executable with `{}`; install an appliance that links its canvas archive, built by `spice dev xtask -- install` or taken from a sealed release package",
            inspect_path(path),
        ),
        InstallError::HookPathUnrepresentable { path, .. } => format!(
            "move or recreate the repository at a UTF-8 path with no line separators, then rerun this executable's install preview; the refused Git-common path was {path:?}",
        ),
        InstallError::ReferenceStorage { common, format } => format!(
            "Spice gates commits by reading references through gix, which has no {format} backend; migrate the repository with `{GIT} refs migrate --ref-format=files` in {}, or create it with `{GIT} init --ref-format=files`, then rerun this executable's `install` preview",
            common.display(),
        ),
        InstallError::OccupantUnrestorable { path, .. } => format!(
            "inspect the existing target with `{}`; move it out of the install tree, or restrict it to owner-only access, then preview again",
            inspect_path(path),
        ),
        InstallError::SymbolsRefused { path, .. } => format!(
            "inspect the optional symbol input with `{}`; remove it or acquire symbols from the same build as this executable, then rerun this executable's install preview",
            inspect_path(path),
        ),
        InstallError::BinaryContextChanged => format!(
            "the `{INSTALLED_SPICE}` asset was planned from another executable; rerun \
             `spice dev xtask -- install`, which plans and applies one linked build, or preview a \
             sealed release package and pass that preview's digest to the same `install --package`",
        ),
        InstallError::DigestMismatch { .. } => concat!(
            "rerun this executable's `install` preview and pass its newly printed digest to ",
            "the same executable as `install --apply=<digest>`; do not reuse the refused digest",
        )
        .to_owned(),
        InstallError::ForeignRecord { path, version } => format!(
            "inspect the schema-{version} record with `{}`; run the appliance that wrote it, \
             its Git-common `{INSTALLED_SPICE}`, with `install --unapply`, then rerun this \
             executable's `install` preview",
            inspect_path(path),
        ),
        InstallError::CanvasRefused { path, .. } => format!(
            "inspect the selected executable with `{}`; install an appliance that links its canvas archive, built by `spice dev xtask -- install` or taken from a sealed release package",
            inspect_path(path),
        ),
        InstallError::RecordMissing => format!(
            "the `{}/{RECORD_PATH}` record is absent; omit `--unapply` and run this executable's `install` preview instead",
            wire::STATE_DIRECTORY
        ),
        InstallError::ApplyInProgress => format!(
            "the `{}/{RECORD_PATH}` record notes an interrupted apply; rerun this executable's `install` preview, finish `install --apply=<printed-digest>`, then retry `--unapply`",
            wire::STATE_DIRECTORY
        ),
        InstallError::UnapplyInProgress => format!(
            "the `{}/{RECORD_PATH}` record notes an interrupted reversal; rerun this executable with `install --unapply` to completion before previewing another install",
            wire::STATE_DIRECTORY
        ),
        InstallError::Drift { path, .. } => format!(
            "resolve the asset root with `{GIT} rev-parse --path-format=absolute --git-common-dir`, inspect its install target with `{}`, and preserve the operator replacement or restore the recorded installed image before retrying reversal",
            inspect_path(&Path::new("<printed-common-git>").join(path)),
        ),
        _ => {
            unreachable!("fault-only install errors never construct refusal guidance")
        }
    }
}

fn state_remedy(error: &InstallError) -> Option<String> {
    match error {
        InstallError::State(StateError::Linked { path }) => Some(format!(
            "inspect the Rust state link with `{}` and replace it with the fixed unlinked state component before retrying preview",
            inspect_path(path),
        )),
        InstallError::State(StateError::NotDirectory { path }) => Some(format!(
            "inspect the Rust state parent with `{}` and restore that fixed component as a directory before retrying preview",
            inspect_path(path),
        )),
        InstallError::State(StateError::NotRegularFile { path }) => Some(format!(
            "inspect the Rust state leaf with `{}` and restore that fixed component as a regular file before retrying preview",
            inspect_path(path),
        )),
        _ => None,
    }
}

/// The primary Git command the installer resolves, spelled for the operator's shell.
const GIT: &str = "git";

#[cfg(unix)]
const INSTALLED_SPICE: &str = ".spice-install/bin/spice";
#[cfg(windows)]
const INSTALLED_SPICE: &str = ".spice-install/bin/spice.exe";

#[cfg(unix)]
fn inspect_path(path: &Path) -> String {
    shell_words::join([
        "ls".to_owned(),
        "-ld".to_owned(),
        "--".to_owned(),
        path.to_string_lossy().into_owned(),
    ])
}

#[cfg(windows)]
fn inspect_path(path: &Path) -> String {
    format!("icacls \"{}\"", path.display())
}

/// The program word that reruns this executable as this invocation named it.
///
/// A preview names its plan from this executable's bytes, so a bare `spice`
/// that resolves elsewhere (or nowhere, before `.spice-install/bin` is on
/// `PATH`) would derive another digest; replaying argv[0] reaches the same file.
fn invoked_program() -> String {
    std::env::args_os().next().map_or_else(
        || "spice".to_owned(),
        |program| program.to_string_lossy().into_owned(),
    )
}

#[cfg(unix)]
fn source_apply_command(program: &str, digest: &Digest) -> String {
    format!("{} install --apply={digest}", program_word(program))
}

#[cfg(windows)]
fn source_apply_command(program: &str, digest: &Digest) -> String {
    windows_apply_command(program, None, digest)
}

#[cfg(unix)]
fn program_word(program: &str) -> String {
    shell_words::quote(program).into_owned()
}

/// Render one Windows install replay with a literal PowerShell program word.
#[cfg(any(windows, test))]
fn windows_apply_command(program: &str, package: Option<&str>, digest: &Digest) -> String {
    let apostrophes = program.bytes().filter(|byte| *byte == b'\'').count();
    let package_len = package.map_or(0, |path| " --package \"\"".len() + path.len());
    let mut command = String::with_capacity(
        "& ''".len()
            + program.len()
            + apostrophes
            + " install".len()
            + package_len
            + " --apply=".len()
            + digest.as_str().len(),
    );
    command.push_str("& '");
    for character in program.chars() {
        if character == '\'' {
            command.push_str("''");
        } else {
            command.push(character);
        }
    }
    command.push_str("' install");
    if let Some(package) = package {
        command.push_str(" --package \"");
        command.push_str(package);
        command.push('"');
    }
    command.push_str(" --apply=");
    command.push_str(digest.as_str());
    command
}

/// The command that reopens `package` and applies its previewed `digest`.
#[cfg(unix)]
fn package_apply_command(program: &str, package: &str, digest: &Digest) -> String {
    format!(
        "{} install --package {} --apply={digest}",
        program_word(program),
        shell_words::quote(package)
    )
}

/// The command that reopens `package` and applies its previewed `digest`.
#[cfg(windows)]
fn package_apply_command(program: &str, package: &str, digest: &Digest) -> String {
    windows_apply_command(program, Some(package), digest)
}

#[cfg(test)]
mod tests;
