use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use sill::Deadlines;
use spice::ambient::Ambient;

const USAGE_EXIT: u8 = 2;
type CommandResult = Result<(), Box<dyn std::error::Error>>;
const USAGE: &str = concat!(
    "usage: spice dev xtask -- <help|verify --package PACKAGE|",
    "gate [--report] ARM|app [--stage-root DIR]|app-demo [--open] [--scripted-omp]|",
    "install [--host IP] [--port PORT] [--release] [--no-symbols] [--no-resident] ",
    "[-- UP-OPTION...]|",
    "build-cache [--dry-run|--apply] [--max-bytes BYTES] ",
    "[--free-space-floor BYTES] [--retain-live-worktrees N]|",
    "proof-image <build [ARCHIVE]|load ARCHIVE|verify [ARCHIVE]>|",
    "seal-darwin [--output DIR] [--unsigned]|source-archive [OUT]|",
    "share-darwin EXECUTABLE --identity IDENTITY --notary-profile PROFILE|",
    "sign-staged-darwin INPUT OUTPUT --identity IDENTITY --notary-profile PROFILE|",
    "verify-staged-darwin STAGE|",
    "stage-target TARGET [--unsigned] [--proof-container IMAGE] [--staged-app|--app-only]|windows-appliance --unsigned|windows-msvc|windows-hosted -- PROGRAM [ARG...]|",
    "windows-guest <provision [--replace]|start|stop|status|reset|",
    "sync [--artifact PATH]... [--source PATH]...|",
    "run [--timeout SECONDS] [--output-bytes BYTES] -- PROGRAM [ARG...]>>"
);

fn help() -> String {
    let arms = gate::GateId::ALL.map(gate::GateId::as_str).join("\n  ");
    format!(
        concat!(
            "{usage}\n\n",
            "For focused arms, gate ARM emits bounded diagnostics only on refusal; ",
            "gate --report ARM emits one canonical machine report.\n",
            "build-cache inventories and collects Cargo's configured managed build roots ",
            "and Cargo-tagged worktree target roots through one lock-aware policy.\n\n",
            "gate ARM accepts:\n  {arms}"
        ),
        usage = USAGE,
        arms = arms,
    )
}

fn main() -> ExitCode {
    let ambient = capture_ambient();
    let mut arguments = env::args_os().skip(1);
    let command = arguments.next();

    let mut output = io::stdout().lock();
    // Dispatch only current spellings; removed command names have no alias arm.
    let result: CommandResult = match command.as_deref() {
        Some(value) if is_exact_command(value, "help", arguments.len(), 0) => {
            writeln!(output, "{}", help()).map_err(Into::into)
        }
        Some(value) if value == OsStr::new("verify") => {
            run_verify(arguments.collect(), &ambient, &mut output)
        }
        Some(value) if value == OsStr::new("gate") => run_gate(
            arguments.collect(),
            &ambient,
            &mut output,
            &mut io::stderr().lock(),
        ),
        Some(value) if value == OsStr::new("app") => {
            run_app(arguments.collect(), &ambient, &mut output)
        }
        #[cfg(not(target_arch = "wasm32"))]
        Some(value) if value == OsStr::new("app-demo") => {
            run_app_demo(arguments.collect(), &ambient, &mut output)
        }
        Some(value) if value == OsStr::new("build-cache") => {
            run_build_cache(arguments.collect(), &mut output)
        }
        Some(value) if value == OsStr::new("install") => {
            drop(output);
            return run_install(arguments.collect(), &ambient);
        }
        Some(value) if value == OsStr::new("proof-image") => {
            drop(output);
            return run_proof_image(arguments.collect());
        }
        #[cfg(not(target_arch = "wasm32"))]
        Some(value) if value == OsStr::new("seal-darwin") => {
            run_seal_darwin(arguments.collect(), &ambient, &mut output)
        }
        #[cfg(not(target_arch = "wasm32"))]
        Some(value) if value == OsStr::new("share-darwin") => {
            run_share_darwin(arguments.collect(), &mut output)
        }
        #[cfg(not(target_arch = "wasm32"))]
        Some(value) if value == OsStr::new("sign-staged-darwin") => {
            run_sign_staged_darwin(arguments.collect(), &mut output)
        }
        #[cfg(not(target_arch = "wasm32"))]
        Some(value) if value == OsStr::new("verify-staged-darwin") => {
            run_verify_staged_darwin(arguments.collect())
        }
        #[cfg(not(target_arch = "wasm32"))]
        Some(value) if value == OsStr::new("source-archive") => {
            run_source_archive(arguments.collect(), &ambient)
        }
        Some(value) if value == OsStr::new("stage-target") => run_stage_target(
            arguments.collect(),
            &ambient,
            &mut output,
            &mut io::stderr().lock(),
        ),
        Some(value) if value == OsStr::new("windows-appliance") => {
            run_windows_appliance(arguments.collect(), &ambient, &mut output)
        }
        Some(value) if is_exact_command(value, "windows-msvc", arguments.len(), 0) => {
            run_windows_msvc(&ambient, &mut output)
        }
        Some(value) if value == OsStr::new("windows-hosted") => {
            drop(output);
            return run_windows_hosted(arguments.collect());
        }
        #[cfg(target_os = "macos")]
        Some(value) if value == OsStr::new("windows-guest") => {
            drop(output);
            return run_windows_guest(arguments.collect(), &ambient);
        }
        _ => {
            eprintln!("{}", help());
            return ExitCode::from(USAGE_EXIT);
        }
    };
    finish(result)
}

