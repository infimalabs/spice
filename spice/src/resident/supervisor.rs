//! Detached lane supervisors that may outlive any resident generation.

#[cfg(not(test))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::process::Stdio;
use std::process::{Command as ProcessCommand, ExitStatus};
use std::sync::Arc;
#[cfg(not(test))]
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lane::capture::CaptureReporter;
use lane::occupant::DetachedProcess;
use serde::{Deserialize, Serialize};
use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::{CanonicalUuid, Refusal};

pub(crate) mod archival;
pub(super) mod command;
pub(super) mod ending;
mod entry;
mod events;
mod handle;
mod reporter;
mod start;
pub(super) mod terminal;
#[cfg(test)]
pub(super) mod test_support;

#[cfg(not(test))]
use super::lane_surface::admit_worktree;
use super::lane_surface::{
    HostProbes, OccupantLaunch, OccupantLaunchRequest, SessionProofError, await_session,
    occupant_launch,
};
use super::{CommandResult, RuntimePaths, environment_fault, store_fault};
pub(crate) use entry::entry;
use events::StateEvents;
#[cfg(test)]
use handle::StandInOccupant;
use handle::{Handle, process_exists, process_exists_raw, status_from_raw, status_raw};
use reporter::DurableReporter;

#[cfg(all(unix, not(test)))]
use std::os::unix::fs::OpenOptionsExt as _;

const ENTRY: &str = "__lane-supervisor";
// Launch requests are consumed by this executable generation. Durable state
// is also consumed by replacement residents, so version its schema separately.
const REQUEST_VERSION: u32 = 1;
const STATE_VERSION: u32 = 1;
const REQUEST_BYTES: u64 = 64 * 1024;
const STATE_BYTES: u64 = 64 * 1024;
#[cfg(not(test))]
const START_HORIZON: Duration = Duration::from_secs(15);
const PRESENCE_POLL: Duration = Duration::from_secs(1);

/// Minimal best-effort notification projected from already-durable tape.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Notice {
    Record {
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        stream: String,
        boundary: String,
        locus: wire::TapeLocus,
    },
    /// The process reached a clean claim-owned boundary and needs the
    /// resident's held-claim scheduler to decide its next activation.
    Idle {
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
    },
    /// The idle process admitted a turn, so its claim renews as attended
    /// again.
    Active {
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
    },
    Exited {
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
    },
    Failed {
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        failure: String,
    },
    /// The occupant opened a confirm dialog, stated here as one consent
    /// request; the answer returns through the lane's kick door.
    Asked {
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        request: CanonicalUuid,
        operation: String,
    },
    /// The vendor refused the occupant's fast mode in these words; the
    /// occupant turned it off and continues.
    FastRefused {
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        refusal: String,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    worktree: String,
    vendor: String,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    resume: Option<String>,
    fast_mode: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    state: PathBuf,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    supervisor: u32,
    worktree: String,
    vendor: String,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    phase: Phase,
    group: Option<u32>,
    session: Option<wire::LaneSession>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claim: Option<terminal::ServiceClaim>,
    git: Option<String>,
    started_millis: Option<u64>,
    runtime_millis: Option<u64>,
    grace_millis: Option<u64>,
    reap_millis: Option<u64>,
    completion: CompletionField,
    /// Proven by the local SILL wait, never inferred from child exit.
    #[serde(default)]
    capture_drained: bool,
    failure: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(transparent)]
struct CompletionField(Option<PersistedCompletion>);

impl CompletionField {
    const fn none() -> Self {
        Self(None)
    }

    const fn some(completion: PersistedCompletion) -> Self {
        Self(Some(completion))
    }

