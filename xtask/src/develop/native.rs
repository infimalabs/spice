//! The native half of one developer refresh.
//!
//! Everything here drives the surrounding machine — Cargo, the filesystem, the
//! resident process, and the digest-bound installer that [`spice::install`]
//! itself gates off the wasm target. [`super::refresh`] publishes the signature
//! on every target and delegates to [`perform`] only where these exist.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use spice::install::{self, InstallError};
use spice::repository::Repository;
use spice::resident_lock::{
    ResidentAccess, ResidentLaunch, auth_token_is_valid, holder_epoch, holder_launch,
};
use wire::Digest;

use super::{RefreshError, RefreshReport, RefreshRequest};

/// Repository-common home for developer-refresh rollback executables.
const WORKSPACE: &str = "appliance/rollback";
/// Executable parked before a refresh so a failure can restore it.
const ROLLBACK: &str = "spice.rollback";
/// Staged predecessor while the stable rollback remains shutdown authority.
const NEXT_ROLLBACK: &str = "spice.rollback.next";
/// Serving executable retained across no-resident publications.
const RESIDENT_ROLLBACK: &str = "spice.resident.rollback";
/// Staging copy used to atomically replace the serving executable authority.
const NEXT_RESIDENT_ROLLBACK: &str = "spice.resident.rollback.next";
/// Maximum resident bootstrap response accepted while reconciling a restart.
const MAX_BOOTSTRAP_BYTES: u64 = 64 * 1024;
/// One local resident identity probe must not hold an install indefinitely.
const BOOTSTRAP_PROBE_BOUND: Duration = Duration::from_secs(2);

#[derive(Clone, Default)]
struct RestartBinding {
    host: Option<String>,
    port: Option<u16>,
    allow_insecure_bind: bool,
    auth_token: Option<OsString>,
}

/// The effects a refresh orders without deciding: publishing one executable
/// through the installer and running one appliance command to completion.
/// Every choice around them — what to park, stop, restore, or restart, and how
/// to read each answer — belongs to the refresh itself.
trait Host {
    /// Republish the installed executable from `binary`.
    fn install(&mut self, repository: &Repository, binary: &Path) -> Result<Digest, InstallError>;

    /// Run `command` and answer its status and captured streams.
    fn run(&mut self, command: &mut Command) -> std::io::Result<Output>;
}

/// The digest-bound installer and real child processes.
struct System;

impl Host for System {
    fn install(&mut self, repository: &Repository, binary: &Path) -> Result<Digest, InstallError> {
        refresh_install(repository, binary)
    }

    fn run(&mut self, command: &mut Command) -> std::io::Result<Output> {
        command.output()
    }
}

/// Build the current tree, republish the appliance, and restart its resident.
pub(super) fn perform(
    request: &RefreshRequest,
    environment: &crate::ReleaseEnvironment,
    output: &mut impl Write,
) -> Result<RefreshReport, RefreshError> {
    let repository = Repository::discover(Path::new("."))?;
    refuse_seated_lane(&repository, request.lane.as_deref())?;
    if request.restart {
        request
            .resident
            .host(repository.worktree_root())
            .map_err(RefreshError::ResidentHost)?;
    }
    writeln!(output, "building app")?;
    crate::app(environment, output)?;
    let build_info = spice::version::build_info(repository.worktree_root(), dist::Build::Dev)
        .map_err(RefreshError::SourceVersion)?;
    writeln!(output, "product version {}", build_info.product)?;
    let canvas = crate::app::StagedCanvas::read(repository.worktree_root(), &build_info)?;
    let build_root = environment.build_root(repository.worktree_root());
    let built = build(
        request.release,
        request.symbols,
        &canvas,
        &build_root,
        output,
    )?;
    install_built(&mut System, &repository, built, request, output)
}

/// Refuse a refresh from a lane this repository seats, before anything builds.
///
/// Only the operator republishes this repository's appliance and stops or
/// restarts its resident: every seated lane runs that appliance, so one lane
/// replacing it takes the resident out from under all of them. A lane shell may
/// still refresh a scratch repository that has never seated it. Seating is read
/// from the runtime's own per-lane state, so the refusal holds whether or not
/// the resident is serving. A binding that is not a lane identity cannot be
/// shown to be unseated and refuses too.
fn refuse_seated_lane(repository: &Repository, lane: Option<&OsStr>) -> Result<(), RefreshError> {
    let Some(lane) = lane else {
        return Ok(());
    };
    let text = lane.to_string_lossy();
    let seated = match text.parse::<wire::CanonicalUuid>() {
        Ok(identity) => {
            let state = repository.admit_state().map_err(InstallError::from)?;
            fs::symlink_metadata(state.lane(identity)).is_ok()
        }
        Err(_) => true,
    };
    if seated {
        Err(RefreshError::LaneSeated(text.into_owned()))
    } else {
        Ok(())
    }
}