fn is_exact_command(
    command: &OsStr,
    expected: &str,
    actual_arity: usize,
    expected_arity: usize,
) -> bool {
    command == OsStr::new(expected) && actual_arity == expected_arity
}

fn finish(result: Result<(), Box<dyn std::error::Error>>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is::<UsageError>() => ExitCode::from(USAGE_EXIT),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run_verify(
    arguments: Vec<OsString>,
    ambient: &Ambient,
    output: &mut impl io::Write,
) -> CommandResult {
    match arguments.as_slice() {
        [option, package] if option == OsStr::new("--package") => {
            let Some(package) = package.to_str() else {
                eprintln!("{}", help());
                return Err(usage_error().into());
            };
            xtask::verify_package(package, ambient, output).map_err(Into::into)
        }
        _ => {
            eprintln!("{}", help());
            Err(usage_error().into())
        }
    }
}

fn run_app(
    arguments: Vec<OsString>,
    ambient: &Ambient,
    output: &mut impl io::Write,
) -> CommandResult {
    match arguments.as_slice() {
        [] => xtask::app(&release_environment(ambient), output).map_err(Into::into),
        [option, root] if option == OsStr::new("--stage-root") => xtask::app_to(
            &release_environment(ambient),
            std::path::Path::new(root),
            output,
        )
        .map_err(Into::into),
        _ => {
            eprintln!("{}", help());
            Err(usage_error().into())
        }
    }
}

fn run_windows_appliance(
    arguments: Vec<OsString>,
    ambient: &Ambient,
    output: &mut impl io::Write,
) -> CommandResult {
    let unsigned = match arguments.as_slice() {
        [] => false,
        [flag] if flag == OsStr::new("--unsigned") => true,
        _ => {
            eprintln!("{}", help());
            return Err(usage_error().into());
        }
    };
    require_unsigned("x86_64-pc-windows-msvc", unsigned)?;
    xtask::build_windows_appliance(release_environment(ambient), ambient.environment()?, output)
        .map(|_| ())
        .map_err(Into::into)
}

#[cfg(not(target_arch = "wasm32"))]
fn run_app_demo(
    arguments: Vec<OsString>,
    ambient: &Ambient,
    output: &mut impl io::Write,
) -> CommandResult {
    let mut open = false;
    let mut mode = xtask::AppDemoMode::Browser;
    for argument in arguments {
        if argument == OsStr::new("--open") {
            if open {
                eprintln!("{}", help());
                return Err(usage_error().into());
            }
            open = true;
            continue;
        }
        let text = argument.to_string_lossy();
        if text == "--scripted-omp" && matches!(mode, xtask::AppDemoMode::Browser) {
            mode = xtask::AppDemoMode::ScriptedOmp;
            continue;
        }
        eprintln!("{}", help());
        return Err(usage_error().into());
    }
    xtask::app_demo(&release_environment(ambient), open, mode, output).map_err(Into::into)
}

fn run_gate(
    arguments: Vec<OsString>,
    ambient: &Ambient,
    output: &mut impl io::Write,
    progress: &mut impl io::Write,
) -> CommandResult {
    let (arm, emit_report) = match arguments.as_slice() {
        [arm] => (arm, false),
        [flag, arm] if flag == OsStr::new("--report") => (arm, true),
        _ => {
            eprintln!("{}", help());
            return Err(usage_error().into());
        }
    };
    let Some(arm) = arm.to_str() else {
        eprintln!("{}", help());
        return Err(usage_error().into());
    };
    let Some(gate_id) = gate::GateId::ALL
        .into_iter()
        .find(|gate_id| gate_id.as_str() == arm)
    else {
        eprintln!("unknown gate arm {arm:?}; run `spice dev xtask -- help` for accepted gate arms");
        return Err(usage_error().into());
    };

    if emit_report {
        xtask::gate_arm_report(ambient, gate_id, output, progress).map_err(Into::into)
    } else {
        xtask::gate_arm(ambient, gate_id, output, progress).map_err(Into::into)
    }
}

fn run_build_cache(arguments: Vec<OsString>, output: &mut impl io::Write) -> CommandResult {
    let Some(request) = build_cache_request(&arguments) else {
        eprintln!("{}", help());
        return Err(usage_error().into());
    };
    xtask::build_cache(&request, output)
        .map(|_| ())
        .map_err(Into::into)
}

/// Seal this worktree's runner at HEAD, bind it here, stage the sealed Darwin
/// target, and place the executable: one point-in-time build that touches no
/// other worktree, the installed appliance, or the resident.
#[cfg(not(target_arch = "wasm32"))]
fn run_seal_darwin(
    arguments: Vec<OsString>,
    ambient: &Ambient,
    output: &mut impl io::Write,
) -> CommandResult {
    let mut destination = None;
    let mut unsigned = false;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match (argument.to_str(), destination.is_none()) {
            (Some("--output"), true) => {
                let Some(directory) = arguments.next() else {
                    eprintln!("{}", help());
                    return Err(usage_error().into());
                };
                destination = Some(PathBuf::from(directory));
            }
            (Some("--unsigned"), _) if !unsigned => unsigned = true,
            _ => {
                eprintln!("{}", help());
                return Err(usage_error().into());
            }
        }
    }
    const TARGET: &str = "aarch64-apple-darwin";
    let signing = darwin_signing(unsigned)?;
    let root = env::current_dir()?;
    xtask::seal_runner(&root, output)?;
    xtask::source_archive(None, ambient.environment()?)?;
    preflight_native_release(TARGET)?;
    let build = xtask::stage_target(
        TARGET,
        release_environment(ambient),
        signing.as_ref(),
        None,
        false,
        output,
    )?;
    if let Some(directory) = destination {
        xtask::place_executable(&build.executable, &directory, output)?;
    }
    Ok(())
}
#[cfg(not(target_arch = "wasm32"))]
fn run_share_darwin(arguments: Vec<OsString>, output: &mut impl io::Write) -> CommandResult {
    let [executable, identity_flag, identity, profile_flag, profile] = arguments.as_slice() else {
        eprintln!("{}", help());
        return Err(usage_error().into());
    };
    if identity_flag != OsStr::new("--identity") || profile_flag != OsStr::new("--notary-profile") {
        eprintln!("{}", help());
        return Err(usage_error().into());
    }
    let (Some(identity), Some(profile)) = (identity.to_str(), profile.to_str()) else {
        eprintln!("{}", help());
        return Err(usage_error().into());
    };
    let signing = dist::DarwinSigning::new(
        dist::DeveloperIdentity::new(identity)?,
        dist::NotaryProfile::new(profile)?,
    );
    xtask::share_darwin(&xtask::ShareDarwinRequest::new(executable, signing), output)
        .map_err(Into::into)
}

