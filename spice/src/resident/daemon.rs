//! The detached `spice up` and `spice app` launcher and its one-shot child handoff.

use super::*;

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use spice::cargo_isolation::ResidentLaunchEnvironment;

pub(crate) mod observer;

use observer::{DirectObserver, Signal, wait_for_change, wait_for_exit};

#[cfg(unix)]
use std::net::Shutdown;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

/// The launcher's and the child's end of the one-shot launch channel.
#[cfg(unix)]
type Channel = UnixStream;
#[cfg(windows)]
type Channel = File;

#[cfg(unix)]
const HANDOFF_FD: i32 = 9;
/// Names the launch channel the launcher granted its detached child.
#[cfg(windows)]
const HANDOFF_VARIABLE: &str = wire::SpiceVariable::DaemonHandoff.as_str();
const HANDOFF_MAGIC: &[u8] = b"SPICE-DAEMON\0";
const MAX_HANDOFF_BYTES: usize = 16 * 1024;
const MAX_BOOTSTRAP_BYTES: u64 = 64 * 1024;
const ARRIVAL_HORIZON: Duration = Duration::from_secs(60);
/// When an unfinished wait logs a warning rather than refusing outright.
const ARRIVAL_WARN_HORIZON: Duration = Duration::from_secs(30);
const PROBE_BOUND: Duration = Duration::from_secs(20);
#[cfg(unix)]
const TERMINATION_HORIZON: Duration = Duration::from_secs(5);
/// Size past which the previous residents' log is set aside at start-up.
///
/// The log is appended by every resident this repository has run, and with
/// nothing bounding it one repository carried 47 MiB of stale diagnostics that
/// every launch-failure report then read end to end.
const LOG_ROTATE_BYTES: u64 = 8 * 1024 * 1024;
/// Suffix of the one set-aside log; a second rotation replaces it.
const LOG_ROTATED_SUFFIX: &str = ".1";

/// The only secret crossing from the launcher into its child.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HandoffRequest {
    token: Option<String>,
}

/// The detached child's half of the launch channel.
#[derive(Clone)]
pub(crate) struct ChildStatus {
    stream: Arc<Mutex<Option<Channel>>>,
}

/// Authentication and status state captured by the controlled foreground child.
pub(crate) struct ChildLaunch {
    pub(crate) token: Option<String>,
    pub(crate) status: ChildStatus,
}

impl ChildLaunch {
    /// Capture a launcher-created handoff, if this is a detached child.
    pub(crate) fn capture(ambient: &Ambient) -> Result<Option<Self>, Outcome<()>> {
        let Some(mut stream) = inherited_channel(ambient)? else {
            return Ok(None);
        };
        let request = read_framed(&mut stream)?;
        let request: HandoffRequest = wire::decode(&request).map_err(|error| {
            environment_fault("resident::daemon-handoff-invalid", &error.to_string())
        })?;
        #[cfg(unix)]
        let _ = stream.shutdown(Shutdown::Read);
        Ok(Some(Self {
            token: request.token,
            status: ChildStatus {
                stream: Arc::new(Mutex::new(Some(stream))),
            },
        }))
    }
}

/// Take the launch channel the launcher passed on descriptor
/// [`HANDOFF_FD`], if this is a detached child.
#[cfg(unix)]
fn inherited_channel(_ambient: &Ambient) -> Result<Option<Channel>, Outcome<()>> {
    if !handoff_present(HANDOFF_FD)? {
        return Ok(None);
    }
    // SAFETY: the launcher exclusively transfers ownership of this inherited
    // descriptor to the controlled child.
    let stream = unsafe { UnixStream::from_raw_fd(HANDOFF_FD) };
    stream
        .set_read_timeout(Some(PROBE_BOUND))
        .map_err(|error| daemon_fault("resident::daemon-handoff-unavailable", error))?;
    Ok(Some(stream))
}

