//! One pass decision as immutable room history and, when it carries
//! feedback, as the active Design holder's next input.

use std::collections::BTreeMap;

use serde_json::Value;

use super::contribution::history;
use super::*;
use crate::board_command::{owed_returns, record_return};

fn admit_principal(bus: &Bus, epoch: CanonicalUuid, principal: CanonicalUuid) {
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("resident fixture origin");
    let now = Instant::new(jiff::Timestamp::now()).expect("current fixture instant");
    let mut write = JournalWrite::new(Actor::Spice, origin, now, Producer::CommandDoor);
    let roles = std::collections::BTreeSet::from([wire::principal::PrincipalRole::Operator]);
    let born = write
        .fact(
            wire::principal::PrincipalAdmitted {
                principal,
                profile: wire::principal::PrincipalProfile::default(),
                roles: roles.clone(),
                capability: None,
            },
            FactCause::Root,
        )
        .expect("principal birth fact builds");
    write.principal_head(born, move |born| wire::principal::PrincipalHead {
        principal,
        born,
        profile: wire::principal::PrincipalProfile::default(),
        roles,
        capability: None,
    });
    assert!(matches!(
        bus.commit(write).expect("principal birth commits"),
        CommitOutcome::Committed(_)
    ));
}

struct Returnable {
    root: FixtureRoot,
    bus: Bus,
    epoch: CanonicalUuid,
    seats: Vec<Seat>,
    design: TaskHandle,
    outputs: [TaskHandle; 2],
}

impl Returnable {
    fn new(name: &str, lanes: usize) -> Self {
        let (root, bus, epoch, seats) = fixture(name, lanes);
        let design = add_controller(&bus, epoch, name, wire::TaskIntent::Design, None);
        capture_design(&bus, epoch, &seats[0], &design);
        let outputs = [
            add_plan_output(&bus, epoch, &design, "First output"),
            add_plan_output(&bus, epoch, &design, "Second output"),
        ];
        assert_committed(end_pass(&bus, epoch, &seats[0], &design));
        Self {
            root,
            bus,
            epoch,
            seats,
            design,
            outputs,
        }
    }

    fn decide(&self, outcome: DecisionOutcome) -> Outcome<CommandAnswer> {
        decide(&self.bus, self.epoch, &self.design, 1, outcome)
    }

    fn returned(reason: &str) -> DecisionOutcome {
        DecisionOutcome::Returned {
            reason: reason.to_owned(),
        }
    }

    fn rejected(&self) -> DecisionOutcome {
        DecisionOutcome::Resolved {
            outputs: vec![
                OutputDecision {
                    output: self.outputs[0].born().expect("first output stamp"),
                    verdict: OutputVerdict::Rejected,
                    comment: Some("revise the boundary".to_owned()),
                },
                OutputDecision {
                    output: self.outputs[1].born().expect("second output stamp"),
                    verdict: OutputVerdict::Rejected,
                    comment: None,
                },
            ],
            comment: Some("keep the useful evidence".to_owned()),
        }
    }

    fn occupied(&self) -> BTreeMap<CanonicalUuid, CanonicalUuid> {
        self.seats
            .iter()
            .map(|seat| (seat.lane, seat.occupant))
            .collect()
    }

    fn tell(
        &self,
        occupied: &BTreeMap<CanonicalUuid, CanonicalUuid>,
    ) -> Vec<(CanonicalUuid, String)> {
        let owed = owed_returns(&self.bus, occupied).expect("owed input reads");
        for owed in &owed {
            record_return(&self.bus, owed).expect("delivery records");
        }
        owed.into_iter()
            .map(|owed| (owed.lane, owed.reason))
            .collect()
    }

    fn free(&self, seat: usize) {
        let seat = &self.seats[seat];
        free_active(
            &self.bus,
            self.epoch,
            &Actor::Lane(seat.lane),
            seat,
            &self.design,
        );
    }

    /// Capture the Design by name for `seat`, as a lane told to take it does.
    fn claim(&self, seat: usize) {
        capture_for(&self.bus, self.epoch, &self.seats[seat], &self.design);
    }

    fn decision_outcomes(&self) -> Vec<Value> {
        history(
            self.root.as_ref(),
            self.root.as_ref(),
            &[self.design.as_str()],
        )["decisions"]["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| row["outcome"].clone())
            .collect()
    }

    fn decision_actors(&self) -> Vec<Actor> {
        history(
            self.root.as_ref(),
            self.root.as_ref(),
            &[self.design.as_str()],
        )["decisions"]["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| {
                serde_json::from_value(row["actor"].clone()).expect("decision actor decodes")
            })
            .collect()
    }

