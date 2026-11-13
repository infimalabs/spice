//! Whether one substantive peer body may start its idle recipient lane.
//!
//! A body that reaches a seated recipient is admitted as active delivery and
//! commits no start. A body bound for a vacant lane is admitted only with one
//! cause that commits the lane's start: the opening sender's unused exact
//! first return on its line, another admitted body already committing that
//! start, a same-round relationship, or one token of the sender→recipient
//! ordered lane pair. Without any of them the body is refused before it
//! becomes peer input. The cause is decided once, recorded on the admitting
//! body, and re-proved by the commit's guard; wakes read it back instead of
//! judging the relationship again, so no later round, claim or occupant
//! change withdraws a committed start.
//!
//! Each ordered lane pair holds [`CAPACITY`] tokens and accrues one each
//! [`ACCRUAL`] up to that cap. Only counted starts spend; nothing but time
//! refills a pair, and no occupant, session, task or reverse traffic resets
//! it. A pair that never spent holds its full allowance, so its first counted
//! start initializes it, and the guard that re-proves the pair's newest spend
//! keeps two first uses from both drawing on one full allowance.

use jiff::{RoundMode, SignedDuration, Timestamp, TimestampRound, Unit};
use journal::{GuardError, JournalGuard, JournalRead};
use wire::body::{PairSpend, PeerAdmission};
use wire::{CanonicalUuid, Instant};

/// Why a body bound for an idle recipient outside every shared round was
/// refused: its pair has no start left.
pub(in crate::resident) const SPENT: &str = "steer::pair-starts-spent";

/// The refusal a publication meets when the authority behind a wake it states
/// or commits moved between its view and its commit; the publication then
/// resolves again.
pub(in crate::resident) const JUDGMENT_MOVED: &str = "steer::wake-authority-moved";

/// Starts one ordered lane pair holds when full.
const CAPACITY: u8 = 5;

/// How long one spent start takes to accrue again.
const ACCRUAL: SignedDuration = SignedDuration::from_secs(60);

/// One body bound for a vacant recipient lane.
#[derive(Clone, Debug)]
pub(in crate::resident) struct Idle {
    /// Lane that authored the body.
    pub(in crate::resident) from: CanonicalUuid,
    /// Vacant recipient lane.
    pub(in crate::resident) lane: CanonicalUuid,
    /// Room context the body froze, which scopes its round relationship.
    pub(in crate::resident) room: Option<wire::rooms::RoomAnchor>,
    /// Line whose opening input this body answers, when it does.
    pub(in crate::resident) returns: Option<CanonicalUuid>,
}

/// An idle body its pair cannot start: when the pair's next start accrues.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::resident) struct Exhausted {
    next: Timestamp,
}

impl Exhausted {
    /// What the author hears about the recipient this refusal names.
    pub(in crate::resident) fn detail(self, recipient: impl std::fmt::Display) -> String {
        format!(
            "{recipient} is idle outside your rounds and your lane has no start left for it; \
             one accrues at {}",
            second(self.next)
        )
    }
}

impl Idle {
    /// Decide this body's start cause at the commit instant `at` under `read`.
    pub(in crate::resident) fn admit(
        &self,
        read: &JournalRead<'_>,
        at: Instant,
    ) -> Result<Result<PeerAdmission, Exhausted>, GuardError> {
        if let Some(line) = self.returns
            && !read
                .peer_line_head(line)?
                .is_some_and(|head| head.opening_covered())
        {
            return Ok(Ok(PeerAdmission::FirstReturn));
        }
        if starting(read, self.lane)? {
            return Ok(Ok(PeerAdmission::Cohort));
        }
        if super::peer_wakes_now(read, self.from, self.lane, self.room.as_ref(), None, None)? {
            return Ok(Ok(PeerAdmission::SameRound));
        }
        let head = read.peer_pair_head(self.from, self.lane)?;
        Ok(spend(head.map(|head| head.spend), at)?.map(PeerAdmission::PairToken))
    }

    /// A guard proving at commit that `admission` is still this body's cause
    /// at `at`, the instant its write commits under.
    pub(in crate::resident) fn guard(self, admission: PeerAdmission, at: Instant) -> JournalGuard {
        JournalGuard::new(move |read| match self.admit(read, at)? {
            Ok(current) if current == admission => Ok(()),
            _ => Err(GuardError::refused(JUDGMENT_MOVED)),
        })
    }
}