/// Publish one completed build and reconcile the resident around that cutover.
fn install_built(
    host: &mut impl Host,
    repository: &Repository,
    built: PathBuf,
    request: &RefreshRequest,
    output: &mut impl Write,
) -> Result<RefreshReport, RefreshError> {
    let installed = install::installed_binary(repository);
    let before = snapshot_install(repository, &installed)?;
    let schema = schema_recovery_boundary(repository, request.restart)?;
    let (published, binding) = restart_state(repository, request)?;

    // Resolve and preserve the running generation before changing either the
    // installed executable or the transaction rollback. No-resident
    // publications can span worktrees and generations while one predecessor
    // keeps serving, so its command authority has a separate common slot.
    let authority = if published.is_some() {
        Some(resident_stop_executable(repository, &installed)?)
    } else {
        None
    };
    let retained_authority = authority
        .as_deref()
        .filter(|executable| *executable != installed);
    let resident_rollback = authority
        .as_deref()
        .map(|authority| retain_resident_authority(repository, authority))
        .transpose()?
        .flatten();
    let mut rollback = if request.restart && retained_authority.is_some() {
        park_as(repository, &installed, NEXT_ROLLBACK)?
    } else {
        park(repository, &installed)?
    };

    if let (true, Some(authority)) = (request.restart, authority.as_deref()) {
        let stopping = Instant::now();
        if let Err(error) = stop(host, repository, authority, output) {
            if let (true, Some(rollback)) = (retained_authority.is_some(), &rollback) {
                let _ = fs::remove_file(rollback);
            }
            return Err(error);
        }
        let _ = writeln!(output, "resident stopped in {:.1?}", stopping.elapsed());
    }

    if request.restart && retained_authority.is_some() {
        let staged = rollback
            .take()
            .expect("an installed successor was staged beside its live predecessor");
        let stable = workspace(repository)?.join(ROLLBACK);
        if let Err(error) = fs::rename(&staged, &stable) {
            let unwind = Rollback {
                installed: &installed,
                executable: None,
                resident_executable: resident_rollback.as_deref(),
                published: published.as_deref(),
                restart: request.restart,
                binding: binding.clone(),
                resident: request.resident.clone(),
            };
            restore_resident(host, repository, &unwind, output);
            return Err(error.into());
        }
        rollback = Some(stable);
    }
    let unwind = Rollback {
        installed: &installed,
        executable: rollback.as_deref(),
        resident_executable: resident_rollback.as_deref(),
        published: published.as_deref(),
        restart: request.restart,
        binding: binding.clone(),
        resident: request.resident.clone(),
    };
    let installing = Instant::now();
    let digest = match host.install(repository, &built) {
        Ok(digest) => digest,
        Err(error) => {
            return Err(restore_if_changed(
                host, repository, &unwind, &before, output, error,
            ));
        }
    };
    let _ = writeln!(
        output,
        "installed {} plan={digest} in {:.1?}",
        installed.display(),
        installing.elapsed()
    );

    let endpoint = match restart_admitting_reappearance(
        host, repository, &installed, &binding, request, output,
    ) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            let Some((journal, before_schema)) = &schema else {
                return Err(error);
            };
            return Err(recover_start_failure(
                host,
                repository,
                &unwind,
                journal,
                *before_schema,
                output,
                error,
            ));
        }
    };
    if let (true, Some(resident)) = (request.restart, resident_rollback) {
        let _ = fs::remove_file(resident);
    }
    Ok(RefreshReport {
        built,
        installed,
        digest,
        endpoint,
    })
}

fn schema_recovery_boundary(
    repository: &Repository,
    restart: bool,
) -> Result<Option<(PathBuf, Option<u32>)>, RefreshError> {
    if !restart {
        return Ok(None);
    }
    let journal = repository
        .admit_state()
        .map_err(InstallError::from)?
        .journal()
        .to_path_buf();
    let schema =
        inspect_schema(&journal).map_err(|error| RefreshError::SchemaRecoveryUncertain {
            before: None,
            cause: "pre-refresh schema inspection failed".to_owned(),
            error,
        })?;
    Ok(Some((journal, schema)))
}

fn inspect_schema(path: &Path) -> Result<Option<u32>, spice::journal_schema::InspectionError> {
    if !path.exists() {
        return Ok(None);
    }
    spice::journal_schema::inspect_stored(path).map(Some)
}

fn recover_start_failure(
    host: &mut impl Host,
    repository: &Repository,
    rollback: &Rollback<'_>,
    journal: &Path,
    before_schema: Option<u32>,
    output: &mut impl Write,
    cause: RefreshError,
) -> RefreshError {
    recover_start_failure_after(
        host,
        repository,
        rollback,
        before_schema,
        inspect_schema(journal),
        output,
        cause,
    )
}

fn recover_start_failure_after(
    host: &mut impl Host,
    repository: &Repository,
    rollback: &Rollback<'_>,
    before_schema: Option<u32>,
    after_schema: Result<Option<u32>, spice::journal_schema::InspectionError>,
    output: &mut impl Write,
    cause: RefreshError,
) -> RefreshError {
    let cause_text = cause.to_string();
    let after_schema = match after_schema {
        Ok(schema) => schema,
        Err(error) => {
            return RefreshError::SchemaRecoveryUncertain {
                before: before_schema,
                cause: cause_text,
                error,
            };
        }
    };
    if schema_rollback_safe(before_schema, after_schema) {
        return restore(host, repository, rollback, output, cause);
    }
    let _ = writeln!(
        output,
        "journal schema moved from {before_schema:?} to {after_schema:?}; preserving new executable"
    );
    RefreshError::SchemaAdvanced {
        before: before_schema,
        after: after_schema,
        cause: cause_text,
    }
}

const fn schema_rollback_safe(before: Option<u32>, after: Option<u32>) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(before), Some(after)) => before == after,
        _ => false,
    }
}

/// Republish the installed executable from `binary` under one fresh plan.
///
/// A rebuilt binary over a completed receipt derives a refresh, so there is no
/// plan to withdraw first: every published file is exchanged in place through
/// the installer's own atomic rename, and the receipt names the exact digest
/// applied. Nothing here is ever absent, which is what lets a hook fire and a
/// worktree resolve `.spice-install/bin/spice` for the whole of a refresh.
///
/// The plan is derived once and carried into apply, which proves the sources,
/// destinations, and receipt it was derived from are unchanged before any
/// mutation instead of deriving the plan a second time.
pub fn refresh_install(repository: &Repository, binary: &Path) -> Result<Digest, InstallError> {
    let prepared = install::prepare(repository, Some(binary))?;
    let digest = prepared.digest().clone();
    install::apply_prepared(repository, Some(binary), prepared)?;
    Ok(digest)
}

