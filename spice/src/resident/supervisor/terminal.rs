//! The lane's own terminal: every command the occupant's model composes runs
//! here, in the occupant's environment, kept beyond the call that started it.
//!
//! omp's `bash` is left off the roster and the lane registers a host tool in
//! its place, so a command never runs inside the agent process. The lane
//! runs it in the occupant's native shell, in its working directory, under
//! its launch environment. The lane terminal applies RTK itself and steers
//! the occupant over its own protocol rather than through a hook. Every
//! command is kept as a
//! [`Job`] by a detached keeper, so it survives the turn and the lane can
//! answer a call without ending the command: a command that outruns its
//! foreground window, or that is running when a message arrives for the
//! occupant, is answered with what it has said so far and its ending is
//! delivered when it comes, in omp's own words for both. The model follows
//! kept commands through the `job` tool.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant as Monotonic};

use sill::{Environment, Spec};
use vendor::omp::{JobCall, JobOp, Lifetime, OutputPattern, ShellCall};
use wire::intent::{LaneJob, LaneJobStanding};
use wire::output::{Admission, Budget, ClippedText, Resume};

pub(super) mod job;
#[cfg(test)]
mod tests;

pub(in crate::resident) use job::{
    Control, Handle, Job, Jobs, ServiceAuthority, ServiceClaim, reconcile_services,
    reconcile_turn_services,
};
use job::{Ended, Ending, Looked, Observed, Tail};

/// How long after a stop is asked the lane waits for the keeper's ending
/// before answering with the command still ending: the keeper's grace and
/// the reap that follows it.
const STOP_BOUND: Duration = Duration::from_secs(3);

/// omp's margin under a deadline: the foreground window stops this far
/// short of the deadline, as omp's own does.
const FOREGROUND_BUFFER: Duration = Duration::from_secs(1);

/// How long a follow waits between looks when nothing signals: the job's
/// deadline is the keeper's business, so a follow is bounded only in cadence.
const FOLLOW_CADENCE: Duration = Duration::from_secs(60);

/// How often a finite wait looks at its job's output while nothing signals,
/// and the most often it reports progress. A job's log can grow without any
/// change a platform's directory watch reports, or with a change on every
/// write.
const PROGRESS_CADENCE: Duration = Duration::from_secs(2);

/// omp's own notice when a message backgrounds the running command.
const STEERED: &str =
    "Backgrounded early to handle an incoming message; the command keeps running.";

/// Where and as whom the occupant's commands run, and who keeps them.
#[derive(Clone, Debug)]
pub(in crate::resident) struct Terminal {
    shell: OsString,
    cwd: PathBuf,
    environment: Environment,
    jobs: Jobs,
    service_authority: Option<ServiceAuthority>,
    /// Consecutive elapsed waits and jobs already given the idle hint, shared
    /// by every clone serving this occupant.
    waits: Arc<Mutex<WaitHints>>,
    /// How long a command runs in the foreground before it is kept.
    foreground: Duration,
}

/// What one call said and how it ended, as the tool result carries it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::resident) struct Report {
    pub(in crate::resident) output: String,
    pub(in crate::resident) exit_code: Option<i32>,
    pub(in crate::resident) failed: bool,
}

/// How one command's call was answered.
#[derive(Debug)]
pub(in crate::resident) enum Run {
    /// The command ended within the call.
    Ended(Report),
    /// The call was answered with what the command said so far; the job
    /// keeps running and its ending is still to be delivered.
    Backgrounded(Report, Box<Job>),
}

/// One wait answer and whether its observation bound elapsed first.
struct Waited {
    report: Report,
    elapsed: bool,
}

/// Per-occupant state that keeps an idle hint both deliberate and one-shot.
#[derive(Debug, Default)]
struct WaitHints {
    current: Option<String>,
    elapsed: u8,
    hinted: BTreeSet<String>,
}

impl Terminal {
    /// The terminal that runs what the occupant's own shell tool would have.
    pub(in crate::resident) fn new(
        shell: impl Into<OsString>,
        cwd: impl Into<PathBuf>,
        environment: Environment,
        jobs: Jobs,
    ) -> Self {
        Self {
            shell: shell.into(),
            cwd: cwd.into(),
            environment,
            jobs,
            service_authority: None,
            waits: Arc::new(Mutex::new(WaitHints::default())),
            foreground: Duration::from_secs(vendor::omp::SHELL_FOREGROUND),
        }
    }