/// Join the launch channel the launcher named in [`HANDOFF_VARIABLE`], if
/// this is a detached child.
///
/// The launcher writes the request as soon as it accepts this process, and
/// its exit breaks the pipe, so the request read needs no deadline of its own.
#[cfg(windows)]
fn inherited_channel(ambient: &Ambient) -> Result<Option<Channel>, Outcome<()>> {
    let Some(name) = ambient
        .get(HANDOFF_VARIABLE)
        .map(std::ffi::OsStr::to_os_string)
    else {
        return Ok(None);
    };
    let name = name.into_string().map_err(|_| {
        environment_fault(
            "resident::daemon-handoff-invalid",
            "the launch channel name is not Unicode",
        )
    })?;
    door::handoff::join(&name)
        .map(Some)
        .map_err(|error| daemon_fault("resident::daemon-handoff-unavailable", error))
}

impl ChildStatus {
    /// Close the last launcher channel as soon as discovery is published.
    pub(crate) fn published(self) {
        drop(self.stream.lock().expect("daemon status mutex").take());
    }

    /// Tell the launcher the window is up over the admitted endpoint, then
    /// close the channel; the launcher answers with this receipt.
    pub(crate) fn opened(self, opened: &wire::AppOpen) {
        let outcome = Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body: opened,
        });
        self.failed(&outcome);
    }

    /// Return an exact typed startup outcome when publication never happened.
    pub(crate) fn failed<B: Serialize>(self, outcome: &Outcome<B>) {
        if let Some(mut stream) = self.stream.lock().expect("daemon status mutex").take() {
            if let Ok(bytes) = wire::encode(outcome) {
                let _ = write_framed(&mut stream, &bytes);
            }
            #[cfg(unix)]
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

/// Start this exact executable as a detached resident and prove it is ready.
/// `handoff_token` is the bearer token a launcher handed this process
/// privately, forwarded the same way; `open` asks that resident to dial the
/// system browser once it announces.
pub(crate) fn launch(
    leaf: &clap::ArgMatches,
    handoff_token: Option<String>,
    open: bool,
    ambient: &Ambient,
) -> Outcome<wire::AppOpen> {
    launch_held(leaf, handoff_token, open, ambient).0
}

/// [`launch`], also handing back the ready child so a lifecycle owner can
/// wait on its exit. The child is detached either way; dropping the handle
/// leaves it running.
pub(crate) fn launch_held(
    leaf: &clap::ArgMatches,
    handoff_token: Option<String>,
    open: bool,
    ambient: &Ambient,
) -> (Outcome<wire::AppOpen>, Option<Child>) {
    let spawned = PreparedLaunch::admit(leaf, handoff_token)
        .and_then(|launch| launch.spawn(leaf, ChildRole::Resident { open }, ambient));
    match spawned {
        Ok(launch) => launch.wait(),
        Err(outcome) => (map_failure(outcome), None),
    }
}

/// Start this exact executable as a detached `spice app --foreground` and
/// answer once it reports the window up over the endpoint it admitted. The
/// child owns the window and any resident it starts; its terminal receipt
/// lands in the resident log, never on this terminal.
pub(crate) fn launch_window(leaf: &clap::ArgMatches, ambient: &Ambient) -> Outcome<wire::AppOpen> {
    let spawned = PreparedLaunch::admit(leaf, None)
        .and_then(|launch| launch.spawn(leaf, ChildRole::Window, ambient));
    match spawned {
        Ok(launch) => launch.wait_opened(),
        Err(outcome) => map_failure(outcome),
    }
}

/// What the detached child runs as.
#[derive(Clone, Copy)]
enum ChildRole {
    /// `up --foreground`, dialing the browser when `open`.
    Resident { open: bool },
    /// `app --foreground`.
    Window,
}

impl ChildRole {
    const fn verb(self) -> &'static str {
        match self {
            Self::Resident { .. } => "up",
            Self::Window => "app",
        }
    }
}

struct PreparedLaunch {
    listen: SocketAddr,
    token: Option<String>,
    current: PathBuf,
    state: spice::repository::StatePaths,
    baseline: Option<Vec<u8>>,
    observer: DirectObserver,
    executable: PathBuf,
    log: File,
    log_offset: u64,
    handoff: Handoff,
}

impl PreparedLaunch {
    fn admit(leaf: &clap::ArgMatches, handoff_token: Option<String>) -> Result<Self, Outcome<()>> {
        let listen = requested_listen(leaf)?;
        let token = requested_access(leaf, listen, handoff_token)?.token;
        let current = std::env::current_dir()
            .map_err(|error| daemon_fault("resident::cwd-unavailable", error))?;
        let repository = Repository::discover(&current)
            .map_err(|error| daemon_fault(spice::repository::ResolveError::REASON, error))?;
        let state = repository
            .admit_resident_state()
            .map_err(|error| crate::state_failure(&error).outcome())?;
        fs::create_dir_all(state.live())
            .map_err(|error| daemon_fault("resident::runtime-unavailable", error))?;
        let baseline = read_optional(state.serve())
            .map_err(|error| daemon_fault("resident::daemon-discovery-unavailable", error))?;
        let observer = DirectObserver::arm(state.live())
            .map_err(|error| daemon_fault("resident::daemon-observer-unavailable", error))?;
        let executable = std::env::current_exe()
            .map_err(|error| daemon_fault("resident::executable-unavailable", error))?;
        let log = rotate_log(state.resident_log())
            .and_then(|()| open_log(state.resident_log()))
            .map_err(|error| daemon_fault("resident::log-unavailable", error))?;
        let log_offset = log.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let handoff = prepare_handoff(token.clone())?;
        Ok(Self {
            listen,
            token,
            current,
            state,
            baseline,
            observer,
            executable,
            log,
            log_offset,
            handoff,
        })
    }

    fn spawn(
        self,
        leaf: &clap::ArgMatches,
        role: ChildRole,
        ambient: &Ambient,
    ) -> Result<SpawnedLaunch, Outcome<()>> {
        let Self {
            listen,
            token,
            current,
            state,
            baseline,
            observer,
            executable,
            log,
            log_offset,
            handoff,
        } = self;
        let stdout = log
            .try_clone()
            .map_err(|error| daemon_fault("resident::log-unavailable", error))?;
        let mut command = ProcessCommand::new(&executable);
        ResidentLaunchEnvironment::derive(ambient.variables().iter().cloned()).apply(&mut command);
        command
            .args([
                "--json",
                role.verb(),
                "--foreground",
                "--host",
                &listen.ip().to_string(),
                "--port",
                &listen.port().to_string(),
            ])
            .current_dir(&current)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(log);
        if leaf.get_flag("allow-insecure-bind") {
            command.arg("--allow-insecure-bind");
        }
        if let ChildRole::Resident { open: true } = role {
            command.arg("--open");
        }
        handoff.grant(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| daemon_fault("resident::daemon-launch-failed", error))?;
        let parent_handoff = match handoff.connect(&child) {
            Ok(channel) => channel,
            Err(outcome) => {
                terminate_group(&mut child);
                return Err(outcome);
            }
        };
        Ok(SpawnedLaunch {
            token,
            state,
            baseline,
            observer,
            log_offset,
            parent_handoff,
            child,
        })
    }
}

struct SpawnedLaunch {
    token: Option<String>,
    state: spice::repository::StatePaths,
    baseline: Option<Vec<u8>>,
    observer: DirectObserver,
    log_offset: u64,
    parent_handoff: Channel,
    child: Child,
}

impl SpawnedLaunch {
    fn wait(mut self) -> (Outcome<wire::AppOpen>, Option<Child>) {
        let ready = wait_for_ready(
            &mut self.child,
            self.observer,
            self.state.live(),
            &Arrival {
                serve: self.state.serve(),
                baseline: self.baseline.as_deref(),
                token: self.token.as_deref(),
            },
            self.state.resident_log(),
            &mut self.parent_handoff,
        );
        match ready {
            Ok(discovery) => (
                Outcome::Reply(Reply {
                    position: ReplyPosition::Direct {},
                    body: wire::AppOpen {
                        endpoint: discovery.endpoint,
                        epoch: discovery.epoch,
                    },
                }),
                Some(self.child),
            ),
            Err(outcome) => {
                terminate_group(&mut self.child);
                let failure = outcome.map_or_else(
                    || {
                        map_failure(environment_fault(
                            "resident::daemon-child-exited",
                            &child_diagnostic(self.state.resident_log(), self.log_offset),
                        ))
                    },
                    map_failure,
                );
                (failure, None)
            }
        }
    }

    /// Answer with the window child's own report: the receipt it sends once
    /// the window is up, or the failure it ends with. A child gone without a
    /// word is diagnosed from the resident log.
    fn wait_opened(mut self) -> Outcome<wire::AppOpen> {
        match self.window_report() {
            Ok(outcome) => outcome,
            Err(outcome) => {
                terminate_group(&mut self.child);
                map_failure(outcome)
            }
        }
    }

    fn window_report(&mut self) -> Result<Outcome<wire::AppOpen>, Outcome<()>> {
        #[cfg(unix)]
        self.parent_handoff
            .set_read_timeout(Some(ARRIVAL_HORIZON))
            .map_err(|error| daemon_fault("resident::daemon-handoff-unavailable", error))?;
        let bytes = read_status(&mut self.parent_handoff).map_err(|error| {
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) {
                environment_fault(
                    "resident::daemon-readiness-elapsed",
                    &format!("the window did not open within {ARRIVAL_HORIZON:?}"),
                )
            } else {
                daemon_fault("resident::daemon-handoff-unavailable", error)
            }
        })?;
        if bytes.is_empty() {
            return Err(environment_fault(
                "resident::daemon-child-exited",
                &child_diagnostic(self.state.resident_log(), self.log_offset),
            ));
        }
        wire::decode::<Outcome<wire::AppOpen>>(&bytes)
            .map_err(|error| daemon_fault("resident::daemon-outcome-invalid", error))
    }
}