/// Build `spice` linking `canvas`, and answer the final executable Cargo
/// reported for it once it links and reports that exact archive.
///
/// `symbols` overrides the profile's own strip setting so the linked
/// executable keeps its debug information long enough for
/// [`generate_worktree_symbols`] to capture it, then strips the executable
/// back down to its ordinary installed shape once the dSYM is retained.
fn build(
    release: bool,
    symbols: bool,
    canvas: &crate::app::StagedCanvas,
    build_root: &Path,
    output: &mut impl Write,
) -> Result<PathBuf, RefreshError> {
    writeln!(output, "building spice profile={}", build_profile(release))?;
    let mut command = Command::new("cargo");
    command.args([
        "rustc",
        "--locked",
        "--package",
        "spice",
        "--bin",
        "spice",
        "--message-format",
        "json-render-diagnostics",
    ]);
    if release {
        command.arg("--release");
    }
    if symbols {
        let profile = if release { "release" } else { "dev" };
        command
            .arg("--config")
            .arg(format!("profile.{profile}.package.spice.strip=false"));
    }
    let host =
        rack::link::Target::new(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
            RefreshError::Canvas(format!(
                "no rack object format for {}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
        })?;
    let profile = if release {
        crate::app::LinkProfile::Release
    } else {
        crate::app::LinkProfile::Development
    };
    let link = canvas
        .native_link(build_root, host, profile)
        .map_err(|error| RefreshError::Canvas(error.to_string()))?;
    command.args(&link.arguments);
    let built = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()?;
    if !built.status.success() {
        return Err(RefreshError::Build);
    }
    let cargo = crate::native_link::cargo_output(&built.stdout, &link.object)
        .map_err(|error| RefreshError::Canvas(error.to_string()))?;
    let artifacts = cargo.artifacts;
    let rebuilt = cargo.rebuilt;
    let emitted = cargo.emitted.is_some();
    if let Some(plan) = cargo.emitted {
        plan.publish(&link.plan)
            .map_err(|error| RefreshError::Canvas(error.to_string()))?;
    }
    let executable = cargo.executable;
    let linked = fs::read(&executable)?;
    if canvas.canvas().verify_linked(&linked).is_err() {
        if emitted {
            return Err(RefreshError::Canvas(
                "rustc completed a final link that did not carry the staged canvas".to_owned(),
            ));
        }
        let plan = crate::native_link::LinkPlan::read(&link.plan, &link.object)
            .map_err(|error| RefreshError::Canvas(error.to_string()))?;
        writeln!(
            output,
            "Cargo reported {artifacts} compiler artifacts and {rebuilt} rebuilt; relinking {} with {}",
            executable.display(),
            plan.program().display(),
        )?;
        let status = plan
            .execute()
            .map_err(|error| RefreshError::Canvas(error.to_string()))?;
        if !status.success() {
            return Err(RefreshError::Canvas(format!(
                "saved final linker exited with {status}"
            )));
        }
        plan.publish_output(&executable)
            .map_err(|error| RefreshError::Canvas(error.to_string()))?;
    }
    if symbols {
        generate_worktree_symbols(&executable, output)?;
    }
    verify_canvas(&executable, canvas, output)?;
    Ok(executable)
}

/// Refuse a final executable whose section is not the staged archive or whose
/// one version report does not name its generation.
fn verify_canvas(
    executable: &Path,
    staged: &crate::app::StagedCanvas,
    output: &mut impl Write,
) -> Result<(), RefreshError> {
    let canvas = staged.canvas();
    canvas
        .verify_linked(&fs::read(executable)?)
        .map_err(|error| RefreshError::Canvas(error.to_string()))?;
    let report = Command::new(executable).arg("--version").output()?;
    if !report.status.success() {
        return Err(RefreshError::Canvas(format!(
            "version report failed: {}",
            refusal_text(&report.stderr)
        )));
    }
    canvas
        .verify_report(&String::from_utf8_lossy(&report.stdout))
        .map_err(|error| RefreshError::Canvas(error.to_string()))?;
    crate::release::render_canvas(output, canvas, staged.tree())?;
    Ok(())
}

const fn build_profile(release: bool) -> &'static str {
    if release { "release" } else { "development" }
}