    fn with_service_authority(mut self, authority: ServiceAuthority) -> Self {
        self.service_authority = Some(authority);
        self
    }

    /// Whether job admission would seat this terminal's occupant now. A
    /// terminal without admission authority refuses no ordinary job, so its
    /// occupant counts as seated; an unreadable journal does not.
    pub(in crate::resident::supervisor) fn seated(&self) -> bool {
        self.service_authority.as_ref().is_none_or(|authority| {
            authority.seated().unwrap_or_else(|error| {
                eprintln!("spice supervisor: seating unread: {error}");
                false
            })
        })
    }

    /// Apply the occupant's exact command environment to an internal helper.
    pub(in crate::resident::supervisor) fn configure_process(
        &self,
        command: &mut std::process::Command,
    ) {
        command
            .current_dir(&self.cwd)
            .env_clear()
            .envs(self.environment.entries().iter().cloned());
    }

    /// The foreground window for one command: the threshold, or the deadline
    /// less omp's buffer when that is shorter, exactly as omp bounds its own
    /// wait before backgrounding. A service never ends on its own, and a
    /// command with `until` is watched rather than waited on, so neither
    /// opens a window: each is kept at once.
    fn window(&self, call: &ShellCall) -> Duration {
        match call.lifetime {
            _ if call.until.is_some() => Duration::ZERO,
            Lifetime::Deadline(deadline) => self
                .foreground
                .min(deadline.saturating_sub(FOREGROUND_BUFFER)),
            Lifetime::Service => Duration::ZERO,
        }
    }

    /// The occupant's own terminal, running its selected native shell in its
    /// working directory under the launch environment. `SHELL` exists only at
    /// this command boundary, not in the vendor process environment.
    pub(in crate::resident) fn from_launch(
        shell: impl Into<OsString>,
        spec: &Spec,
        jobs: Jobs,
        service_authority: ServiceAuthority,
    ) -> Self {
        let shell = shell.into();
        let environment = Environment::new(
            spec.environment
                .entries()
                .iter()
                .filter(|(name, _)| name != "SHELL")
                .cloned()
                .chain([(OsString::from("SHELL"), shell.clone())]),
        )
        .expect("adding the terminal shell to a valid environment preserves validity");
        Self::new(shell, spec.cwd(), environment, jobs).with_service_authority(service_authority)
    }

    /// Run one command: to its end within the foreground window, or until a
    /// message arrives for the occupant, after which it is kept.
    pub(in crate::resident) fn run(&self, call: &ShellCall, control: &Control) -> Run {
        let mut run = self.run_command(call, control);
        let (Run::Ended(report) | Run::Backgrounded(report, _)) = &mut run;
        for advice in spice::command_advice::for_shell(&call.command, Some(&self.cwd)) {
            append(&mut report.output, advice);
        }
        run
    }

    fn run_command(&self, call: &ShellCall, control: &Control) -> Run {
        let service = matches!(call.lifetime, Lifetime::Service);
        let (task, service) = match &self.service_authority {
            Some(authority) => match authority.admit_job(service) {
                Ok(association) => association,
                Err(error) => {
                    return Run::Ended(Report::refused(&format!("job launch refused: {error}")));
                }
            },
            None if service => {
                return Run::Ended(Report::refused(
                    "service launch requires an active occupant turn",
                ));
            }
            None => (None, None),
        };
        let (argv, diagnostic) = self.argv(call);
        let started = self.jobs.start(
            job::Launch {
                command: &call.command,
                argv,
                cwd: &self.cwd,
                environment: &self.environment,
                timeout: call.lifetime.deadline(),
                notices: &call.notices,
                until: call.until.as_ref().map(OutputPattern::as_str),
            },
            task,
            service,
        );
        let job = match started {
            Ok(job) => job,
            Err(error) => {
                return Run::Ended(Report::refused(&format!("shell launch failed: {error}")));
            }
        };
        let window = self.window(call);
        let ended = match job.observe(control, window) {
            Ok(Observed::Ended(ended)) => ended,
            Ok(Observed::Cancelled) => stop(&job, control).unwrap_or(Ended {
                ending: Ending::Stopped,
                wall: None,
            }),
            Ok(Observed::Interrupted) => {
                return Run::Backgrounded(
                    prefixed(kept(&job, Some(STEERED), false), diagnostic),
                    Box::new(job),
                );
            }
            Ok(Observed::Elapsed) => {
                let promoted = !window.is_zero() && window == self.foreground;
                return Run::Backgrounded(
                    prefixed(kept(&job, None, promoted), diagnostic),
                    Box::new(job),
                );
            }
            Err(error) => Ended {
                ending: Ending::Failed {
                    reason: format!("job observation failed: {error}"),
                },
                wall: None,
            },
        };
        let _heard = job.claim_heard();
        Run::Ended(prefixed(finished(&job, &ended), diagnostic))
    }