    const fn as_ref(&self) -> Option<&PersistedCompletion> {
        self.0.as_ref()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PersistedCompletion {
    Exited { status_raw: i32 },
    TimedOut { status_raw: i32 },
    Cancelled { status_raw: i32 },
}

impl PersistedCompletion {
    fn capture(completion: &sill::Completion) -> Self {
        let status_raw = status_raw(completion.status());
        match completion {
            sill::Completion::Exited(_) => Self::Exited { status_raw },
            sill::Completion::TimedOut(_) => Self::TimedOut { status_raw },
            sill::Completion::Cancelled(_) => Self::Cancelled { status_raw },
        }
    }

    fn decode(&self) -> io::Result<sill::Completion> {
        let (status_raw, constructor): (i32, fn(ExitStatus) -> sill::Completion) = match self {
            Self::Exited { status_raw } => (*status_raw, sill::Completion::Exited),
            Self::TimedOut { status_raw } => (*status_raw, sill::Completion::TimedOut),
            Self::Cancelled { status_raw } => (*status_raw, sill::Completion::Cancelled),
        };
        status_from_raw(status_raw).map(constructor)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Starting,
    Running,
    Idle,
    Exited,
    Failed,
}

impl State {
    fn validate(self) -> io::Result<Self> {
        let completion = self.completion.as_ref();
        let valid = match self.phase {
            Phase::Idle => self.claim.is_some() && completion.is_none(),
            Phase::Exited => self.claim.is_none() && completion.is_some(),
            Phase::Starting | Phase::Running | Phase::Failed => {
                self.claim.is_none() && completion.is_none()
            }
        };
        if valid {
            return Ok(self);
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{:?} supervisor state has an invalid claim or completion",
                self.phase
            ),
        ))
    }
}

/// Session and process evidence returned after a detached launch is proven.
pub(super) struct Launch {
    pub(super) occupant: CanonicalUuid,
    pub(super) group: u32,
    pub(super) session: wire::LaneSession,
    pub(super) git: String,
    pub(super) process: Box<dyn DetachedProcess>,
}

/// Admitted detached launch whose process and session proof are pending.
///
/// Establishment runs outside the resident writer loop. Predecessor maintenance
/// finishes before spawning, so it consumes neither the session-proof horizon
/// nor the detached supervisor's start horizon.
pub(super) struct PendingLaunch {
    establish: Box<dyn FnOnce() -> CommandResult<Launch> + Send>,
}

impl PendingLaunch {
    fn new(establish: impl FnOnce() -> CommandResult<Launch> + Send + 'static) -> Self {
        Self {
            establish: Box::new(establish),
        }
    }

    pub(super) fn establish(self) -> CommandResult<Launch> {
        (self.establish)()
    }
}

fn maintenance(paths: &RuntimePaths, lane: CanonicalUuid) -> CommandResult<archival::Maintenance> {
    match archival::acquire_maintenance(&paths.tape, lane) {
        Ok(archival::MaintenanceAttempt::Acquired(maintenance)) => Ok(maintenance),
        Ok(archival::MaintenanceAttempt::Contended) => Err(wire::Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "lane::tape-maintenance-active".to_owned(),
                Some("lane".to_owned()),
                Some("no historical tape conversion in progress".to_owned()),
                Some(lane.to_string()),
            )
            .with_detail("retry the launch after the bounded archival pass releases this lane"),
        ))),
        Err(error) => Err(environment_fault(
            "lane::tape-maintenance-unavailable",
            &error.to_string(),
        )),
    }
}

/// Durable supervisor state found for an occupied lane during replacement.
pub(super) enum Adoption {
    /// The same supervisor and agent remain live.
    Running(Launch),
    /// The supervisor reaped the agent while no resident was listening.
    Exited(Launch),
    /// The supervisor could not preserve a safely adoptable agent.
    Failed(String),
}

