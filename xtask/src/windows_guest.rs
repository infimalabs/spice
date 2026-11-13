//! Persistent Windows 11 ARM developer guest under QEMU on an Apple-silicon host.
//!
//! The guest runs the pinned x64 cross-build under Windows' x64 emulation for
//! developer smokes only. It publishes no receipt, satisfies no release,
//! sealing, or package-replay claim, and no gate or release path executes or
//! trusts it.

mod declaration;
mod machine;
mod media;
mod sync;

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use declaration::Declaration;
use machine::{HostKey, Paths};

const USAGE: &str = concat!(
    "usage: spice dev xtask -- windows-guest ",
    "<provision [--replace]|start|stop|status|reset|",
    "sync [--artifact PATH]... [--source PATH]...|",
    "run [--timeout SECONDS] [--output-bytes BYTES] -- PROGRAM [ARG...]>"
);

/// Why a guest command could not complete.
#[derive(Debug)]
pub enum WindowsGuestError {
    Usage(String),
    Failed(String),
}

impl WindowsGuestError {
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::Failed(_) => 1,
        }
    }
}

impl fmt::Display for WindowsGuestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(formatter, "{message}\n{USAGE}"),
            Self::Failed(message) => write!(formatter, "windows-guest: {message}"),
        }
    }
}

impl Error for WindowsGuestError {}

impl From<String> for WindowsGuestError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