#[cfg(not(target_arch = "wasm32"))]
fn run_sign_staged_darwin(arguments: Vec<OsString>, output: &mut impl io::Write) -> CommandResult {
    let [
        input,
        destination,
        identity_flag,
        identity,
        profile_flag,
        profile,
    ] = arguments.as_slice()
    else {
        eprintln!("{}", help());
        return Err(usage_error().into());
    };
    if identity_flag != OsStr::new("--identity") || profile_flag != OsStr::new("--notary-profile") {
        eprintln!("{}", help());
        return Err(usage_error().into());
    }
    let (Some(identity), Some(profile)) = (identity.to_str(), profile.to_str()) else {
        eprintln!("{}", help());
        return Err(usage_error().into());
    };
    let signing = dist::DarwinSigning::new(
        dist::DeveloperIdentity::new(identity)?,
        dist::NotaryProfile::new(profile)?,
    );
    xtask::sign_staged_darwin(Path::new(input), Path::new(destination), signing, output)
        .map_err(Into::into)
}

#[cfg(not(target_arch = "wasm32"))]
fn run_verify_staged_darwin(arguments: Vec<OsString>) -> CommandResult {
    let [stage] = arguments.as_slice() else {
        eprintln!("{}", help());
        return Err(usage_error().into());
    };
    xtask::verify_staged_darwin(Path::new(stage)).map_err(Into::into)
}

