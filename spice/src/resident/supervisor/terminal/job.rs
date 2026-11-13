//! A job: one command the lane keeps beyond the call that started it.
//!
//! Every command the occupant runs is kept by a detached keeper process,
//! `spice __lane-job`, that owns nothing but the command: it runs the argv
//! in the command's directory under its environment, reads both streams into
//! one bounded log, ends the command's process group at its deadline, when
//! asked to stop, or when its output runs away, and publishes how the
//! command ended. A command started with `until` also has the first line of
//! its log that matches published before the ending. Once the ending is
//! published the keeper archives the log as a verified seekable Zstandard
//! file. The keeper is its own process group under nobody's supervision.
//! Deadline-bounded commands survive the turn, occupant, and supervisor that
//! started them; task-associated services survive only their exact claim win,
//! while an unanchored service is stopped with its launching occupant turn.
//! A result waits in the job's state for whoever asks. The lane observes a job
//! through its durable state: a direct kernel watch on the job's directory
//! wakes every read, and the keeper's exit names a keeper that died without an
//! ending. The keeper reads its stop request the same way, and an ending or
//! match nobody hears within the grace wakes the lane, so its next occupant
//! hears it.

use std::convert::Infallible;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant as Monotonic};

use serde::{Deserialize, Serialize};
use sill::Environment;
use steer::Key;
use wire::{CanonicalUuid, TaskStamp};

use super::super::super::RuntimePaths;
use super::super::super::daemon::observer;
use super::super::handle::process_exists_raw;
use super::super::{
    REQUEST_BYTES, STATE_BYTES, detach, millis_since_epoch, now_millis, publish_json, read_json,
};

mod index;
mod keeper;
mod launch;
mod reconcile;
mod retention;
mod service;
#[cfg(test)]
mod tests;
mod until;

use index::ServiceIndex;
pub(in crate::resident::supervisor) use keeper::keep;
pub(in crate::resident) use keeper::{Control, Handle, Observed};
use keeper::{Signal, watch};
pub(in crate::resident) use launch::Launch;
use launch::{KeeperLaunch, failed_start_error, launch_detached};
pub(in crate::resident) use reconcile::{reconcile_services, reconcile_turn_services};
pub(in crate::resident) use retention::CAPPED;
use retention::{MIB, Output, RETAINED_CAP, TERMINATE_AT};
use service::ServiceOwner;
pub(in crate::resident) use service::{ServiceAdmission, ServiceAuthority, ServiceClaim};

/// The hidden entry the keeper runs under.
pub(in crate::resident::supervisor) const ENTRY: &str = "__lane-job";

const VERSION: u32 = 1;
pub(super) const REQUEST: &str = "request.json";
pub(super) const STATE: &str = "state.json";
const LOG: &str = "log";
/// The log once its job ended, archived and verified.
const ARCHIVE: &str = "log.zst";
const STOP: &str = "stop";
const OPERATOR_STOP: &str = "operator-stop";
pub(super) const DELIVERED: &str = "delivered";
const WAKE: &str = "wake";

/// Bytes of the log a reading keeps: the end, where a failure explains
/// itself.
const LOG_TAIL: usize = 16 * 1024;

/// Observed output, in MiB, past which Spice ends a command.
pub(super) const OUTPUT_LIMIT_MIB: u64 = TERMINATE_AT / MIB;

/// How long a command has to leave on its own once the keeper ends it.
const TERMINATE_GRACE: Duration = Duration::from_secs(1);

/// How long an occupant has to hear an ending or a match before the keeper
/// wakes the lane to deliver it.
const DELIVERY_GRACE: Duration = Duration::from_secs(2);

/// What the keeper reads: the command as the model wrote it, the argv the
/// lane runs, and where and under what to run it.
#[derive(Deserialize, Serialize)]
struct Request {
    version: u32,
    id: String,
    command: String,
    argv: Vec<OsString>,
    cwd: PathBuf,
    environment: Environment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_millis: Option<u64>,
    /// What the results carry ahead of the command's output, as omp noted it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    notices: Vec<String>,
    /// Ambient task held when this job was launched, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task: Option<TaskStamp>,
    /// The exact active claim that owns an unbounded service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_claim: Option<ServiceClaim>,
    /// Occupant turn owning an unanchored service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_turn: Option<CanonicalUuid>,
    /// The lane to wake when nobody hears the ending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lane: Option<CanonicalUuid>,
    /// The pattern whose first matching log line is published while the
    /// command runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    until: Option<String>,
}

/// What the keeper publishes: who keeps the command, and how it ended.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(in crate::resident) struct State {
    version: u32,
    pub(in crate::resident) id: String,
    pub(in crate::resident) command: String,
    pub(in crate::resident) keeper: u32,
    pub(in crate::resident) started_millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::resident) timeout_millis: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::resident) ending: Option<Ending>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::resident) ended_millis: Option<u64>,
    /// What became of the command's output, once the keeper closed its log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::resident) output: Option<Output>,
    /// Signals Spice sent because the command's output ran away.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(in crate::resident) interventions: Vec<Intervention>,
}