    /// The jobs whose endings the lane has not heard: kept commands still
    /// running, and those that ended while nobody was there to be told.
    pub(in crate::resident) fn undelivered(&self) -> Vec<Job> {
        undelivered(&self.jobs)
    }

    /// Follow a kept command to its ending, calling `progress` whenever the
    /// job changes and once more at its ending: what the delivery says, or
    /// nothing for a stopped job, since whoever stopped it was answered and
    /// omp delivers no cancelled job.
    pub(in crate::resident) fn follow(
        &self,
        job: &Job,
        control: &Control,
        progress: &mut dyn FnMut(),
    ) -> Option<Report> {
        loop {
            match job.observe_with(control, FOLLOW_CADENCE, FOLLOW_CADENCE, progress) {
                Ok(Observed::Ended(Ended {
                    ending: Ending::Stopped,
                    ..
                })) => return None,
                Ok(Observed::Ended(ended)) => return Some(finished(job, &ended)),
                Ok(Observed::Cancelled | Observed::Interrupted | Observed::Elapsed) => {}
                Err(error) => {
                    return Some(Report::refused(&format!(
                        "job {} could not be followed: {error}",
                        job.id()
                    )));
                }
            }
        }
    }

    /// Answer one request on the lane's jobs. An answer that reports how a
    /// job ended is the lane hearing it, so its completion notice is not owed.
    ///
    /// A finite wait reports its current output when observation starts and,
    /// at most once per [`PROGRESS_CADENCE`], whenever that output changes,
    /// whether or not the platform reports the log's growth. The caller can
    /// present those snapshots while the final answer remains pending.
    pub(in crate::resident) fn job(
        &self,
        call: &JobCall,
        control: &Control,
        progress: &mut dyn FnMut(&Report),
    ) -> Report {
        // History's id is a cursor into delivered jobs, not a job to open.
        let id = match call.op {
            JobOp::History { .. } => None,
            _ => call.id.as_deref(),
        };
        let job = match id.map(|id| self.jobs.open(id)) {
            Some(Ok(job)) => Some(job),
            Some(Err(error)) => return Report::refused(&error.to_string()),
            None => None,
        };
        match (call.op, job) {
            (JobOp::List, _) => self.list(),
            (JobOp::History { budget }, _) => self.history(budget, call.id.as_deref()),
            (JobOp::Logs, Some(job)) => match job.ended() {
                Some(ended) => told(&job, &ended),
                None => snapshot(&job, None),
            },
            (JobOp::Wait, Some(job)) => {
                let waited = match &call.until {
                    Some(pattern) => wait_until(
                        &job,
                        control,
                        call.timeout,
                        pattern,
                        PROGRESS_CADENCE,
                        progress,
                    ),
                    None => wait(&job, control, call.timeout, PROGRESS_CADENCE, progress),
                };
                self.finish_wait(&job, waited)
            }
            (JobOp::Stop, Some(job)) => match stop(&job, control) {
                Some(ended) => told(&job, &ended),
                None => still(&job, "stop asked"),
            },
            (op, None) => Report::refused(&format!("job {} needs an id", op.name())),
        }
    }