fn run_stage_target(
    arguments: Vec<OsString>,
    ambient: &Ambient,
    output: &mut impl io::Write,
    error_output: &mut impl io::Write,
) -> CommandResult {
    let (target, unsigned, proof_container, scope) = match arguments.as_slice() {
        [target] => (target.to_str(), false, None, StageScope::Target),
        [target, flag] if flag == OsStr::new("--unsigned") => {
            (target.to_str(), true, None, StageScope::Target)
        }
        [target, flag, image] if flag == OsStr::new("--proof-container") => {
            (target.to_str(), false, image.to_str(), StageScope::Target)
        }
        [target, unsigned_flag, image_flag, image]
            if unsigned_flag == OsStr::new("--unsigned")
                && image_flag == OsStr::new("--proof-container") =>
        {
            (target.to_str(), true, image.to_str(), StageScope::Target)
        }
        [target, unsigned_flag, scope_flag]
            if unsigned_flag == OsStr::new("--unsigned")
                && scope_flag == OsStr::new("--app-only") =>
        {
            (target.to_str(), true, None, StageScope::AppOnly)
        }
        [target, unsigned_flag, image_flag, image, scope_flag]
            if unsigned_flag == OsStr::new("--unsigned")
                && image_flag == OsStr::new("--proof-container") =>
        {
            let scope = if scope_flag == OsStr::new("--staged-app") {
                Some(StageScope::StagedApp)
            } else if scope_flag == OsStr::new("--app-only") {
                Some(StageScope::AppOnly)
            } else {
                None
            };
            match scope {
                Some(scope) => (target.to_str(), true, image.to_str(), scope),
                None => (None, false, None, StageScope::Target),
            }
        }
        _ => (None, false, None, StageScope::Target),
    };
    let Some(target) = target else {
        eprintln!("{}", help());
        return Err(usage_error().into());
    };
    if UNSIGNED_TARGETS.contains(&target) {
        require_unsigned(target, unsigned)?;
    }
    let environment = release_environment(ambient);
    if target == "x86_64-pc-windows-msvc" && !environment.is_hosted_guest() {
        let archive = Path::new("dist/stage").join(dist::SOURCE_ARCHIVE_FILE);
        let resolved_proof_container;
        let proof_container = match proof_container {
            Some(image) => image,
            None => {
                resolved_proof_container = xtask::release_proof_image()?;
                &resolved_proof_container
            }
        };
        return xtask::windows_hosted_stage(
            &archive,
            proof_container,
            scope == StageScope::AppOnly,
            output,
            error_output,
        )
        .and_then(|execution| {
            (execution.exit_code() == 0).then_some(()).ok_or_else(|| {
                xtask::WindowsHostedError::ProviderResponse(format!(
                    "hosted Windows staging exited {}",
                    execution.exit_code()
                ))
            })
        })
        .map_err(Into::into);
    }
    if scope == StageScope::AppOnly {
        let resolved_proof_container;
        let proof_container = match proof_container {
            Some(image) => image,
            None => {
                resolved_proof_container = xtask::release_proof_image()?;
                &resolved_proof_container
            }
        };
        return xtask::stage_target_app(target, environment, proof_container, output)
            .map_err(Into::into);
    }
    if target == "x86_64-unknown-linux-musl" && !cfg!(target_os = "linux") {
        return xtask::linux_sealed_stage(&environment, output, error_output).map_err(Into::into);
    }
    let signing = if target == "aarch64-apple-darwin" {
        darwin_signing(unsigned)?
    } else {
        None
    };
    preflight_native_release(target).and_then(|()| {
        xtask::stage_target(
            target,
            environment,
            signing.as_ref(),
            proof_container,
            scope == StageScope::StagedApp,
            output,
        )
        .map(|_| ())
        .map_err(Into::into)
    })
}