/// Admit and spawn a new detached supervisor from this exact executable
/// generation. Session proof remains pending for an off-loop worker.
#[cfg(not(test))]
pub(super) fn begin_launch(
    paths: &RuntimePaths,
    request: OccupantLaunchRequest<'_>,
    _reporter: Arc<dyn CaptureReporter>,
) -> CommandResult<PendingLaunch> {
    // The detached child derives its runtime paths from this worktree, so its
    // own admission cannot tell a foreign repository apart; admit it here.
    admit_worktree(paths, request.worktree)?;
    let maintenance = maintenance(paths, request.lane)?;
    let directory = supervisor_directory(paths, request.lane, request.occupant);
    fs::create_dir_all(&directory).map_err(|error| {
        environment_fault("lane::supervisor-state-unavailable", &error.to_string())
    })?;
    let state = directory.join("state.json");
    let request_path = directory.join("request.json");
    let encoded = Request {
        version: REQUEST_VERSION,
        worktree: request.worktree.to_owned(),
        vendor: request.vendor.as_str().to_owned(),
        epoch: request.epoch,
        lane: request.lane,
        occupant: request.occupant,
        resume: request.resume.session_id().map(str::to_owned),
        fast_mode: request.fast_mode,
        model: request.model.map(str::to_owned),
        effort: request.effort.map(str::to_owned),
        state: state.clone(),
    };
    publish_json(&request_path, &encoded).map_err(|error| {
        environment_fault("lane::supervisor-request-unavailable", &error.to_string())
    })?;
    let executable = std::env::current_exe().map_err(|error| {
        environment_fault(
            "lane::supervisor-executable-unavailable",
            &error.to_string(),
        )
    })?;
    let log = supervisor_log(&directory).map_err(|error| {
        environment_fault("lane::supervisor-log-unavailable", &error.to_string())
    })?;
    let stdout = log.try_clone().map_err(|error| {
        environment_fault("lane::supervisor-log-unavailable", &error.to_string())
    })?;
    let events = StateEvents::watch(&state).map_err(|error| {
        environment_fault("lane::supervisor-observer-unavailable", &error.to_string())
    })?;
    let paths = paths.clone();
    let lane = request.lane;
    let occupant = request.occupant;
    Ok(PendingLaunch::new(move || {
        let _maintenance = maintenance;
        archival::startup(&paths, &encoded).map_err(|error| {
            environment_fault("lane::supervisor-handoff-failed", &error.to_string())
        })?;
        let mut command = ProcessCommand::new(executable);
        command
            .arg(ENTRY)
            .arg(&request_path)
            .current_dir(&encoded.worktree)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(log);
        detach(&mut command);
        let mut supervisor = command.spawn().map_err(|error| {
            environment_fault("lane::supervisor-launch-failed", &error.to_string())
        })?;
        events.observe_exit(supervisor.id());
        let _reaper = thread::Builder::new()
            .name("spice-supervisor-reaper".to_owned())
            .spawn(move || {
                let _ = supervisor.wait();
            });
        await_running(&paths, &state, lane, occupant, events)
    }))
}

/// Launch an occupant without a process: the launch composes exactly as a
/// supervised one, and the omp stand-in's stream is replayed through the same
/// capture that proves the session.
#[cfg(test)]
pub(super) fn begin_launch_test(
    paths: &RuntimePaths,
    request: OccupantLaunchRequest<'_>,
    reporter: Arc<dyn CaptureReporter>,
) -> CommandResult<PendingLaunch> {
    let occupant = request.occupant;
    let maintenance = maintenance(paths, request.lane)?;
    let probes = super::lane_surface::InertProbes::within(paths);
    let launch = occupant_launch(paths, request, reporter, &probes)?;
    let stream = super::lane_surface::stand_in_stream(&launch.spec);
    let process = StandInOccupant::new(&launch.spec);
    let git = launch.git;
    let proof = launch.session;
    let capture = launch.capture;
    Ok(PendingLaunch::new(move || {
        let _maintenance = maintenance;
        capture.replay(&stream).map_err(|error| {
            environment_fault("lane::supervisor-launch-failed", &error.to_string())
        })?;
        let session =
            await_session(proof).map_err(|error| wire::Outcome::Refusal(error.refusal()))?;
        Ok(Launch {
            occupant,
            group: StandInOccupant::GROUP,
            session,
            git,
            process: Box::new(process),
        })
    }))
}
#[cfg(test)]
pub(super) use begin_launch_test as begin_launch;

/// Start `command` as the leader of a new session and process group. It holds
/// no controlling terminal, so neither the interrupts nor the device of the
/// terminal it was launched from reach it or anything it starts; it outlives
/// the launcher and is ended only through its own group.
pub(super) fn detach(command: &mut ProcessCommand) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: setsid is async-signal-safe and the closure captures nothing.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
}

