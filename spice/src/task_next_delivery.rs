//! CLI delivery law for an exhausted allocator drain.
//!
//! A steered lane asks the allocator on every turn, so an empty allocator is
//! its expected terminal state rather than a refusal to act on. The rim
//! therefore speaks the empty-drain readout as a reply and exits 0, in place
//! of the typed `board::allocator-empty` refusal every other caller keeps.

use wire::intent::CommandAnswer;
use wire::{Outcome, Reply};

use crate::board_command::{DrainReadout, TaskNext, allocator_empty};

/// Deliver one `task next` result, speaking a steered drain as its readout.
///
/// The readout is prepared by the client from the snapshot it drained, so
/// this returns at once and starts nothing. A steered drain whose readout
/// could not be prepared keeps the earlier silent exit, since the refusal in
/// hand is still the expected one and not a fault.
pub(super) fn deliver(result: &TaskNext, form: super::Form, steered: bool) -> ! {
    match steered_drain(result, steered) {
        Some(readout) => {
            super::deliver_with_reply_text(&Outcome::Reply(readout.clone()), form, drain_text)
        }
        None if expected_empty(&result.outcome, steered) => {
            super::terminate_code(super::REPLY_EXIT)
        }
        None => super::deliver(&result.outcome, form),
    }
}

fn drain_text(readout: &DrainReadout) -> Option<&str> {
    Some(&readout.text)
}

fn steered_drain(result: &TaskNext, steered: bool) -> Option<&Reply<DrainReadout>> {
    expected_empty(&result.outcome, steered)
        .then_some(result.drain.as_ref())
        .flatten()
}

fn expected_empty(outcome: &Outcome<CommandAnswer>, steered: bool) -> bool {
    steered && allocator_empty(outcome)
}

#[cfg(test)]
mod tests {
    use wire::{Fault, FaultClass, Outcome, Refusal, Reply, ReplyPosition, Seq};

    use super::{DrainReadout, TaskNext, expected_empty, steered_drain};
    use crate::board_command::ParkedCounts;

    fn refusal(reason: &str) -> Outcome<wire::intent::CommandAnswer> {
        Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
            reason.to_owned(),
            None,
            None,
            None,
        )))
    }

    fn fault() -> Outcome<wire::intent::CommandAnswer> {
        Outcome::Fault(Fault {
            class: FaultClass::Store,
            reason: "team::subscription-missing".to_owned(),
            detail: "allocator state is incomplete".to_owned(),
        })
    }

    fn readout() -> Reply<DrainReadout> {
        Reply {
            position: ReplyPosition::Snapshot(Seq::new(7).expect("positive fixture cursor")),
            body: DrainReadout {
                text: "no available tasks".to_owned(),
                board: ParkedCounts {
                    active: 0,
                    stale: 0,
                    blocked: 0,
                    workflow_waiting: 0,
                    designs: Default::default(),
                    deferred: 0,
                },
                drive: true,
                drain: false,
            },
        }
    }

    #[test]
    fn steered_task_next_converts_only_expected_allocator_emptiness() {
        assert!(expected_empty(&refusal("board::allocator-empty"), true));
        assert!(!expected_empty(&refusal("board::allocator-empty"), false));
        assert!(!expected_empty(&refusal("claim::lane-unavailable"), true));
        assert!(!expected_empty(&fault(), true));
    }

    #[test]
    fn only_a_steered_allocator_drain_speaks_its_readout() {
        let drained = TaskNext {
            outcome: refusal("board::allocator-empty"),
            drain: Some(readout()),
        };
        assert_eq!(steered_drain(&drained, true), Some(&readout()));
        assert_eq!(steered_drain(&drained, false), None);
        let refused = TaskNext {
            outcome: refusal("claim::lane-unavailable"),
            drain: Some(readout()),
        };
        assert_eq!(steered_drain(&refused, true), None);
        let faulted = TaskNext {
            outcome: fault(),
            drain: Some(readout()),
        };
        assert_eq!(steered_drain(&faulted, true), None);
        let unprepared = TaskNext {
            outcome: refusal("board::allocator-empty"),
            drain: None,
        };
        assert_eq!(steered_drain(&unprepared, true), None);
    }
}
