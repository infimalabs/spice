//! Closing one shared phase round from its durable arrivals.
//!
//! An arrival, a target change, a free, and a lapse can each leave a round
//! satisfied. Every one of them reads the round's evidence and writes its
//! closure here, so arrivals validate identically on every path and the
//! completion fact has one shape.

use std::borrow::Borrow;
use std::error::Error;
use std::fmt;
use std::num::NonZeroU32;

use journal::{BuildError, FactCause, JournalWrite, PendingFact};
use wire::{AnyFact, Instant, Phase, Seq, TaskStamp, body};

use crate::{Entry, PhaseRefusal, Transition, TransitionKind};

/// What a round's accepted arrivals contribute to its completion: every
/// participant in arrival order, and the one Plan disposition they agree on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoundEvidence {
    participants: Vec<body::RoundParticipant>,
    disposition: Option<body::PlanDisposition>,
}

impl RoundEvidence {
    /// Read and check every accepted arrival of `entry`'s current round
    /// through `fact`, which answers the retained fact at a position.
    ///
    /// # Errors
    ///
    /// Refuses an arrival whose fact is absent, is not `task::arrived`, or
    /// names another task, round, claim, or lane than its completion head, and
    /// arrivals that record conflicting Plan dispositions. A failing `fact`
    /// answers [`RoundError::Read`].
    pub fn read<B, E>(
        entry: &Entry,
        mut fact: impl FnMut(Seq) -> Result<Option<B>, E>,
    ) -> Result<Self, RoundError<E>>
    where
        B: Borrow<AnyFact>,
    {
        let completion = entry.completion();
        let mut evidence = Self {
            participants: Vec::with_capacity(completion.arrivals.len() + 1),
            disposition: None,
        };
        for arrival in &completion.arrivals {
            let found = fact(arrival.fact)
                .map_err(RoundError::Read)?
                .ok_or(RoundError::Missing(arrival.fact))?;
            let arrived = found
                .borrow()
                .read::<body::TaskArrived>()
                .ok_or(RoundError::NotArrival(arrival.fact))?;
            if arrived.task != *entry.born()
                || arrived.round != completion.round
                || arrived.claimed != arrival.claimed
                || arrived.lane != arrival.lane
            {
                return Err(RoundError::Disagrees(arrival.fact));
            }
            evidence.arrive(
                body::RoundParticipant {
                    claimed: arrival.claimed,
                    actor: arrival.actor.clone(),
                    lane: arrival.lane,
                    occupant: arrived.occupant,
                    validation: arrived.validation,
                    git: arrived.git,
                    source: arrived.source,
                },
                arrived.disposition,
            )?;
        }
        Ok(evidence)
    }

    /// Add a participant arriving in the same write, merging its disposition.
    ///
    /// # Errors
    ///
    /// Refuses a disposition that conflicts with one an earlier arrival
    /// recorded.
    pub fn arrive<E>(
        &mut self,
        participant: body::RoundParticipant,
        disposition: Option<body::PlanDisposition>,
    ) -> Result<(), RoundError<E>> {
        if let Some(current) = disposition {
            if self
                .disposition
                .as_ref()
                .is_some_and(|prior| *prior != current)
            {
                return Err(RoundError::DispositionConflict);
            }
            self.disposition = Some(current);
        }
        self.participants.push(participant);
        Ok(())
    }
}

/// One satisfied round ready to close: its evidence and the ladder transition
/// after it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Closure {
    task: TaskStamp,
    phase: Phase,
    round: Seq,
    evidence: RoundEvidence,
    transition: Transition,
    completion: wire::CompletionHead,
}

impl Closure {
    /// The closure `post`, an entry as it stands after a change, owes now.
    ///
    /// None unless the round meets its barrier.
    ///
    /// # Errors
    ///
    /// Refuses as [`RoundEvidence::read`] does, and when the workflow cannot
    /// leave the completed round.
    pub fn owed<B, E>(
        post: &Entry,
        at: Instant,
        fact: impl FnMut(Seq) -> Result<Option<B>, E>,
    ) -> Result<Option<Self>, RoundError<E>>
    where
        B: Borrow<AnyFact>,
    {
        if !post.round_satisfied(None) {
            return Ok(None);
        }
        let evidence = RoundEvidence::read(post, fact)?;
        Self::new(post, evidence, at).map(Some)
    }

    /// Close `entry`'s current round with `evidence`, advancing its ladder as
    /// the agreed disposition directs.
    ///
    /// # Errors
    ///
    /// Refuses when the workflow cannot leave the completed round.
    pub fn new<E>(
        entry: &Entry,
        evidence: RoundEvidence,
        at: Instant,
    ) -> Result<Self, RoundError<E>> {
        let transition = Transition::after_round(entry, at, evidence.disposition.is_some())
            .map_err(RoundError::Phase)?;
        Ok(Self {
            task: entry.born().clone(),
            phase: entry.workflow().phase,
            round: entry.completion().round,
            evidence,
            transition,
            completion: entry.completion().clone(),
        })
    }