    /// Add the idle hint only after the second consecutive elapsed wait on one
    /// finite job, while this lane still holds a task claim.
    fn finish_wait(&self, job: &Job, mut waited: Waited) -> Report {
        let candidate = self
            .waits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observe(job.id(), waited.elapsed && job.timeout().is_some());
        let claimed = candidate
            && self.service_authority.as_ref().is_some_and(|authority| {
                authority.current_claim().is_ok_and(|claim| claim.is_some())
            });
        let hint = self
            .waits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .mark_hinted(job.id(), claimed);
        if hint {
            append(
                &mut waited.report.output,
                &format!(
                    "If other work remains, keep at it while job {} runs; if waiting is all that \
                     is left, you may end the turn, and the lane wakes you, still holding the \
                     claim, when it ends.",
                    job.id()
                ),
            );
        }
        waited.report
    }

    /// Every job still owed to the lane, oldest first: running, or ended and
    /// not yet heard. Delivered jobs are history's, so a long-lived lane's
    /// listing stays as short as its open work.
    fn list(&self) -> Report {
        let jobs = match self.jobs.kept() {
            Ok(jobs) => jobs,
            Err(error) => return Report::refused(&format!("jobs could not be listed: {error}")),
        };
        let mut output = String::new();
        for job in &jobs {
            let status = status(job.ended().as_ref().map(|ended| &ended.ending));
            output.push_str(&format!("{}  {status}  {}\n", job.id(), job.command()));
        }
        if output.is_empty() {
            output.push_str("no jobs running or awaiting delivery");
        }
        Report::answered(output)
    }

    /// Delivered jobs newest first from `at`, admitted under `budget`; a
    /// page that withholds any names the exact call that continues it.
    fn history(&self, budget: Budget, at: Option<&str>) -> Report {
        let refused = |error: &dyn std::fmt::Display| {
            Report::refused(&format!("job history could not be read: {error}"))
        };
        let ids = match self.jobs.delivered(at) {
            Ok(ids) => ids,
            Err(error) => return refused(&error),
        };
        let mut admission = Admission::new(budget);
        for (index, id) in ids.iter().enumerate() {
            if admission.is_closed() {
                admission.withhold(ids.len() - index);
                break;
            }
            let job = match self.jobs.present(id) {
                Ok(Some(job)) => job,
                Ok(None) => continue,
                Err(error) => return refused(&error),
            };
            let row = HistoryRow {
                status: status(job.ended().as_ref().map(|ended| &ended.ending)),
                command: budget.clip(job.command()),
                id: job.id().to_owned(),
            };
            if let Err(error) = admission.offer(row) {
                return refused(&error);
            }
        }
        let rows = budget.limits().rows;
        let page = admission.finish(|resume| {
            let (Resume::At(next) | Resume::Widened { record: next, .. } | Resume::Export(next)) =
                resume;
            format!("job op=history id={} limit={rows}", next.id)
        });
        let mut output = String::new();
        for row in &page.rows {
            output.push_str(&format!("{}  {}  {}\n", row.id, row.status, row.command));
        }
        match &page.omission {
            Some(omission) => output.push_str(&omission.to_string()),
            None if output.is_empty() => output.push_str("no delivered jobs"),
            None => {}
        }
        Report::answered(output)
    }

    /// The command as the shell runs it, rewritten by RTK when the launch
    /// configured one; a degraded rewrite runs the native command and says
    /// why, as the wrapper does.
    fn argv(&self, call: &ShellCall) -> (Vec<OsString>, Option<String>) {
        let native = vec![
            self.shell.clone(),
            OsString::from("-c"),
            OsString::from(&call.command),
        ];
        let Some(rtk) = self.variable(spice::rtk::EXECUTABLE_VARIABLE) else {
            return (native, None);
        };
        let decision = spice::rtk::rewrite_in(&native, rtk, &self.cwd, &self.environment);
        (
            decision.command().to_vec(),
            decision.diagnostic().map(str::to_owned),
        )
    }

    fn variable(&self, name: &str) -> Option<&OsStr> {
        self.environment
            .entries()
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_os_str())
    }
}

impl WaitHints {
    /// Observe one completed wait. Only an elapsed finite wait advances the
    /// same-job streak; every other wait clears it.
    fn observe(&mut self, job: &str, elapsed: bool) -> bool {
        if !elapsed {
            self.current = None;
            self.elapsed = 0;
            return false;
        }
        if self.current.as_deref() == Some(job) {
            self.elapsed = self.elapsed.saturating_add(1);
        } else {
            self.current = Some(job.to_owned());
            self.elapsed = 1;
        }
        self.elapsed >= 2 && !self.hinted.contains(job)
    }

