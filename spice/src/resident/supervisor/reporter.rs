//! The supervisor's best-effort notice door to whichever resident is present.
//!
//! The lane observer commits every record to tape before this reporter hears
//! of it, so a notice that never arrives costs the resident nothing durable.
//! A child steered over its stdin also shows every decoded record to its
//! command driver here, since this is the one place the supervisor sees them.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lane::capture::{
    CaptureExit, CaptureFailure, CaptureRecord, CaptureReportError, CaptureReporter,
};

use super::super::RuntimePaths;
use super::{Notice, capture_path, command};

pub(super) struct DurableReporter {
    sender: door::Sender,
    path: PathBuf,
    driver: Mutex<Option<Arc<command::Driver>>>,
    driving: AtomicBool,
}

impl DurableReporter {
    pub(super) fn new(paths: &RuntimePaths) -> io::Result<Self> {
        Ok(Self {
            sender: door::Sender::new()?,
            path: capture_path(paths),
            driver: Mutex::new(None),
            driving: AtomicBool::new(false),
        })
    }

    /// Let every decoded record reach the command driver of a child that is
    /// steered over its stdin, so turn ends and compactions shape delivery.
    pub(super) fn drive(&self, driver: Arc<command::Driver>) {
        *self
            .driver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(driver);
    }

    /// Called only after SILL joined every selected capture delivery.
    pub(super) fn quiescent(&self) -> bool {
        self.driver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_none_or(|driver| driver.quiescent())
    }

    pub(super) fn send(&self, notice: &Notice) {
        let Ok(bytes) = serde_json::to_vec(notice) else {
            return;
        };
        let _sent = self.sender.send(&self.path, &bytes);
    }
}

impl CaptureReporter for DurableReporter {
    fn record(&self, record: CaptureRecord) -> Result<(), CaptureReportError> {
        self.driving.store(true, Ordering::Relaxed);
        let raised = self
            .driver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|driver| driver.observe_capture(&record));
        self.driving.store(false, Ordering::Relaxed);
        // The lane observer committed the bytes to tape before this callback.
        // Resident notification is best effort; durability never depends on a
        // resident generation being present.
        self.send(&Notice::Record {
            lane: record.lane,
            occupant: record.occupant,
            stream: match record.stream {
                vendor::RecordStream::Stdout => "stdout",
                vendor::RecordStream::Stderr => "stderr",
            }
            .to_owned(),
            boundary: match record.boundary {
                sill::RecordBoundary::Complete => "complete",
                sill::RecordBoundary::Terminal => "terminal",
                sill::RecordBoundary::Truncated => "truncated",
            }
            .to_owned(),
            locus: wire::TapeLocus {
                member: record.locus.member,
                offset: record.locus.offset,
                length: record.locus.length,
            },
        });
        match raised {
            Some(command::Raised::Ask(ask)) => self.send(&Notice::Asked {
                lane: record.lane,
                occupant: record.occupant,
                request: ask.request,
                operation: ask.operation,
            }),
            Some(command::Raised::FastRefused(refusal)) => self.send(&Notice::FastRefused {
                lane: record.lane,
                occupant: record.occupant,
                refusal,
            }),
            None => {}
        }
        Ok(())
    }

    fn stage(&self) -> &'static str {
        if self.driving.load(Ordering::Relaxed) {
            "driver"
        } else {
            "notice"
        }
    }

    fn failed(&self, _failure: CaptureFailure) {}

    fn exited(&self, _exit: CaptureExit) {
        // SILL reports capture completion before `Running::wait` publishes the
        // exact exit status. `run_agent` emits this notice only after the final
        // supervisor state is durable, so a resident never blocks on the race.
    }
}
