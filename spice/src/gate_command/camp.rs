//! Decision-first output for the long shared-bound audit, `gate camp`.

use std::sync::Mutex;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use gate::{CampProgress, CampReport, CampStanding, Selector};
use serde::Serialize;
use wire::Outcome;
use wire::output::{Export, clip};

use super::output::{self, Elapsed, Progress, Summary};
use super::{Failure, SelectedContract, direct};

/// Camping loci shown by default and under `--verbose`.
const SHOWN_LOCI: usize = 20;
const VERBOSE_LOCI: usize = 200;
/// Bytes of one locus name a line carries.
const NAME_BYTES: usize = 160;
/// How often the heartbeat thread asks whether a quiet interval has passed.
const TICK: Duration = Duration::from_secs(1);

/// `gate camp`'s typed report and its decision-first text.
#[derive(Serialize)]
pub(crate) struct CampAnswer {
    #[serde(flatten)]
    report: CampReport,
    #[serde(skip)]
    text: String,
}

impl CampAnswer {
    pub(crate) fn text(&self) -> Option<&str> {
        Some(&self.text)
    }
}

/// Audit the compiled shared bound without changing a tracked floor.
pub(crate) fn camp(matches: &clap::ArgMatches, live: bool) -> Outcome<CampAnswer> {
    let verbose = matches.get_flag("verbose");
    let selected = match SelectedContract::capture(Selector::staged()) {
        Ok(selected) => selected,
        Err(failure) => return failure.outcome(),
    };
    let progress = Progress::stderr("gate camp", live);
    let started = Instant::now();
    let campaign = Mutex::new(CampCampaign::new(output::HEARTBEAT, started));
    let audit = beating(&progress, &campaign, CampCampaign::heartbeat, || {
        selected
            .contract
            .audit_camp_with_progress(selected.root(), &mut |observation| {
                let line = campaign
                    .lock()
                    .ok()
                    .and_then(|mut campaign| campaign.observe(observation, Instant::now()));
                if let Some(line) = line {
                    progress.line(format_args!("{line}"));
                }
            })
    });
    let report = match audit {
        Ok(report) => report,
        Err(error) => return refused("gate::camp-refused", selected.captures(), &error),
    };
    let retained = output::retain(selected.captures(), report.render().as_bytes());
    let text = camp_summary(&report, started.elapsed(), &retained, verbose);
    direct(CampAnswer { report, text })
}

/// A run that ended before a report: its bounded tail and retained text.
fn refused<B>(
    reason: &'static str,
    captures: &output::Captures,
    error: &dyn std::fmt::Display,
) -> Outcome<B> {
    Failure::Refusal {
        reason,
        expected: "one complete bounded current gate result".to_owned(),
        observed: output::failure_evidence(captures, error.to_string().as_bytes()),
        detail: "restore the declared inputs and tools, then run the gate again".to_owned(),
    }
    .outcome()
}

/// Run `work` while a thread writes each heartbeat the campaign says is due.
fn beating<T, C: Send>(
    progress: &Progress,
    campaign: &Mutex<C>,
    heartbeat: fn(&mut C, Instant) -> Option<String>,
    work: impl FnOnce() -> T,
) -> T {
    let (stop, stopped) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            while matches!(stopped.recv_timeout(TICK), Err(RecvTimeoutError::Timeout)) {
                let line = campaign
                    .lock()
                    .ok()
                    .and_then(|mut campaign| heartbeat(&mut campaign, Instant::now()));
                if let Some(line) = line {
                    progress.line(format_args!("{line}"));
                }
            }
        });
        let value = work();
        drop(stop);
        value
    })
}

/// Live state of one camping campaign.
struct CampCampaign {
    interval: Duration,
    started: Instant,
    quiet_since: Instant,
    total: Option<usize>,
    completed: usize,
    running: Option<String>,
}

impl CampCampaign {
    const fn new(interval: Duration, now: Instant) -> Self {
        Self {
            interval,
            started: now,
            quiet_since: now,
            total: None,
            completed: 0,
            running: None,
        }
    }

    fn observe(&mut self, observation: CampProgress<'_>, now: Instant) -> Option<String> {
        self.total = Some(observation.total);
        let line = if let Some(metric) = observation.metric {
            if observation.completed > self.completed {
                self.running = None;
            } else {
                self.running = Some(clip(metric, NAME_BYTES).to_string());
            }
            self.completed = observation.completed;
            None
        } else {
            self.completed = observation.completed;
            Some(format!(
                "campaign: {} distributions, {} loci, {} measurements",
                observation.distributions, observation.loci, observation.total
            ))
        };
        if line.is_some() {
            self.quiet_since = now;
        }
        line
    }

    fn heartbeat(&mut self, now: Instant) -> Option<String> {
        if now.saturating_duration_since(self.quiet_since) < self.interval {
            return None;
        }
        self.quiet_since = now;
        let elapsed = Elapsed(now.saturating_duration_since(self.started));
        Some(match (self.total, &self.running) {
            (None, _) => format!("campaign preparation running {elapsed}"),
            (Some(total), Some(metric)) => {
                format!(
                    "{}/{total} measured; {metric} running {elapsed}",
                    self.completed
                )
            }
            (Some(total), None) => {
                format!("{}/{total} measured; auditing {elapsed}", self.completed)
            }
        })
    }
}

fn retained_line(summary: &mut Summary, retained: &Result<Export, String>) {
    match retained {
        Ok(export) => summary.closing(
            "report",
            &format!("{} ({} bytes)", export.path, export.bytes),
        ),
        Err(error) => summary.closing("report", &format!("not retained: {error}")),
    }
}

/// The ordered decisions of one camping audit.
fn camp_summary(
    report: &CampReport,
    elapsed: Duration,
    retained: &Result<Export, String>,
    verbose: bool,
) -> String {
    let camped = report
        .distributions
        .iter()
        .filter(|distribution| distribution.standing == CampStanding::Camped)
        .count();
    let mut summary = Summary::new(format_args!(
        "gate camp: {} ({} distributions, {camped} camped)",
        report.standing,
        report.distributions.len()
    ));
    let limit = if verbose { VERBOSE_LOCI } else { SHOWN_LOCI };
    let mut shown = 0;
    let mut withheld = 0;
    for finding in &report.findings {
        let near = finding.loci.len();
        summary.row(
            "camped",
            &format!(
                "{}: {near} of {} loci near limit {}",
                clip(&finding.bound, NAME_BYTES),
                finding.population,
                finding.limit
            ),
            (shown == 0 && withheld == 0).then_some(elapsed),
        );
        for locus in &finding.loci {
            if shown == limit {
                withheld += 1;
                continue;
            }
            shown += 1;
            summary.detail(format_args!(
                "{} measured {} {} distance {}/{}",
                clip(&locus.locus, NAME_BYTES),
                locus.measured,
                locus.relation,
                locus.delta,
                locus.denominator
            ));
        }
    }
    if withheld > 0 {
        let more = if verbose {
            "the retained report"
        } else {
            "spice gate camp --verbose"
        };
        summary.detail(format_args!(
            "{withheld} of {} near loci withheld; see {more}",
            shown + withheld
        ));
    }
    let clean = report.distributions.len() - camped;
    summary.row(
        "clean",
        &format!("{clean} distributions clear of their limits"),
        report.findings.is_empty().then_some(elapsed),
    );
    retained_line(&mut summary, retained);
    summary.closing(
        "next",
        if camped == 0 {
            "git commit"
        } else {
            "spread the listed loci away from their shared limit, git add, then spice gate camp"
        },
    );
    summary.finish()
}

#[cfg(test)]
mod tests;
