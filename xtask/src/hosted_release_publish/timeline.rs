//! Durable phase evidence for one hosted publication invocation.
//!
//! Each phase names the phases it waits on and why, when it started and
//! finished relative to the invocation, and the provider run it admitted. The
//! critical path follows each phase's latest-finishing dependency back from
//! plan capture, so an idle gap or an avoidable handoff is directly visible.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::PublishError;
use crate::hosted_release_session::NativeTarget;
use crate::windows_hosted::TargetSpan;

const TIMELINE_FILE: &str = "timeline.json";

/// One step of hosted publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Phase {
    Notes,
    Inputs,
    Linux,
    Macos,
    Windows,
    Proofs,
    Plan,
}

impl Phase {
    const fn name(self) -> &'static str {
        match self {
            Self::Notes => "notes",
            Self::Inputs => "inputs",
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
            Self::Proofs => "proofs",
            Self::Plan => "plan",
        }
    }

    /// The phases this one consumes and why it cannot start before them.
    const fn dependency(self) -> (&'static [Self], &'static str) {
        match self {
            Self::Notes => (&[], "curated notes admit against the pinned commit"),
            Self::Inputs => (
                &[],
                "provider preflight admits the pinned source, canvas, and proof image",
            ),
            Self::Linux | Self::Macos | Self::Windows => (
                &[Self::Inputs, Self::Notes],
                "consumes the admitted source archive, canvas, and proof image; refused notes \
                 dispatch no native runner",
            ),
            Self::Proofs => (
                &[Self::Linux, Self::Macos, Self::Windows],
                "the release manifest composes every target payload receipt",
            ),
            Self::Plan => (
                &[Self::Proofs],
                "plan capture joins every admitted output before any external effect",
            ),
        }
    }
}

#[derive(Debug, Serialize)]
struct Span {
    phase: Phase,
    after: &'static [Phase],
    reason: &'static str,
    #[serde(skip)]
    started: SystemTime,
    #[serde(skip)]
    finished: SystemTime,
    start_ms: u64,
    finish_ms: u64,
    idle_ms: u64,
    run: Option<u64>,
}

#[derive(Debug, Serialize)]
struct Evidence<'a> {
    origin_unix_ms: u64,
    phases: &'a [Span],
    critical_path: Vec<Phase>,
    critical_path_ms: u64,
}

/// Phase spans recorded in invocation order.
#[derive(Debug)]
pub(super) struct Timeline {
    origin: SystemTime,
    spans: Vec<Span>,
}

impl Timeline {
    pub(super) fn begin() -> Self {
        Self {
            origin: SystemTime::now(),
            spans: Vec::new(),
        }
    }

    /// Run one local phase and record its span.
    pub(super) fn measure<T>(
        &mut self,
        phase: Phase,
        operation: impl FnOnce() -> Result<T, PublishError>,
    ) -> Result<T, PublishError> {
        let started = SystemTime::now();
        let value = operation()?;
        self.record(phase, started, SystemTime::now(), None);
        Ok(value)
    }

    /// Record one native stage the hosted session admitted.
    pub(super) fn record_target(&mut self, span: TargetSpan) {
        let phase = match span.target {
            NativeTarget::Linux => Phase::Linux,
            NativeTarget::Macos => Phase::Macos,
            NativeTarget::Windows => Phase::Windows,
        };
        self.record(phase, span.started, span.finished, span.run);
    }

    /// Record one phase that ran between `started` and `finished`.
    pub(super) fn record(
        &mut self,
        phase: Phase,
        started: SystemTime,
        finished: SystemTime,
        run: Option<u64>,
    ) {
        let (after, reason) = phase.dependency();
        let ready = self
            .spans
            .iter()
            .filter(|span| after.contains(&span.phase))
            .map(|span| span.finished)
            .max()
            .unwrap_or(self.origin);
        self.spans.push(Span {
            phase,
            after,
            reason,
            started,
            finished,
            start_ms: millis(started.duration_since(self.origin).unwrap_or_default()),
            finish_ms: millis(finished.duration_since(self.origin).unwrap_or_default()),
            idle_ms: millis(started.duration_since(ready).unwrap_or_default()),
            run,
        });
    }