    fn finish(self) {
        clean(&self.root, self.bus);
    }
}

#[test]
fn a_returned_pass_records_feedback_without_authorizing_work() {
    let fixture = Returnable::new("design-returned-pass", 1);
    let returned = Returnable::returned("revise the premise");
    assert_committed(fixture.decide(returned.clone()));
    assert_eq!(
        fixture.decision_outcomes(),
        [serde_json::to_value(returned).expect("decision encodes")]
    );
    let current = board(&fixture.bus, fixture.epoch, Actor::TEST_PRINCIPAL);
    for output in &fixture.outputs {
        assert!(at(&current, output).workflow.body.is_draft());
    }
    fixture.finish();
}

#[test]
fn rejected_output_and_overall_comments_reach_the_holder_once() {
    let fixture = Returnable::new("design-resolved-feedback", 1);
    let rejected = fixture.rejected();
    assert_committed(fixture.decide(rejected.clone()));
    fixture.claim(0);
    assert_eq!(
        fixture.decision_outcomes(),
        [serde_json::to_value(rejected).expect("decision encodes")]
    );
    let heard = fixture.tell(&fixture.occupied());
    assert_eq!(heard.len(), 1);
    assert_eq!(heard[0].0, fixture.seats[0].lane);
    assert!(heard[0].1.contains("rejected output"));
    assert!(heard[0].1.contains("revise the boundary"));
    assert!(heard[0].1.contains("overall: keep the useful evidence"));
    assert_eq!(
        fixture.tell(&fixture.occupied()),
        [],
        "restart reconciliation does not redeliver consumed feedback"
    );
    fixture.finish();
}

#[test]
fn room_history_keeps_the_principal_that_authored_the_decision() {
    let fixture = Returnable::new("design-decision-author", 1);
    assert_committed(fixture.decide(fixture.rejected()));
    assert_eq!(fixture.decision_actors(), [Actor::TEST_PRINCIPAL]);

    let first: CanonicalUuid = "018f82d4-6f59-7d06-92e3-bce9131163f0"
        .parse()
        .expect("fixture principal");
    admit_principal(&fixture.bus, fixture.epoch, first);
    assert_eq!(fixture.decision_actors(), [Actor::TEST_PRINCIPAL]);

    admit_principal(
        &fixture.bus,
        fixture.epoch,
        "018f82d4-6f59-7d06-92e3-bce9131163a0"
            .parse()
            .expect("fixture principal"),
    );
    assert_eq!(
        fixture.decision_actors(),
        [Actor::TEST_PRINCIPAL],
        "later principal births cannot move historical authorship"
    );
    fixture.finish();
}

#[test]
fn feedback_remains_owed_when_the_claim_moves_before_consumption() {
    let fixture = Returnable::new("design-feedback-claim-race", 2);
    assert_committed(fixture.decide(fixture.rejected()));
    fixture.claim(0);
    let occupied = fixture.occupied();
    let stale = owed_returns(&fixture.bus, &occupied)
        .expect("owed input reads")
        .pop()
        .expect("first holder is owed input");
    fixture.free(0);
    // Freeing before the round completes abandons the pass; the next lane
    // takes the Design by name.
    fixture.claim(1);
    assert!(record_return(&fixture.bus, &stale).is_err());
    assert_eq!(
        fixture.tell(&occupied).first().map(|(lane, _)| *lane),
        Some(fixture.seats[1].lane)
    );
    fixture.finish();
}

#[test]
fn a_collapse_decision_uses_the_operator_conclusion_path() {
    let fixture = Returnable::new("design-collapse", 1);
    assert_committed(fixture.decide(DecisionOutcome::Collapsed {
        reason: "the premise did not hold".to_owned(),
    }));
    let current = board(&fixture.bus, fixture.epoch, Actor::TEST_PRINCIPAL);
    let workflow = &at(&current, &fixture.design).workflow.body;
    assert!(workflow.status.is_completed());
    assert!(!workflow.concluded_positively());
    for output in &fixture.outputs {
        assert!(at(&current, output).workflow.body.status.is_completed());
    }
    fixture.finish();
}
