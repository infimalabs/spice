//! Pausing re-seating when a seat keeps killing its occupants.
//!
//! An occupant that dies young dies before it did any work, so re-seating it
//! immediately produces another young death and a lane that spends its whole
//! life starting. After enough of them in a row the seat is held: re-seating
//! stops until an operator wakes it or the hold reaches its deadline.
//!
//! An occupant whose last word was a vendor failure holds the seat at once.
//! The vendor refused the exact session and pending directions recorded with
//! the hold. Re-seating stays quiet after the deadline while that input is
//! unchanged; operator wake or changed input releases it. This prevents a
//! deterministic refusal from consuming one identical turn per hold period.
//!
//! A hold is a fact, not a log line. [`LaneHeld`] carries its evidence,
//! deadline, and refused input; [`LaneResumed`] carries what released it. The
//! reason a lane stopped filling is answerable from the journal instead of
//! from whatever the supervisor happened to print.
//!
//! Youth and now both arrive from the caller. This module compares the
//! deadlines and input identities it was handed and never reads a clock, so a
//! held lane replays to the same hold.

use std::collections::BTreeMap;

use journal::crossing::body::{LaneHeld, LaneResumed, Release, VendorFailureInput};
use journal::crossing::{CanonicalUuid, Instant};

/// How an occupant's life ended, as the caller measured it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Death {
    /// The occupant died before it had lived long enough to do work.
    Young,
    /// The occupant lived long enough that its death says nothing about the seat.
    Mature,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HoldKind {
    YoungDeath,
    VendorFailure(VendorFailureInput),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Hold {
    until: Instant,
    kind: HoldKind,
    expiry_observed: bool,
}

/// Consecutive young deaths per lane, and the holds they produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holds {
    limit: u32,
    young: BTreeMap<CanonicalUuid, u32>,
    held: BTreeMap<CanonicalUuid, Hold>,
}

impl Holds {
    /// Holds that pause a lane after this many consecutive young deaths.
    ///
    /// # Panics
    ///
    /// Panics when the limit is zero, which would hold every lane before it
    /// ever seated an occupant.
    #[must_use]
    pub fn new(limit: u32) -> Self {
        assert!(limit > 0, "a hold limit of zero would hold every lane");
        Self {
            limit,
            young: BTreeMap::new(),
            held: BTreeMap::new(),
        }
    }

    /// How many consecutive young deaths hold a lane.
    #[must_use]
    pub const fn limit(&self) -> u32 {
        self.limit
    }

    /// Records one occupant death, holding the lane once they are repeated.
    ///
    /// A mature death clears the run: the seat proved it can carry an occupant,
    /// so the young deaths before it no longer describe it. The returned fact
    /// is the one to commit; the hold is already recorded here either way.
    pub fn died(&mut self, lane: CanonicalUuid, death: Death, until: Instant) -> Option<LaneHeld> {
        if self.held.contains_key(&lane) {
            return None;
        }
        if death == Death::Mature {
            self.young.remove(&lane);
            return None;
        }
        let deaths = self.young.entry(lane).or_insert(0);
        *deaths += 1;
        if *deaths < self.limit {
            return None;
        }
        let evidence = format!("{deaths} consecutive young occupant deaths");
        self.young.remove(&lane);
        self.held.insert(
            lane,
            Hold {
                until,
                kind: HoldKind::YoungDeath,
                expiry_observed: false,
            },
        );
        Some(LaneHeld {
            lane,
            evidence,
            until,
            vendor_failure: None,
        })
    }

    /// Records one occupant exit whose last word was a vendor failure, holding
    /// the lane at once.
    ///
    /// One such exit is enough: the hold carries the vendor's own text as its
    /// evidence, and any run of young deaths before it is forgotten because
    /// the hold supersedes what the run was counting toward. A lane already
    /// held keeps its hold and its deadline.
    pub fn failed(
        &mut self,
        lane: CanonicalUuid,
        evidence: String,
        until: Instant,
        input: VendorFailureInput,
    ) -> Option<LaneHeld> {
        if self.held.contains_key(&lane) {
            return None;
        }
        self.young.remove(&lane);
        self.held.insert(
            lane,
            Hold {
                until,
                kind: HoldKind::VendorFailure(input.clone()),
                expiry_observed: false,
            },
        );
        Some(LaneHeld {
            lane,
            evidence,
            until,
            vendor_failure: Some(input),
        })
    }