/// One signal Spice sent on its own because the command's output ran away.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(in crate::resident) struct Intervention {
    pub(in crate::resident) signal: Sent,
    pub(in crate::resident) trigger: Trigger,
    /// Bytes the command had written when the signal was sent.
    pub(in crate::resident) observed: u64,
    pub(in crate::resident) at_millis: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(in crate::resident) enum Sent {
    #[serde(rename = "TERM")]
    Term,
    #[serde(rename = "KILL")]
    Kill,
}

/// Why Spice sent a signal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::resident) enum Trigger {
    /// Observed output reached the signal's threshold.
    Output,
    /// The command was still running a grace after it was asked to leave.
    Grace,
}

impl Intervention {
    fn describe(&self) -> String {
        let at = i64::try_from(self.at_millis)
            .ok()
            .and_then(|millis| jiff::Timestamp::from_millisecond(millis).ok())
            .map_or_else(|| self.at_millis.to_string(), |at| at.to_string());
        let (signal, threshold) = match self.signal {
            Sent::Term => ("SIGTERM", TERMINATE_AT),
            Sent::Kill => ("SIGKILL", retention::KILL_AT),
        };
        let why = match self.trigger {
            Trigger::Output => format!("observed output reached {} MiB", threshold / MIB),
            Trigger::Grace => format!(
                "the command was still running {} second(s) after SIGTERM",
                TERMINATE_GRACE.as_secs()
            ),
        };
        format!(
            "[Spice sent {signal} at {at}: {why}; {} bytes observed]",
            self.observed
        )
    }
}

impl State {
    /// The ending with its wall time, once the keeper has published one.
    fn ended(&self) -> Option<Ended> {
        let ending = self.ending.clone()?;
        let wall = self
            .ended_millis
            .map(|ended| Duration::from_millis(ended.saturating_sub(self.started_millis)));
        Some(Ended { ending, wall })
    }
}

/// How a job ended and how long it ran, as the keeper recorded them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::resident) struct Ended {
    pub(in crate::resident) ending: Ending,
    /// From the keeper's start to the ending; unknown for a lost job.
    pub(in crate::resident) wall: Option<Duration>,
}

/// How a kept command ended.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(in crate::resident) enum Ending {
    Exited {
        code: i32,
    },
    Signalled {
        signal: i32,
    },
    TimedOut,
    Stopped,
    /// Spice ended the command because its output ran away.
    OutputLimit,
    Failed {
        reason: String,
    },
    /// The keeper is gone without publishing an ending; read, never written.
    Lost,
}

/// The last of a job's log, and whether earlier output was left behind.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(in crate::resident) struct Tail {
    pub(in crate::resident) text: String,
    pub(in crate::resident) truncated: bool,
}

/// Where one lane's jobs live, the lane to wake for an ending nobody hears,
/// and the index its services enter while they may run.
#[derive(Clone, Debug)]
pub(in crate::resident) struct Jobs {
    root: PathBuf,
    lane: CanonicalUuid,
    services: ServiceIndex,
}

/// One kept command, by the directory that records it.
#[derive(Clone, Debug)]
pub(in crate::resident) struct Job {
    directory: PathBuf,
    id: String,
    command: String,
    cwd: PathBuf,
    timeout: Option<Duration>,
    task: Option<TaskStamp>,
    service_claim: Option<ServiceClaim>,
    service_turn: Option<CanonicalUuid>,
    notices: Vec<String>,
    until: Option<String>,
    keeper: Option<u32>,
}

impl Jobs {
    /// This lane's shell jobs, beside its occupant records.
    pub(in crate::resident) fn for_lane(paths: &RuntimePaths, lane: CanonicalUuid) -> Self {
        Self::beneath(&paths.supervisors, lane)
    }

    /// This lane's shell jobs beneath the canonical lane root.
    pub(in crate::resident) fn beneath(supervisors: &Path, lane: CanonicalUuid) -> Self {
        Self {
            root: spice::repository::lane_jobs(supervisors, lane),
            lane,
            services: ServiceIndex::beneath(supervisors),
        }
    }

    /// No running, unheard, or incompletely published lane-owned job remains.
    /// Missing delivery markers and unrecognizable partial records are pending,
    /// not evidence that their work has ended.
    pub(in crate::resident::supervisor) fn quiescent(&self) -> io::Result<bool> {
        Ok(self
            .ids(|directory| !directory.join(DELIVERED).is_file())?
            .is_empty())
    }

    /// Start keeping one command.
    ///
    /// # Errors
    ///
    /// Returns the failure of recording the request, revalidating a service's
    /// claim after publication, launching the keeper, or removing a failed
    /// start's record. Only a launch known not to own a process is removed.
    pub(in crate::resident) fn start(
        &self,
        job: Launch<'_>,
        task: Option<TaskStamp>,
        service: Option<ServiceAdmission<'_>>,
    ) -> io::Result<Job> {
        self.start_with(
            job,
            task,
            service,
            |directory, request, cwd, environment| {
                launch_detached(directory, request, cwd, environment)
            },
            |path| fs::remove_dir_all(path),
        )
    }