    /// Claim this job's one hint if its streak still qualifies and the lane
    /// currently holds a task claim.
    fn mark_hinted(&mut self, job: &str, claimed: bool) -> bool {
        if !claimed || self.current.as_deref() != Some(job) || self.elapsed < 2 {
            return false;
        }
        self.elapsed = 0;
        self.hinted.insert(
            self.current
                .take()
                .expect("a qualifying hint retains its job"),
        )
    }
}

/// The jobs a lane has yet to hear the end of, oldest first: still running,
/// or ended while nobody was there.
pub(in crate::resident) fn undelivered(jobs: &Jobs) -> Vec<Job> {
    jobs.kept().unwrap_or_default()
}

/// The answer that tells the lane how a job ended, claiming that telling.
fn told(job: &Job, ended: &Ended) -> Report {
    let _heard = job.claim_heard();
    snapshot(job, Some(ended))
}

/// Report one observed ending only when this wait wins the durable delivery
/// claim. A running wait must not reserve that claim before an ending exists:
/// a resident crash would otherwise leave the later completion falsely marked
/// as delivered.
fn waited_ending(job: &Job, ended: &Ended) -> Report {
    if !job.claim_heard().unwrap_or(true) {
        return Report::answered(format!(
            "[job {}] {}\ncompletion was already claimed for automatic delivery",
            job.id(),
            job.command()
        ));
    }
    snapshot(job, Some(ended))
}

/// What a lane is still waiting on, as its row in the lane table lists it.
pub(in crate::resident) fn unheard(jobs: &Jobs) -> Vec<LaneJob> {
    undelivered(jobs)
        .into_iter()
        .map(|job| LaneJob {
            id: job.id().to_owned(),
            command: job.command().to_owned(),
            task: job.task().cloned(),
            kind: job.kind(),
            standing: if job.ended().is_some() {
                LaneJobStanding::Unheard
            } else {
                LaneJobStanding::Running
            },
        })
        .collect()
}

/// The seat-start notice naming this lane's kept commands that still run, so
/// no seat inherits a forgotten service unawares; `None` when nothing runs.
/// Ended commands are not listed: startup catch-up delivers those.
pub(in crate::resident) fn running_notice(jobs: &Jobs) -> Option<String> {
    let mut rows = String::new();
    for job in undelivered(jobs) {
        if job.ended().is_some() {
            continue;
        }
        let kind = if job.timeout().is_none() {
            "service"
        } else {
            "job"
        };
        rows.push_str(&format!("\n- {kind} {}: {}", job.id(), job.command()));
    }
    (!rows.is_empty()).then(|| {
        format!(
            "<system-notice>\n# Kept jobs still running\n\nThese commands keep running under this lane's keeper; follow them with the `job` tool. A service runs while its task claim remains active; stop it sooner when no longer needed.{rows}\n</system-notice>"
        )
    })
}

/// One delivered job as a history page shows it: its encoded length is what
/// the page's byte limit charges.
#[derive(serde::Serialize)]
struct HistoryRow {
    id: String,
    status: String,
    command: ClippedText,
}

/// One job's standing in a listing.
fn status(ending: Option<&Ending>) -> String {
    match ending {
        None => "running".to_owned(),
        Some(Ending::Exited { code }) => format!("exited {code}"),
        Some(Ending::Signalled { signal }) => format!("signal {signal}"),
        Some(Ending::TimedOut) => "timed out".to_owned(),
        Some(Ending::Stopped) => "stopped".to_owned(),
        Some(Ending::OutputLimit) => "stopped by spice".to_owned(),
        Some(Ending::Failed { .. }) => "failed".to_owned(),
        Some(Ending::Lost) => "lost".to_owned(),
    }
}

impl Report {
    fn answered(output: String) -> Self {
        Self {
            output,
            exit_code: None,
            failed: false,
        }
    }

    fn refused(reason: &str) -> Self {
        Self {
            output: reason.to_owned(),
            exit_code: None,
            failed: true,
        }
    }
}