/// Adopt the exact live supervisor already naming a durable lane occupant.
pub(super) fn adopt(
    paths: &RuntimePaths,
    row: &wire::LaneResidency,
) -> CommandResult<Option<Adoption>> {
    let Some(occupant) = row.occupant else {
        return Ok(None);
    };
    let Some((path, state)) = ending::durable_state(paths, row.lane, occupant)? else {
        return Ok(None);
    };
    match state.phase {
        Phase::Running | Phase::Idle if process_exists(state.supervisor)? => {
            state_launch(paths, path, state)
                .map(Adoption::Running)
                .map(Some)
        }
        Phase::Running | Phase::Idle => Ok(Some(Adoption::Failed(
            ending::SUPERVISOR_DISAPPEARED.to_owned(),
        ))),
        Phase::Exited => state_launch(paths, path, state)
            .map(Adoption::Exited)
            .map(Some),
        Phase::Failed => Ok(Some(Adoption::Failed(ending::failure_detail(
            state.failure,
        )))),
        Phase::Starting => Ok(Some(Adoption::Failed(
            ending::DID_NOT_FINISH_STARTING.to_owned(),
        ))),
    }
}

/// The exact claim owned by an idle live occupant, when its durable supervisor
/// state proves one.
pub(super) fn idle_claim(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> CommandResult<Option<terminal::ServiceClaim>> {
    let Some((_, state)) = ending::durable_state(paths, lane, occupant)? else {
        return Ok(None);
    };
    Ok((state.phase == Phase::Idle)
        .then_some(state.claim)
        .flatten())
}

/// Whether the supervisor record beneath `supervisors` for this seated
/// occupant publishes an idle process. Absent or unreadable state is not idle:
/// the claim keeps its attended schedule, and adoption reports the record.
pub(super) fn idle_beneath(
    supervisors: &Path,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> bool {
    let path = occupant_directory(supervisors, lane, occupant).join("state.json");
    read_state(&path)
        .is_ok_and(|state| matches_state(&state, lane, occupant) && state.phase == Phase::Idle)
}

/// Ask every occupied command driver to reconcile an idle claim owner against
/// the journal snapshot that triggered this lifecycle pass.
pub(super) fn reconcile_claims(paths: &RuntimePaths, snapshot: &bus::TaskSnapshot) {
    let Some(residency) = snapshot.residency() else {
        return;
    };
    for row in &residency.body().lanes {
        if row.occupant.is_some() {
            command::reconcile_claim(paths, row.lane);
        }
    }
}

#[cfg(not(test))]
fn await_running(
    paths: &RuntimePaths,
    path: &Path,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    events: StateEvents,
) -> CommandResult<Launch> {
    let state = start::await_published(path, lane, occupant, START_HORIZON, |remaining| {
        events.wait_for(remaining)
    })?;
    // A short-lived agent can publish its session and end between two parent
    // observations. The terminal state still carries the complete launch
    // authority; returning its handle lets the resident commit the start and
    // then observe the exact ending instead of turning a harmless race into
    // resident failure.
    let events = matches!(state.phase, Phase::Running | Phase::Idle).then_some(events);
    let door = command::kick_path(paths, lane);
    state_launch_inner(path.to_path_buf(), door, state, events)
}

fn state_launch(paths: &RuntimePaths, path: PathBuf, state: State) -> CommandResult<Launch> {
    let door = command::kick_path(paths, state.lane);
    let events = if matches!(state.phase, Phase::Running | Phase::Idle) {
        Some(StateEvents::arm(&path, state.supervisor).map_err(|error| {
            environment_fault("lane::supervisor-observer-unavailable", &error.to_string())
        })?)
    } else {
        None
    };
    state_launch_inner(path, door, state, events)
}

fn state_launch_inner(
    path: PathBuf,
    door: PathBuf,
    state: State,
    events: Option<StateEvents>,
) -> CommandResult<Launch> {
    let group = required(state.group, "process group")?;
    let session = required(state.session.clone(), "session proof")?;
    let git = required(state.git.clone(), "launch Git identity")?;
    let handle = Handle::new(path, door, &state, events)?;
    Ok(Launch {
        occupant: state.occupant,
        group,
        session,
        git,
        process: Box::new(handle),
    })
}

fn run(request_path: &Path, ambient: &Ambient) -> io::Result<()> {
    let request: Request = read_json(request_path, REQUEST_BYTES)?;
    if request.version != REQUEST_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported supervisor request version {}", request.version),
        ));
    }
    let starting = State {
        version: STATE_VERSION,
        supervisor: std::process::id(),
        worktree: request.worktree.clone(),
        vendor: request.vendor.clone(),
        lane: request.lane,
        occupant: request.occupant,
        phase: Phase::Starting,
        group: None,
        session: None,
        git: None,
        claim: None,
        started_millis: None,
        runtime_millis: None,
        grace_millis: None,
        reap_millis: None,
        completion: CompletionField::none(),
        failure: None,
        capture_drained: false,
    };
    publish_json(&request.state, &starting)?;
    if let Err(error) = run_agent(&request, starting.clone(), ambient) {
        let mut failed = read_state(&request.state).unwrap_or(starting);
        failed.phase = Phase::Failed;
        failed.claim = None;
        failed.completion = CompletionField::none();
        failed.failure = Some(error.to_string());
        publish_json(&request.state, &failed)?;
        return Err(error);
    }
    Ok(())
}

