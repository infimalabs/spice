//! Worker lifetime, including withdrawn tools whose work has not returned.

use listen::{Decoded, TurnMoment};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError, mpsc};

use vendor::omp::{JobCall, JobOp, ShellCall};

use super::super::terminal::{Control, Handle, Report, Run, Terminal};
use super::Driver;
/// What one host tool call asks of the lane.
pub(super) enum Work {
    Shell(ShellCall),
    Job(JobCall),
}

impl Work {
    fn steering_blocking(&self) -> bool {
        matches!(
            self,
            Self::Shell(_)
                | Self::Job(JobCall {
                    op: JobOp::Wait,
                    ..
                })
        )
    }
}

#[derive(Default)]
pub(super) struct StepState {
    /// Every human directive that successfully invalidated this provider step,
    /// and whether the latest authoritative/local state still owes it.
    triggered: BTreeMap<String, bool>,
    replayed: bool,
}

impl StepState {
    pub(super) fn started(&mut self) {
        self.triggered.clear();
        self.replayed = false;
    }

    pub(super) fn mark_stale<'a>(&mut self, keys: impl IntoIterator<Item = &'a str>) {
        for key in keys {
            self.triggered.insert(key.to_owned(), true);
        }
    }

    pub(super) fn retire<'a>(&mut self, keys: impl IntoIterator<Item = &'a str>) {
        for key in keys {
            if let Some(owed) = self.triggered.get_mut(key) {
                *owed = false;
            }
        }
    }

    pub(super) fn reconcile(&mut self, mut still_owed: impl FnMut(&str) -> bool) {
        for (key, owed) in &mut self.triggered {
            *owed = still_owed(key);
        }
    }

    pub(super) fn is_stale(&self) -> bool {
        self.triggered.values().any(|owed| *owed)
    }

    pub(super) fn owes(&self, key: &str) -> bool {
        self.triggered.get(key).is_some_and(|owed| *owed)
    }

    pub(super) fn claim_replay(&mut self) -> bool {
        if !self.is_stale() || self.replayed {
            return false;
        }
        self.replayed = true;
        true
    }

    pub(super) fn release_replay(&mut self) {
        self.replayed = false;
    }
}

#[derive(Clone)]
pub(super) struct ActiveCall {
    pub(super) handle: Handle,
    pub(super) blocking_wait: bool,
    /// Dropping the registered call releases its bound watcher.
    _bound: mpsc::Sender<()>,
}

impl ActiveCall {
    pub(super) fn new(handle: Handle, work: &Work) -> (Self, mpsc::Receiver<()>) {
        let (bound, released) = mpsc::channel();
        (
            Self {
                handle,
                blocking_wait: matches!(work, Work::Job(call) if call.op == JobOp::Wait),
                _bound: bound,
            },
            released,
        )
    }

    #[cfg(test)]
    pub(super) fn fixture(handle: Handle, blocking_wait: bool) -> Self {
        let (bound, _) = mpsc::channel();
        Self {
            handle,
            blocking_wait,
            _bound: bound,
        }
    }
}

pub(super) struct Worker<W: Write + Send> {
    pub(super) driver: Arc<Driver<W>>,
}

impl<W: Write + Send> Drop for Worker<W> {
    fn drop(&mut self) {
        self.driver.workers.fetch_sub(1, Ordering::AcqRel);
    }
}