/// Follow a finite job without changing it. A running service never ends on
/// its own, so waiting on one answers at once with what it has said so far;
/// its task claim's end, or an earlier `stop`, ends it.
///
/// While the job runs, its current output is read at most once per `cadence`
/// and reported whenever it differs from the last report.
fn wait(
    job: &Job,
    control: &Control,
    bound: Duration,
    cadence: Duration,
    progress: &mut dyn FnMut(&Report),
) -> Waited {
    if job.timeout().is_none() && job.ended().is_none() {
        return Waited {
            report: still(job, SERVICE_WAIT),
            elapsed: false,
        };
    }
    let mut previous = String::new();
    let mut looked: Option<Monotonic> = None;
    let mut look = || {
        let now = Monotonic::now();
        if looked.is_some_and(|at| now.duration_since(at) < cadence) {
            return;
        }
        looked = Some(now);
        let report = wait_progress(job);
        if report.output != previous {
            progress(&report);
            previous = report.output;
        }
    };
    let (report, elapsed) = match job.observe_with(control, bound, cadence, &mut look) {
        Ok(Observed::Ended(ended)) => (waited_ending(job, &ended), false),
        Ok(Observed::Elapsed) => (snapshot(job, None), true),
        Ok(Observed::Interrupted) => (still(job, "wait interrupted by an incoming message"), false),
        Ok(Observed::Cancelled) => (still(job, "wait cancelled"), false),
        Err(error) => (
            Report::refused(&format!("job {} could not be observed: {error}", job.id())),
            false,
        ),
    };
    Waited { report, elapsed }
}

/// Why a wait on a running service returned without following it.
const SERVICE_WAIT: &str = "a service runs while its task claim remains active, so wait does \
not follow it to an ending; give wait an until pattern to block until a line of its output \
matches, and stop it sooner once no longer needed";

/// Follow a job's output, service included, until a line matches `pattern`,
/// leaving the job running. Lines written before the wait count, so a
/// service that announced its readiness earlier is answered at once.
///
/// The newest line read is reported as progress at most once per `cadence`;
/// only the log's own growth wakes a look. A verdict that no line matched
/// says so when the log reached its cap, since later output was never kept.
fn wait_until(
    job: &Job,
    control: &Control,
    bound: Duration,
    pattern: &OutputPattern,
    cadence: Duration,
    progress: &mut dyn FnMut(&Report),
) -> Waited {
    let mut lines = job.log_lines();
    let mut reported: Option<Monotonic> = None;
    let mut capped = false;
    let mut latest = String::new();
    let mut look = || {
        let mut matched = None;
        let mut fresh = false;
        if let Err(error) = lines.read(&mut |line| {
            capped |= line == job::CAPPED;
            if matched.is_none() && pattern.is_match(line) {
                matched = Some(line.to_owned());
            } else {
                latest.clear();
                latest.push_str(line);
                fresh = true;
            }
        }) {
            return Some(Err(error));
        }
        let now = Monotonic::now();
        if fresh && matched.is_none() && reported.is_none_or(|at| now.duration_since(at) >= cadence)
        {
            reported = Some(now);
            progress(&Report::answered(format!(
                "[job {}] {}\n{latest}",
                job.id(),
                job.command()
            )));
        }
        matched.map(Ok)
    };
    let looked = job.observe_output(control, bound, &mut look);
    let (report, elapsed) = match looked {
        Ok(Looked::Found(Ok(line))) => (
            match job.ended() {
                Some(ended) => ended_until(job, &ended, pattern, Some(&line)),
                None => Report::answered(format!(
                    "[job {}] {}\n{line}\n\nmatched {}; job {} keeps running",
                    job.id(),
                    job.command(),
                    pattern.as_str(),
                    job.id()
                )),
            },
            false,
        ),
        Ok(Looked::Found(Err(error))) => (
            Report::refused(&format!("job {} log could not be read: {error}", job.id())),
            false,
        ),
        Ok(Looked::Observed(Observed::Ended(ended))) => {
            let last = lines.finish().filter(|line| pattern.is_match(line));
            let mut report = ended_until(job, &ended, pattern, last.as_deref());
            if last.is_none() && capped {
                append(&mut report.output, UNSEARCHED);
            }
            (report, false)
        }
        Ok(Looked::Observed(Observed::Elapsed)) => {
            let mut verdict = format!(
                "no line matched {} within {} seconds",
                pattern.as_str(),
                bound.as_secs()
            );
            if capped {
                verdict = format!("{verdict}; {UNSEARCHED}");
            }
            (still(job, &verdict), true)
        }
        Ok(Looked::Observed(Observed::Interrupted)) => {
            (still(job, "wait interrupted by an incoming message"), false)
        }
        Ok(Looked::Observed(Observed::Cancelled)) => (still(job, "wait cancelled"), false),
        Err(error) => (
            Report::refused(&format!("job {} could not be observed: {error}", job.id())),
            false,
        ),
    };
    Waited { report, elapsed }
}