    /// The workflow change this closure commits.
    #[must_use]
    pub const fn transition(&self) -> &Transition {
        &self.transition
    }

    /// Append the round's completion, its phase change when the ladder
    /// advances, and the next workflow head, caused by `cause`. A Design pass
    /// leaves the workflow in place and reopens the round. Answers the
    /// completion fact.
    ///
    /// # Errors
    ///
    /// Returns the journal's build error when a fact cannot be staged.
    pub fn append(
        self,
        write: &mut JournalWrite,
        next: Option<Box<str>>,
        cause: FactCause,
    ) -> Result<PendingFact, BuildError> {
        let status = if self.transition.next().status.is_pending() {
            body::TaskCompletionStatus::Pending
        } else {
            body::TaskCompletionStatus::Completed
        };
        let completed = write.fact(
            body::TaskRoundCompleted {
                task: self.task.clone(),
                phase: self.phase,
                round: self.round,
                participants: self.evidence.participants,
                status,
                next,
                disposition: self.evidence.disposition,
            },
            cause,
        )?;
        let workflow_fact = match self.transition.kind() {
            TransitionKind::Advance { to, .. } => write.fact(
                body::TaskPhased {
                    task: self.task.clone(),
                    phase: *to,
                },
                FactCause::Pending(completed),
            )?,
            TransitionKind::Complete { .. } => completed,
            TransitionKind::Pass { .. } => {
                let completion = self.completion;
                write.completion_head(self.task, completed, move |fact| completion.reopened(fact));
                return Ok(completed);
            }
        };
        let workflow = self.transition.next().clone();
        write.workflow_head(self.task, workflow_fact, move |_| workflow);
        Ok(completed)
    }
}

/// One phase target shrinking back as a participant leaves its round.
///
/// A slot the operator asked for stays open after its holder leaves, so
/// allocation fills it again; a slot a join grew belongs to the lane that
/// joined and leaves with it, so a freed or lapsed participant never turns
/// into allocation demand. The retraction projects onto the entry before the
/// round's closure is read, and writes its target fact in the same write as
/// the release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Retraction {
    task: TaskStamp,
    phase: Phase,
    target: NonZeroU32,
    completion: wire::CompletionHead,
}

impl Retraction {
    /// The retraction `post`, an entry as it stands once a participant's
    /// claim ended, owes: none while the target already matches the
    /// operator's ask or the remaining occupancy.
    #[must_use]
    pub fn owed(post: &Entry) -> Option<Self> {
        let phase = post.workflow().phase;
        let occupied = post.active_claims().count() + post.completion().arrivals.len();
        let target = post.completion().retraction(&phase, occupied)?;
        let mut completion = post.completion().clone();
        completion.set_target(phase, target);
        Some(Self {
            task: post.born().clone(),
            phase,
            target,
            completion,
        })
    }

    /// `post` as it stands with the retracted target.
    #[must_use]
    pub fn apply(&self, post: Entry) -> Entry {
        post.with_completion(self.completion.clone())
    }

    /// Append the retracted target and its completion head, caused by the
    /// release.
    ///
    /// # Errors
    ///
    /// Returns the journal's build error when the fact cannot be staged.
    pub fn append(self, write: &mut JournalWrite, cause: FactCause) -> Result<(), BuildError> {
        let targeted = write.fact(
            body::CompletionTargeted {
                task: self.task.clone(),
                phase: self.phase,
                target: self.target,
            },
            cause,
        )?;
        let completion = self.completion;
        write.completion_head(self.task, targeted, move |_| completion);
        Ok(())
    }
}

/// Why a round's evidence cannot close it.
#[derive(Debug)]
pub enum RoundError<E> {
    /// The fact source failed.
    Read(E),
    /// An accepted arrival names a fact the source does not hold.
    Missing(Seq),
    /// An accepted arrival names a fact that is not `task::arrived`.
    NotArrival(Seq),
    /// An arrival fact names another task, round, claim, or lane than its
    /// completion head.
    Disagrees(Seq),
    /// Two arrivals of one round record different Plan dispositions.
    DispositionConflict,
    /// The workflow cannot leave the completed round.
    Phase(PhaseRefusal),
}

impl<E: fmt::Display> fmt::Display for RoundError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(formatter, "round evidence read failed: {error}"),
            Self::Missing(fact) => write!(formatter, "arrival fact {fact} is absent"),
            Self::NotArrival(fact) => write!(formatter, "fact {fact} is not task::arrived"),
            Self::Disagrees(fact) => write!(
                formatter,
                "arrival fact {fact} disagrees with its completion head"
            ),
            Self::DispositionConflict => {
                formatter.write_str("one shared round retained conflicting Plan dispositions")
            }
            Self::Phase(error) => write!(formatter, "completed round cannot advance: {error}"),
        }
    }
}

impl<E: Error + 'static> Error for RoundError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read(error) => Some(error),
            Self::Phase(error) => Some(error),
            Self::Missing(_)
            | Self::NotArrival(_)
            | Self::Disagrees(_)
            | Self::DispositionConflict => None,
        }
    }
}