fn daemon_fault(reason: &str, error: impl std::fmt::Display) -> Outcome<()> {
    environment_fault(reason, &error.to_string())
}

/// The launcher's half of the launch channel before the child exists.
#[cfg(unix)]
struct Handoff {
    parent: UnixStream,
    child: UnixStream,
}

/// The launcher's half of the launch channel before the child exists.
#[cfg(windows)]
struct Handoff {
    offer: door::handoff::Offer,
    request: Vec<u8>,
}

#[cfg(unix)]
fn prepare_handoff(token: Option<String>) -> Result<Handoff, Outcome<()>> {
    let (mut parent, child) = UnixStream::pair()
        .map_err(|error| daemon_fault("resident::daemon-handoff-unavailable", error))?;
    let request = HandoffRequest { token };
    let encoded = wire::encode(&request).expect("daemon handoff request encodes");
    write_framed(&mut parent, &encoded)
        .map_err(|error| daemon_fault("resident::daemon-handoff-unavailable", error))?;
    let _ = parent.shutdown(Shutdown::Write);
    Ok(Handoff { parent, child })
}

#[cfg(windows)]
fn prepare_handoff(token: Option<String>) -> Result<Handoff, Outcome<()>> {
    let offer = door::handoff::Offer::open()
        .map_err(|error| daemon_fault("resident::daemon-handoff-unavailable", error))?;
    let request = wire::encode(&HandoffRequest { token }).expect("daemon handoff request encodes");
    Ok(Handoff { offer, request })
}

