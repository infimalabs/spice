use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

use spice::ambient::Ambient;

use super::{
    CommandResult, cross_host, help, preflight_native_release, release_environment, usage_error,
};

pub(super) fn run_stage_target(
    arguments: Vec<OsString>,
    ambient: &Ambient,
    output: &mut impl io::Write,
    error_output: &mut impl io::Write,
) -> CommandResult {
    let Some(StageRequest {
        target,
        unsigned,
        proof_container,
        proof_runner,
        scope,
        route,
        attach,
        release_inputs,
        prebuilt,
    }) = parse_stage_target(&arguments)
    else {
        eprintln!("{}", help());
        return Err(usage_error().into());
    };
    if UNSIGNED_TARGETS.contains(&target) {
        require_unsigned(target, unsigned)?;
    }
    let environment = release_environment(ambient);
    if let Some((manifest, bundle)) = prebuilt {
        let proof_container = proof_container.ok_or_else(usage_error)?;
        let app = if environment.release_canvas.is_some() {
            xtask::AppOrigin::Provider
        } else {
            xtask::AppOrigin::Linux
        };
        return xtask::stage_prebuilt_target(
            environment,
            proof_container,
            app,
            manifest,
            bundle,
            output,
        )
        .map(|_| ())
        .map_err(Into::into);
    }
    if target == "x86_64-pc-windows-msvc" && !environment.is_hosted_guest() {
        if scope != StageScope::Target {
            eprintln!("{}", help());
            return Err(usage_error().into());
        }
        let inputs = xtask::hosted_release_inputs(release_inputs.ok_or_else(usage_error)?)?;
        return xtask::windows_hosted_stage(
            &inputs,
            &cross_host(ambient)?,
            attach,
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
        let proof_container = resolve_proof_container(proof_container)?;
        return xtask::stage_target_app(target, environment, &proof_container, output)
            .map_err(Into::into);
    }
    if route == StageRoute::Hosted {
        let inputs = xtask::hosted_release_inputs(release_inputs.ok_or_else(usage_error)?)?;
        return match target {
            LINUX_TARGET => xtask::linux_hosted_stage(&inputs, attach, output).map_err(Into::into),
            DARWIN_TARGET => xtask::macos_hosted_stage(&inputs, attach, output).map_err(Into::into),
            _ => unreachable!("hosted route parser admits only hosted targets"),
        };
    }
    // Only the hosted guest's own Windows stage reaches here with a run to
    // re-enter, and it has none.
    if attach.is_some() {
        eprintln!("{}", help());
        return Err(usage_error().into());
    }
    if target == LINUX_TARGET && (route == StageRoute::Sealed || !cfg!(target_os = "linux")) {
        return xtask::linux_sealed_stage(&environment, output, error_output).map_err(Into::into);
    }
    let signing = if target == DARWIN_TARGET {
        darwin_signing(unsigned)?
    } else {
        None
    };
    // Every Darwin or Windows stage links the canvas of the Linux payload
    // staged from the same archive: the local stage root's, or the one a
    // hosted runner downloaded beside its checkout.
    let app = if scope == StageScope::StagedApp {
        xtask::AppOrigin::Workspace
    } else if environment.release_canvas.is_some() {
        xtask::AppOrigin::Provider
    } else if target == LINUX_TARGET {
        xtask::AppOrigin::Compile
    } else {
        xtask::AppOrigin::Linux
    };
    preflight_native_release(target, proof_runner).and_then(|()| {
        xtask::stage_target(
            target,
            environment,
            signing.as_ref(),
            proof_container,
            app,
            false,
            output,
        )
        .map(|_| ())
        .map_err(Into::into)
    })
}

/// The named proof image, or the current vendor-set image this worktree
/// resolves.
fn resolve_proof_container(named: Option<&str>) -> Result<String, xtask::TargetBuildError> {
    named.map_or_else(xtask::release_proof_image, |image| Ok(image.to_owned()))
}

/// One admitted `stage-target` argument vector.
struct StageRequest<'a> {
    target: &'a str,
    unsigned: bool,
    proof_container: Option<&'a str>,
    proof_runner: Option<&'a str>,
    scope: StageScope,
    route: StageRoute,
    /// The provider run a hosted stage re-enters instead of dispatching.
    attach: Option<xtask::Attach>,
    /// The provider preflight run whose canonical inputs this stage consumes.
    release_inputs: Option<xtask::Attach>,
    /// The cross-built bundle's manifest and unpacked root a prebuilt
    /// Windows stage adopts its candidate from.
    prebuilt: Option<(&'a Path, &'a Path)>,
}

fn parse_provider_controls(
    arguments: &[OsString],
) -> Option<(Option<xtask::Attach>, Option<xtask::Attach>, &[OsString])> {
    let (attach, arguments) = match arguments {
        [rest @ .., flag] if flag == OsStr::new("--attach") => (Some(xtask::Attach::Sole), rest),
        [rest @ .., flag, run_id] if flag == OsStr::new("--attach") => {
            (Some(xtask::Attach::run(run_id.to_str()?)?), rest)
        }
        _ => (None, arguments),
    };
    let (release_inputs, arguments) = match arguments {
        [rest @ .., flag, run_id] if flag == OsStr::new("--release-inputs") => {
            (Some(xtask::Attach::run(run_id.to_str()?)?), rest)
        }
        _ => (None, arguments),
    };
    Some((attach, release_inputs, arguments))
}