/// The coverage gap of a search over a log that reached its cap.
const UNSEARCHED: &str = "output after the log reached its cap was not retained, so it was not \
searched";

/// A pattern wait whose job ended: how it ended, then whether its output
/// matched.
fn ended_until(job: &Job, ended: &Ended, pattern: &OutputPattern, line: Option<&str>) -> Report {
    let mut report = waited_ending(job, ended);
    let verdict = match line {
        Some(line) => format!("matched {}: {line}", pattern.as_str()),
        None => format!("no line matched {} before the job ended", pattern.as_str()),
    };
    append(&mut report.output, &verdict);
    report
}

/// Ask the keeper to end the command and wait briefly for its ending.
fn stop(job: &Job, control: &Control) -> Option<Ended> {
    job.stop().ok()?;
    match job.observe(control, STOP_BOUND) {
        Ok(Observed::Ended(ended)) => Some(ended),
        Ok(_) | Err(_) => None,
    }
}

/// The RTK diagnostic ahead of what the command said, as the wrapper puts it.
fn prefixed(mut report: Report, diagnostic: Option<String>) -> Report {
    if let Some(diagnostic) = diagnostic {
        report.output.insert(0, '\n');
        report.output.insert_str(0, &diagnostic);
    }
    report
}

/// A finished command as its result, laid out as omp lays out its own: what
/// it said, the wall time and notices, then how it ended when that was not
/// a clean exit. A nonzero exit is a completed command that failed; a
/// deadline, a stop, a signal, a launch failure, and a lost keeper are each
/// stated as such, and every one of them is an error to the model.
fn finished(job: &Job, ended: &Ended) -> Report {
    let said = transcript(job);
    let said = said.trim_end();
    if ended.ending == Ending::Stopped {
        let output = if said.is_empty() {
            "Command aborted".to_owned()
        } else {
            format!("{said}\n\n[Command aborted]")
        };
        return Report {
            output,
            exit_code: None,
            failed: true,
        };
    }
    let mut lines = vec![if said.is_empty() { "(no output)" } else { said }.to_owned()];
    let wall = ended
        .wall
        .map(|wall| format!("Wall time: {:.2} seconds", wall.as_secs_f64()));
    let unsearched = job
        .until()
        .filter(|_| job.watch_unsearched())
        .map(|pattern| format!("no line matched {pattern}; {UNSEARCHED}"));
    let notices: Vec<String> = wall
        .into_iter()
        .chain(job.notices().iter().cloned())
        .chain(unsearched)
        .collect();
    if !notices.is_empty() {
        lines.push(String::new());
        lines.extend(notices);
    }
    let (exit_code, footer) = footer(&ended.ending, job.timeout());
    let failed = footer.is_some();
    if let Some(footer) = footer {
        lines.push(String::new());
        lines.push(footer);
    }
    Report {
        output: lines.join("\n"),
        exit_code,
        failed,
    }
}