/// Generate a dSYM for `executable` beside it, then strip the executable.
///
/// [`spice::install::symbols`] resolves this exact sibling path when no
/// sealed release package is present, so an ordinary refresh gets a verified
/// symbol receipt the same way a packaged release install does, without a
/// release build or a signed payload, unless `--no-symbols` suppressed it.
///
/// The linker derives the executable's UUID from its contents, so a bundle
/// beside it that already carries that UUID was extracted from this exact
/// link. Cargo restores the unstripped executable from its deps on every
/// build, even one that relinked nothing, and extracting its DWARF again took
/// seven seconds of every refresh that changed no native source.
#[cfg(target_os = "macos")]
fn generate_worktree_symbols(
    executable: &Path,
    output: &mut impl Write,
) -> Result<(), RefreshError> {
    let bundle = worktree_dsym_path(executable);
    let executable_uuid = symbol_uuid(executable)?;
    if bundle.exists() && symbol_uuid(&bundle).is_ok_and(|uuid| uuid == executable_uuid) {
        writeln!(
            output,
            "reusing dSYM {} for UUID {executable_uuid}",
            bundle.display()
        )?;
    } else {
        writeln!(output, "generating dSYM for {}", executable.display())?;
        run_symbols_tool(
            Command::new("dsymutil")
                .arg(executable)
                .arg("-o")
                .arg(&bundle),
        )?;
        let dwarf_uuid = symbol_uuid(&bundle)?;
        if executable_uuid != dwarf_uuid {
            return Err(RefreshError::Symbols(format!(
                "executable UUID {executable_uuid} differs from dSYM UUID {dwarf_uuid}"
            )));
        }
    }
    // Plain `strip` matches `[profile.dev.package.spice] strip = "symbols"`;
    // `-S` alone only drops the debug symbol table and left the installed
    // executable at 149.8M against the ordinary 95.8M shape (measured
    // directly against this exact debug build).
    run_symbols_tool(Command::new("strip").arg(executable))?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn generate_worktree_symbols(
    _executable: &Path,
    _output: &mut impl Write,
) -> Result<(), RefreshError> {
    Err(RefreshError::Symbols(
        "dSYM generation is only available on macOS".to_owned(),
    ))
}

#[cfg(target_os = "macos")]
fn worktree_dsym_path(executable: &Path) -> PathBuf {
    let mut path = executable.as_os_str().to_owned();
    path.push(".dSYM");
    PathBuf::from(path)
}

#[cfg(target_os = "macos")]
fn symbol_uuid(path: &Path) -> Result<String, RefreshError> {
    let output = Command::new("dwarfdump")
        .arg("--uuid")
        .arg(path)
        .output()
        .map_err(|error| {
            RefreshError::Symbols(format!("dwarfdump --uuid {}: {error}", path.display()))
        })?;
    if !output.status.success() {
        return Err(RefreshError::Symbols(format!(
            "dwarfdump --uuid {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim(),
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next().ok_or_else(|| {
        RefreshError::Symbols(format!(
            "dwarfdump --uuid {} produced no output",
            path.display()
        ))
    })?;
    let uuid = line
        .strip_prefix("UUID: ")
        .and_then(|rest| rest.split(' ').next())
        .ok_or_else(|| {
            RefreshError::Symbols(format!(
                "dwarfdump --uuid {} is malformed: {line:?}",
                path.display()
            ))
        })?;
    Ok(uuid.to_owned())
}

#[cfg(target_os = "macos")]
fn run_symbols_tool(command: &mut Command) -> Result<(), RefreshError> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .map_err(|error| RefreshError::Symbols(format!("{program} failed to start: {error}")))?;
    if !status.success() {
        return Err(RefreshError::Symbols(format!(
            "{program} exited with {status}"
        )));
    }
    Ok(())
}

/// Atomically replace the stable executable rollback from a complete staging copy.
fn park(repository: &Repository, installed: &Path) -> Result<Option<PathBuf>, RefreshError> {
    let Some(staged) = park_as(repository, installed, NEXT_ROLLBACK)? else {
        return Ok(None);
    };
    let rollback = workspace(repository)?.join(ROLLBACK);
    fs::rename(staged, &rollback)?;
    Ok(Some(rollback))
}

/// Copy one executable under `name` without touching any stable authority path.
fn park_as(
    repository: &Repository,
    installed: &Path,
    name: &str,
) -> Result<Option<PathBuf>, RefreshError> {
    if !installed.is_file() {
        return Ok(None);
    }
    let rollback = workspace(repository)?.join(name);
    fs::copy(installed, &rollback)?;
    Ok(Some(rollback))
}

/// Preserve the serving executable without risking the prior complete authority.
fn retain_resident_authority(
    repository: &Repository,
    authority: &Path,
) -> Result<Option<PathBuf>, RefreshError> {
    let retained = workspace(repository)?.join(RESIDENT_ROLLBACK);
    if authority == retained {
        return Ok(Some(retained));
    }
    let Some(staged) = park_as(repository, authority, NEXT_RESIDENT_ROLLBACK)? else {
        return Ok(None);
    };
    fs::rename(staged, &retained)?;
    Ok(Some(retained))
}

/// Exact receipt and executable state immediately before one refresh attempt.
#[derive(Debug, Eq, PartialEq)]
struct InstallSnapshot {
    receipt: PathSnapshot,
    executable: PathSnapshot,
}

/// Comparison surface for one install path, including Unix permission drift.
#[derive(Debug, Eq, PartialEq)]
enum PathSnapshot {
    Missing,
    Regular {
        bytes: Vec<u8>,
        readonly: bool,
        #[cfg(unix)]
        mode: u32,
    },
    Other,
}

fn snapshot_install(
    repository: &Repository,
    installed: &Path,
) -> Result<InstallSnapshot, RefreshError> {
    let state = repository.admit_state().map_err(InstallError::from)?;
    Ok(InstallSnapshot {
        receipt: snapshot_path(state.install_record())?,
        executable: snapshot_path(installed)?,
    })
}

fn snapshot_path(path: &Path) -> Result<PathSnapshot, std::io::Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PathSnapshot::Missing);
        }
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Ok(PathSnapshot::Other);
    }
    let permissions = metadata.permissions();
    Ok(PathSnapshot::Regular {
        bytes: fs::read(path)?,
        readonly: permissions.readonly(),
        #[cfg(unix)]
        mode: {
            use std::os::unix::fs::PermissionsExt as _;
            permissions.mode() & 0o777
        },
    })
}

/// What a failed refresh needs to put the appliance back the way it found it.
struct Rollback<'a> {
    /// Published executable a restored resident is started from.
    installed: &'a Path,
    /// Parked copy of the executable that was installed before the refresh.
    executable: Option<&'a Path>,
    /// Retained command authority when the resident predates `installed`.
    resident_executable: Option<&'a Path>,
    /// Endpoint the stopped resident had published, when one was running.
    published: Option<&'a str>,
    /// Whether this refresh was the kind that stops and starts a resident.
    restart: bool,
    /// Exact bind, consent, and one-shot authentication needed for restoration.
    binding: RestartBinding,
    /// Environment the restored resident launches with.
    resident: spice::cargo_isolation::ResidentLaunchEnvironment,
}