    /// The chain of latest-finishing dependencies ending at the last recorded
    /// phase, with the wall time from the chain's first start to its end.
    fn critical_path(&self) -> (Vec<Phase>, Duration) {
        let Some(mut current) = self.spans.last() else {
            return (Vec::new(), Duration::ZERO);
        };
        let finished = current.finished;
        let mut path = vec![current.phase];
        while let Some(previous) = self
            .spans
            .iter()
            .filter(|span| current.after.contains(&span.phase))
            .max_by_key(|span| span.finished)
        {
            path.push(previous.phase);
            current = previous;
        }
        path.reverse();
        let duration = finished.duration_since(current.started).unwrap_or_default();
        (path, duration)
    }

    /// Persist the evidence beside the publication plan and summarize it.
    pub(super) fn persist(
        &self,
        operation: &Path,
        output: &mut impl Write,
    ) -> Result<(), PublishError> {
        let (critical_path, duration) = self.critical_path();
        let evidence = Evidence {
            origin_unix_ms: millis(self.origin.duration_since(UNIX_EPOCH).unwrap_or_default()),
            phases: &self.spans,
            critical_path: critical_path.clone(),
            critical_path_ms: millis(duration),
        };
        let bytes = serde_json::to_vec_pretty(&evidence).map_err(PublishError::Encode)?;
        let path = operation.join(TIMELINE_FILE);
        super::plan::publish(&path, &bytes)?;
        let mut report = format!(
            "phase timeline {}: critical path {} in {}s\n",
            path.display(),
            critical_path
                .iter()
                .map(|phase| phase.name())
                .collect::<Vec<_>>()
                .join(" -> "),
            duration.as_secs()
        );
        for span in &self.spans {
            let seconds =
                |at: SystemTime| at.duration_since(self.origin).unwrap_or_default().as_secs();
            report.push_str(&format!(
                "  {} {}s..{}s idle {}s{}\n",
                span.phase.name(),
                seconds(span.started),
                seconds(span.finished),
                Duration::from_millis(span.idle_ms).as_secs(),
                span.run
                    .map(|run| format!(" run {run}"))
                    .unwrap_or_default()
            ));
        }
        output
            .write_all(report.as_bytes())
            .map_err(PublishError::Output)
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{Phase, Timeline};

    #[test]
    fn critical_path_follows_the_latest_dependency_and_exposes_idle_gaps() {
        let origin = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let at = |seconds: u64| origin + Duration::from_secs(seconds);
        let mut timeline = Timeline {
            origin,
            spans: Vec::new(),
        };
        timeline.record(Phase::Notes, at(0), at(2), None);
        timeline.record(Phase::Inputs, at(0), at(300), Some(1));
        timeline.record(Phase::Linux, at(300), at(900), Some(2));
        timeline.record(Phase::Macos, at(300), at(2_100), Some(3));
        timeline.record(Phase::Windows, at(360), at(1_500), Some(4));
        timeline.record(Phase::Proofs, at(2_160), at(2_170), None);
        timeline.record(Phase::Plan, at(2_170), at(2_180), None);

        let (path, duration) = timeline.critical_path();
        assert_eq!(
            path,
            [Phase::Inputs, Phase::Macos, Phase::Proofs, Phase::Plan]
        );
        assert_eq!(duration, Duration::from_secs(2_180));

        let idle = |phase: Phase| {
            timeline
                .spans
                .iter()
                .find(|span| span.phase == phase)
                .map(|span| Duration::from_millis(span.idle_ms))
                .expect("phase recorded")
        };
        assert_eq!(idle(Phase::Inputs), Duration::ZERO);
        assert_eq!(idle(Phase::Windows), Duration::from_secs(60));
        assert_eq!(idle(Phase::Proofs), Duration::from_secs(60));
        assert_eq!(idle(Phase::Macos), Duration::ZERO);
    }
}