/// Run one guest command and return the exit status the caller reports: the
/// guest program's exact status for `run`, zero otherwise.
///
/// # Errors
///
/// Returns usage errors for malformed arguments and failures for any refused
/// pin, host tool, guest, or transport step.
pub fn windows_guest(
    arguments: Vec<OsString>,
    lane: Option<wire::CanonicalUuid>,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<i32, WindowsGuestError> {
    let action = Action::parse(arguments)?;
    let context = Context::discover()?;
    let _resource = action
        .requires_resource()
        .then(|| context.acquire_resource(lane.as_ref()))
        .transpose()?;
    match action {
        Action::Provision { replace } => provision(&context, replace, output)?,
        Action::Start => start(&context, output)?,
        Action::Stop => machine::stop(&context.paths, &context.declaration.machine, output)?,
        Action::Status => status(&context, output)?,
        Action::Reset => {
            require_provisioned(&context)?;
            machine::reset(&context.paths, &context.declaration)?;
            writeln!(
                output,
                "guest disk and firmware variables returned to the clean base"
            )
            .map_err(output_error)?;
        }
        Action::Sync(selection) => {
            require_provisioned(&context)?;
            sync::sync(
                &context.paths,
                &context.declaration.machine,
                &context.root,
                &selection,
                output,
            )?;
        }
        Action::Run(run) => {
            require_provisioned(&context)?;
            return Ok(sync::run(
                &context.paths,
                &context.declaration.machine,
                run,
                output,
                error_output,
            )?);
        }
    }
    Ok(0)
}

enum Action {
    Provision { replace: bool },
    Start,
    Stop,
    Status,
    Reset,
    Sync(sync::Selection),
    Run(sync::RunArguments),
}

impl Action {
    fn parse(arguments: Vec<OsString>) -> Result<Self, WindowsGuestError> {
        let mut arguments = arguments.into_iter();
        let command = arguments
            .next()
            .ok_or_else(|| usage("missing guest command"))?
            .into_string()
            .map_err(|_| usage("guest command is not Unicode"))?;
        let rest = arguments.collect::<Vec<_>>();
        let bare = |action: Self| {
            if rest.is_empty() {
                Ok(action)
            } else {
                Err(usage(&format!("{command} takes no arguments")))
            }
        };
        match command.as_str() {
            "provision" => match rest.as_slice() {
                [] => Ok(Self::Provision { replace: false }),
                [flag] if flag == "--replace" => Ok(Self::Provision { replace: true }),
                _ => Err(usage("provision accepts only --replace")),
            },
            "start" => bare(Self::Start),
            "stop" => bare(Self::Stop),
            "status" => bare(Self::Status),
            "reset" => bare(Self::Reset),
            "sync" => Ok(Self::Sync(sync::Selection::parse(rest)?)),
            "run" => Ok(Self::Run(sync::RunArguments::parse(rest)?)),
            other => Err(usage(&format!("unknown guest command {other:?}"))),
        }
    }

    fn requires_resource(&self) -> bool {
        matches!(
            self,
            Self::Start | Self::Stop | Self::Reset | Self::Sync(_) | Self::Run(_)
        )
    }
}

struct Context {
    root: PathBuf,
    paths: Paths,
    declaration: Declaration,
    identity: String,
    resources: sill::LockTopology,
}

impl Context {
    fn discover() -> Result<Self, WindowsGuestError> {
        let working = std::env::current_dir()
            .map_err(|error| format!("current directory is unavailable: {error}"))?;
        let repository = spice::repository::Repository::discover(&working)
            .map_err(|error| format!("repository is unavailable: {error}"))?;
        let loaded = spice::config::capture_repository(&repository, &[])
            .map_err(|error| format!("repository configuration is unavailable: {error}"))?;
        let root = repository.worktree_root().to_path_buf();
        let paths = Paths::new(
            repository
                .common_dir()
                .join(spice::repository::STATE_DIRECTORY)
                .join("machine")
                .join("windows"),
        );
        let (declaration, identity) = Declaration::load(&root)?;
        let resources = sill::LockTopology::new(&root, loaded.resolved().root().lock());
        Ok(Self {
            root,
            paths,
            declaration,
            identity,
            resources,
        })
    }

    fn acquire_resource(
        &self,
        lane: Option<&wire::CanonicalUuid>,
    ) -> Result<GuestResource, WindowsGuestError> {
        acquire_guest_resource(&self.resources, lane)
    }
}

fn acquire_guest_resource(
    resources: &sill::LockTopology,
    lane: Option<&wire::CanonicalUuid>,
) -> Result<GuestResource, WindowsGuestError> {
    match resources.try_acquire("windows-guest", None) {
        Ok(sill::LockAttempt::Acquired(shard)) => Ok(GuestResource::Held { _shard: shard }),
        Ok(sill::LockAttempt::Contended { .. }) if claim_owned_by(resources, lane)? => {
            Ok(GuestResource::Borrowed)
        }
        Ok(sill::LockAttempt::Contended { .. }) => Err(
            "the windows-guest resource is held by another claim or process"
                .to_owned()
                .into(),
        ),
        Err(sill::LockSurfaceError::UnknownName(_)) => Ok(GuestResource::Unguarded),
        Err(error) => Err(format!("cannot acquire windows-guest resource: {error}").into()),
    }
}

fn claim_owned_by(
    resources: &sill::LockTopology,
    lane: Option<&wire::CanonicalUuid>,
) -> Result<bool, WindowsGuestError> {
    let Some(lane) = lane else {
        return Ok(false);
    };
    let report = resources
        .report()
        .map_err(|error| format!("cannot inspect windows-guest resource: {error}"))?;
    Ok(report
        .pools
        .iter()
        .find(|pool| pool.name == "windows-guest")
        .into_iter()
        .flat_map(|pool| &pool.entries)
        .filter_map(|entry| entry.owner.as_ref())
        .filter_map(|owner| owner.context.as_deref())
        .any(|context| claim_context_owned_by(context, lane)))
}

fn claim_context_owned_by(context: &str, lane: &wire::CanonicalUuid) -> bool {
    serde_json::from_str::<wire::ResourceClaimOwner>(context).is_ok_and(|owner| owner.lane == *lane)
}

enum GuestResource {
    Held { _shard: sill::HeldShard },
    Borrowed,
    Unguarded,
}

fn provision(
    context: &Context,
    replace: bool,
    output: &mut impl Write,
) -> Result<(), WindowsGuestError> {
    let paths = &context.paths;
    let declaration = &context.declaration;
    if recorded_identity(paths)?.is_some() && !replace {
        return Err(format!(
            "a guest is already provisioned in {}; `reset` returns it to its clean base and `provision --replace` rebuilds it",
            paths.root.display()
        )
        .into());
    }
    if machine::running(declaration.machine.qmp_port) {
        return Err("stop the guest before provisioning it".to_owned().into());
    }
    // Also clears a base left by an attempt that never verified over SSH.
    paths.remove_guest()?;
    declaration.tools.verify()?;
    fs::create_dir_all(&paths.root)
        .map_err(|error| format!("cannot create {}: {error}", paths.root.display()))?;
    let image = media::fetch(
        &paths.cache(),
        "Windows image",
        &declaration.image.download(),
        output,
    )?;
    let drivers = media::fetch(
        &paths.cache(),
        "virtio drivers",
        &declaration.drivers.download(),
        output,
    )?;
    let openssh = media::fetch(
        &paths.cache(),
        "OpenSSH server",
        &declaration.openssh,
        output,
    )?;
    let runtime = media::fetch(
        &paths.cache(),
        "x64 VC++ runtime",
        &declaration.runtime,
        output,
    )?;
    media::ensure_key(paths)?;
    media::build(
        declaration,
        paths,
        &context.root,
        &media::Inputs {
            image: &image,
            drivers: &drivers,
            openssh: &openssh,
            runtime: &runtime,
        },
        output,
    )?;
    machine::install(paths, declaration, output)?;
    media::remove_build(paths)?;
    machine::seal(paths)?;
    machine::reset(paths, declaration)?;
    writeln!(output, "verifying the installed guest over SSH").map_err(output_error)?;
    machine::boot(paths, declaration, HostKey::LearnFirst)?;
    machine::stop(paths, &declaration.machine, output)?;
    write_record(paths, &context.identity)?;
    writeln!(
        output,
        "provisioned the clean Windows guest in {}",
        paths.root.display()
    )
    .map_err(output_error)?;
    Ok(())
}

fn start(context: &Context, output: &mut impl Write) -> Result<(), WindowsGuestError> {
    require_provisioned(context)?;
    let machine = &context.declaration.machine;
    if machine::running(machine.qmp_port) {
        writeln!(output, "guest is already running").map_err(output_error)?;
        return Ok(());
    }
    context.declaration.tools.verify()?;
    if !context.paths.overlay().exists() {
        machine::reset(&context.paths, &context.declaration)?;
    }
    machine::boot(&context.paths, &context.declaration, HostKey::Pinned)?;
    writeln!(
        output,
        "guest is running; SSH 127.0.0.1:{}, QMP 127.0.0.1:{}",
        machine.ssh_port, machine.qmp_port
    )
    .map_err(output_error)?;
    Ok(())
}

fn status(context: &Context, output: &mut impl Write) -> Result<(), WindowsGuestError> {
    let provisioned = match recorded_identity(&context.paths)? {
        None => "no",
        Some(identity) if identity == context.identity => "yes",
        Some(_) => "stale: provisioned from different pins; run provision --replace",
    };
    let running = machine::running(context.declaration.machine.qmp_port);
    writeln!(
        output,
        "state: {}\nprovisioned: {provisioned}\nrunning: {}",
        context.paths.root.display(),
        if running { "yes" } else { "no" }
    )
    .map_err(output_error)?;
    Ok(())
}

fn require_provisioned(context: &Context) -> Result<(), WindowsGuestError> {
    match recorded_identity(&context.paths)? {
        Some(identity) if identity == context.identity => Ok(()),
        Some(_) => Err(
            "the guest was provisioned from different pins or templates; run provision --replace"
                .to_owned()
                .into(),
        ),
        None => Err("no guest is provisioned; run provision first"
            .to_owned()
            .into()),
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    identity: String,
}

fn write_record(paths: &Paths, identity: &str) -> Result<(), WindowsGuestError> {
    let record = serde_json::to_vec_pretty(&Record {
        identity: identity.to_owned(),
    })
    .map_err(|error| format!("cannot encode guest record: {error}"))?;
    fs::write(paths.record(), record)
        .map_err(|error| format!("cannot write {}: {error}", paths.record().display()))?;
    Ok(())
}

fn recorded_identity(paths: &Paths) -> Result<Option<String>, WindowsGuestError> {
    if !paths.base().exists() {
        return Ok(None);
    }
    let bytes = match fs::read(paths.record()) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!("cannot read {}: {error}", paths.record().display()).into());
        }
    };
    let record: Record = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "guest record {} is malformed: {error}",
            paths.record().display()
        )
    })?;
    Ok(Some(record.identity))
}