/// Return an untouched refusal directly; reinstall only when the receipt or
/// executable changed, while still bringing back a resident this attempt
/// stopped before the installer refused.
fn restore_if_changed(
    host: &mut impl Host,
    repository: &Repository,
    rollback: &Rollback<'_>,
    before: &InstallSnapshot,
    output: &mut impl Write,
    cause: impl Into<RefreshError>,
) -> RefreshError {
    let cause = cause.into();
    if snapshot_install(repository, rollback.installed).is_ok_and(|after| after == *before) {
        restore_resident(host, repository, rollback, output);
        cause
    } else {
        restore(host, repository, rollback, output, cause)
    }
}

/// Reinstall the parked executable through its own installer, restart what
/// was running, and report the failure that forced both.
fn restore(
    host: &mut impl Host,
    repository: &Repository,
    rollback: &Rollback<'_>,
    output: &mut impl Write,
    cause: impl Into<RefreshError>,
) -> RefreshError {
    let cause = cause.into();
    let Some(executable) = rollback.executable else {
        return cause;
    };
    let _ = writeln!(output, "restoring previous executable after: {cause}");
    match restore_install(host, repository, executable) {
        Ok(digest) => {
            let _ = writeln!(output, "restored plan={digest}");
        }
        Err(error) => {
            return RefreshError::RestoreFailed {
                cause: cause.to_string(),
                error,
            };
        }
    }
    restore_resident(host, repository, rollback, output);
    cause
}

/// Republish the parked executable's complete asset set through that
/// executable's own `install`: its preview names the plan digest, and its
/// apply publishes exactly that plan with itself as the executable.
///
/// The in-process installer is never run for a restore. It publishes the
/// skill, hooks, and wrapper compiled into this xtask build beside whatever
/// executable it is handed, and a resident over a skill that is not its own
/// refuses every lane launch. A parked executable that cannot read the record
/// it refreshes over, or whose install is refused, is reported as it answered.
fn restore_install(
    host: &mut impl Host,
    repository: &Repository,
    executable: &Path,
) -> Result<Digest, RestoreError> {
    let preview: install::InstallPreview =
        restore_answer(host, repository, executable, RestoreStage::Plan, None)?;
    let digest = preview.digest().clone();
    let receipt: install::InstallReceipt = restore_answer(
        host,
        repository,
        executable,
        RestoreStage::Apply,
        Some(&digest),
    )?;
    Ok(receipt.digest().clone())
}

/// Run one `--json install` stage of the parked executable and decode the
/// outcome it answered: a reply on stdout, a refusal or fault on stderr with a
/// failing status.
fn restore_answer<T: serde::de::DeserializeOwned>(
    host: &mut impl Host,
    repository: &Repository,
    executable: &Path,
    stage: RestoreStage,
    digest: Option<&Digest>,
) -> Result<T, RestoreError> {
    let mut command = Command::new(executable);
    command
        .current_dir(repository.worktree_root())
        .args(["--json", "install"]);
    if let Some(digest) = digest {
        command.arg(format!("--apply={digest}"));
    }
    let answer = host
        .run(&mut command)
        .map_err(|source| RestoreError::Launch {
            executable: executable.to_path_buf(),
            stage,
            source,
        })?;
    let document = if answer.status.success() {
        &answer.stdout
    } else {
        &answer.stderr
    };
    let outcome: wire::Outcome<T> =
        wire::decode(document).map_err(|source| RestoreError::Unreadable {
            stage,
            status: answer.status,
            source,
            diagnostics: refusal_text(&answer.stderr),
        })?;
    match outcome {
        wire::Outcome::Reply(reply) => Ok(reply.body),
        wire::Outcome::Refusal(refusal) => Err(RestoreError::Refused {
            stage,
            reason: refusal.reason().to_owned(),
            detail: refusal.explanation().to_owned(),
        }),
        wire::Outcome::Fault(fault) => Err(RestoreError::Refused {
            stage,
            reason: fault.reason,
            detail: fault.detail,
        }),
    }
}

/// The `install` stage of the parked executable a restore was running.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RestoreStage {
    /// `install`: the preview that names the plan digest.
    Plan,
    /// `install --apply <digest>`: publishing that plan.
    Apply,
}

impl fmt::Display for RestoreStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Plan => "install",
            Self::Apply => "install --apply",
        })
    }
}

/// Why the parked executable did not republish itself.
#[derive(Debug)]
pub enum RestoreError {
    /// The parked executable did not start.
    Launch {
        executable: PathBuf,
        stage: RestoreStage,
        source: std::io::Error,
    },
    /// The parked executable refused or faulted the stage.
    Refused {
        stage: RestoreStage,
        reason: String,
        detail: String,
    },
    /// The parked executable's answer was not the stage's outcome.
    Unreadable {
        stage: RestoreStage,
        status: std::process::ExitStatus,
        source: serde_json::Error,
        diagnostics: String,
    },
}

impl fmt::Display for RestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Launch {
                executable,
                stage,
                source,
            } => write!(
                formatter,
                "the parked executable {} did not start `{stage}`: {source}",
                executable.display()
            ),
            Self::Refused {
                stage,
                reason,
                detail,
            } => write!(
                formatter,
                "the parked executable's `{stage}` answered {reason}: {detail}"
            ),
            Self::Unreadable {
                stage,
                status,
                source,
                diagnostics,
            } => write!(
                formatter,
                "the parked executable's `{stage}` answer did not decode ({status}): {source}; {diagnostics}"
            ),
        }
    }
}