#[cfg(unix)]
impl Handoff {
    /// Detach the child into a session and process group of its own, without
    /// a controlling terminal, holding the channel on [`HANDOFF_FD`].
    fn grant(&self, command: &mut ProcessCommand) {
        super::supervisor::detach(command);
        inherit_handoff(command, self.child.as_raw_fd());
    }

    /// The launcher's channel once the child holds its own end.
    fn connect(self, _child: &Child) -> Result<Channel, Outcome<()>> {
        drop(self.child);
        Ok(self.parent)
    }
}

#[cfg(windows)]
impl Handoff {
    /// Detach the child into its own process group and windowless console,
    /// naming the channel through [`HANDOFF_VARIABLE`].
    fn grant(&self, command: &mut ProcessCommand) {
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        self.offer.grant(command, HANDOFF_VARIABLE);
    }

    /// Accept the channel only from `child` within [`PROBE_BOUND`], then hand
    /// it the request.
    fn connect(self, child: &Child) -> Result<Channel, Outcome<()>> {
        let mut channel = self
            .offer
            .accept(child.id(), PROBE_BOUND)
            .map_err(|error| {
                let reason = if error.kind() == io::ErrorKind::PermissionDenied {
                    "resident::daemon-handoff-refused"
                } else {
                    "resident::daemon-handoff-unavailable"
                };
                daemon_fault(reason, error)
            })?;
        write_framed(&mut channel, &self.request)
            .map_err(|error| daemon_fault("resident::daemon-handoff-unavailable", error))?;
        Ok(channel)
    }
}