fn parse_stage_route(arguments: &[OsString]) -> (StageRoute, &[OsString]) {
    match arguments.split_last() {
        Some((flag, rest)) if flag == OsStr::new("--sealed") => (StageRoute::Sealed, rest),
        Some((flag, rest)) if flag == OsStr::new("--hosted") => (StageRoute::Hosted, rest),
        _ => (StageRoute::Default, arguments),
    }
}

fn parse_stage_target(arguments: &[OsString]) -> Option<StageRequest<'_>> {
    let (attach, release_inputs, arguments) = parse_provider_controls(arguments)?;
    let (route, arguments) = parse_stage_route(arguments);
    let (prebuilt, arguments) = parse_prebuilt(arguments);
    let (proof_runner, arguments) = match arguments {
        [rest @ .., flag, runner] if flag == OsStr::new("--proof-runner") => {
            (Some(runner.to_str()?), rest)
        }
        _ => (None, arguments),
    };
    let (target, unsigned, proof_container, scope) = match arguments {
        [target] => (target, false, None, StageScope::Target),
        [target, flag] if flag == OsStr::new("--unsigned") => {
            (target, true, None, StageScope::Target)
        }
        [target, flag, image] if flag == OsStr::new("--proof-container") => {
            (target, false, Some(image.to_str()?), StageScope::Target)
        }
        [target, unsigned_flag, image_flag, image]
            if unsigned_flag == OsStr::new("--unsigned")
                && image_flag == OsStr::new("--proof-container") =>
        {
            (target, true, Some(image.to_str()?), StageScope::Target)
        }
        [target, unsigned_flag, scope_flag]
            if unsigned_flag == OsStr::new("--unsigned")
                && scope_flag == OsStr::new("--app-only") =>
        {
            (target, true, None, StageScope::AppOnly)
        }
        [target, unsigned_flag, image_flag, image, scope_flag]
            if unsigned_flag == OsStr::new("--unsigned")
                && image_flag == OsStr::new("--proof-container") =>
        {
            let scope = if scope_flag == OsStr::new("--staged-app") {
                StageScope::StagedApp
            } else if scope_flag == OsStr::new("--app-only") {
                StageScope::AppOnly
            } else {
                return None;
            };
            (target, true, Some(image.to_str()?), scope)
        }
        _ => return None,
    };
    let target = target.to_str()?;
    if !stage_route_admitted(
        target,
        unsigned,
        proof_container,
        proof_runner,
        scope,
        route,
    ) {
        return None;
    }
    // Only a provider-hosted stage has a run to re-enter: an explicit hosted
    // route, or Windows, whose every non-guest stage is hosted.
    if (attach.is_some() || release_inputs.is_some())
        && route != StageRoute::Hosted
        && target != WINDOWS_TARGET
    {
        return None;
    }
    let request = StageRequest {
        target,
        unsigned,
        proof_container,
        proof_runner,
        scope,
        route,
        attach,
        release_inputs,
        prebuilt,
    };
    request.prebuilt_admitted().then_some(request)
}

fn parse_prebuilt(arguments: &[OsString]) -> (Option<(&Path, &Path)>, &[OsString]) {
    match arguments {
        [rest @ .., flag, manifest, bundle] if flag == OsStr::new("--prebuilt") => {
            (Some((Path::new(manifest), Path::new(bundle))), rest)
        }
        _ => (None, arguments),
    }
}

impl StageRequest<'_> {
    /// A prebuilt stage adopts its candidate on the machine that runs it: an
    /// unsigned Windows target stage with a named proof image and nothing to
    /// dispatch or re-enter.
    fn prebuilt_admitted(&self) -> bool {
        self.prebuilt.is_none()
            || (self.target == WINDOWS_TARGET
                && self.unsigned
                && self.proof_container.is_some()
                && self.proof_runner.is_none()
                && self.scope == StageScope::Target
                && self.route == StageRoute::Default
                && self.attach.is_none()
                && self.release_inputs.is_none())
    }
}

fn stage_route_admitted(
    target: &str,
    unsigned: bool,
    proof_container: Option<&str>,
    proof_runner: Option<&str>,
    scope: StageScope,
    route: StageRoute,
) -> bool {
    if proof_runner.is_some()
        && (target != DARWIN_TARGET || route != StageRoute::Default || scope != StageScope::Target)
    {
        return false;
    }
    if scope != StageScope::Target {
        return route == StageRoute::Default;
    }
    match route {
        StageRoute::Default => true,
        StageRoute::Sealed => target == LINUX_TARGET && proof_container.is_none(),
        StageRoute::Hosted if target == LINUX_TARGET => proof_container.is_none(),
        StageRoute::Hosted if target == DARWIN_TARGET => !unsigned && proof_container.is_some(),
        StageRoute::Hosted if target == WINDOWS_TARGET => unsigned && proof_container.is_some(),
        StageRoute::Hosted => false,
    }
}