impl std::error::Error for RestoreError {}

/// Start the restored executable's resident back on the exact bind and access
/// policy it had.
///
/// A refresh that stopped the resident and then failed leaves the appliance
/// installed but unserved, so putting the executable back is only half of what
/// was taken away. A resident that will not come back is reported rather than
/// raised, because the failure worth answering is the one that forced the
/// restore, not the restore's own aftermath.
fn restore_resident(
    host: &mut impl Host,
    repository: &Repository,
    rollback: &Rollback<'_>,
    output: &mut impl Write,
) {
    if !rollback.restart || rollback.published.is_none() {
        return;
    }
    let _ = writeln!(output, "restarting previous resident");
    let request = RefreshRequest {
        restart: true,
        resident: rollback.resident.clone(),
        ..RefreshRequest::default()
    };
    let executable = rollback.resident_executable.unwrap_or(rollback.installed);
    if let Err(error) = restart_admitting_reappearance(
        host,
        repository,
        executable,
        &rollback.binding,
        &request,
        output,
    ) {
        let _ = writeln!(output, "previous resident did not come back: {error}");
    }
}

/// Create and answer the repository-common scratch directory for refreshes.
fn workspace(repository: &Repository) -> Result<PathBuf, RefreshError> {
    let state = repository.admit_state().map_err(InstallError::from)?;
    let workspace = state.runtime().join(WORKSPACE);
    fs::create_dir_all(&workspace)?;
    Ok(workspace)
}

/// Discovery the currently published resident names, when one is running.
fn read_discovery(repository: &Repository) -> Option<spice::serve::Discovery> {
    let bytes = fs::read(repository.admit_state().ok()?.serve()).ok()?;
    wire::decode(&bytes).ok()
}

/// Snapshot the live generation's restart contract before any install mutation.
fn restart_state(
    repository: &Repository,
    request: &RefreshRequest,
) -> Result<(Option<String>, RestartBinding), RefreshError> {
    let discovery = read_discovery(repository);
    let published = discovery
        .as_ref()
        .map(|discovery| discovery.endpoint.clone());
    let launch = if request.restart {
        match discovery.as_ref() {
            Some(discovery) => match read_resident_launch(repository, discovery) {
                Ok(launch) => launch,
                Err(_) if !listening(&discovery.endpoint) => None,
                Err(error) => return Err(error),
            },
            None => None,
        }
    } else {
        None
    };
    let binding = requested_binding(request, published.as_deref(), launch)?;
    Ok((published, binding))
}

/// Bind and access metadata published by the same resident generation.
fn read_resident_launch(
    repository: &Repository,
    discovery: &spice::serve::Discovery,
) -> Result<Option<ResidentLaunch>, RefreshError> {
    let state = repository
        .admit_state()
        .map_err(|error| RefreshError::ResidentLaunchUncertain(error.to_string()))?;
    let holder = fs::read(state.resident())
        .map_err(|error| RefreshError::ResidentLaunchUncertain(error.to_string()))?;
    if holder_epoch(&holder) != Some(discovery.epoch) {
        return Err(RefreshError::ResidentLaunchUncertain(format!(
            "holder generation does not match discovery generation {}",
            discovery.epoch
        )));
    }
    holder_launch(&holder).map_err(|error| RefreshError::ResidentLaunchUncertain(error.to_string()))
}

/// Endpoint the published discovery names, when a resident published one.
fn read_endpoint(repository: &Repository) -> Option<String> {
    read_discovery(repository).map(|discovery| discovery.endpoint)
}

/// Exact restart state, with explicit host and port overrides honoured alone.
fn requested_binding(
    request: &RefreshRequest,
    published: Option<&str>,
    launch: Option<ResidentLaunch>,
) -> Result<RestartBinding, RefreshError> {
    let (host, port) = requested_endpoint(request, published, launch);
    let mut binding = RestartBinding {
        host,
        port,
        ..RestartBinding::default()
    };
    let Some(launch) = launch else {
        return Ok(binding);
    };
    binding.allow_insecure_bind = matches!(launch.access(), ResidentAccess::Insecure)
        || (matches!(launch.access(), ResidentAccess::Authenticated)
            && !launch.listen().ip().is_loopback());
    if matches!(launch.access(), ResidentAccess::Authenticated) {
        let Some(token) = forwarded_option_value(&request.up_arguments, "--auth-token")
            .and_then(OsStr::to_str)
            .filter(|token| auth_token_is_valid(token))
        else {
            return Err(RefreshError::ResidentAuthTokenRequired);
        };
        binding.auth_token = Some(token.into());
    }
    Ok(binding)
}

fn requested_endpoint(
    request: &RefreshRequest,
    published: Option<&str>,
    launch: Option<ResidentLaunch>,
) -> (Option<String>, Option<u16>) {
    let inherited = launch
        .map(|launch| (launch.listen().ip().to_string(), launch.listen().port()))
        .or_else(|| published.and_then(endpoint_parts));
    let host = request
        .host
        .clone()
        .or_else(|| inherited.as_ref().map(|(host, _)| host.clone()));
    let port = request.port.or_else(|| inherited.map(|(_, port)| port));
    (host, port)
}

/// Split a published origin into the host and port `up` accepts.
fn endpoint_parts(endpoint: &str) -> Option<(String, u16)> {
    let authority = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, rest)| rest)
        .split('/')
        .next()?;
    let address = authority.parse::<SocketAddr>().ok()?;
    Some((address.ip().to_string(), address.port()))
}

