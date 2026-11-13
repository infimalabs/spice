//! A shared round closes once from durable arrivals its completion head owns.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::num::NonZeroU32;

use board::round::{Closure, RoundError, RoundEvidence};
use board::{Entry, TransitionKind};
use wire::{
    Actor, AnyFact, CanonicalUuid, ClaimDisposition, ClaimHead, CompletionHead, Instant, Phase,
    Seq, TaskHead, TaskStamp, WorkflowHead, body,
};

const ARRIVED: &str = "task::arrived";

fn seq(value: u64) -> Seq {
    Seq::new(value).expect("non-zero sequence")
}

fn phase(text: &str) -> Phase {
    text.parse().expect("phase")
}

fn uuid(last: u64) -> CanonicalUuid {
    format!("019fbbde-5105-75a3-a76a-722afde1e{last:03}")
        .parse()
        .expect("canonical UUID")
}

fn at() -> Instant {
    "2026-08-01T06:00:00.000Z"
        .parse()
        .expect("canonical instant")
}

fn task() -> TaskStamp {
    TaskStamp::for_micros(1_000).expect("fixture stamp")
}

fn statement() -> TaskHead {
    TaskHead {
        preparation: None,
        intent: None,
        born: task(),
        topic_origin: seq(1),
        title: "Close a shared round".to_owned(),
        pitch: "Every participant must agree before the phase moves.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: "/board/kernel".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: wire::TaskProvenance::Task(task()),
        creation: body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::SpiceActor),
        revision: seq(1),
    }
}

/// A todo round needing two lanes, with `arrivals` accepted and `held` claims
/// still active.
fn entry(arrivals: u64, held: &[u64]) -> Entry {
    let mut completion = CompletionHead::opened(seq(1));
    completion.set_target(phase("todo"), NonZeroU32::new(2).expect("two lanes"));
    completion.arrivals = (0..arrivals)
        .map(|index| wire::Arrival {
            claimed: seq(2 + index),
            actor: Actor::Lane(uuid(index)),
            lane: uuid(index),
            fact: seq(10 + index),
        })
        .collect();
    let claims = held
        .iter()
        .map(|&index| {
            let claim = ClaimHead {
                actor: Actor::Lane(uuid(index)),
                fact: seq(2 + index),
                claimed: seq(2 + index),
                cell: wire::ClaimCell {
                    topic: Some("/board/kernel".parse().expect("fixture topic")),
                    phase: phase("todo"),
                },
                lane: uuid(index),
                occupant: uuid(50 + index),
                epoch: uuid(80 + index),
                recovery: None,
                disposition: ClaimDisposition::Active,
                acknowledged: 0,
            };
            (claim, None)
        })
        .collect();
    let workflow = WorkflowHead {
        flow: vec![
            phase("draft"),
            phase("todo"),
            phase("design"),
            phase("review"),
        ],
        phase: phase("todo"),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    };
    Entry::new(statement(), workflow, completion, claims).expect("valid heads")
}

fn arrived(index: u64, disposition: Option<body::PlanDisposition>) -> body::TaskArrived {
    body::TaskArrived {
        task: task(),
        operation: None,
        claimed: seq(2 + index),
        lane: uuid(index),
        occupant: uuid(50 + index),
        phase: phase("todo"),
        round: seq(1),
        validation: format!("participant {index} verified"),
        git: None,
        source: body::SourceBoundary::Proven { heads: Vec::new() },
        disposition,
    }
}

fn fact<T: serde::Serialize>(kind: &str, body: &T) -> AnyFact {
    let encoded = wire::encode(body).expect("fixture body encodes");
    let mut bytes = format!("{{\"kind\":\"{kind}\",").into_bytes();
    bytes.extend_from_slice(&encoded[1..]);
    wire::decode(&bytes).expect("fixture fact decodes")
}

fn reader<'a>(
    facts: &'a BTreeMap<Seq, AnyFact>,
) -> impl FnMut(Seq) -> Result<Option<&'a AnyFact>, Infallible> {
    |position| Ok(facts.get(&position))
}

fn no_work() -> Option<body::PlanDisposition> {
    Some(body::PlanDisposition::NoWork {})
}

fn duplicate() -> Option<body::PlanDisposition> {
    Some(body::PlanDisposition::Duplicate {
        cover: body::DispositionCover::Task(task()),
    })
}