#[cfg(unix)]
fn inherit_handoff(command: &mut ProcessCommand, source: RawFd) {
    // SAFETY: dup2 and fcntl are async-signal-safe, and the closure touches
    // only copied descriptor integers before exec.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(source, HANDOFF_FD) == -1 {
                return Err(io::Error::last_os_error());
            }
            let flags = libc::fcntl(HANDOFF_FD, libc::F_GETFD);
            if flags == -1
                || libc::fcntl(HANDOFF_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Note an unfinished readiness wait in the resident log, best-effort.
///
/// A missed write here must not fail the wait itself: this is an operator
/// aid for a still-running launch, not a condition the caller can act on.
fn warn_readiness_pending(log_path: &Path, warn_horizon: Duration, horizon: Duration) {
    let Ok(mut log) = open_log(log_path) else {
        return;
    };
    let _ = writeln!(
        log,
        "resident readiness still pending after {warn_horizon:?}; continuing to wait up to {horizon:?}"
    );
}

/// Create and admit the one canonical directory that owns `path`.
///
/// The runtime root already exists before a daemon or keeper opens its log.
/// Creating only its direct child avoids following an unsafe intermediate
/// link, then `symlink_metadata` distinguishes a real directory from every
/// colliding shape.
fn ensure_log_parent(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "log path has no parent directory",
        )
    })?;
    match fs::create_dir(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("log parent is not a real directory: {}", parent.display()),
        ));
    }
    Ok(())
}

/// Set an oversized log aside under [`LOG_ROTATED_SUFFIX`] before a resident
/// starts appending to a fresh one; a missing or small log is left alone.
pub(crate) fn rotate_log(path: &Path) -> io::Result<()> {
    ensure_log_parent(path)?;
    let oversized = fs::metadata(path).is_ok_and(|metadata| metadata.len() > LOG_ROTATE_BYTES);
    if !oversized {
        return Ok(());
    }
    let mut rotated = path.as_os_str().to_owned();
    rotated.push(LOG_ROTATED_SUFFIX);
    fs::rename(path, rotated)
}

pub(crate) fn open_log(path: &Path) -> io::Result<File> {
    ensure_log_parent(path)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true).read(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn wait_for_ready(
    child: &mut Child,
    observer: DirectObserver,
    watched: &Path,
    arrival: &Arrival<'_>,
    log_path: &Path,
    status: &mut Channel,
) -> Result<serve::Discovery, Option<Outcome<()>>> {
    let horizon = ARRIVAL_HORIZON;
    let started = Instant::now();
    let deadline = started + horizon;
    let warn_at = started + horizon.min(ARRIVAL_WARN_HORIZON);
    let mut warned = false;
    let (sender, receiver) = mpsc::channel();
    wait_for_change(observer, sender.clone(), Signal::woke);
    wait_for_exit(child.id(), sender.clone());

    if let Some(discovery) = arrival.candidate()? {
        return Ok(discovery);
    }
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Some(environment_fault(
                "resident::daemon-readiness-elapsed",
                &format!("resident did not become ready within {horizon:?}"),
            )));
        }
        if !warned && Instant::now() >= warn_at {
            warned = true;
            warn_readiness_pending(log_path, ARRIVAL_WARN_HORIZON, horizon);
        }
        let wait_bound = remaining.min(warn_at.saturating_duration_since(Instant::now()));
        let wait_bound = if wait_bound.is_zero() {
            remaining
        } else {
            wait_bound
        };
        match receiver.recv_timeout(wait_bound) {
            Ok(Signal::Changed) => {
                let next = DirectObserver::arm(watched).map_err(|error| {
                    Some(environment_fault(
                        "resident::daemon-observer-unavailable",
                        &error.to_string(),
                    ))
                })?;
                if let Some(discovery) = arrival.candidate()? {
                    return Ok(discovery);
                }
                wait_for_change(next, sender.clone(), Signal::woke);
            }
            Ok(Signal::Exited) => {
                if let Some(discovery) = arrival.candidate()? {
                    return Ok(discovery);
                }
                let _ = child.wait();
                let bytes = read_status(status).map_err(|error| {
                    Some(environment_fault(
                        "resident::daemon-handoff-unavailable",
                        &error.to_string(),
                    ))
                })?;
                if bytes.is_empty() {
                    return Err(None);
                }
                let outcome = wire::decode::<Outcome<()>>(&bytes).map_err(|error| {
                    Some(environment_fault(
                        "resident::daemon-outcome-invalid",
                        &error.to_string(),
                    ))
                })?;
                return Err(Some(outcome));
            }
            Ok(Signal::ObserverFailed(error)) => {
                return Err(Some(environment_fault(
                    "resident::daemon-observer-failed",
                    &error,
                )));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // This bound is the warn point, not necessarily the real
                // deadline: loop back so the top of the loop re-checks
                // `remaining` and raises the readiness-elapsed refusal only
                // once the full horizon has actually passed.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Some(environment_fault(
                    "resident::daemon-observer-failed",
                    "resident readiness completion channel disconnected",
                )));
            }
        }
    }
}