/// Executable whose command principal can authenticate the resident now live.
///
/// No-resident refreshes retain the serving executable separately from each
/// publication's transaction rollback. The bootstrap digest chooses among the
/// installed generation, that serving authority, and the immediate rollback.
/// A different authenticated generation is foreign to this transaction and is
/// refused before any executable is replaced.
fn resident_stop_executable(
    repository: &Repository,
    installed: &Path,
) -> Result<PathBuf, RefreshError> {
    let common = workspace(repository)?;
    let retained = common.join(RESIDENT_ROLLBACK);
    let Some((_, version)) = running_resident_version(repository) else {
        // A transient or remote endpoint cannot identify the resident. Keep any
        // authority already proven by an earlier publication; otherwise `down`
        // remains the exact principal check for the installed executable.
        return Ok(if retained.is_file() {
            retained
        } else {
            installed.to_path_buf()
        });
    };
    if executable_digest(installed)? == version.binary {
        return Ok(installed.to_path_buf());
    }
    let workspace = common;
    for candidate in [RESIDENT_ROLLBACK, ROLLBACK] {
        let executable = workspace.join(candidate);
        if executable.is_file() && executable_digest(&executable)? == version.binary {
            return Ok(executable);
        }
    }
    Err(RefreshError::ResidentStopRefused(format!(
        "resident::foreign-generation binary {} matches neither the installed nor retained predecessor executable",
        version.binary
    )))
}

/// Stop the running resident and wait for its discovery to withdraw.
///
/// A published discovery outlives the process that wrote it, so `down` failing
/// is not itself a refusal: a resident that was already gone refuses the same
/// way one that is still serving does, and only the endpoint separates them.
/// The status is therefore classified against what is still answering. A clean
/// stop waits for the discovery to withdraw; a refusal with nothing answering
/// is a resident already gone and the refresh proceeds; a refusal with the
/// endpoint still live is named here, because waiting it out spends the whole
/// shutdown horizon to report a stop that was refused outright.
///
/// `down` answers its own reply on standard output, which is this command's
/// progress stream and not a result the developer asked for, so that stream is
/// captured and dropped. Its diagnostics are captured too and republished here:
/// an inherited stream puts the refusal wherever the parent's happens to go,
/// which is not this command's own report and is not what the error carries.
fn stop(
    host: &mut impl Host,
    repository: &Repository,
    installed: &Path,
    output: &mut impl Write,
) -> Result<(), RefreshError> {
    writeln!(output, "stopping resident")?;
    let answer = host.run(
        Command::new(installed)
            .arg("down")
            .current_dir(repository.worktree_root()),
    )?;
    if !answer.status.success() {
        let refusal = refusal_text(&answer.stderr);
        writeln!(output, "down refused ({}): {refusal}", answer.status)?;
        if answering(repository) {
            return Err(RefreshError::ResidentStopRefused(refusal));
        }
        writeln!(output, "nothing answers the endpoint; continuing")?;
        return Ok(());
    }
    // `spice down` does not return until discovery is withdrawn and the
    // resident lock has been reacquired by its client-side release verifier.
    // This one postcondition check catches a broken contract without creating a
    // second wait mechanism in the upgrader.
    if answering(repository) {
        return Err(RefreshError::ResidentStop);
    }
    Ok(())
}

/// Whether the published discovery still names an endpoint that answers.
fn answering(repository: &Repository) -> bool {
    read_endpoint(repository).is_some_and(|endpoint| listening(&endpoint))
}

/// Everything a refused `down` said, as one line this command can report.
///
/// Joining rather than taking the last line keeps a multi-line refusal whole,
/// because the line that names the reason is not reliably the last one, and a
/// refusal reported in part is a refusal the developer has to go looking for.
fn refusal_text(diagnostics: &[u8]) -> String {
    let text = String::from_utf8_lossy(diagnostics)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if text.is_empty() {
        "no diagnostic".to_owned()
    } else {
        text
    }
}

/// Admit a resident that won the restart race when it already runs this install.
///
/// The harness or another operator can observe the deliberate dark interval
/// between `down` and the completed install and start a resident itself. A
/// launcher refusal is then correct, but rollback is not: if that resident runs
/// the executable just installed, the requested final state already exists. If
/// it runs the predecessor, stop it once and let the installed successor take
/// the lock.
fn restart_admitting_reappearance(
    host: &mut impl Host,
    repository: &Repository,
    installed: &Path,
    binding: &RestartBinding,
    request: &RefreshRequest,
    output: &mut impl Write,
) -> Result<Option<String>, RefreshError> {
    let mut replaced_predecessor = false;
    loop {
        match restart(host, repository, installed, binding, request, output) {
            Ok(endpoint) => return Ok(endpoint),
            Err(error) => {
                let RefreshError::ResidentStartRefused(refusal) = &error else {
                    return Err(error);
                };
                if !refusal.contains("resident::already-up") {
                    return Err(error);
                }
                let Some((discovery, version)) = running_resident_version(repository) else {
                    return Err(error);
                };
                let installed_binary = executable_digest(installed)?;
                if version.binary == installed_binary {
                    writeln!(
                        output,
                        "resident already ready at {} from the installed executable",
                        discovery.endpoint
                    )?;
                    return Ok(Some(discovery.endpoint));
                }
                if replaced_predecessor {
                    return Err(error);
                }
                writeln!(
                    output,
                    "previous resident reappeared during install; stopping it and retrying"
                )?;
                let authority = resident_stop_executable(repository, installed)?;
                stop(host, repository, &authority, output)?;
                replaced_predecessor = true;
            }
        }
    }
}

