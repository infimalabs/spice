//! How long an operator direction waited before the lane's transcript carried it.
//!
//! A direction's card and the occupant's view of it are one event: the
//! operator-role record omp emits when it drains its steering queue at a step
//! boundary. Nothing durable says whether the wait before that record was
//! spent in spice or behind the lane's current model step, so the live sweep
//! that first sees the record writes the split down, from clocks already
//! recorded: the creating fact's instant, omp's receipt stamp on the message,
//! and the ingest instant of the batch that read the record.
//!
//! A directive is measured once per resident process, at its first landing. A
//! post-compaction replay, a reminder, and any later rendering of a key already
//! measured say nothing: they are the same direction said again, not a new
//! wait.

use std::collections::BTreeSet;

use wire::CanonicalUuid;

/// Directives already measured by this resident, keyed by lane and key.
#[derive(Debug, Default)]
pub(super) struct Measured(BTreeSet<(CanonicalUuid, String)>);

impl Measured {
    /// The keys one operator-role rendering lands for the first time on
    /// `lane`, in rendering order, and marks them measured.
    ///
    /// Only a `published` header counts: a reminder or escalation restates a
    /// direction the lane already holds, and a replay restates every pending
    /// key after compaction.
    pub(super) fn first_landings(&mut self, lane: CanonicalUuid, rendered: &str) -> Vec<String> {
        if steer::inbox::replays(rendered) {
            return Vec::new();
        }
        steer::inbox::keys(rendered)
            .filter(|key| steer::inbox::stage(rendered, key) == Some("published"))
            .filter(|key| self.0.insert((lane, (*key).to_owned())))
            .map(str::to_owned)
            .collect()
    }
}

/// The clocks one landing can be measured from, each absent when unrecorded.
#[derive(Clone, Copy, Debug)]
pub(super) struct Clocks {
    /// Instant the creating fact was journaled.
    pub(super) published: wire::Instant,
    /// omp's millisecond receipt stamp on the user message, when the record
    /// carried one.
    pub(super) received_millis: Option<u64>,
    /// Ingest instant of the batch that read the landing record.
    pub(super) landed: Option<wire::Instant>,
}

/// The diagnostic line one first landing earns, or nothing when no recorded
/// clock follows publication.
#[must_use]
pub(super) fn describe(lane: CanonicalUuid, key: &str, clocks: Clocks) -> Option<String> {
    let published = clocks.published.get().as_millisecond();
    let received = clocks
        .received_millis
        .and_then(|millis| i64::try_from(millis).ok());
    let landed = clocks.landed.map(|landed| landed.get().as_millisecond());
    let seconds = |from: i64, to: i64| format!("{:.1}s", (to - from).max(0) as f64 / 1000.0);
    let line = match (received, landed) {
        (Some(received), Some(landed)) => format!(
            "landed after {}: spice {}, lane queue {}",
            seconds(published, landed),
            seconds(published, received),
            seconds(received, landed)
        ),
        (Some(received), None) => format!(
            "reached omp after {}; landing time absent",
            seconds(published, received)
        ),
        (None, Some(landed)) => format!(
            "landed after {}; omp receipt time absent",
            seconds(published, landed)
        ),
        (None, None) => return None,
    };
    Some(format!("lane {lane} steering {key} {line}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane() -> CanonicalUuid {
        "00000000-0000-4000-8000-000000000001"
            .parse()
            .expect("fixture lane")
    }

    fn instant(text: &str) -> wire::Instant {
        text.parse().expect("fixture instant")
    }

    const PUBLISHED: &str = "<seal>\n\
        directive=d1 key=1kJK6N3Rx stage=published expires=never\nfirst\n\
        directive=d2 key=1kJK6M0Vx stage=reminded expires=never\nagain\n</seal>\n";

    #[test]
    fn only_a_published_header_lands_and_only_once() {
        let mut measured = Measured::default();
        assert_eq!(measured.first_landings(lane(), PUBLISHED), ["1kJK6N3Rx"]);
        assert!(
            measured.first_landings(lane(), PUBLISHED).is_empty(),
            "a resend of a measured key is the same direction said again"
        );
        let other: CanonicalUuid = "00000000-0000-4000-8000-000000000002"
            .parse()
            .expect("fixture lane");
        assert_eq!(
            measured.first_landings(other, PUBLISHED),
            ["1kJK6N3Rx"],
            "the same key fanned out to another lane lands there on its own"
        );
    }

    #[test]
    fn a_post_compaction_replay_lands_nothing() {
        let replay = "<seal>\n[POST-COMPACTION] Pending keys: 1kJK6N3Rx\n\
            directive=d1 key=1kJK6N3Rx stage=published expires=never\nfirst\n</seal>\n";
        let mut measured = Measured::default();
        assert!(measured.first_landings(lane(), replay).is_empty());
        assert_eq!(
            measured.first_landings(lane(), PUBLISHED),
            ["1kJK6N3Rx"],
            "a replay does not spend the key's first landing"
        );
    }

    #[test]
    fn the_line_splits_the_wait_at_omp_receipt() {
        let clocks = Clocks {
            published: instant("2030-04-12T18:44:09.000Z"),
            received_millis: Some(1_902_249_849_300),
            landed: Some(instant("2030-04-12T18:44:40.900Z")),
        };
        assert_eq!(
            describe(lane(), "1kJK6N3Rx", clocks).as_deref(),
            Some(
                "lane 00000000-0000-4000-8000-000000000001 steering 1kJK6N3Rx landed after \
                 31.9s: spice 0.3s, lane queue 31.6s"
            )
        );
    }

    #[test]
    fn a_missing_clock_is_named_not_invented() {
        let published = instant("2030-04-12T18:44:09.000Z");
        let only_landed = Clocks {
            published,
            received_millis: None,
            landed: Some(instant("2030-04-12T18:44:12.000Z")),
        };
        assert!(
            describe(lane(), "k", only_landed)
                .expect("a landing clock measures")
                .ends_with("landed after 3.0s; omp receipt time absent")
        );
        let only_received = Clocks {
            published,
            received_millis: Some(1_902_249_849_500),
            landed: None,
        };
        assert!(
            describe(lane(), "k", only_received)
                .expect("a receipt clock measures")
                .ends_with("reached omp after 0.5s; landing time absent")
        );
        let none = Clocks {
            published,
            received_millis: None,
            landed: None,
        };
        assert_eq!(describe(lane(), "k", none), None);
    }

    #[test]
    fn a_clock_behind_publication_reads_as_no_wait() {
        let clocks = Clocks {
            published: instant("2030-04-12T18:44:09.000Z"),
            received_millis: Some(1_902_249_848_000),
            landed: Some(instant("2030-04-12T18:44:09.000Z")),
        };
        assert!(
            describe(lane(), "k", clocks)
                .expect("measures")
                .ends_with("landed after 0.0s: spice 0.0s, lane queue 1.0s")
        );
    }
}