/// The discovery a launch waits for: a probed publication at `serve` that
/// differs from the `baseline` present before launch.
struct Arrival<'a> {
    serve: &'a Path,
    baseline: Option<&'a [u8]>,
    token: Option<&'a str>,
}

impl Arrival<'_> {
    fn candidate(&self) -> Result<Option<serve::Discovery>, Option<Outcome<()>>> {
        let Some(bytes) = read_optional(self.serve).map_err(|error| {
            Some(environment_fault(
                "resident::daemon-discovery-unavailable",
                &error.to_string(),
            ))
        })?
        else {
            return Ok(None);
        };
        if self.baseline == Some(bytes.as_slice()) {
            return Ok(None);
        }
        let discovery = wire::decode::<serve::Discovery>(&bytes).map_err(|error| {
            Some(environment_fault(
                "resident::daemon-discovery-invalid",
                &error.to_string(),
            ))
        })?;
        probe(&discovery, self.token).map_err(Some)?;
        Ok(Some(discovery))
    }
}

fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn probe(discovery: &serve::Discovery, token: Option<&str>) -> CommandResult<()> {
    let authority = discovery.endpoint.strip_prefix("http://").ok_or_else(|| {
        environment_fault(
            "resident::daemon-endpoint-invalid",
            "published resident endpoint is not an HTTP origin",
        )
    })?;
    let address = authority.parse::<SocketAddr>().map_err(|error| {
        environment_fault("resident::daemon-endpoint-invalid", &error.to_string())
    })?;
    let mut stream = TcpStream::connect_timeout(&address, PROBE_BOUND)
        .map_err(|error| environment_fault("resident::daemon-probe-failed", &error.to_string()))?;
    stream
        .set_read_timeout(Some(PROBE_BOUND))
        .map_err(|error| environment_fault("resident::daemon-probe-failed", &error.to_string()))?;
    stream
        .set_write_timeout(Some(PROBE_BOUND))
        .map_err(|error| environment_fault("resident::daemon-probe-failed", &error.to_string()))?;
    let route = token.map_or_else(
        || "/bootstrap".to_owned(),
        |token| format!("/bootstrap?auth={token}"),
    );
    write!(
        stream,
        "GET {route} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    )
    .and_then(|()| stream.flush())
    .map_err(|error| environment_fault("resident::daemon-probe-failed", &error.to_string()))?;
    let mut response = Vec::new();
    stream
        .take(MAX_BOOTSTRAP_BYTES)
        .read_to_end(&mut response)
        .map_err(|error| environment_fault("resident::daemon-probe-failed", &error.to_string()))?;
    if !response.starts_with(b"HTTP/1.1 200 OK\r\n") {
        let status = response
            .split(|byte| *byte == b'\n')
            .next()
            .map(String::from_utf8_lossy)
            .unwrap_or_default();
        return Err(environment_fault(
            "resident::daemon-probe-refused",
            status.trim(),
        ));
    }
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .ok_or_else(|| {
            environment_fault(
                "resident::daemon-probe-invalid",
                "bootstrap response had no header terminator",
            )
        })?;
    let bootstrap = wire::decode::<wire::AppBootstrap>(&response[split..])
        .map_err(|error| environment_fault("resident::daemon-probe-invalid", &error.to_string()))?;
    if bootstrap.epoch != discovery.epoch
        || bootstrap.capability != discovery.capability
        || bootstrap.endpoint != discovery.endpoint
    {
        return Err(environment_fault(
            "resident::daemon-generation-moved",
            "authenticated bootstrap did not match the published resident generation",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn terminate_group(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    let Some(group) = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::TERM);
    let (sender, receiver) = mpsc::sync_channel(1);
    wait_for_exit(child.id(), sender);
    if receiver.recv_timeout(TERMINATION_HORIZON).is_err() {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
    let _ = child.wait();
}

/// End a child that never became ready.
#[cfg(windows)]
fn terminate_group(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn child_diagnostic(path: &Path, offset: u64) -> String {
    let bytes = fs::read(path).unwrap_or_default();
    let start = usize::try_from(offset)
        .unwrap_or(bytes.len())
        .min(bytes.len());
    let recent = &bytes[start..];
    let recent = &recent[recent.len().saturating_sub(4096)..];
    let detail = String::from_utf8_lossy(recent).trim().to_owned();
    if detail.is_empty() {
        "detached child exited before publishing discovery and left no diagnostic".to_owned()
    } else {
        format!("detached child exited before publishing discovery: {detail}")
    }
}

fn map_failure(outcome: Outcome<()>) -> Outcome<wire::AppOpen> {
    match outcome {
        Outcome::Reply(_) => Outcome::Fault(Fault {
            class: FaultClass::Protocol,
            reason: "resident::daemon-outcome-invalid".to_owned(),
            detail: "detached startup returned a terminal reply before readiness".to_owned(),
        }),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

fn write_framed(stream: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let length = u32::try_from(bytes.len())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    stream.write_all(HANDOFF_MAGIC)?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()
}

fn read_framed(stream: &mut impl Read) -> Result<Vec<u8>, Outcome<()>> {
    let mut magic = vec![0; HANDOFF_MAGIC.len()];
    stream.read_exact(&mut magic).map_err(|error| {
        environment_fault("resident::daemon-handoff-unavailable", &error.to_string())
    })?;
    if magic != HANDOFF_MAGIC {
        return Err(environment_fault(
            "resident::daemon-handoff-invalid",
            "detached child handoff did not carry the internal protocol marker",
        ));
    }
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).map_err(|error| {
        environment_fault("resident::daemon-handoff-unavailable", &error.to_string())
    })?;
    let length = usize::try_from(u32::from_be_bytes(length)).map_err(|error| {
        environment_fault("resident::daemon-handoff-invalid", &error.to_string())
    })?;
    if length > MAX_HANDOFF_BYTES {
        return Err(environment_fault(
            "resident::daemon-handoff-invalid",
            "detached child handoff exceeded its fixed byte bound",
        ));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).map_err(|error| {
        environment_fault("resident::daemon-handoff-unavailable", &error.to_string())
    })?;
    Ok(bytes)
}

fn read_status(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut magic = vec![0; HANDOFF_MAGIC.len()];
    match stream.read_exact(&mut magic) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(Vec::new()),
        Err(error) => return Err(error),
    }
    if magic != HANDOFF_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "detached child outcome had no internal protocol marker",
        ));
    }
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_be_bytes(length))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if length > MAX_HANDOFF_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "detached child outcome exceeded its fixed byte bound",
        ));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[cfg(unix)]
fn handoff_present(descriptor: RawFd) -> Result<bool, Outcome<()>> {
    let mut prefix = vec![0_u8; HANDOFF_MAGIC.len()];
    // SAFETY: recv only inspects the named descriptor and writes within prefix.
    let read = unsafe {
        libc::recv(
            descriptor,
            prefix.as_mut_ptr().cast(),
            prefix.len(),
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if read == -1 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::EBADF) | Some(libc::ENOTSOCK) | Some(libc::EAGAIN) => Ok(false),
            _ => Err(environment_fault(
                "resident::daemon-handoff-unavailable",
                &error.to_string(),
            )),
        };
    }
    let read = usize::try_from(read).map_err(|error| {
        environment_fault("resident::daemon-handoff-unavailable", &error.to_string())
    })?;
    Ok(read == HANDOFF_MAGIC.len() && prefix == HANDOFF_MAGIC)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("spice-resident-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture root exists");
        root
    }

    #[test]
    fn an_oversized_log_is_set_aside_once_and_a_small_one_is_left_alone() {
        let root = fixture("rotate");
        let log = root.join("resident.log");
        let rotated = root.join("resident.log.1");

        fs::write(&log, b"short").expect("small log written");
        rotate_log(&log).expect("small log rotation succeeds");
        assert_eq!(fs::read(&log).expect("small log remains"), b"short");
        assert!(!rotated.exists());

        let oversized = vec![b'x'; usize::try_from(LOG_ROTATE_BYTES).expect("bound fits") + 1];
        fs::write(&log, &oversized).expect("oversized log written");
        rotate_log(&log).expect("oversized log rotation succeeds");
        assert!(!log.exists(), "the oversized log was not set aside");
        assert_eq!(
            fs::metadata(&rotated).expect("rotated log exists").len(),
            LOG_ROTATE_BYTES + 1
        );

        fs::write(&log, &oversized[..oversized.len() - 1]).expect("second log written");
        rotate_log(&log).expect("boundary-sized log rotation succeeds");
        assert!(log.exists(), "a log exactly at the bound was rotated");
        fs::write(&log, &oversized).expect("third log written");
        rotate_log(&log).expect("second oversized rotation succeeds");
        assert!(!log.exists());
        assert_eq!(
            fs::metadata(&rotated).expect("rotated log replaced").len(),
            LOG_ROTATE_BYTES + 1
        );
        fs::remove_dir_all(root).expect("fixture removed");
    }
    #[test]
    fn a_first_log_creates_its_parent_and_a_colliding_parent_is_refused() {
        let root = fixture("parent");
        let runtime = root.join("runtime");
        fs::create_dir(&runtime).expect("runtime exists");
        let logs = runtime.join("logs");
        let log = logs.join("resident.log");

        let mut opened = open_log(&log).expect("missing log parent is created");
        opened.write_all(b"canonical").expect("log is writable");
        drop(opened);
        assert_eq!(fs::read(&log).expect("log remains canonical"), b"canonical");

        fs::remove_dir_all(&logs).expect("created log parent removed");
        fs::write(&logs, b"collision").expect("parent collision created");
        let error = open_log(&log).expect_err("non-directory parent is refused");
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
        assert_eq!(fs::read(&logs).expect("collision remains"), b"collision");
        fs::remove_dir_all(root).expect("fixture removed");
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_log_parent_is_refused_before_rotation() {
        let root = fixture("linked-parent");
        let runtime = root.join("runtime");
        let outside = root.join("outside");
        fs::create_dir(&runtime).expect("runtime exists");
        fs::create_dir(&outside).expect("outside directory exists");
        let outside_log = outside.join("resident.log");
        File::create(&outside_log)
            .and_then(|file| file.set_len(LOG_ROTATE_BYTES + 1))
            .expect("outside log exists");
        std::os::unix::fs::symlink(&outside, runtime.join("logs"))
            .expect("linked log parent exists");

        let error =
            rotate_log(&runtime.join("logs/resident.log")).expect_err("linked parent is refused");
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
        assert_eq!(
            fs::metadata(&outside_log)
                .expect("outside log remains")
                .len(),
            LOG_ROTATE_BYTES + 1
        );
        assert!(!outside.join("resident.log.1").exists());
        fs::remove_dir_all(root).expect("fixture removed");
    }
}