/// How much of one release target `stage-target` builds.
#[derive(Clone, Copy, Eq, PartialEq)]
enum StageScope {
    /// Build the app and the target, and stage the target payload.
    Target,
    /// Link the app generation already staged in the workspace.
    StagedApp,
    /// Build only the app through the release executor, staging nothing.
    AppOnly,
}

/// Release targets with no signing route yet. Their unsigned executables are
/// the releases for now, built only when the caller names that result.
const UNSIGNED_TARGETS: [&str; 2] = ["x86_64-unknown-linux-musl", "x86_64-pc-windows-msvc"];

fn require_unsigned(target: &str, unsigned: bool) -> CommandResult {
    if unsigned {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "{target} has no signing route yet; pass --unsigned to build its unsigned release executable"
        ),
    )
    .into())
}

/// The Developer ID pair Darwin staging signs with. Without one, staging
/// refuses before any build unless the operator explicitly asks for an
/// unsigned executable, which Gatekeeper blocks on every other Mac.
fn darwin_signing(
    unsigned: bool,
) -> Result<Option<dist::DarwinSigning>, Box<dyn std::error::Error>> {
    if unsigned {
        return Ok(None);
    }
    let repository = spice::repository::Repository::discover(Path::new("."))?;
    let config = spice::config::load_repository(&repository, &[])?;
    config.root().release().darwin_signing()?.map_or_else(
        || {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                concat!(
                    "Darwin release staging signs with a Developer ID; set ",
                    "release.darwin.identity and release.darwin.notary_profile in ",
                    "Git-common configuration (spice config KEY VALUE previews each write ",
                    "and its --apply=DIGEST applies it), or pass --unsigned for an ",
                    "executable Gatekeeper blocks"
                ),
            )
            .into())
        },
        |signing| Ok(Some(signing)),
    )
}
fn run_windows_msvc(ambient: &Ambient, output: &mut impl io::Write) -> CommandResult {
    xtask::windows_msvc(&ambient.environment()?, output)
        .map(|_| ())
        .map_err(Into::into)
}

#[derive(Debug)]
struct UsageError;

impl std::fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("invalid developer xtask invocation")
    }
}

impl std::error::Error for UsageError {}

fn usage_error() -> UsageError {
    UsageError
}

fn run_install(arguments: Vec<OsString>, ambient: &Ambient) -> ExitCode {
    let Some(mut request) = install_request(&arguments) else {
        eprintln!("{}", help());
        return ExitCode::from(USAGE_EXIT);
    };
    request.lane = ambient
        .get(wire::SpiceVariable::Lane.as_str())
        .map(OsStr::to_os_string);
    request.resident = spice::cargo_isolation::ResidentLaunchEnvironment::derive(
        ambient.variables().iter().cloned(),
    );
    let mut output = io::stdout().lock();
    match xtask::refresh(&request, &release_environment(ambient), &mut output) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Read one `spice dev xtask -- install` invocation, or refuse it.
fn install_request(arguments: &[OsString]) -> Option<xtask::RefreshRequest> {
    let mut request = xtask::RefreshRequest {
        restart: true,
        // Worktree dSYMs exist only on macOS; elsewhere the bare command
        // publishes without them instead of refusing.
        symbols: cfg!(target_os = "macos"),
        ..xtask::RefreshRequest::default()
    };
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.to_str()? {
            "--release" => request.release = true,
            "--no-symbols" => request.symbols = false,
            "--no-resident" => request.restart = false,
            "--host" => request.host = Some(arguments.next()?.to_str()?.to_owned()),
            "--port" => request.port = Some(arguments.next()?.to_str()?.parse().ok()?),
            "--" => {
                request.up_arguments.extend(arguments.cloned());
                break;
            }
            _ => return None,
        }
    }
    Some(request)
}