/// The first arrival disposes the Plan; the second is `second`.
fn evidence(second: Option<AnyFact>) -> BTreeMap<Seq, AnyFact> {
    let mut facts = BTreeMap::from([(seq(10), fact(ARRIVED, &arrived(0, no_work())))]);
    facts.extend(second.map(|second| (seq(11), second)));
    facts
}

fn second_arrival(change: impl FnOnce(&mut body::TaskArrived)) -> Option<AnyFact> {
    let mut second = arrived(1, None);
    change(&mut second);
    Some(fact(ARRIVED, &second))
}

#[test]
fn a_round_closes_only_once_satisfied_and_moves_as_its_arrivals_agree() {
    let unread = |_: Seq| -> Result<Option<AnyFact>, &str> { Err("evidence was read") };
    for (label, open) in [
        ("below target", entry(1, &[])),
        ("still held", entry(2, &[9])),
    ] {
        let owed = Closure::owed(&open, at(), unread).expect("an open round reads nothing");
        assert!(owed.is_none(), "{label} must stay open");
    }
    assert!(entry(1, &[1]).round_satisfied(Some(&Actor::Lane(uuid(1)))));
    assert!(!entry(1, &[1, 9]).round_satisfied(Some(&Actor::Lane(uuid(1)))));

    let facts = evidence(second_arrival(|_| {}));
    let closure = Closure::owed(&entry(2, &[]), at(), reader(&facts))
        .expect("agreeing arrivals")
        .expect("a satisfied round closes");
    assert_eq!(
        closure.transition().kind(),
        &TransitionKind::Advance {
            from: phase("todo"),
            to: phase("review"),
        },
        "an arrival's Plan disposition carries the round to review"
    );
}

#[test]
fn every_closure_refuses_arrivals_its_completion_head_disowns() {
    type Expect = fn(&RoundError<Infallible>) -> bool;
    let cases: [(&str, Option<AnyFact>, Expect); 7] = [
        (
            "another task",
            second_arrival(|arrived| {
                arrived.task = TaskStamp::for_micros(2_000).expect("fixture stamp");
            }),
            |error| matches!(error, RoundError::Disagrees(fact) if *fact == seq(11)),
        ),
        (
            "another round",
            second_arrival(|arrived| arrived.round = seq(5)),
            |error| matches!(error, RoundError::Disagrees(fact) if *fact == seq(11)),
        ),
        (
            "another claim",
            second_arrival(|arrived| arrived.claimed = seq(9)),
            |error| matches!(error, RoundError::Disagrees(fact) if *fact == seq(11)),
        ),
        (
            "another lane",
            second_arrival(|arrived| arrived.lane = uuid(7)),
            |error| matches!(error, RoundError::Disagrees(fact) if *fact == seq(11)),
        ),
        (
            "an absent fact",
            None,
            |error| matches!(error, RoundError::Missing(fact) if *fact == seq(11)),
        ),
        (
            "another kind",
            Some(fact(
                "task::phased",
                &body::TaskPhased {
                    task: task(),
                    phase: phase("design"),
                },
            )),
            |error| matches!(error, RoundError::NotArrival(fact) if *fact == seq(11)),
        ),
        (
            "a conflicting disposition",
            second_arrival(|arrived| arrived.disposition = duplicate()),
            |error| matches!(error, RoundError::DispositionConflict),
        ),
    ];
    for (label, second, expected) in cases {
        let facts = evidence(second);
        let refused = Closure::owed(&entry(2, &[]), at(), reader(&facts)).expect_err(label);
        assert!(expected(&refused), "{label}: {refused}");
    }

    let facts = evidence(None);
    let mut evidence =
        RoundEvidence::read(&entry(1, &[1]), reader(&facts)).expect("one agreeing arrival");
    let refused = evidence
        .arrive::<Infallible>(
            body::RoundParticipant {
                claimed: seq(3),
                actor: Actor::Lane(uuid(1)),
                lane: uuid(1),
                occupant: uuid(51),
                validation: "final participant verified".to_owned(),
                git: None,
                source: body::SourceBoundary::Proven { heads: Vec::new() },
            },
            duplicate(),
        )
        .expect_err("the final arrival cannot contradict an earlier disposition");
    assert!(matches!(refused, RoundError::DispositionConflict));
}