impl<W: Write + Send + 'static> Driver<W> {
    /// The guard belongs to the spawned closure; failed spawning drops it too.
    /// Withdrawing a call does not prematurely end its worker's lifetime.
    pub(super) fn worker(self: &Arc<Self>) -> Worker<W> {
        self.workers.fetch_add(1, Ordering::AcqRel);
        Worker {
            driver: Arc::clone(self),
        }
    }
    /// Advance only at OMP's provider-step boundary, which is also where
    /// refusals an idle occupant could not take are queued.
    pub(super) fn observe_step(&self, decoded: &Decoded) {
        if decoded
            .context()
            .is_some_and(|context| context.turn == Some(TurnMoment::Started))
        {
            self.step
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .started();
            self.release_quiet();
        }
    }

    /// Answer one host tool call: a command run in the lane's terminal, or a
    /// request on the lane's jobs.
    ///
    /// The work runs on its own thread, so the stream the call arrived on
    /// keeps being read: the withdrawal that ends the call early and the
    /// message that interrupts it arrive there. A tool the lane never
    /// registered, arguments that name no command, or an occupant with no
    /// terminal are refused at once.
    pub(super) fn run(
        self: &Arc<Self>,
        id: &str,
        tool: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) {
        let work = match tool {
            vendor::omp::SHELL_TOOL => ShellCall::parse(arguments).map(Work::Shell),
            vendor::omp::JOB_TOOL => JobCall::parse(arguments).map(Work::Job),
            _ => Err(format!("{tool} is not a tool this lane registered")),
        };
        let (terminal, work) = match (self.terminal.clone(), work) {
            (Some(terminal), Ok(work)) => (terminal, work),
            (_, work) => {
                let reason = work
                    .err()
                    .unwrap_or_else(|| "this lane has no terminal".to_owned());
                let _refused = self.write(&vendor::omp::host_tool_refused(id, &reason));
                return;
            }
        };
        let control = Control::new();
        let (active, released) = ActiveCall::new(control.handle(), &work);
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned(), active);
        if let Err(error) =
            self.watch_tool_bound(id.to_owned(), released, crate::cli::APP_ANSWER_BOUND)
        {
            self.cancel(id);
            let _refused = self.write(&vendor::omp::host_tool_refused(
                id,
                &format!("tool-bound watcher failed: {error}"),
            ));
            return;
        }
        // Interject scans registered calls while holding the same step lock it
        // uses to publish fresh steering. Register first, then recheck: either
        // that scan sees this call, or this check sees its stale generation.
        let replay_inbox = self.preempt_stale(&work, &control);
        let worker = self.worker();
        let answered = id.to_owned();
        let started = std::thread::Builder::new()
            .name("spice-shell".to_owned())
            .spawn(move || {
                worker
                    .driver
                    .resolve(&terminal, &answered, &work, &control, replay_inbox);
            });
        if let Err(error) = started {
            self.cancel(id);
            let _refused = self.write(&vendor::omp::host_tool_refused(
                id,
                &format!("shell thread failed: {error}"),
            ));
        }
    }

    pub(super) fn watch_tool_bound(
        self: &Arc<Self>,
        id: String,
        released: mpsc::Receiver<()>,
        horizon: std::time::Duration,
    ) -> std::io::Result<std::thread::JoinHandle<()>> {
        let driver = Arc::clone(self);
        std::thread::Builder::new()
            .name("spice-tool-bound".to_owned())
            .spawn(move || {
                if matches!(
                    released.recv_timeout(horizon),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) && driver
                    .calls
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .contains_key(&id)
                {
                    driver.deliver_inbox();
                }
            })
    }

    pub(super) fn preempt_stale(&self, work: &Work, control: &Control) -> bool {
        let replay_inbox = work.steering_blocking()
            && self
                .step
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_stale();
        if replay_inbox {
            control.handle().interrupt();
        }
        replay_inbox
    }

    /// Answer one call with what its work said, then deliver the ending of a
    /// command the answer left running.
    pub(super) fn resolve(
        &self,
        terminal: &Terminal,
        id: &str,
        work: &Work,
        control: &Control,
        replay_inbox: bool,
    ) {
        let (mut report, kept) = match work {
            Work::Shell(call) => match terminal.run(call, control) {
                Run::Ended(report) => (report, None),
                Run::Backgrounded(report, job) => (report, Some(*job)),
            },
            Work::Job(call) => {
                let mut progress = |report: &Report| self.report_wait(id, report);
                (terminal.job(call, control, &mut progress), None)
            }
        };
        if replay_inbox {
            if !report.output.ends_with('\n') {
                report.output.push('\n');
            }
            report
                .output
                .push_str("Newer steering arrived after this model step began; its inbox follows.");
        }
        self.end_wait(id);
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        let answered = self.write(&vendor::omp::host_tool_result(
            id,
            &report.output,
            report.exit_code,
            report.failed,
        ));
        if replay_inbox && answered.is_ok() {
            self.replay_inbox();
        }
        if let Some(job) = kept {
            self.hear(terminal, &job, control);
        }
    }

    /// Capture is already joined, so no further tool calls can be admitted.
    /// Closing input also prevents kicks from reactivating this dead occupant.
    pub(in crate::resident::supervisor) fn quiescent(&self) -> bool {
        self.close();
        self.workers.load(Ordering::Acquire) == 0
            && self
                .asks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
            && self
                .retunes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
    }
}
