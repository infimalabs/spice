//! A Design awaiting the operator's decision interrupts lanes only at the
//! decision floor, and the decision returns floor ownership to maturity.

use board::similarity::{DECISION_MATCH_FLOOR, WIDE_MATCH_FLOOR};
use wire::rooms::{
    DecisionBasis, DecisionOutcome, DecisionRef, PreparationDecided, PreparationDecision,
    RoomSource,
};

use super::*;

/// A Design statement the lane's completion matches between the unsteeped
/// floor and the decision floor.
const BELOW_DECISION: &str = "Zebras hum lullabies near glaciers. Retained cursors.";
/// A Design statement the lane's completion matches above the decision floor.
const ABOVE_DECISION: &str = "Zebras hum lullabies near glaciers while retained publications page.";
/// The completion recorded while a pass awaits decision.
const COMPLETION: &str = "Zebras sleep.";
/// A later completion of comparable relevance; identical text would add no
/// new profile evidence.
const AFTER_DECISION: &str = "Zebras slept.";

/// `design`'s current statement position.
fn statement(lanes: &Lanes, design: &wire::TaskStamp) -> Seq {
    lanes
        .bus
        .read({
            let design = design.clone();
            move |read| read.task_head(&design)
        })
        .expect("statement reads")
        .expect("the Design exists")
        .position()
}

/// Complete `design`'s pass round, leaving it awaiting decision, and return
/// the completion's position.
fn end_pass(lanes: &Lanes, design: &wire::TaskHandle) -> Seq {
    let task = stamp(design);
    let round = statement(lanes, &task);
    let mut completed =
        write(Actor::Spice, lanes.epoch, Producer::CommandDoor).expect("completion write starts");
    completed
        .fact(
            wire::body::TaskRoundCompleted {
                task,
                phase: "design".parse().expect("design phase"),
                round,
                participants: Vec::new(),
                status: wire::body::TaskCompletionStatus::Pending,
                next: None,
                disposition: None,
            },
            FactCause::Root,
        )
        .expect("round completion fact is valid");
    let committed = commit(&lanes.bus, completed).expect("round completion commits");
    Seq::new(committed.last()).expect("committed position")
}

/// The operator returns the pass at `pass`, leaving no pass awaiting
/// decision.
fn decide(lanes: &Lanes, design: &wire::TaskHandle, pass: Seq) {
    let controller = stamp(design);
    let statement = statement(lanes, &controller);
    let mut decided = write(Actor::TEST_PRINCIPAL, lanes.epoch, Producer::CommandDoor)
        .expect("decision write starts");
    decided
        .fact(
            PreparationDecided {
                decision: PreparationDecision {
                    decision: DecisionRef {
                        controller,
                        decision: wire::SequenceCode::new(1),
                    },
                    source: RoomSource::Journal {
                        fact: wire::SequenceCode::new(pass.get()),
                    },
                    basis: DecisionBasis {
                        statement,
                        room: None,
                        outputs: Vec::new(),
                    },
                    outcome: DecisionOutcome::Returned {
                        reason: "Ask again after another pass.".to_owned(),
                    },
                },
            },
            FactCause::Root,
        )
        .expect("decision fact is valid");
    commit(&lanes.bus, decided).expect("decision commits");
}

/// The match that admitted FIRST's owed request, if one formed.
fn admitted(lanes: &Lanes, config: &SurfaceConfig) -> Option<wire::rooms::DesignNoticeMatch> {
    Some(lanes.request(config, FIRST)?.admission)
}

#[test]
fn a_match_below_the_decision_floor_waits_for_the_decision() {
    let lanes = Lanes::new("design-request-below-decision-floor");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let design = lanes.design("dVrD245Kd", "Hum near glaciers", BELOW_DECISION);
    let pass = end_pass(&lanes, &design);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());

    let awaiting = lanes.claimed("dVrD245Kc", FIRST);
    lanes.complete(&awaiting, COMPLETION);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let before_decision = admitted(&lanes, &config);
    assert!(
        before_decision.is_none() && lanes.noticed().is_empty(),
        "a match below the decision floor does not interrupt the lane: {before_decision:?}"
    );

    decide(&lanes, &design, pass);
    let decided = lanes.claimed("dVrD245Kg", FIRST);
    lanes.complete(&decided, AFTER_DECISION);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let matching = admitted(&lanes, &config).expect("the decided Design invites again");
    assert_eq!(matching.reach(), DesignReach::Wide);
    assert_eq!(matching.floor(), WIDE_MATCH_FLOOR);
    assert!(
        (WIDE_MATCH_FLOOR..DECISION_MATCH_FLOOR).contains(&matching.score()),
        "comparable evidence scores {} between the maturity and decision floors",
        matching.score()
    );
    seated.release_all();
    lanes.finish();
}

#[test]
fn a_match_above_the_decision_floor_interrupts_while_awaiting_decision() {
    let lanes = Lanes::new("design-request-above-decision-floor");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let design = lanes.design("dVrD245Kd", "Hum near glaciers", ABOVE_DECISION);
    end_pass(&lanes, &design);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();

    let task = lanes.claimed("dVrD245Kc", FIRST);
    lanes.complete(&task, COMPLETION);
    lanes.reconcile_completions(&seated, &mut completions);
    let matching = admitted(&lanes, &config).expect("a strong match still invites");
    assert_eq!(matching.reach(), DesignReach::Wide);
    assert_eq!(
        matching.floor(),
        DECISION_MATCH_FLOOR,
        "the notice records the decision floor that admitted it"
    );
    assert!(matching.score() >= DECISION_MATCH_FLOOR);
    seated.release_all();
    lanes.finish();
}