/// A command answered while it keeps running, as omp answers its own
/// backgrounded `bash`: the preview, the notices, and the job it became. A
/// watched command shows no preview, since its matching line is delivered
/// on its own.
fn kept(job: &Job, notice: Option<&str>, promoted: bool) -> Report {
    let mut lines = Vec::new();
    let preview = if job.until().is_some() {
        String::new()
    } else {
        transcript(job)
    };
    let preview = preview.trim_end();
    if !preview.is_empty() {
        lines.push(preview.to_owned());
        lines.push(String::new());
    }
    let notices: Vec<&str> = job
        .notices()
        .iter()
        .map(String::as_str)
        .chain(notice)
        .collect();
    if !notices.is_empty() {
        lines.extend(notices.iter().map(|notice| (*notice).to_owned()));
        lines.push(String::new());
    }
    lines.push(match job.until() {
        Some(pattern) if job.timeout().is_some() => format!(
            "Watching job {} for a line matching {pattern}: the first such line will be \
             delivered as a background notice, and its result when it ends.",
            job.id()
        ),
        Some(pattern) => format!(
            "Watching service {} for a line matching {pattern}: the first such line will be \
             delivered as a background notice while you are here; it never wakes the lane.",
            job.id()
        ),
        None => {
            let parallel = if promoted {
                " Keep working in parallel while it runs; its result arrives on its own."
            } else {
                ""
            };
            format!(
                "Backgrounded as job {}; result will be delivered automatically.{parallel}",
                job.id()
            )
        }
    });
    Report {
        output: lines.join("\n"),
        exit_code: None,
        failed: false,
    }
}

/// Current output of the job one in-flight wait follows.
///
/// The progress frame itself states that the call remains active. Only the
/// latest bounded line rides repeated frames: a cumulative tail would copy the
/// same growing output on every keeper wake.
fn wait_progress(job: &Job) -> Report {
    const LATEST_BYTES: usize = 2 * 1024;

    let mut output = format!("[job {}] {}\n", job.id(), job.command());
    let tail = job.tail().unwrap_or_else(|error| Tail {
        text: format!("log unreadable: {error}"),
        truncated: false,
    });
    let latest = tail
        .text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("(no output yet)");
    let mut start = latest.len().saturating_sub(LATEST_BYTES);
    while !latest.is_char_boundary(start) {
        start += 1;
    }
    output.push_str(&latest[start..]);
    Report::answered(output)
}

/// One job as the `job` tool reads it: named, then its result so far.
fn snapshot(job: &Job, ended: Option<&Ended>) -> Report {
    let mut output = format!("[job {}] {}\n", job.id(), job.command());
    let exit_code = match ended {
        Some(ended) => {
            let finished = finished(job, ended);
            output.push_str(&finished.output);
            finished.exit_code
        }
        None => {
            let mut said = transcript(job);
            append(&mut said, "Command still running");
            output.push_str(&said);
            None
        }
    };
    Report {
        output,
        exit_code,
        failed: false,
    }
}

/// A job op that returned without the job ending.
fn still(job: &Job, why: &str) -> Report {
    let mut report = snapshot(job, None);
    append(
        &mut report.output,
        &format!("{why}; job {} keeps running", job.id()),
    );
    report
}

/// A bounded log preview, led by what Spice did to the output.
fn transcript(job: &Job) -> String {
    let state = job.state().ok().flatten();
    job.preview(state.as_ref())
        .map_or_else(|error| format!("log unreadable: {error}"), |tail| tail.text)
}

/// The exit code a result carries and the footer that states an ending
/// other than a clean exit, in omp's own words where it has them.
fn footer(ending: &Ending, timeout: Option<Duration>) -> (Option<i32>, Option<String>) {
    match ending {
        Ending::Exited { code: 0 } => (Some(0), None),
        Ending::Exited { code } => (
            Some(*code),
            Some(format!("Command exited with code {code}")),
        ),
        Ending::Signalled { signal } => (None, Some(format!("Command ended by signal {signal}"))),
        Ending::TimedOut => (
            None,
            Some(match timeout {
                Some(timeout) => format!("[Command timed out after {} seconds]", timeout.as_secs()),
                None => "[Command timed out]".to_owned(),
            }),
        ),
        Ending::Stopped => (None, Some("[Command aborted]".to_owned())),
        Ending::OutputLimit => (
            None,
            Some(format!(
                "[Spice stopped the command: its output passed {} MiB]",
                job::OUTPUT_LIMIT_MIB
            )),
        ),
        Ending::Failed { reason } => (None, Some(format!("Command failed to run: {reason}"))),
        Ending::Lost => (
            None,
            Some("Command lost: its keeper ended without reporting how it ended".to_owned()),
        ),
    }
}

/// A notice after the output, set off by a blank line.
fn append(output: &mut String, notice: &str) {
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(notice);
}
