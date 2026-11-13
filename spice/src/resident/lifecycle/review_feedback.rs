//! Durable peer-review feedback delivered to the phase author's vendor session.
//!
//! A finding reaches the lane seat that continues the session which completed
//! the judged phase: the same occupant, or a later occupant that resumed the
//! same vendor session after a seat boundary or compaction. While the author
//! lane is between seats the finding is held and judged again on the next
//! reconcile; a renewal that starts another session never receives it.

use std::collections::BTreeSet;
use std::time::Duration;

use bus::{AuthorshipFault, ClaimAuthor, FindingAuthors};
use journal::FactCause;
use wire::rooms::DesignSession;
use wire::{CanonicalUuid, Instant, LaneResidency, Seq, TaskStamp};

use super::*;

struct Feedback {
    reviewed: Seq,
    task: TaskStamp,
    lane: CanonicalUuid,
    text: String,
    residency: journal::JournalHead<wire::ResidencyHead>,
}

/// What one finding's author can receive at this reconcile.
enum Judgment {
    /// A seat continuing the author's session is live.
    Deliver(Feedback),
    /// The author lane is between seats; judge the finding again later.
    Hold,
    /// Never deliverable: judged by its own author's session, continued by no
    /// seat because another session holds the lane, or past its lifetime.
    Drop,
}

/// Which findings one reconcile judges, and the clock and directive lifetime
/// it judges them by.
pub(super) struct FindingWindow<'a> {
    /// Lifecycle projection cursor; every finding after it is new.
    pub(super) cursor: Option<Seq>,
    /// Findings still waiting for a seat; replaced by those that still wait.
    pub(super) held: &'a mut BTreeSet<Seq>,
    pub(super) now: Instant,
    pub(super) lifetime: Duration,
}

/// Deliver every finding after the window's cursor or held for a seat whose
/// author session is seated, and leave held exactly the findings still
/// waiting for a seat. Each finding's authors come from the snapshot's
/// authorship index, so no reconcile walks task history.
pub(super) fn reconcile(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    snapshot: &bus::TaskSnapshot,
    window: FindingWindow<'_>,
) -> CommandResult<bool> {
    let FindingWindow {
        cursor,
        held,
        now,
        lifetime,
    } = window;
    let mut refreshed = None;
    let mut changed = false;
    loop {
        let observed = refreshed.as_ref().unwrap_or(snapshot);
        let mut findings = new_findings(observed.facts(), cursor)?;
        findings.extend(held.iter().copied());
        let mut waiting = BTreeSet::new();
        let mut appended = false;
        for seq in &findings {
            let envelope = observed.facts().at(*seq).ok_or_else(|| {
                store_fault(
                    "review-feedback::finding-missing",
                    "a finding has no fact in the snapshot it was judged from",
                )
            })?;
            let Some(reviewed) = envelope.fact.read::<wire::body::TaskReviewed>() else {
                continue;
            };
            let FindingAuthors { authors, reviewer } = finding_authors(observed.facts(), *seq)?;
            let mut held_author = false;
            for author in authors {
                let feedback = match judge(
                    observed, envelope, &reviewed, author, reviewer, now, lifetime,
                )? {
                    Judgment::Deliver(feedback) => feedback,
                    Judgment::Hold => {
                        held_author = true;
                        continue;
                    }
                    Judgment::Drop => continue,
                };
                let control = wire::review_feedback_control(&feedback.task, feedback.reviewed);
                let directive = super::super::steering::internal_directive_identity(&format!(
                    "lane:{}:{control}",
                    feedback.lane
                ));
                if super::super::steering::control_recorded(
                    bus,
                    directive,
                    feedback.lane,
                    &control,
                )? {
                    continue;
                }
                let mut proposal = super::super::steering::internal_write(epoch)?;
                guard_residency(&mut proposal, &feedback.residency);
                super::super::steering::append_internal_directive(
                    bus,
                    &mut proposal,
                    super::super::steering::InternalDirective {
                        directive,
                        lane: feedback.lane,
                        text: feedback.text,
                        control,
                        lifetime,
                        cause: FactCause::Root,
                    },
                )?;
                match commit(bus, proposal) {
                    Ok(_) => {
                        super::super::steering::notify_inbox(paths, feedback.lane);
                        changed = true;
                    }
                    Err(Outcome::Refusal(refusal))
                        if refusal.reason() == "resident::head-moved" => {}
                    Err(outcome) => return Err(outcome),
                }
                appended = true;
                break;
            }
            if held_author {
                waiting.insert(*seq);
            }
            if appended {
                break;
            }
        }
        if !appended {
            *held = waiting;
            return Ok(changed);
        }
        refreshed = Some(bus.task_snapshot().map_err(|error| {
            store_fault("review-feedback::snapshot-unreadable", &error.to_string())
        })?);
    }
}