    /// Record one job and establish its keeper's process ownership with `launch`.
    fn start_with(
        &self,
        job: Launch<'_>,
        task: Option<TaskStamp>,
        service: Option<ServiceAdmission<'_>>,
        launch: impl FnOnce(&Path, &Path, &Path, &Environment) -> KeeperLaunch,
        cleanup: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<Job> {
        let (service_claim, service_turn) = match service.as_ref().map(|service| &service.owner) {
            Some(ServiceOwner::Task(claim)) => (Some(claim.clone()), None),
            Some(ServiceOwner::Turn(turn)) => (None, Some(*turn)),
            None => (None, None),
        };
        fs::create_dir_all(&self.root)?;
        let (id, directory) = self.claim()?;
        let request_path = directory.join(REQUEST);
        publish_json(
            &request_path,
            &Request {
                version: VERSION,
                id: id.clone(),
                command: job.command.to_owned(),
                argv: job.argv,
                cwd: job.cwd.to_path_buf(),
                environment: job.environment.clone(),
                timeout_millis: job
                    .timeout
                    .map(|timeout| u64::try_from(timeout.as_millis()))
                    .transpose()
                    .map_err(io::Error::other)?,
                notices: job.notices.to_vec(),
                task: task.clone(),
                service_claim: service_claim.clone(),
                service_turn,
                lane: Some(self.lane),
                until: job.until.map(str::to_owned),
            },
        )?;
        if job.timeout.is_none()
            && let Err(error) = self.services.insert(self.lane, &id)
        {
            return Err(failed_start_error(&directory, error, cleanup));
        }
        if let Some(service) = service {
            let validation = service.validate().and_then(|()| {
                (!directory.join(STOP).exists())
                    .then_some(())
                    .ok_or_else(|| {
                        io::Error::new(
                            ErrorKind::PermissionDenied,
                            "the service task claim ended during launch",
                        )
                    })
            });
            if let Err(error) = validation {
                return Err(failed_start_error(&directory, error, cleanup));
            }
        }
        let keeper = match launch(&directory, &request_path, job.cwd, job.environment) {
            KeeperLaunch::Started(keeper) => keeper,
            KeeperLaunch::NotStarted(error) => {
                return Err(failed_start_error(&directory, error, cleanup));
            }
            KeeperLaunch::Unknown(error) => return Err(error),
        };
        Ok(Job {
            directory,
            id,
            command: job.command.to_owned(),
            cwd: job.cwd.to_path_buf(),
            timeout: job.timeout,
            task,
            service_claim,
            service_turn,
            notices: job.notices.to_vec(),
            until: job.until.map(str::to_owned),
            keeper: Some(keeper),
        })
    }

    /// A directory of this lane's for a new job, named by the microsecond
    /// key the system names everything with, so a listing is a timeline; a
    /// microsecond already taken yields the next.
    fn claim(&self) -> io::Result<(String, PathBuf)> {
        let mut micros = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_micros(),
        )
        .map_err(io::Error::other)?;
        loop {
            let id = Key::for_micros(micros)
                .map_err(io::Error::other)?
                .to_string();
            let directory = self.root.join(&id);
            match fs::create_dir(&directory) {
                Ok(()) => return Ok((id, directory)),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => micros += 1,
                Err(error) => return Err(error),
            }
        }
    }

    /// One job of this lane, by the identity the model quotes: a job exists
    /// from the moment its request is recorded, before its keeper speaks.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` for an identity that names no job.
    pub(in crate::resident) fn open(&self, id: &str) -> io::Result<Job> {
        let named = Key::shaped(id);
        let directory = self.root.join(id);
        let request: Request = named
            .then(|| read_json(&directory.join(REQUEST), REQUEST_BYTES))
            .transpose()
            .map_err(|error| match error.kind() {
                ErrorKind::NotFound => io::Error::new(ErrorKind::NotFound, format!("no job {id}")),
                _ => error,
            })?
            .ok_or_else(|| io::Error::new(ErrorKind::NotFound, format!("no job {id}")))?;
        let keeper = read_state(&directory)?.map(|state| state.keeper);
        Ok(Job {
            directory,
            id: request.id,
            command: request.command,
            cwd: request.cwd,
            timeout: request.timeout_millis.map(Duration::from_millis),
            task: request.task,
            service_claim: request.service_claim,
            service_turn: request.service_turn,
            notices: request.notices,
            until: request.until,
            keeper,
        })
    }

    /// The jobs whose ending nobody has heard, oldest first, without opening
    /// the record of any job already delivered: a lane's row is read far more
    /// often than a job ends.
    ///
    /// # Errors
    ///
    /// Returns the failure of reading the lane's job directory.
    pub(in crate::resident) fn kept(&self) -> io::Result<Vec<Job>> {
        let mut jobs = Vec::new();
        for id in self.ids(|directory| !directory.join(DELIVERED).exists())? {
            if let Some(job) = self.present(&id)? {
                jobs.push(job);
            }
        }
        Ok(jobs)
    }

    /// The names of delivered jobs, newest first, from `at` (or the newest when
    /// none is named) toward the oldest. Names cost a directory read; a
    /// history page opens only the records it shows.
    ///
    /// # Errors
    ///
    /// Returns the failure of reading the lane's job directory.
    pub(in crate::resident) fn delivered(&self, at: Option<&str>) -> io::Result<Vec<String>> {
        let mut ids = self.ids(|directory| directory.join(DELIVERED).exists())?;
        if let Some(at) = at {
            ids.truncate(ids.partition_point(|id| id.as_str() <= at));
        }
        ids.reverse();
        Ok(ids)
    }