fn run_agent(request: &Request, mut state: State, ambient: &Ambient) -> io::Result<()> {
    let repository = Repository::discover(&request.worktree).map_err(io::Error::other)?;
    let paths = RuntimePaths::from_repository(&repository).map_err(outcome_error)?;
    let vendor = request
        .vendor
        .parse::<wire::Name>()
        .map_err(io::Error::other)?;
    let resume = request
        .resume
        .as_ref()
        .map(|identity| vendor::SessionId::new(identity.clone()).map(vendor::Resume::Session))
        .transpose()
        .map_err(io::Error::other)?
        .unwrap_or(vendor::Resume::Fresh);
    let reporter = Arc::new(DurableReporter::new(&paths)?);
    let capture_reporter: Arc<dyn CaptureReporter> = reporter.clone();
    let launch = occupant_launch(
        &paths,
        OccupantLaunchRequest {
            worktree: &request.worktree,
            vendor: &vendor,
            epoch: request.epoch,
            lane: request.lane,
            occupant: request.occupant,
            resume,
            fast_mode: request.fast_mode,
            model: request.model.as_deref(),
            effort: request.effort.as_deref(),
        },
        capture_reporter,
        &HostProbes { ambient },
    )
    .map_err(outcome_error)?;
    let git = launch.git;
    let proof = launch.session;
    let cancellation = launch.cancellation;
    let lease = launch.lease;
    let started_millis = now_millis()?;
    let jobs = terminal::Jobs::for_lane(&paths, request.lane);
    let service_authority =
        terminal::ServiceAuthority::new(paths.journal.clone(), request.lane, request.occupant);
    let terminal = Some(terminal::Terminal::from_launch(
        launch.shell,
        &launch.spec,
        jobs,
        service_authority.clone(),
    ));
    let observation = OccupantLaunch::observation(launch.capture).map_err(outcome_error)?;
    let mut running = spawn_occupant(&vendor, launch.spec, observation)?;
    let group = running.id();
    let runtime = running.spec().deadlines.runtime();
    let grace = running.spec().grace;
    let reap = running.spec().deadlines.reap();
    let _door = launch
        .opening
        .map(|opening| {
            let idle_reporter = Arc::clone(&reporter);
            let lane = request.lane;
            let occupant = request.occupant;
            let driver = command::Driver::new(
                running.take_stdin()?,
                &request.state,
                request.lane,
                repository.worktree_root().to_path_buf(),
                terminal,
                service_authority.clone(),
                move |phase| {
                    idle_reporter.send(&match phase {
                        Phase::Idle => Notice::Idle { lane, occupant },
                        _ => Notice::Active { lane, occupant },
                    });
                },
            )?;
            reporter.drive(Arc::clone(&driver));
            driver.open(&opening, launch.model_selection, launch.first_prompt)?;
            let door = command::listen_for_kicks(
                &paths,
                request.lane,
                request.occupant,
                Arc::clone(&driver),
            )?;
            Ok::<_, io::Error>(door)
        })
        .transpose()?;
    let session = await_session(proof).map_err(session_error)?;
    state.phase = Phase::Running;
    state.group = Some(group);
    state.session = Some(session);
    state.claim = None;
    state.git = Some(git);
    state.started_millis = Some(started_millis);
    state.runtime_millis = Some(duration_millis(runtime)?);
    state.grace_millis = Some(duration_millis(grace)?);
    state.reap_millis = Some(duration_millis(reap)?);
    publish_json(&request.state, &state)?;
    finish_agent(
        &paths,
        request,
        state,
        running,
        &cancellation,
        lease,
        reporter.as_ref(),
    )
}

trait PresenceProbe {
    fn present(&self) -> bool;
}