fn build_cache_request(arguments: &[OsString]) -> Option<xtask::BuildCacheRequest> {
    let mut request = xtask::BuildCacheRequest::default();
    let mut mode = None;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.to_str()? {
            "--dry-run" => {
                if mode.replace(false).is_some() {
                    return None;
                }
                request.apply = false;
            }
            "--apply" => {
                if mode.replace(true).is_some() {
                    return None;
                }
                request.apply = true;
            }
            "--max-bytes" => {
                request.max_bytes =
                    Some(xtask::parse_byte_quantity(arguments.next()?.to_str()?).ok()?);
            }
            "--free-space-floor" => {
                request.free_space_floor =
                    Some(xtask::parse_byte_quantity(arguments.next()?.to_str()?).ok()?);
            }
            "--retain-live-worktrees" => {
                request.retain_live_worktrees = arguments.next()?.to_str()?.parse().ok()?;
            }
            _ => return None,
        }
    }
    Some(request)
}

fn run_proof_image(arguments: Vec<OsString>) -> ExitCode {
    let (operation, archive) = match arguments.as_slice() {
        [operation] if operation == OsStr::new("build") => ("build", None),
        [operation] if operation == OsStr::new("verify") => ("verify", None),
        [operation, archive] if operation == OsStr::new("build") => {
            ("build", Some(PathBuf::from(archive)))
        }
        [operation, archive] if operation == OsStr::new("load") => {
            ("load", Some(PathBuf::from(archive)))
        }
        [operation, archive] if operation == OsStr::new("verify") => {
            ("verify", Some(PathBuf::from(archive)))
        }
        _ => {
            eprintln!("{}", help());
            return ExitCode::from(USAGE_EXIT);
        }
    };
    let repository = match spice::repository::Repository::discover(Path::new(".")) {
        Ok(repository) => repository,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let archive = archive.unwrap_or_else(|| {
        repository
            .worktree_root()
            .join("target/proof-image/spice-proof-image.tar")
    });
    let result = match operation {
        "build" => xtask::build_proof_image(
            repository.worktree_root(),
            &archive,
            &mut stdout,
            &mut stderr,
        ),
        "load" => xtask::load_proof_image(
            repository.worktree_root(),
            &archive,
            &mut stdout,
            &mut stderr,
        ),
        "verify" => xtask::build_proof_image(
            repository.worktree_root(),
            &archive,
            &mut stdout,
            &mut stderr,
        )
        .and_then(|()| {
            xtask::load_proof_image(
                repository.worktree_root(),
                &archive,
                &mut stdout,
                &mut stderr,
            )
        }),
        _ => unreachable!("proof-image operation was admitted above"),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error.exit_code())
        }
    }
}