    /// The deadline a lane is held until, absent when it is not held.
    #[must_use]
    pub fn held(&self, lane: CanonicalUuid) -> Option<Instant> {
        self.held.get(&lane).map(|hold| hold.until)
    }

    /// Refused vendor input, absent for a young-death hold.
    #[must_use]
    pub fn vendor_failure(&self, lane: CanonicalUuid) -> Option<&VendorFailureInput> {
        match &self.held.get(&lane)?.kind {
            HoldKind::YoungDeath => None,
            HoldKind::VendorFailure(input) => Some(input),
        }
    }

    /// Earliest deadline that still needs a timer event.
    #[must_use]
    pub fn next(&self) -> Option<(CanonicalUuid, Instant)> {
        self.held
            .iter()
            .filter(|(_, hold)| !hold.expiry_observed)
            .map(|(lane, hold)| (*lane, hold.until))
            .min_by_key(|(lane, until)| (*until, *lane))
    }

    /// Replay one durable hold fact into this reducer.
    pub fn absorb_held(&mut self, held: &LaneHeld) {
        self.young.remove(&held.lane);
        let kind = held
            .vendor_failure
            .clone()
            .map_or(HoldKind::YoungDeath, HoldKind::VendorFailure);
        self.held.insert(
            held.lane,
            Hold {
                until: held.until,
                kind,
                expiry_observed: false,
            },
        );
    }

    /// Replay one durable release fact into this reducer.
    pub fn absorb_resumed(&mut self, resumed: &LaneResumed) {
        self.held.remove(&resumed.lane);
    }

    /// Whether the lane may be re-seated after all fact-producing releases.
    #[must_use]
    pub fn seats(&self, lane: CanonicalUuid) -> bool {
        !self.held.contains_key(&lane)
    }

    /// Releases a young-death hold whose deadline passed. A vendor-failure
    /// deadline instead marks that its unchanged refused input stays quiet.
    pub fn expire(&mut self, lane: CanonicalUuid, now: Instant) -> Option<LaneResumed> {
        let hold = self.held.get_mut(&lane)?;
        if now < hold.until || hold.expiry_observed {
            return None;
        }
        if matches!(hold.kind, HoldKind::VendorFailure(_)) {
            hold.expiry_observed = true;
            return None;
        }
        self.held.remove(&lane);
        Some(LaneResumed {
            lane,
            release: Release::Expiry,
        })
    }

    /// Releases a vendor hold once its resumable input differs.
    pub fn input_changed(
        &mut self,
        lane: CanonicalUuid,
        current: &VendorFailureInput,
    ) -> Option<LaneResumed> {
        let HoldKind::VendorFailure(refused) = &self.held.get(&lane)?.kind else {
            return None;
        };
        if refused == current {
            return None;
        }
        self.held.remove(&lane);
        Some(LaneResumed {
            lane,
            release: Release::InputChanged,
        })
    }

