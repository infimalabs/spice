//! Who completed the phase each finding review judged, and who judged it,
//! advanced once per commit so delivering a finding never walks task history.
//! Completions and reviews do not carry the claim occupant, so authorship
//! follows each task's active claims as they are won, carried, rebound,
//! released, and ended by an arrival or a verdict. A shared round holds one
//! active claim per lane, so the index keeps every active claim and resolves
//! each completion or verdict to the claim its actor holds.

use std::collections::HashMap;

use journal::crossing::body::{ClaimCarried, ClaimLapsed, ClaimRebound, TaskReviewed, Verdict};
use journal::crossing::{Actor, AnyFact, CanonicalUuid, Envelope, FactKind, Seq, TaskStamp};

use super::persistent_index::PersistentIndex;
use super::{Indexes, TaskFacts};

/// One claim's lane and the occupant seated on it when the claim was taken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaimAuthor {
    pub lane: CanonicalUuid,
    pub occupant: CanonicalUuid,
}

/// Who authored the completed phase one finding judged, and who judged it.
/// Shared rounds retain every author in arrival order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingAuthors {
    pub authors: Vec<ClaimAuthor>,
    pub reviewer: ClaimAuthor,
}

/// Authority history that leaves a finding review's authors undefined.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorshipFault {
    /// The judged completion had no active claim before it.
    CompletionUnclaimed,
    /// The judged completion's actor holds none of its active claims.
    CompletionMismatched,
    /// The review had no completed phase before it.
    ReviewUncompleted,
    /// The reviewing actor held no active claim on the task.
    ReviewUnclaimed,
}

/// Each task's active and completed phase authors, every finding review's
/// authors keyed by its durable position, and the reviewers whose findings
/// judged each task's newest completed phase.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ReviewAuthors {
    /// Each task's active claim authors, one per lane, in claim order.
    active: PersistentIndex<TaskStamp, Vec<ClaimAuthor>>,
    /// Each task's newest completed phase.
    completed: PersistentIndex<TaskStamp, CompletedPhase>,
    findings: PersistentIndex<Seq, Result<FindingAuthors, AuthorshipFault>>,
    /// Reviewers whose findings judged each task's newest completed phase, in
    /// finding order with each reviewer's first finding; a later completion of
    /// the task starts the list over.
    finders: PersistentIndex<TaskStamp, Vec<(Seq, Actor)>>,
}

/// One completed phase: its authors, or why it has none, and whether its
/// completion published repository history.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CompletedPhase {
    authors: Result<Vec<ClaimAuthor>, AuthorshipFault>,
    published: bool,
}

impl ReviewAuthors {
    fn active(&self, task: &TaskStamp) -> &[ClaimAuthor] {
        self.active.get(task).map_or(&[], Vec::as_slice)
    }

    fn set_active(&mut self, task: &TaskStamp, authors: Vec<ClaimAuthor>) {
        self.active = self.active.pushed(HashMap::from([(task.clone(), authors)]));
    }

    /// `occupant` on `lane` now holds an active claim on `task`; a lane holds
    /// at most one, so a rebound replaces the occupant it was won with.
    pub(super) fn claim(&mut self, task: TaskStamp, lane: CanonicalUuid, occupant: CanonicalUuid) {
        let mut authors = self.active(&task).to_vec();
        match authors.iter_mut().find(|author| author.lane == lane) {
            Some(author) => author.occupant = occupant,
            None => authors.push(ClaimAuthor { lane, occupant }),
        }
        self.set_active(&task, authors);
    }

    /// The active claim `actor` holds on `task`, when it holds one.
    fn held_by(&self, task: &TaskStamp, actor: &Actor) -> Option<ClaimAuthor> {
        self.active(task)
            .iter()
            .copied()
            .find(|author| *actor == Actor::Lane(author.lane))
    }

    /// `actor`'s active claim on `task` ended without completing its phase.
    pub(super) fn release(&mut self, task: &TaskStamp, actor: &Actor) {
        self.retain(task, |author| *actor != Actor::Lane(author.lane));
    }

    /// `lane`'s active claim on `task` ended by arriving in a shared round.
    pub(super) fn arrive(&mut self, task: &TaskStamp, lane: CanonicalUuid) {
        self.retain(task, |author| author.lane != lane);
    }

    /// `occupant`'s active claim on `task` lapsed.
    pub(super) fn lapse(&mut self, task: &TaskStamp, occupant: CanonicalUuid) {
        self.retain(task, |author| author.occupant != occupant);
    }

    fn retain(&mut self, task: &TaskStamp, keep: impl Fn(&ClaimAuthor) -> bool) {
        let before = self.active(task);
        if before.iter().all(&keep) {
            return;
        }
        let authors = before.iter().copied().filter(keep).collect();
        self.set_active(task, authors);
    }

    /// `actor` completed `task`'s single-participant phase; `published` when
    /// the completion published repository history.
    pub(super) fn complete(&mut self, actor: &Actor, task: &TaskStamp, published: bool) {
        let authors = match self.held_by(task, actor) {
            Some(author) => Ok(vec![author]),
            None if self.active(task).is_empty() => Err(AuthorshipFault::CompletionUnclaimed),
            None => Err(AuthorshipFault::CompletionMismatched),
        };
        self.release(task, actor);
        self.record_completed(task, CompletedPhase { authors, published });
    }