    /// The names of the jobs whose directories are `wanted`, oldest first: the
    /// keys order themselves.
    fn ids(&self, wanted: impl Fn(&Path) -> bool) -> io::Result<Vec<String>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry?;
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if wanted(&entry.path()) {
                ids.push(id);
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }

    /// The job named, or none when its directory is not a job's record.
    ///
    /// # Errors
    ///
    /// Returns the failure of reading a record that exists.
    pub(in crate::resident) fn present(&self, id: &str) -> io::Result<Option<Job>> {
        match self.open(id) {
            Ok(job) => Ok(Some(job)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The wall instant by which every unfinished deadline-bounded job of this
    /// lane has ended and had its ending delivered: the keeper's start plus the
    /// job's timeout, the keeper's terminate grace, and the delivery grace. A
    /// job whose keeper has not spoken counts from its recorded request, which
    /// precedes the keeper. Services and ended jobs bound nothing; `None` when
    /// no job remains or the lane's job records are unreadable.
    pub(in crate::resident) fn finite_horizon(&self) -> Option<wire::Instant> {
        let millis = self.finite_horizon_millis().ok()??;
        let at = jiff::Timestamp::from_millisecond(i64::try_from(millis).ok()?).ok()?;
        wire::Instant::new(at).ok()
    }

    fn finite_horizon_millis(&self) -> io::Result<Option<u64>> {
        let mut horizon = None;
        for job in self.kept()? {
            let Some(timeout) = job.timeout else {
                continue;
            };
            let started = match job.state()? {
                Some(state) if state.ending.is_some() => continue,
                Some(state) => state.started_millis,
                None => millis_since_epoch(fs::metadata(job.directory.join(REQUEST))?.modified()?)?,
            };
            let bound = Duration::from_millis(started)
                .saturating_add(timeout)
                .saturating_add(TERMINATE_GRACE)
                .saturating_add(DELIVERY_GRACE);
            let bound = u64::try_from(bound.as_millis()).unwrap_or(u64::MAX);
            horizon = Some(horizon.map_or(bound, |horizon: u64| horizon.max(bound)));
        }
        Ok(horizon)
    }
}

/// The state a job directory holds, with a keeper that vanished before
/// publishing an ending read as lost; `None` before the keeper has spoken.
fn read_state(directory: &Path) -> io::Result<Option<State>> {
    let mut state: State = match read_json(&directory.join(STATE), STATE_BYTES) {
        Ok(state) => state,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if state.ending.is_none()
        && state.keeper != std::process::id()
        && !process_exists_raw(state.keeper).unwrap_or(true)
    {
        state.ending = Some(Ending::Lost);
    }
    Ok(Some(state))
}

impl Job {
    pub(in crate::resident) fn id(&self) -> &str {
        &self.id
    }

    pub(in crate::resident) fn command(&self) -> &str {
        &self.command
    }
    /// Ambient task held when the job launched, if any.
    pub(in crate::resident) fn task(&self) -> Option<&TaskStamp> {
        self.task.as_ref()
    }

    /// How long the command may run before the keeper ends it, if bounded.
    pub(in crate::resident) fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// What the results carry ahead of the command's output.
    pub(in crate::resident) fn notices(&self) -> &[String] {
        &self.notices
    }

    /// The pattern the command is watched for, if it is.
    pub(in crate::resident) fn until(&self) -> Option<&str> {
        self.until.as_deref()
    }

    /// The line the watcher matched, unless the lane heard it or the ending
    /// that follows it.
    ///
    /// # Errors
    ///
    /// Returns the failure of reading the match.
    pub(in crate::resident) fn unheard_match(&self) -> io::Result<Option<String>> {
        if until::heard(&self.directory) {
            return Ok(None);
        }
        until::matched(&self.directory)
    }

    /// Record that the lane heard the match.
    ///
    /// # Errors
    ///
    /// Returns the failure of recording it.
    pub(in crate::resident) fn hear_match(&self) -> io::Result<()> {
        until::hear(&self.directory)
    }

    /// Whether output past the log's cap went unsearched with no line
    /// matched: the log reached its cap and the watcher found nothing.
    pub(in crate::resident) fn watch_unsearched(&self) -> bool {
        self.until.is_some()
            && until::matched(&self.directory).is_ok_and(|line| line.is_none())
            && self
                .state()
                .ok()
                .flatten()
                .and_then(|state| state.output)
                .is_some_and(|output| output.omitted > 0)
    }

    /// How the job ended, if it has.
    pub(in crate::resident) fn ended(&self) -> Option<Ended> {
        self.state().ok().flatten().and_then(|state| state.ended())
    }

    /// The job's published state, if the keeper has spoken.
    ///
    /// # Errors
    ///
    /// Returns the failure of reading the state document.
    pub(in crate::resident) fn state(&self) -> io::Result<Option<State>> {
        read_state(&self.directory)
    }
    /// How the command lives, as its call declared it.
    pub(in crate::resident) fn kind(&self) -> wire::intent::LaneJobKind {
        use wire::intent::LaneJobKind;
        match (self.until.is_some(), self.timeout.is_some()) {
            (true, true) => LaneJobKind::Watch,
            (true, false) => LaneJobKind::WatchedService,
            (false, true) => LaneJobKind::Command,
            (false, false) => LaneJobKind::Service,
        }
    }

    fn phase(&self, state: Option<&State>) -> io::Result<wire::intent::LaneJobPhase> {
        use wire::intent::LaneJobPhase;
        Ok(if state.is_some_and(|state| state.ending.is_some()) {
            LaneJobPhase::Ended
        } else if self.directory.join(STOP).try_exists()? {
            LaneJobPhase::Stopping
        } else if state.is_some() {
            LaneJobPhase::Running
        } else {
            LaneJobPhase::Starting
        })
    }

    /// Metadata only; listing does not read logs or claim result delivery.
    pub(in crate::resident) fn listing(self) -> io::Result<wire::intent::KeptJob> {
        if !Key::shaped(&self.id) {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "job key is not native base52",
            ));
        }
        let state = self.state()?;
        Ok(wire::intent::KeptJob {
            kind: self.kind(),
            phase: self.phase(state.as_ref())?,
            started_millis: state.as_ref().map(|state| state.started_millis),
            id: self.id,
            command: self.command,
            task: self.task,
        })
    }

    /// Bounded operator evidence without claiming the agent's result delivery.
    pub(in crate::resident) fn inspect(
        self,
        target: wire::intent::LaneJobRef,
    ) -> io::Result<wire::intent::LaneJobAnswer> {
        let state = self.state()?;
        let ending = state
            .as_ref()
            .and_then(|state| state.ending.as_ref())
            .map(|ending| match ending {
                Ending::Exited { code } => format!("Exited {code}"),
                Ending::Signalled { signal } => format!("Signal {signal}"),
                Ending::TimedOut => "Deadline exceeded".to_owned(),
                Ending::Stopped => "Stopped by request".to_owned(),
                Ending::OutputLimit => {
                    format!("Stopped by Spice: output passed {} MiB", OUTPUT_LIMIT_MIB)
                }
                Ending::Failed { reason } => format!("Failed: {reason}"),
                Ending::Lost => "Keeper lost without an ending".to_owned(),
            });
        let output = self.preview(state.as_ref())?;
        Ok(wire::intent::LaneJobAnswer {
            target,
            kind: self.kind(),
            phase: self.phase(state.as_ref())?,
            cwd: self.cwd.to_string_lossy().into_owned(),
            task: self.task,
            started_millis: state.as_ref().map(|state| state.started_millis),
            timeout_millis: self
                .timeout
                .map(|timeout| u64::try_from(timeout.as_millis()))
                .transpose()
                .map_err(io::Error::other)?,
            ended_millis: state.as_ref().and_then(|state| state.ended_millis),
            ending,
            command: self.command,
            output: output.text,
            output_truncated: output.truncated,
        })
    }

    /// Ask the keeper to end the command.
    ///
    /// # Errors
    ///
    /// Returns the failure of writing the stop request.
    pub(in crate::resident) fn stop(&self) -> io::Result<()> {
        File::create(self.directory.join(STOP)).map(drop)
    }

    /// Operator termination leaves the ending eligible for agent notification.
    pub(in crate::resident) fn stop_by_operator(&self) -> io::Result<()> {
        File::create(self.directory.join(OPERATOR_STOP))?;
        self.stop()
    }

    /// Claim the record that the lane has heard how the job ended: in the
    /// answer to the call that ran it, in a job answer that reported its
    /// ending, or as steering once it was kept. The record is created at most
    /// once, so of two tellers racing to it exactly one wins; the loser has
    /// nothing left to say.
    ///
    /// # Errors
    ///
    /// Returns the failure of creating the record other than its existence.
    pub(in crate::resident) fn claim_heard(&self) -> io::Result<bool> {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.directory.join(DELIVERED))
        {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Give back a claim whose telling never reached the lane, so the next
    /// seat's catch-up owes it again.
    ///
    /// # Errors
    ///
    /// Returns the failure of removing the record other than its absence.
    pub(in crate::resident) fn release_heard(&self) -> io::Result<()> {
        match fs::remove_file(self.directory.join(DELIVERED)) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Where the log is written while the job runs.
    pub(super) fn log_path(&self) -> PathBuf {
        self.directory.join(LOG)
    }

    /// Where the retained log lives: its plaintext while that exists,
    /// otherwise its archive.
    fn log_location(&self) -> String {
        let plain = self.log_path();
        let archive = self.directory.join(ARCHIVE);
        if !plain.exists() && archive.exists() {
            format!("{} (seekable Zstandard)", archive.display())
        } else {
            plain.display().to_string()
        }
    }

    /// The last of the log, bounded.
    ///
    /// # Errors
    ///
    /// Returns the failure of reading the log; a log not yet written is empty.
    pub(in crate::resident) fn tail(&self) -> io::Result<Tail> {
        let limit = u64::try_from(LOG_TAIL).map_err(io::Error::other)?;
        let Some((length, reader)) = open_log(&self.directory, Some(limit))? else {
            return Ok(Tail::default());
        };
        let truncated = length > limit;
        let mut bytes = Vec::new();
        reader.take(limit).read_to_end(&mut bytes)?;
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        // Lossy decoding can expand invalid bytes. Keep the displayed UTF-8
        // body within the same byte budget without cutting a code point.
        let mut start = text.len().saturating_sub(LOG_TAIL);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        if start > 0 {
            text.drain(..start);
        }
        Ok(Tail {
            text,
            truncated: truncated || start > 0,
        })
    }

    /// The tail as a report shows it: first what the log alone cannot say
    /// (repeats collapsed, output not retained, signals Spice sent) and where
    /// the whole retained log lives when the tail leaves some behind.
    ///
    /// # Errors
    ///
    /// Returns the failure of reading the log.
    pub(in crate::resident) fn preview(&self, state: Option<&State>) -> io::Result<Tail> {
        let tail = self.tail()?;
        let mut notes = Vec::new();
        if tail.truncated {
            notes.push(format!(
                "[Showing the last {} KiB; full output: {}]",
                LOG_TAIL / 1024,
                self.log_location()
            ));
        }
        if let Some(output) = state.and_then(|state| state.output) {
            if output.collapsed > 0 {
                notes.push(format!(
                    "[Spice collapsed {} bytes of consecutive repeated output; each repeated block \
                     is kept once, followed by its repeat count]",
                    output.collapsed
                ));
            }
            if output.omitted > 0 {
                notes.push(format!(
                    "[Spice kept only the first {} MiB of output; the {} bytes after it were not \
                     retained]",
                    RETAINED_CAP / MIB,
                    output.omitted
                ));
            }
        }
        notes.extend(
            state
                .into_iter()
                .flat_map(|state| &state.interventions)
                .map(Intervention::describe),
        );
        if notes.is_empty() {
            return Ok(tail);
        }
        notes.push(tail.text);
        Ok(Tail {
            text: notes.join("\n"),
            truncated: tail.truncated,
        })
    }

    /// Observe the job until it ends, the call is cancelled or interrupted,
    /// or `bound` elapses.
    ///
    /// # Errors
    ///
    /// Returns the failure of watching the job's state or of reading it.
    pub(in crate::resident) fn observe(
        &self,
        control: &Control,
        bound: Duration,
    ) -> io::Result<Observed> {
        self.observe_with(control, bound, bound, &mut || {})
    }

    /// Observe the job as [`Job::observe`] does, calling `progress` once
    /// the watch is armed, on every look at a running job, and once more on
    /// seeing its ending: the keeper publishes a match before the ending, so
    /// the last call sees it.
    ///
    /// A job's directory need not change when its log grows, so a running job
    /// is also looked at after each `cadence` without a change. The armed
    /// watch persists across those looks: a watch that never fires holds its
    /// thread until this process ends.
    ///
    /// # Errors
    ///
    /// Returns the failure of watching the job's state or of reading it.
    pub(in crate::resident) fn observe_with(
        &self,
        control: &Control,
        bound: Duration,
        cadence: Duration,
        progress: &mut dyn FnMut(),
    ) -> io::Result<Observed> {
        let looked = self.observe_until(control, bound, Looks::Every(cadence), &mut || {
            progress();
            None::<Infallible>
        })?;
        Ok(match looked {
            Looked::Observed(observed) => observed,
            Looked::Found(never) => match never {},
        })
    }

    /// Observe the job's output until `look` finds what it seeks, the job
    /// ends, the call is cancelled or interrupted, or `bound` elapses.
    ///
    /// `look` runs once the watches are armed, after every write to the log,
    /// and once more on seeing the ending, when the log is complete. No look
    /// runs on a timer: a watch on the log itself wakes each one, and a watch
    /// that never fires holds its thread until this process ends.
    ///
    /// # Errors
    ///
    /// Returns the failure of watching the job or of reading its state.
    pub(in crate::resident) fn observe_output<T>(
        &self,
        control: &Control,
        bound: Duration,
        look: &mut dyn FnMut() -> Option<T>,
    ) -> io::Result<Looked<T>> {
        self.observe_until(control, bound, Looks::OnOutput, look)
    }

    fn observe_until<T>(
        &self,
        control: &Control,
        bound: Duration,
        looks: Looks,
        look: &mut dyn FnMut() -> Option<T>,
    ) -> io::Result<Looked<T>> {
        let mut keeper = self.keeper;
        let deadline = Monotonic::now() + bound;
        let mut observed_exit = false;
        let mut armed = false;
        // Only an output observation watches the log; a timed one looks on
        // its cadence instead.
        let mut log_armed = matches!(looks, Looks::Every(_));
        loop {
            if !armed {
                watch(&self.directory, &control.sender, Signal::woke)?;
                armed = true;
            }
            if !log_armed && self.log_path().try_exists()? {
                watch(&self.log_path(), &control.sender, Signal::wrote)?;
                log_armed = true;
            }
            if let Some(state) = self.state()? {
                if let Some(ended) = state.ended() {
                    return Ok(
                        look().map_or(Looked::Observed(Observed::Ended(ended)), Looked::Found)
                    );
                }
                keeper = keeper.or(Some(state.keeper));
            }
            if let Some(found) = look() {
                return Ok(Looked::Found(found));
            }
            if let (Some(pid), false) = (keeper, observed_exit) {
                observed_exit = true;
                if pid != std::process::id() {
                    observer::wait_for_exit(pid, control.sender.clone());
                }
            }
            let remaining = deadline.saturating_duration_since(Monotonic::now());
            let wait = match looks {
                Looks::Every(cadence) => remaining.min(cadence),
                Looks::OnOutput => remaining,
            };
            let observed = match control.receiver.recv_timeout(wait) {
                Ok(Signal::Changed) => {
                    armed = false;
                    continue;
                }
                Ok(Signal::Wrote) => {
                    log_armed = false;
                    continue;
                }
                Ok(Signal::KeeperExited) => {
                    if let Some(found) = look() {
                        return Ok(Looked::Found(found));
                    }
                    Observed::Ended(self.state()?.and_then(|state| state.ended()).unwrap_or(
                        Ended {
                            ending: Ending::Lost,
                            wall: None,
                        },
                    ))
                }
                Ok(Signal::ObserverFailed(reason)) => return Err(io::Error::other(reason)),
                Ok(Signal::Cancel) => Observed::Cancelled,
                Ok(Signal::Interrupt) => Observed::Interrupted,
                Err(mpsc::RecvTimeoutError::Timeout) if wait == remaining => Observed::Elapsed,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        ErrorKind::BrokenPipe,
                        "job observer disconnected",
                    ));
                }
            };
            return Ok(Looked::Observed(observed));
        }
    }

    /// The job's log as lines, each read once and in order.
    pub(in crate::resident) fn log_lines(&self) -> LogLines {
        LogLines::new(self.directory.clone())
    }
}

/// What besides a change to its directory wakes a look at a running job.
#[derive(Clone, Copy)]
enum Looks {
    /// A look after each span without a change.
    Every(Duration),
    /// A look after each write to the job's log.
    OnOutput,
}

/// How an observation that looks for something ended.
#[derive(Debug)]
pub(in crate::resident) enum Looked<T> {
    /// The look found what it sought; the job keeps whatever standing it had.
    Found(T),
    /// The observation ended without it.
    Observed(Observed),
}

/// The retained log from `tail` bytes before its end, or from its start,
/// with its whole length: the plaintext while it exists, else its archive.
/// An archive is published before its plaintext is removed, so a missing
/// plaintext means the archive or no log yet.
fn open_log(directory: &Path, tail: Option<u64>) -> io::Result<Option<(u64, Box<dyn Read>)>> {
    match File::open(directory.join(LOG)) {
        Ok(mut file) => {
            let length = file.metadata()?.len();
            if let Some(tail) = tail.filter(|tail| length > *tail) {
                file.seek(SeekFrom::Start(length - tail))?;
            }
            return Ok(Some((length, Box::new(file))));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match tape::ArchivedFile::open(&directory.join(ARCHIVE)) {
        Ok(archive) => {
            let length = archive.length();
            let offset = tail.map_or(0, |tail| length.saturating_sub(tail));
            Ok(Some((length, Box::new(archive.read_from(offset)?))))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Replace an ended job's plaintext log with its archive. The plaintext is
/// removed only after the published archive was proven to reproduce it; an
/// empty log has nothing to archive and stays.
///
/// # Errors
///
/// Returns the failure of archiving or of removing the plaintext.
fn archive_log(directory: &Path) -> io::Result<()> {
    let plain = directory.join(LOG);
    match fs::metadata(&plain) {
        Ok(metadata) if metadata.len() > 0 => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    tape::archive_file(&plain, &directory.join(ARCHIVE))?;
    match fs::remove_file(&plain) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Archive every ended job's plaintext log beneath `supervisors`: logs kept
/// before keepers archived their own, and those whose keeper ended first.
pub(in crate::resident) fn archive_ended_logs(supervisors: &Path, diagnostic: &mut dyn Write) {
    let Ok(lanes) = fs::read_dir(supervisors) else {
        return;
    };
    for lane in lanes.flatten() {
        let Some(Ok(lane)) = lane.file_name().to_str().map(str::parse::<CanonicalUuid>) else {
            continue;
        };
        let jobs = Jobs::beneath(supervisors, lane);
        let Ok(ids) = jobs.ids(|directory| directory.join(LOG).is_file()) else {
            continue;
        };
        for id in ids {
            let directory = jobs.root.join(&id);
            let ended = read_state(&directory)
                .ok()
                .flatten()
                .is_some_and(|state| state.ending.is_some());
            if ended && let Err(error) = archive_log(&directory) {
                let _ = writeln!(diagnostic, "job {id} log not archived: {error}");
            }
        }
    }
}

/// Bytes of one log line kept for reading; the rest of a longer line, up to
/// its newline, is skipped, so a newline-free log never grows the reader.
const LINE_CAP: usize = 64 * 1024;

/// A job's log read line by line: every read resumes where the last one
/// stopped, so each line is taken once and in order. Only the line still
/// being written is held between reads, clipped to [`LINE_CAP`] bytes.
pub(in crate::resident) struct LogLines {
    directory: PathBuf,
    file: Option<Box<dyn Read>>,
    partial: Vec<u8>,
}

impl LogLines {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            file: None,
            partial: Vec::new(),
        }
    }

    /// Hand each line completed since the last read to `line`, oldest
    /// first, borrowed: a caller copies only the line it keeps. A log not
    /// yet created has no lines.
    ///
    /// # Errors
    ///
    /// Returns the failure of opening or reading the log.
    pub(in crate::resident) fn read(&mut self, line: &mut dyn FnMut(&str)) -> io::Result<()> {
        let file = match self.file.take() {
            Some(file) => file,
            None => match open_log(&self.directory, None)? {
                Some((_, file)) => file,
                None => return Ok(()),
            },
        };
        let file = self.file.insert(file);
        let mut chunk = [0_u8; retention::CHUNK];
        loop {
            match file.read(&mut chunk) {
                Ok(0) => return Ok(()),
                Ok(read) => Self::push(&mut self.partial, &chunk[..read], &mut *line),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn push(partial: &mut Vec<u8>, bytes: &[u8], line: &mut dyn FnMut(&str)) {
        for piece in bytes.split_inclusive(|byte| *byte == b'\n') {
            let (body, ended) = match piece.strip_suffix(b"\n") {
                Some(body) => (body, true),
                None => (piece, false),
            };
            let kept = body.len().min(LINE_CAP.saturating_sub(partial.len()));
            partial.extend_from_slice(&body[..kept]);
            if ended {
                line(&Self::text(partial));
                partial.clear();
            }
        }
    }

    fn text(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(bytes.strip_suffix(b"\r").unwrap_or(bytes))
    }

    /// The unterminated last line, read once the log is complete.
    pub(in crate::resident) fn finish(self) -> Option<String> {
        (!self.partial.is_empty()).then(|| Self::text(&self.partial).into_owned())
    }
}

/// Job records a test writes without a keeper: this process stands as the
/// keeper, so a test says what the command printed and how it ended.
#[cfg(test)]
impl Jobs {
    /// Record one job as its call declared it; `None` timeout is an unbound
    /// service fixture.
    pub(in crate::resident) fn record_for_tests(
        &self,
        command: &str,
        until: Option<&str>,
        timeout_millis: Option<u64>,
    ) -> Job {
        self.record_bound_for_tests(command, until, timeout_millis, None, None, None)
    }

    pub(super) fn record_bound_for_tests(
        &self,
        command: &str,
        until: Option<&str>,
        timeout_millis: Option<u64>,
        task: Option<TaskStamp>,
        service_claim: Option<ServiceClaim>,
        service_turn: Option<CanonicalUuid>,
    ) -> Job {
        fs::create_dir_all(&self.root).expect("job root");
        let (id, directory) = self.claim().expect("job directory");
        publish_json(
            &directory.join(REQUEST),
            &Request {
                version: VERSION,
                id: id.clone(),
                command: command.to_owned(),
                argv: Vec::new(),
                cwd: directory.clone(),
                environment: Environment::empty(),
                timeout_millis,
                notices: Vec::new(),
                task,
                service_claim,
                service_turn,
                lane: Some(self.lane),
                until: until.map(str::to_owned),
            },
        )
        .expect("job request");
        if timeout_millis.is_none() {
            self.services
                .insert(self.lane, &id)
                .expect("service index entry");
        }
        publish_json(
            &directory.join(STATE),
            &State {
                version: VERSION,
                id: id.clone(),
                command: command.to_owned(),
                keeper: std::process::id(),
                started_millis: now_millis().expect("clock"),
                timeout_millis,
                ending: None,
                ended_millis: None,
                output: None,
                interventions: Vec::new(),
            },
        )
        .expect("job state");
        self.open(&id).expect("recorded job opens")
    }
}

#[cfg(test)]
impl Job {
    /// Publish the watcher's match as the keeper would.
    pub(in crate::resident) fn match_for_tests(&self, line: &str) {
        until::publish(&self.directory, line).expect("match");
    }

    /// Publish the job's ending as the keeper would.
    pub(in crate::resident) fn end_for_tests(&self, ending: Ending) {
        let mut state = self.state().expect("state reads").expect("state");
        state.ending = Some(ending);
        state.ended_millis = Some(now_millis().expect("clock"));
        publish_json(&self.directory.join(STATE), &state).expect("ending");
    }
    /// Publish a successful process exit for a lifecycle fixture.
    pub(in crate::resident) fn exit_for_tests(&self) {
        self.end_for_tests(Ending::Exited { code: 0 });
    }

    /// Close the log as the keeper does once the command ended: account
    /// `omitted` bytes past the cap, then archive what was retained.
    pub(in crate::resident) fn close_log_for_tests(&self, omitted: u64) {
        let mut state = self.state().expect("state reads").expect("state");
        state.output = Some(Output {
            omitted,
            ..Output::default()
        });
        publish_json(&self.directory.join(STATE), &state).expect("output");
        archive_log(&self.directory).expect("log archives");
    }
}