fn usage(message: &str) -> WindowsGuestError {
    WindowsGuestError::Usage(message.to_owned())
}

fn output_error(error: std::io::Error) -> WindowsGuestError {
    WindowsGuestError::Failed(format!("cannot write output: {error}"))
}

/// Run a host tool to completion, returning its output or a failure naming it.
fn run_tool(
    command: &mut std::process::Command,
    what: &str,
) -> Result<std::process::Output, String> {
    let output = command
        .output()
        .map_err(|error| format!("cannot start {what}: {error}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "{what} failed with {}: {}",
            output.status,
            stderr.trim()
        ))
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::Digest as _;
    use std::io::Read as _;
    let mut file =
        fs::File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_pool_leaves_guest_commands_unguarded() {
        let root = std::env::current_dir().expect("fixture worktree root");
        let config = sill::LockConfig::new(75, 76, 77, std::iter::empty())
            .expect("empty lock configuration");
        let topology = sill::LockTopology::new(&root, &config);

        assert!(matches!(
            acquire_guest_resource(&topology, None),
            Ok(GuestResource::Unguarded)
        ));
    }

    #[test]
    fn only_the_lane_named_by_claim_owner_context_may_borrow() {
        let lane: wire::CanonicalUuid = "00000000-0000-0000-0000-000000000001"
            .parse()
            .expect("fixture lane");
        let other: wire::CanonicalUuid = "00000000-0000-0000-0000-000000000002"
            .parse()
            .expect("fixture lane");
        let context = serde_json::to_string(&wire::ResourceClaimOwner {
            lane,
            task: wire::TaskStamp::for_micros(1).expect("fixture task stamp"),
        })
        .expect("owner context encodes");
        assert!(claim_context_owned_by(&context, &lane));
        assert!(!claim_context_owned_by(&context, &other));
        assert!(!claim_context_owned_by("not-json", &lane));
    }
}