    /// Every durable participant authored one shared round.
    pub(super) fn complete_round(
        &mut self,
        task: &TaskStamp,
        participants: &[journal::crossing::body::RoundParticipant],
    ) {
        let published = participants.iter().any(|participant| {
            participant
                .git
                .as_ref()
                .is_some_and(|boundary| boundary.local_commits > 0)
        });
        let authors = participants
            .iter()
            .map(|participant| ClaimAuthor {
                lane: participant.lane,
                occupant: participant.occupant,
            })
            .collect();
        if !self.active(task).is_empty() {
            self.set_active(task, Vec::new());
        }
        self.record_completed(
            task,
            CompletedPhase {
                authors: Ok(authors),
                published,
            },
        );
    }

    fn record_completed(&mut self, task: &TaskStamp, completed: CompletedPhase) {
        self.completed = self
            .completed
            .pushed(HashMap::from([(task.clone(), completed)]));
        if self
            .finders
            .get(task)
            .is_some_and(|finders| !finders.is_empty())
        {
            self.finders = self
                .finders
                .pushed(HashMap::from([(task.clone(), Vec::new())]));
        }
    }

    /// The review at `seq` by `reviewer` ends that reviewer's claim; a finding
    /// records the completed authors it judged and that reviewer.
    fn review(&mut self, seq: Seq, task: &TaskStamp, reviewer: &Actor, verdict: Verdict) {
        let held = self.held_by(task, reviewer);
        self.release(task, reviewer);
        if verdict != Verdict::Finding {
            return;
        }
        let authors = match (self.completed.get(task), held) {
            (None, _) => Err(AuthorshipFault::ReviewUncompleted),
            (
                Some(CompletedPhase {
                    authors: Err(fault),
                    ..
                }),
                _,
            ) => Err(*fault),
            (Some(_), None) => Err(AuthorshipFault::ReviewUnclaimed),
            (
                Some(CompletedPhase {
                    authors: Ok(authors),
                    ..
                }),
                Some(reviewer),
            ) => Ok(FindingAuthors {
                authors: authors.clone(),
                reviewer,
            }),
        };
        self.findings = self.findings.pushed(HashMap::from([(seq, authors)]));
        let mut finders = self.finders.get(task).cloned().unwrap_or_default();
        if !finders.iter().any(|(_, finder)| finder == reviewer) {
            finders.push((seq, reviewer.clone()));
            self.finders = self
                .finders
                .pushed(HashMap::from([(task.clone(), finders)]));
        }
    }
}

impl Indexes {
    /// Advance phase authorship by a fact only it reads: a carried, rebound,
    /// or lapsed claim, or a review. Won and freed claims, arrivals, and
    /// completions reach it from `observe_task_authority`, which decodes them
    /// once for every index.
    pub(super) fn observe_review_authors(&mut self, row: &Envelope<AnyFact>) {
        let authors = &mut self.review_authors;
        match row.fact.kind() {
            FactKind::ClaimCarried => {
                if let Some(carried) = row.fact.read::<ClaimCarried>() {
                    authors.claim(carried.task, carried.lane, carried.occupant);
                }
            }
            FactKind::ClaimRebound => {
                if let Some(rebound) = row.fact.read::<ClaimRebound>() {
                    authors.claim(rebound.task, rebound.lane, rebound.occupant);
                }
            }
            FactKind::ClaimLapsed => {
                if let Some(lapsed) = row.fact.read::<ClaimLapsed>() {
                    authors.lapse(&lapsed.task, lapsed.occupant);
                }
            }
            FactKind::TaskReviewed => {
                if let Some(reviewed) = row.fact.read::<TaskReviewed>() {
                    authors.review(
                        row.seq,
                        &reviewed.task,
                        &reviewed.reviewer,
                        reviewed.verdict,
                    );
                }
            }
            _ => {}
        }
    }
}

impl TaskFacts {
    /// The authors of the finding review at `review`, or the authority fault
    /// that leaves them undefined; `None` when no finding review is there.
    #[must_use]
    pub fn finding_authors(&self, review: Seq) -> Option<Result<FindingAuthors, AuthorshipFault>> {
        self.indexes.review_authors.findings.get(&review).cloned()
    }

    /// Every lane whose completion published `task`'s newest completed phase,
    /// in claim order; empty for an unpublished completion or one whose
    /// authorship is undefined.
    #[must_use]
    pub fn published_authors(&self, task: &TaskStamp) -> Vec<Actor> {
        self.indexes
            .review_authors
            .completed
            .get(task)
            .filter(|completed| completed.published)
            .and_then(|completed| completed.authors.as_ref().ok())
            .map(|authors| {
                authors
                    .iter()
                    .map(|author| Actor::Lane(author.lane))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every reviewer whose finding judged `task`'s newest completed phase, in
    /// finding order with the position of each reviewer's first finding;
    /// empty once a later completion of the task starts a new phase to judge.
    #[must_use]
    pub fn finding_reviewers(&self, task: &TaskStamp) -> &[(Seq, Actor)] {
        self.indexes
            .review_authors
            .finders
            .get(task)
            .map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests;