/// The admission cause the peer body created at `created` recorded.
pub(in crate::resident) fn admitted(
    read: &JournalRead<'_>,
    created: wire::Seq,
) -> Result<Option<PeerAdmission>, GuardError> {
    let page = read.fact_page(
        journal::FactPageStart::At(created),
        created,
        journal::FactReadBudget::RETAINED,
    )?;
    Ok(page
        .rows
        .first()
        .and_then(|envelope| super::recorded_admission(&envelope.fact)))
}

/// Whether an admitted peer body pending for `lane` already commits its start.
fn starting(read: &JournalRead<'_>, lane: CanonicalUuid) -> Result<bool, GuardError> {
    for head in crate::steering_read::pending(read, Some(lane))? {
        if head.body().sender.lane().is_some() && admitted(read, head.body().created)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Spend one start of a pair whose newest counted start left `head`, at `at`.
fn spend(head: Option<PairSpend>, at: Instant) -> Result<Result<PairSpend, Exhausted>, GuardError> {
    let now = at.get();
    let (held, frontier) = match head {
        None => (CAPACITY, now),
        Some(head) => {
            let frontier = head.frontier.get();
            let remaining = head.remaining.min(CAPACITY);
            let intervals = now.duration_since(frontier).as_secs().max(0) / ACCRUAL.as_secs();
            let accrued = u8::try_from(intervals)
                .unwrap_or(u8::MAX)
                .min(CAPACITY - remaining);
            let held = remaining + accrued;
            if held == CAPACITY {
                (held, now)
            } else {
                let advanced = frontier
                    .checked_add(SignedDuration::from_secs(
                        i64::from(accrued) * ACCRUAL.as_secs(),
                    ))
                    .map_err(|error| GuardError::Invalid(error.to_string()))?;
                (held, advanced)
            }
        }
    };
    if held == 0 {
        let next = frontier
            .checked_add(ACCRUAL)
            .map_err(|error| GuardError::Invalid(error.to_string()))?;
        return Ok(Err(Exhausted { next }));
    }
    let frontier =
        Instant::new(frontier).map_err(|error| GuardError::Invalid(error.to_string()))?;
    Ok(Ok(PairSpend {
        remaining: held - 1,
        frontier,
    }))
}

/// `at`, rounded up to its second, so a stated accrual is never early.
fn second(at: Timestamp) -> Timestamp {
    at.round(
        TimestampRound::new()
            .smallest(Unit::Second)
            .mode(RoundMode::Ceil),
    )
    .unwrap_or(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Instant {
        Instant::new(Timestamp::from_second(1_800_000_000 + seconds).expect("instant"))
            .expect("instant")
    }

    fn spent(head: Option<PairSpend>, seconds: i64) -> Result<PairSpend, Exhausted> {
        spend(head, at(seconds)).expect("arithmetic holds")
    }

    #[test]
    fn a_pair_spends_five_starts_then_accrues_one_each_minute_up_to_five() {
        let mut head = None;
        for left in (0..CAPACITY).rev() {
            let spend = spent(head, 0).expect("a full pair starts");
            assert_eq!(spend.remaining, left);
            assert_eq!(
                spend.frontier,
                at(0),
                "a full pair accrues from its first spend"
            );
            head = Some(spend);
        }
        assert_eq!(
            spent(head, 59),
            Err(Exhausted { next: at(60).get() }),
            "an empty pair refuses until its next start accrues",
        );
        let refilled = spent(head, 150).expect("two starts accrued");
        assert_eq!(refilled.remaining, 1);
        assert_eq!(
            refilled.frontier,
            at(120),
            "partial accrual keeps its progress toward the next start",
        );
        let full = spent(Some(refilled), 10_000).expect("the pair refilled");
        assert_eq!(full.remaining, CAPACITY - 1, "accrual stops at the cap");
        assert_eq!(full.frontier, at(10_000));
    }

    #[test]
    fn a_clock_behind_the_frontier_accrues_nothing() {
        let empty = PairSpend {
            remaining: 0,
            frontier: at(100),
        };
        assert_eq!(
            spent(Some(empty), 0),
            Err(Exhausted {
                next: at(160).get()
            }),
        );
    }
}