    /// Releases a hold because the operator woke the lane.
    ///
    /// Returns nothing when the lane is not held, so a wake that raced an
    /// expiry does not resume a lane twice.
    pub fn wake(&mut self, lane: CanonicalUuid) -> Option<LaneResumed> {
        self.held.remove(&lane)?;
        Some(LaneResumed {
            lane,
            release: Release::Wake,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Death, Holds};
    use journal::crossing::body::{Release, VendorFailureInput};
    use journal::crossing::{CanonicalUuid, Instant};

    fn lane(last: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
            .parse()
            .expect("fixture uuid")
    }

    fn instant(minute: u8) -> Instant {
        format!("2026-08-01T07:{minute:02}:00.000Z")
            .parse()
            .expect("fixture instant")
    }

    fn input(session: &str, directive: u8) -> VendorFailureInput {
        VendorFailureInput {
            session: Some(session.to_owned()),
            directives: vec![lane(directive)],
            renewal: None,
        }
    }

    /// A run of young deaths short of the limit does not hold the lane.
    #[test]
    fn young_deaths_short_of_the_limit_hold_nothing() {
        let mut holds = Holds::new(3);

        for _ in 0..2 {
            assert!(holds.died(lane(1), Death::Young, instant(30)).is_none());
        }

        assert_eq!(holds.held(lane(1)), None);
        assert!(holds.seats(lane(1)));
    }

    /// Reaching the limit holds the lane and names its evidence and deadline.
    #[test]
    fn repeated_young_deaths_hold_the_lane_as_a_fact() {
        let mut holds = Holds::new(3);
        holds.died(lane(1), Death::Young, instant(30));
        holds.died(lane(1), Death::Young, instant(30));

        let held = holds
            .died(lane(1), Death::Young, instant(30))
            .expect("the third young death holds the lane");

        assert_eq!(held.lane, lane(1));
        assert_eq!(held.until, instant(30));
        assert_eq!(holds.held(lane(1)), Some(instant(30)));
    }

    /// A held lane does not re-seat until its deadline arrives.
    #[test]
    fn a_held_lane_stops_reseating_until_its_deadline() {
        let mut holds = Holds::new(1);
        holds
            .died(lane(1), Death::Young, instant(30))
            .expect("one young death holds at a limit of one");

        assert!(!holds.seats(lane(1)));
        assert!(holds.seats(lane(2)), "another lane is unheld");
        holds
            .expire(lane(1), instant(30))
            .expect("expiry emits the release before re-seating");
        assert!(holds.seats(lane(1)));
    }

    /// A mature death clears the run, so young deaths must be consecutive.
    #[test]
    fn a_mature_death_clears_the_run_of_young_ones() {
        let mut holds = Holds::new(2);
        holds.died(lane(1), Death::Young, instant(30));
        assert!(holds.died(lane(1), Death::Mature, instant(30)).is_none());

        assert!(
            holds.died(lane(1), Death::Young, instant(30)).is_none(),
            "a young death after a mature one held on the first of a new run",
        );
    }

    /// One vendor failure holds the lane at once. Reaching its deadline with
    /// unchanged input makes the hold quiet instead of resuming the refusal.
    #[test]
    fn one_vendor_failure_stays_held_after_unchanged_input_reaches_its_deadline() {
        let mut holds = Holds::new(3);
        assert!(holds.died(lane(1), Death::Young, instant(30)).is_none());
        let refused = input("session-1", 11);

        let held = holds
            .failed(
                lane(1),
                "occupant ended on a vendor error: refused".to_owned(),
                instant(31),
                refused.clone(),
            )
            .expect("the first vendor failure holds the lane");

        assert_eq!(held.lane, lane(1));
        assert_eq!(held.evidence, "occupant ended on a vendor error: refused");
        assert_eq!(held.until, instant(31));
        assert_eq!(held.vendor_failure, Some(refused));
        assert!(!holds.seats(lane(1)));
        assert_eq!(holds.next(), Some((lane(1), instant(31))));
        assert!(holds.expire(lane(1), instant(31)).is_none());
        assert!(!holds.seats(lane(1)), "unchanged refused input stays held");
        assert_eq!(holds.next(), None, "the quiet hold arms no retry timer");
        assert!(
            holds.died(lane(1), Death::Young, instant(40)).is_none(),
            "the young death before the failure was forgotten with the hold",
        );
    }

    #[test]
    fn changed_vendor_input_resumes_the_lane() {
        let mut holds = Holds::new(3);
        holds.failed(
            lane(1),
            "occupant ended on a vendor error: refused".to_owned(),
            instant(31),
            input("session-1", 11),
        );

        assert!(
            holds
                .input_changed(lane(1), &input("session-1", 11))
                .is_none(),
            "identical input cannot resume the refusal"
        );
        let resumed = holds
            .input_changed(lane(1), &input("session-1", 12))
            .expect("new direction releases the refused input");
        assert_eq!(resumed.release, Release::InputChanged);
        assert!(holds.seats(lane(1)));
    }

    #[test]
    fn a_renewal_revision_releases_the_refused_session() {
        let mut holds = Holds::new(3);
        let refused = input("session-1", 11);
        holds.failed(
            lane(1),
            "occupant ended on a vendor error: refused".to_owned(),
            instant(31),
            refused.clone(),
        );
        let mut renewed = refused;
        renewed.renewal = Some(7);

        let resumed = holds
            .input_changed(lane(1), &renewed)
            .expect("renewal replaces the refused session input");
        assert_eq!(resumed.release, Release::InputChanged);
        assert!(holds.seats(lane(1)));
    }

    /// A vendor failure on a held lane keeps the hold it already has.
    #[test]
    fn a_vendor_failure_while_held_keeps_the_existing_hold() {
        let mut holds = Holds::new(1);
        holds
            .died(lane(1), Death::Young, instant(30))
            .expect("the first death holds");

        assert!(
            holds
                .failed(
                    lane(1),
                    "later failure".to_owned(),
                    instant(59),
                    input("session-2", 12),
                )
                .is_none()
        );
        assert_eq!(holds.held(lane(1)), Some(instant(30)));
    }

    /// A held lane cannot publish another hold before it resumes.
    #[test]
    fn deaths_while_held_do_not_replace_or_repeat_the_hold() {
        let mut holds = Holds::new(1);
        holds
            .died(lane(1), Death::Young, instant(30))
            .expect("the first death holds");

        assert!(holds.died(lane(1), Death::Young, instant(59)).is_none());
        assert_eq!(holds.held(lane(1)), Some(instant(30)));
    }

    /// An expired hold resumes the lane once, naming expiry as the release.
    #[test]
    fn an_expired_hold_resumes_the_lane_once() {
        let mut holds = Holds::new(1);
        holds.died(lane(1), Death::Young, instant(30));

        assert!(holds.expire(lane(1), instant(29)).is_none());
        let resumed = holds
            .expire(lane(1), instant(30))
            .expect("the deadline arrived");
        assert_eq!(resumed.lane, lane(1));
        assert_eq!(resumed.release, Release::Expiry);
        assert!(
            holds.expire(lane(1), instant(31)).is_none(),
            "the lane resumed a second time",
        );
        assert_eq!(holds.held(lane(1)), None);
    }

    /// An operator wake resumes the lane once, naming wake as the release.
    #[test]
    fn an_operator_wake_resumes_the_lane_once() {
        let mut holds = Holds::new(1);
        holds.died(lane(1), Death::Young, instant(30));

        let resumed = holds.wake(lane(1)).expect("the operator woke the lane");
        assert_eq!(resumed.release, Release::Wake);
        assert!(holds.wake(lane(1)).is_none(), "the lane resumed twice");
        assert!(
            holds.expire(lane(1), instant(31)).is_none(),
            "an expiry resumed a lane the operator already woke",
        );
    }

    /// Waking a lane that was never held resumes nothing.
    #[test]
    fn waking_an_unheld_lane_resumes_nothing() {
        let mut holds = Holds::new(1);

        assert!(holds.wake(lane(1)).is_none());
        assert!(holds.expire(lane(1), instant(59)).is_none());
    }

    /// A hold limit of zero would hold every lane before it ever seated.
    #[test]
    #[should_panic(expected = "a hold limit of zero would hold every lane")]
    fn a_zero_limit_is_refused() {
        let _ = Holds::new(0);
    }

    #[test]
    fn durable_hold_and_resume_facts_rebuild_the_same_state() {
        let mut source = Holds::new(1);
        let held = source
            .died(lane(2), Death::Young, instant(30))
            .expect("source holds");
        let resumed = source.wake(lane(2)).expect("source resumes");
        let mut replay = Holds::new(1);
        replay.absorb_held(&held);
        assert_eq!(replay.held(lane(2)), Some(instant(30)));
        assert_eq!(replay.next(), Some((lane(2), instant(30))));
        replay.absorb_resumed(&resumed);
        assert!(replay.seats(lane(2)));
        assert_eq!(replay.next(), None);
    }

    #[test]
    fn durable_vendor_input_rebuilds_the_same_quiet_hold() {
        let refused = input("session-1", 11);
        let mut source = Holds::new(1);
        let held = source
            .failed(
                lane(1),
                "occupant ended on a vendor error: refused".to_owned(),
                instant(30),
                refused.clone(),
            )
            .expect("source holds the refused input");
        let mut replay = Holds::new(1);
        replay.absorb_held(&held);

        assert!(replay.expire(lane(1), instant(30)).is_none());
        assert!(!replay.seats(lane(1)));
        assert_eq!(replay.next(), None);
        assert!(replay.input_changed(lane(1), &refused).is_none());
    }
}