struct ResidentPresence<'a> {
    worktree: &'a Path,
    common: &'a Path,
    state_directory: Option<&'a Path>,
    resident: sill::LockFile,
}

impl<'a> ResidentPresence<'a> {
    fn new(paths: &'a RuntimePaths, state: &'a Path) -> Self {
        Self {
            worktree: &paths.worktree,
            common: &paths.common,
            state_directory: state.parent(),
            resident: sill::LockFile::new(
                sill::LockName::new("resident").expect("resident is a valid lock name"),
                paths.resident.clone(),
            ),
        }
    }
}

impl PresenceProbe for ResidentPresence<'_> {
    fn present(&self) -> bool {
        readable_directory(self.worktree)
            && readable_directory(self.common)
            && self.state_directory.is_some_and(readable_directory)
            && matches!(self.resident.availability(), Ok(sill::Availability::Held))
    }
}

#[derive(Default)]
struct AbsenceWindow {
    since: Option<Duration>,
}

impl AbsenceWindow {
    fn expired(&mut self, at: Duration, lease: Duration, presence: &impl PresenceProbe) -> bool {
        if presence.present() {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(at);
        at.saturating_sub(since) >= lease
    }
}

fn wait_for_occupant(
    mut running: sill::Running,
    cancellation: &sill::CancellationToken,
    lease: Duration,
    presence: &impl PresenceProbe,
) -> (Result<sill::WaitOutcome, sill::WaitError>, bool) {
    let started = Instant::now();
    let mut absence = AbsenceWindow::default();
    loop {
        if absence.expired(started.elapsed(), lease, presence) {
            cancellation.cancel();
            return (running.wait(), true);
        }
        let horizon = PRESENCE_POLL.min(running.runtime_remaining());
        if horizon.is_zero() {
            return (running.wait(), false);
        }
        match running.wait_for(horizon) {
            Ok(sill::WaitStep::Exited(outcome)) => return (Ok(outcome), false),
            Ok(sill::WaitStep::Running(still_running)) => running = *still_running,
            Err(error) => return (Err(error), false),
        }
    }
}

fn readable_directory(path: &Path) -> bool {
    fs::read_dir(path).is_ok()
}

fn publish_terminal_state(path: &Path, state: &State, state_may_be_absent: bool) -> io::Result<()> {
    if state_may_be_absent && !path.parent().is_some_and(readable_directory) {
        return Ok(());
    }
    let published = publish_json(path, state);
    if published.is_err() && state_may_be_absent && !path.parent().is_some_and(readable_directory) {
        return Ok(());
    }
    published
}

fn finish_agent(
    paths: &RuntimePaths,
    request: &Request,
    mut state: State,
    running: sill::Running,
    cancellation: &sill::CancellationToken,
    lease: Duration,
    reporter: &DurableReporter,
) -> io::Result<()> {
    let presence = ResidentPresence::new(paths, &request.state);
    let (outcome, state_may_be_absent) = wait_for_occupant(running, cancellation, lease, &presence);
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let failure = error.to_string();
            state.phase = Phase::Failed;
            state.claim = None;
            state.failure = Some(failure.clone());
            publish_terminal_state(&request.state, &state, state_may_be_absent)?;
            reporter.send(&Notice::Failed {
                lane: request.lane,
                occupant: request.occupant,
                failure,
            });
            return Ok(());
        }
    };
    if let Some(observation_failure) = outcome.observation_failure() {
        let failure = observation_failure.to_string();
        state.phase = Phase::Failed;
        state.claim = None;
        state.completion = CompletionField::none();
        state.failure = Some(failure.clone());
        publish_terminal_state(&request.state, &state, state_may_be_absent)?;
        reporter.send(&Notice::Failed {
            lane: request.lane,
            occupant: request.occupant,
            failure,
        });
        return Ok(());
    }
    state.phase = Phase::Exited;
    state.claim = None;
    state.capture_drained = outcome.outputs_drained();
    state.completion = CompletionField::some(PersistedCompletion::capture(outcome.completion()));
    publish_terminal_state(&request.state, &state, state_may_be_absent)?;
    reporter.send(&Notice::Exited {
        lane: request.lane,
        occupant: request.occupant,
    });
    // Completion is already public. Successors wait for this supervisor's
    // kernel exit before opening capture, outside their launch horizons.
    if state.capture_drained
        && reporter.quiescent()
        && let Some(tape) = archival::shutdown(paths, request)
    {
        archival::compress(&tape);
    }
    Ok(())
}