fn judge(
    snapshot: &bus::TaskSnapshot,
    envelope: &wire::Envelope<wire::AnyFact>,
    reviewed: &wire::body::TaskReviewed,
    author: ClaimAuthor,
    reviewer: ClaimAuthor,
    now: Instant,
    lifetime: Duration,
) -> CommandResult<Judgment> {
    let finding = reviewed
        .finding
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| {
            store_fault(
                "review-feedback::finding-missing",
                "a finding review carries no final disposition",
            )
        })?;
    let author_session = snapshot.facts().lane_session(author.lane, author.occupant);
    if same_session(snapshot, author, author_session, reviewer) {
        return Ok(Judgment::Drop);
    }
    if outlived(envelope.at, now, lifetime) {
        return Ok(Judgment::Drop);
    }
    let Some(residency) = snapshot.residency().cloned() else {
        return Ok(Judgment::Hold);
    };
    let Some(row) = residency
        .body()
        .lanes
        .iter()
        .find(|lane| lane.lane == author.lane)
    else {
        return Ok(Judgment::Drop);
    };
    let seated = row.disposition == "occupied"
        && row
            .session
            .as_ref()
            .is_some_and(|session| row.occupant == Some(session.occupant));
    if !seated {
        return Ok(Judgment::Hold);
    }
    if !continues(row, author, author_session) {
        return Ok(Judgment::Drop);
    }
    let target = row.lane;
    let heads = snapshot
        .tasks()
        .binary_search_by(|heads| heads.statement().body().born.cmp(&reviewed.task))
        .ok()
        .map(|index| &snapshot.tasks()[index])
        .ok_or_else(|| {
            store_fault(
                "review-feedback::task-missing",
                "the reviewed task has no current statement head",
            )
        })?;
    Ok(Judgment::Deliver(Feedback {
        reviewed: envelope.seq,
        task: reviewed.task.clone(),
        lane: target,
        text: feedback_text(
            &wire::TaskHandle::for_topic(
                &heads.statement().body().topic.path,
                &heads.statement().body().born,
            ),
            finding,
        ),
        residency,
    }))
}

/// Whether the seat on the author's lane `row` continues the vendor session
/// that completed the judged phase: the author's own occupant, or a later one
/// that resumed the same session. Widening delivery to every seat in the
/// lane's renewal lineage replaces this body with
/// `row.lineage.contains(&author.occupant)`.
fn continues(
    row: &LaneResidency,
    author: ClaimAuthor,
    author_session: Option<&DesignSession>,
) -> bool {
    row.session.as_ref().is_some_and(|seated| {
        seated.occupant == author.occupant
            || author_session.is_some_and(|session| {
                session.vendor == seated.vendor && session.identity == seated.identity
            })
    })
}

/// Whether `reviewer` judged from the same vendor session as `author`.
fn same_session(
    snapshot: &bus::TaskSnapshot,
    author: ClaimAuthor,
    author_session: Option<&DesignSession>,
    reviewer: ClaimAuthor,
) -> bool {
    author.occupant == reviewer.occupant
        || (author.lane == reviewer.lane
            && author_session.is_some_and(|session| {
                snapshot
                    .facts()
                    .lane_session(reviewer.lane, reviewer.occupant)
                    == Some(session)
            }))
}

/// Whether a finding recorded `at` is past the directive lifetime it would be
/// delivered with.
fn outlived(at: Instant, now: Instant, lifetime: Duration) -> bool {
    let age = now.get().duration_since(at.get());
    age.is_positive() && age.unsigned_abs() >= lifetime
}

/// Every finding review after the cursor.
fn new_findings(facts: &bus::TaskFacts, cursor: Option<Seq>) -> CommandResult<BTreeSet<Seq>> {
    let mut findings = BTreeSet::new();
    for envelope in facts.after(cursor) {
        let Some(reviewed) = envelope.fact.read::<wire::body::TaskReviewed>() else {
            continue;
        };
        if reviewed.reviewer != envelope.actor {
            return Err(store_fault(
                "review-feedback::reviewer-mismatched",
                "the review body and authenticated fact actor disagree",
            ));
        }
        if reviewed.verdict == wire::body::Verdict::Finding {
            findings.insert(envelope.seq);
        }
    }
    Ok(findings)
}

/// The authors the snapshot recorded for the finding review at `review`.
fn finding_authors(facts: &bus::TaskFacts, review: Seq) -> CommandResult<FindingAuthors> {
    let fault = match facts.finding_authors(review) {
        Some(Ok(authors)) => return Ok(authors),
        None => (
            "review-feedback::authors-missing",
            "a finding review has no recorded phase authors",
        ),
        Some(Err(AuthorshipFault::CompletionUnclaimed)) => (
            "review-feedback::author-missing",
            "task completion has no preceding active claim author",
        ),
        Some(Err(AuthorshipFault::CompletionMismatched)) => (
            "review-feedback::author-mismatched",
            "task completion actor disagrees with its active claim lane",
        ),
        Some(Err(AuthorshipFault::ReviewUncompleted)) => (
            "review-feedback::author-missing",
            "review has no preceding completed phase author",
        ),
        Some(Err(AuthorshipFault::ReviewUnclaimed)) => (
            "review-feedback::reviewer-missing",
            "review has no preceding active reviewer claim",
        ),
    };
    Err(store_fault(fault.0, fault.1))
}

fn feedback_text(handle: &wire::TaskHandle, finding: &str) -> String {
    let quoted = finding
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{handle}\n\n{quoted}\n\n{}", wire::REVIEW_FEEDBACK_GUIDANCE,)
}

#[cfg(test)]
mod tests;
