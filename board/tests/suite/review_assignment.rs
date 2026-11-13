//! Reviewers are the review phase's claims, seated like any other phase.
//!
//! Nothing beside the claims gates who may take a review row: once a claim
//! ends, every lane may take it, and while claims are live, the round's target
//! alone decides whether another lane may join them.

use board::Entry;
use wire::{
    Actor, CanonicalUuid, ClaimDisposition, ClaimHead, Instant, LeaseHead, Phase, Seq, TaskHead,
    TaskStamp, WorkflowHead,
};

fn stamp(text: &str) -> TaskStamp {
    let value = text.bytes().fold(0_i64, |value, byte| {
        (value * 131 + i64::from(byte)) % 10_000_000_000_000
    });
    TaskStamp::for_micros(value).expect("fixture name fits the task stamp codec")
}

fn phase(text: &str) -> Phase {
    text.parse().expect("phase")
}

fn uuid(text: &str) -> CanonicalUuid {
    text.parse().expect("canonical UUID")
}

fn instant(text: &str) -> Instant {
    text.parse().expect("canonical instant")
}

fn seq(value: u64) -> Seq {
    Seq::new(value).expect("non-zero sequence")
}

fn environment() -> wire::Environment {
    wire::Environment::journal_schema(journal::SCHEMA_VERSION)
        .expect("the compiled journal schema is positive")
}

fn drain() -> board::Allocation {
    super::support::allocation(uuid("019fbbde-5105-75a3-a76a-722afde1e408"), true)
}

fn reviewer() -> Actor {
    Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e4a1"))
}

fn newcomer() -> Actor {
    Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e4b2"))
}

/// One review row reviewed by `reviewer()`, whose claim is in `disposition`,
/// in a round that wants `target` reviewers.
fn review_row(disposition: ClaimDisposition, target: u32) -> Entry {
    let statement = TaskHead {
        preparation: None,
        intent: None,
        born: stamp("BOARD-r1"),
        topic_origin: seq(1),
        title: "Review the seated row".to_owned(),
        pitch: "A seated review may still want another reviewer.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: "/board/kernel".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: wire::TaskProvenance::Task(stamp("BOARD-r1")),
        creation: wire::body::TaskCreationContext::unavailable(
            wire::body::TaskCreationUnavailable::SpiceActor,
        ),
        revision: seq(1),
    };
    let workflow = WorkflowHead {
        flow: vec![phase("draft"), phase("todo"), phase("review")],
        phase: phase("review"),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    };
    let claim = ClaimHead {
        actor: reviewer(),
        fact: seq(3),
        claimed: seq(3),
        cell: wire::ClaimCell {
            topic: Some("/board/kernel".parse().expect("fixture topic")),
            phase: phase("review"),
        },
        lane: uuid("019fbbde-5105-75a3-a76a-722afde1e408"),
        occupant: uuid("019fbbde-5105-75a3-a76a-722afde1e409"),
        epoch: uuid("019fbbde-5105-75a3-a76a-722afde1e410"),
        recovery: None,
        disposition,
        acknowledged: 0,
    };
    let lease = LeaseHead {
        claimed: seq(3),
        start: instant("2026-08-01T06:00:00.000Z"),
        renewal: instant("2026-08-01T06:00:00.000Z"),
        expiry: instant("2026-08-01T06:10:00.000Z"),
        maximum: instant("2026-08-01T06:30:00.000Z"),
        epoch: uuid("019fbbde-5105-75a3-a76a-722afde1e410"),
    };
    let mut completion = wire::CompletionHead::opened(seq(1));
    completion.set_target(
        phase("review"),
        std::num::NonZeroU32::new(target).expect("nonzero target"),
    );
    Entry::new(statement, workflow, completion, vec![(claim, Some(lease))]).expect("review row")
}

/// Whether `actor` may take `row` by allocation, capture, and steal.
fn routes(row: &Entry, actor: &Actor) -> [bool; 3] {
    let satisfied = board::resolved_tasks(std::slice::from_ref(row));
    [
        board::select(std::slice::from_ref(row), actor, &drain(), &environment()).is_some(),
        board::capturable_entry(&satisfied, row, actor, &environment()),
        board::stealable_entry(&satisfied, row, actor, &environment()),
    ]
}

#[test]
fn an_ended_review_claim_admits_every_lane() {
    for disposition in [ClaimDisposition::Freed, ClaimDisposition::Lapsed] {
        let row = review_row(disposition, 1);
        for actor in [newcomer(), reviewer()] {
            assert_eq!(
                routes(&row, &actor),
                [true; 3],
                "{actor} after a {disposition:?} review claim"
            );
        }
    }
}

#[test]
fn a_sole_live_review_claim_yields_only_to_a_steal() {
    let row = review_row(ClaimDisposition::Active, 1);
    assert_eq!(routes(&row, &newcomer()), [false, false, true]);
}

#[test]
fn a_shared_review_round_seats_another_lane_beside_a_live_reviewer() {
    let row = review_row(ClaimDisposition::Active, 2);
    assert_eq!(
        routes(&row, &newcomer()),
        [true; 3],
        "a second lane joins the live reviewer"
    );
    assert_eq!(
        routes(&row, &reviewer())[..2],
        [false; 2],
        "a seated reviewer is never offered a second seat"
    );
}