/// Spawn the occupant, naming the vendor and program when the launch fails.
fn spawn_occupant(
    vendor: &wire::Name,
    spec: sill::Spec,
    observation: sill::Observation,
) -> io::Result<sill::Running> {
    let program = spec.argv.program().to_owned();
    sill::spawn_observed(spec, observation).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "could not launch vendor {vendor} program {}: {error}",
                program.display()
            ),
        )
    })
}

/// A field a running supervisor's published state must carry.
fn required<T>(value: Option<T>, name: &str) -> CommandResult<T> {
    value.ok_or_else(|| {
        store_fault(
            "lane::supervisor-state-invalid",
            &format!("running supervisor has no {name}"),
        )
    })
}

pub(super) fn supervisor_directory(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> PathBuf {
    occupant_directory(&paths.supervisors, lane, occupant)
}

fn occupant_directory(supervisors: &Path, lane: CanonicalUuid, occupant: CanonicalUuid) -> PathBuf {
    spice::repository::lane_occupants(supervisors, lane).join(occupant.to_string())
}

pub(super) fn capture_path(paths: &RuntimePaths) -> PathBuf {
    #[cfg(unix)]
    {
        // `sockaddr_un.sun_path` is only 104 bytes on Darwin. Repository and
        // worktree roots are not bounded that tightly, so the ephemeral notice
        // door uses one content-addressed leaf beneath the canonical short
        // system temporary path. Durable state remains repository-local.
        let identity = blake3::hash(paths.common.as_os_str().as_encoded_bytes());
        PathBuf::from("/tmp").join(format!("spice-s-{identity}.sock"))
    }
    #[cfg(not(unix))]
    {
        paths.runtime.join("runtime").join("capture.sock")
    }
}

#[cfg(not(test))]
fn supervisor_log(directory: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(directory.join("supervisor.log"))
}

pub(in crate::resident) fn publish_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    atomic::Destination::new(path)
        .map_err(io::Error::other)?
        .publish(|file| file.write_all(&bytes))
        .map_err(io::Error::other)
}

fn read_state(path: &Path) -> io::Result<State> {
    let value: serde_json::Value = read_json(path, STATE_BYTES)?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "supervisor state has no valid contract version",
            )
        })?;
    if version != STATE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "supervisor state contract v{} is not owned by current Spice {} contract v{}",
                version,
                env!("CARGO_PKG_VERSION"),
                STATE_VERSION
            ),
        ));
    }
    let state: State = serde_json::from_value(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    state.validate()
}

pub(in crate::resident) fn read_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    limit: u64,
) -> io::Result<T> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "runtime document exceeds its bound",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn matches_state(state: &State, lane: CanonicalUuid, occupant: CanonicalUuid) -> bool {
    state.lane == lane && state.occupant == occupant
}

fn now_millis() -> io::Result<u64> {
    millis_since_epoch(SystemTime::now())
}

fn millis_since_epoch(at: SystemTime) -> io::Result<u64> {
    let millis = at
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis();
    u64::try_from(millis).map_err(io::Error::other)
}

fn duration_millis(duration: Duration) -> io::Result<u64> {
    u64::try_from(duration.as_millis()).map_err(io::Error::other)
}

fn session_error(error: SessionProofError) -> io::Error {
    let refusal = error.refusal();
    io::Error::other(format!("session proof refused as {refusal:?}"))
}

/// The launch failure a supervisor publishes: the CLI's text rendering of the
/// outcome, without the `spice: ` prefix the resident's own refusal adds.
fn outcome_error(outcome: wire::Outcome<()>) -> io::Error {
    let mut text = crate::render::render(&outcome, crate::render::Form::Text).map_or_else(
        |error| error,
        |rendering| String::from_utf8_lossy(rendering.document()).into_owned(),
    );
    text.truncate(text.trim_end().len());
    const PREFIX: &str = "spice: ";
    if text.starts_with(PREFIX) {
        text.drain(..PREFIX.len());
    }
    io::Error::other(text)
}

#[cfg(all(test, unix))]
mod tests;