/// Where a release target build executes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StageRoute {
    /// Natively on its host, or in the sealed image for Linux on another host.
    Default,
    /// In the sealed image even on a Linux host.
    Sealed,
    /// On one fresh provider-hosted machine.
    Hosted,
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
const UNSIGNED_TARGETS: [&str; 2] = [LINUX_TARGET, WINDOWS_TARGET];

/// The signed Darwin release target with a hosted route.
const DARWIN_TARGET: &str = "aarch64-apple-darwin";

/// The Linux release target with sealed and hosted routes.
const LINUX_TARGET: &str = "x86_64-unknown-linux-musl";

/// The unsigned Windows release target with a hosted route.
const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";

pub(super) fn require_unsigned(target: &str, unsigned: bool) -> CommandResult {
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
pub(super) fn darwin_signing(
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

#[cfg(test)]
mod tests {
    use super::{DARWIN_TARGET, LINUX_TARGET, StageRoute, WINDOWS_TARGET, parse_stage_target};
    use std::ffi::OsString;

    #[test]
    fn hosted_targets_admit_only_their_supported_proof_image_inputs() {
        let image = "sha256:0123456789abcdef";
        let runner = "/tmp/sealed-runner";
        let darwin = [DARWIN_TARGET, "--proof-container", image, "--hosted"].map(OsString::from);
        let darwin = parse_stage_target(&darwin).expect("hosted Darwin proof image");
        assert_eq!(darwin.target, DARWIN_TARGET);
        assert_eq!(darwin.proof_container, Some(image));
        assert_eq!(darwin.proof_runner, None);
        assert_eq!(darwin.route, StageRoute::Hosted);
        assert!(!darwin.unsigned);

        let native_darwin = [
            DARWIN_TARGET,
            "--unsigned",
            "--proof-container",
            image,
            "--proof-runner",
            runner,
        ]
        .map(OsString::from);
        let native_darwin =
            parse_stage_target(&native_darwin).expect("native Darwin sealed runner");
        assert_eq!(native_darwin.proof_runner, Some(runner));
        let non_darwin_runner =
            [LINUX_TARGET, "--unsigned", "--proof-runner", runner].map(OsString::from);
        assert!(parse_stage_target(&non_darwin_runner).is_none());
        let windows = [
            WINDOWS_TARGET,
            "--unsigned",
            "--proof-container",
            image,
            "--hosted",
        ]
        .map(OsString::from);
        let windows = parse_stage_target(&windows).expect("hosted Windows proof image");
        assert_eq!(windows.target, WINDOWS_TARGET);
        assert_eq!(windows.proof_container, Some(image));
        assert_eq!(windows.route, StageRoute::Hosted);
        assert!(windows.unsigned);

        let linux = [
            LINUX_TARGET,
            "--unsigned",
            "--proof-container",
            image,
            "--hosted",
        ]
        .map(OsString::from);
        assert!(parse_stage_target(&linux).is_none());

        let unpinned_darwin = [DARWIN_TARGET, "--hosted"].map(OsString::from);
        assert!(parse_stage_target(&unpinned_darwin).is_none());
        let unpinned_windows = [WINDOWS_TARGET, "--unsigned", "--hosted"].map(OsString::from);
        assert!(parse_stage_target(&unpinned_windows).is_none());
    }

    #[test]
    fn only_provider_hosted_stages_attach_to_a_run() {
        let parse = |arguments: &[&str]| {
            let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
            parse_stage_target(&arguments).map(|request| (request.route, request.attach))
        };
        assert_eq!(
            parse(&[LINUX_TARGET, "--unsigned", "--hosted", "--attach"]),
            Some((StageRoute::Hosted, Some(xtask::Attach::Sole)))
        );
        assert_eq!(
            parse(&[LINUX_TARGET, "--unsigned", "--hosted"]),
            Some((StageRoute::Hosted, None))
        );
        assert_eq!(
            parse(&[WINDOWS_TARGET, "--unsigned", "--attach", "19342"]),
            Some((StageRoute::Default, Some(xtask::Attach::Run(19342))))
        );
        assert_eq!(
            parse(&[WINDOWS_TARGET, "--unsigned", "--app-only", "--attach"]),
            Some((StageRoute::Default, Some(xtask::Attach::Sole)))
        );
        for unhosted in [
            &[LINUX_TARGET, "--unsigned", "--attach"][..],
            &[LINUX_TARGET, "--unsigned", "--sealed", "--attach", "7"],
            &[DARWIN_TARGET, "--attach"],
            &[LINUX_TARGET, "--unsigned", "--hosted", "--attach", "0"],
            &[LINUX_TARGET, "--unsigned", "--hosted", "--attach", "latest"],
            &[LINUX_TARGET, "--unsigned", "--attach", "--hosted"],
        ] {
            assert_eq!(parse(unhosted), None, "{unhosted:?}");
        }
    }
}