/// Exact executable identity used by the resident's bootstrap version.
fn executable_digest(executable: &Path) -> Result<Digest, std::io::Error> {
    let file = File::open(executable)?;
    let limit = file.metadata()?.len();
    Digest::compute_reader(wire::Domain::DistArtifact, file, limit).map(|(_, digest)| digest)
}

/// Authenticated identity of the resident that currently owns the runtime lock.
///
/// A local app needs no bearer token. A remotely authenticated surface keeps
/// that operator secret out of discovery, so an installer that cannot prove
/// identity here retains the launcher's refusal rather than guessing.
fn running_resident_version(
    repository: &Repository,
) -> Option<(spice::serve::Discovery, wire::Version)> {
    let discovery = read_discovery(repository)?;
    let authority = discovery.endpoint.strip_prefix("http://")?;
    let address = authority.parse::<SocketAddr>().ok()?;
    let mut stream = TcpStream::connect_timeout(&address, BOOTSTRAP_PROBE_BOUND).ok()?;
    stream.set_read_timeout(Some(BOOTSTRAP_PROBE_BOUND)).ok()?;
    stream.set_write_timeout(Some(BOOTSTRAP_PROBE_BOUND)).ok()?;
    write!(
        stream,
        "GET /bootstrap HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    )
    .and_then(|()| stream.flush())
    .ok()?;
    let mut response = Vec::new();
    stream
        .take(MAX_BOOTSTRAP_BYTES)
        .read_to_end(&mut response)
        .ok()?;
    if !response.starts_with(b"HTTP/1.1 200 OK\r\n") {
        return None;
    }
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?
        + 4;
    let bootstrap = wire::decode::<wire::AppBootstrap>(&response[split..]).ok()?;
    if bootstrap.endpoint != discovery.endpoint
        || bootstrap.capability != discovery.capability
        || bootstrap.epoch != discovery.epoch
    {
        return None;
    }
    Some((discovery, bootstrap.version))
}

/// Start the refreshed resident and answer the endpoint it published.
fn restart(
    host: &mut impl Host,
    repository: &Repository,
    installed: &Path,
    binding: &RestartBinding,
    request: &RefreshRequest,
    output: &mut impl Write,
) -> Result<Option<String>, RefreshError> {
    if !request.restart {
        return Ok(None);
    }
    let starting = Instant::now();
    let mut command = restart_command(installed, binding, request);
    let answer = host.run(command.current_dir(repository.worktree_root()))?;
    if !answer.status.success() {
        let refusal = refusal_text(&answer.stderr);
        writeln!(
            output,
            "resident start refused ({}): {refusal}",
            answer.status
        )?;
        return Err(RefreshError::ResidentStartRefused(refusal));
    }
    let ready = wire::decode::<wire::Outcome<wire::AppOpen>>(&answer.stdout).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("resident readiness reply did not decode: {error}"),
        )
    })?;
    let wire::Outcome::Reply(ready) = ready else {
        return Err(RefreshError::ResidentStartRefused(format!(
            "successful resident launcher returned {ready:?}"
        )));
    };
    writeln!(
        output,
        "resident ready at {} in {:.1?}",
        ready.body.endpoint,
        starting.elapsed()
    )?;
    Ok(Some(ready.body.endpoint))
}

/// Construct the detached launcher command, with explicit forwarded endpoint
/// options taking precedence over the bind and access state inherited from the
/// live resident. The command's environment is the request's derived resident
/// environment, never the launching shell's.
fn restart_command(
    installed: &Path,
    binding: &RestartBinding,
    request: &RefreshRequest,
) -> Command {
    let mut command = Command::new(installed);
    request.resident.apply(&mut command);
    command.args(["--json", "up"]);
    if !forwards_option(&request.up_arguments, "--host")
        && let Some(host) = binding.host.as_deref()
    {
        command.args(["--host", host]);
    }
    if !forwards_option(&request.up_arguments, "--port")
        && let Some(port) = binding.port
    {
        command.args(["--port", &port.to_string()]);
    }
    if binding.allow_insecure_bind
        && !forwards_option(&request.up_arguments, "--allow-insecure-bind")
    {
        command.arg("--allow-insecure-bind");
    }
    if !forwards_option(&request.up_arguments, "--auth-token")
        && let Some(token) = binding.auth_token.as_deref()
    {
        command.args([OsStr::new("--auth-token"), token]);
    }
    command.args(&request.up_arguments);
    command
}

/// Whether a forwarded argv names one long option, either separately or with
/// its value joined by `=`. Only these two spellings suppress inherited state.
fn forwards_option(arguments: &[OsString], option: &str) -> bool {
    let option_os = OsStr::new(option);
    arguments.iter().any(|argument| {
        argument == option_os
            || argument
                .to_str()
                .and_then(|argument| argument.strip_prefix(option))
                .is_some_and(|suffix| suffix.starts_with('='))
    })
}

fn forwarded_option_value<'a>(arguments: &'a [OsString], option: &str) -> Option<&'a OsStr> {
    let option_os = OsStr::new(option);
    for (index, argument) in arguments.iter().enumerate() {
        if argument == option_os {
            return arguments.get(index + 1).map(OsString::as_os_str);
        }
        if let Some(value) = argument
            .to_str()
            .and_then(|argument| argument.strip_prefix(option))
            .and_then(|suffix| suffix.strip_prefix('='))
        {
            return Some(OsStr::new(value));
        }
    }
    None
}

/// Whether a published endpoint accepts a connection right now.
fn listening(endpoint: &str) -> bool {
    endpoint_parts(endpoint)
        .is_some_and(|(host, port)| TcpStream::connect((host.as_str(), port)).map(drop).is_ok())
}
#[cfg(test)]
mod tests;