fn run_windows_hosted(mut arguments: Vec<OsString>) -> ExitCode {
    if arguments.first().is_some_and(|argument| argument == "--") {
        arguments.remove(0);
    }
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    match xtask::windows_hosted(arguments, &mut stdout, &mut stderr) {
        Ok(execution) => {
            u8::try_from(execution.exit_code()).map_or_else(|_| ExitCode::FAILURE, ExitCode::from)
        }
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "macos")]
fn run_windows_guest(arguments: Vec<OsString>, ambient: &Ambient) -> ExitCode {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let lane = match ambient.get(wire::SpiceVariable::Lane.as_str()) {
        Some(value) => match value.to_str().and_then(|value| value.parse().ok()) {
            Some(lane) => Some(lane),
            None => {
                let _ = writeln!(stderr, "windows-guest: SPICE_LANE is not a canonical UUID");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    match xtask::windows_guest(arguments, lane, &mut stdout, &mut stderr) {
        Ok(code) => u8::try_from(code).map_or_else(|_| ExitCode::FAILURE, ExitCode::from),
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            ExitCode::from(error.exit_code())
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run_source_archive(arguments: Vec<OsString>, ambient: &Ambient) -> CommandResult {
    let output = match arguments.as_slice() {
        [] => None,
        [output] => Some(Path::new(output)),
        _ => {
            eprintln!("{}", help());
            return Err(usage_error().into());
        }
    };
    xtask::source_archive(output, ambient.environment()?).map_err(Into::into)
}

fn preflight_native_release(target: &str) -> Result<(), Box<dyn std::error::Error>> {
    if target != "aarch64-apple-darwin" {
        return Ok(());
    }
    let repository = spice::repository::Repository::discover(Path::new("."))?;
    let config = spice::config::load_repository(&repository, &[])?;
    let runner = config.root().proof().runner().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            concat!(
                "proof.runner is required before Darwin release staging; ",
                "seal and bind this worktree's runner with dist/proof-runner/build.sh ",
                "or run seal-darwin"
            ),
        )
    })?;
    let deadlines = Deadlines::new(
        Duration::from_secs(60),
        Duration::from_secs(10),
        Duration::from_secs(10),
    )?;
    let limits = proof::Limits::new(deadlines, Duration::from_secs(5), 1024 * 1024)?;
    let admitted =
        proof::NativeRunner::attest(runner.as_path(), repository.worktree_root(), limits)
            .map_err(|error| runner_refused(runner.as_path(), &error))?;
    admitted
        .preflight()
        .map_err(|error| runner_refused(runner.as_path(), &error))?;
    Ok(())
}

/// Name the bound runner in its refusal: the runner's own stderr already
/// names its sealed commit, the observed HEAD, and the reseal command.
fn runner_refused(runner: &Path, error: &proof::NativeError) -> io::Error {
    io::Error::other(format!(
        "proof.runner {} (this worktree's binding) refused: {error}",
        runner.display()
    ))
}

/// Capture this process's environment once; `main` threads it to every reader.
#[expect(
    clippy::disallowed_methods,
    reason = "the xtask binary's single capture of its process environment"
)]
fn capture_ambient() -> Ambient {
    Ambient::new(env::vars_os())
}

fn release_environment(ambient: &Ambient) -> xtask::ReleaseEnvironment {
    let read = |name: &str| ambient.get(name).map(OsStr::to_os_string);
    xtask::ReleaseEnvironment {
        path: read("PATH"),
        home: read("HOME"),
        cargo_home: read("CARGO_HOME"),
        rustup_home: read("RUSTUP_HOME"),
        rustup_toolchain: read("RUSTUP_TOOLCHAIN"),
        temporary: read("TMPDIR"),
        temp: read("TEMP"),
        tmp: read("TMP"),
        system_root: read("SystemRoot"),
        path_ext: read("PATHEXT"),
        trunk_offline: read("TRUNK_OFFLINE"),
        cc: read("CC"),
        cxx: read("CXX"),
        include: read("INCLUDE"),
        lib: read("LIB"),
        sdk_root: read("SDKROOT"),
        developer_dir: read("DEVELOPER_DIR"),
        cargo_target_dir: read("CARGO_TARGET_DIR"),
        rustc: read("RUSTC"),
        build_rustc: read("CARGO_BUILD_RUSTC"),
        stage_root: read("SPICE_HOSTED_STAGE"), // env-policy: allow
        source_archive: read("SPICE_SOURCE_ARCHIVE"), // env-policy: allow
    }
}

#[cfg(test)]
mod tests {
    use super::install_request;
    use std::ffi::OsString;

    #[test]
    fn install_forwards_every_argument_after_its_delimiter_to_up() {
        let arguments = [
            "--release",
            "--",
            "--host",
            "0.0.0.0",
            "--allow-insecure-bind",
            "--open",
        ]
        .map(OsString::from);

        let request = install_request(&arguments).expect("install arguments parse");

        assert!(request.release);
        assert_eq!(
            request.up_arguments,
            ["--host", "0.0.0.0", "--allow-insecure-bind", "--open"].map(OsString::from)
        );
    }
}
