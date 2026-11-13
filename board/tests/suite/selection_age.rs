//! Below the match score the ladder ends in topic age: the oldest topic origin
//! wins, then the handle. No claim cell, peer claim or recent work reorders it.

use board::Entry;
use wire::{
    Actor, CanonicalUuid, ClaimDisposition, ClaimHead, Instant, LeaseHead, Phase, Seq, TaskHead,
    TaskPriority, TaskStamp, WorkflowHead,
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

fn all_topics() -> board::Allocation {
    super::support::allocation(uuid("019fbbde-5105-75a3-a76a-722afde1e408"), true)
}

fn workflow(current: &str) -> WorkflowHead {
    WorkflowHead {
        flow: vec![phase("draft"), phase("todo"), phase("review")],
        phase: phase(current),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    }
}

fn statement(name: &str) -> TaskHead {
    TaskHead {
        preparation: None,
        intent: None,
        born: stamp(name),
        topic_origin: seq(1),
        title: format!("Implement {name}"),
        pitch: "Selection follows topic age.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: "/board/kernel".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: wire::TaskProvenance::Task(stamp(name)),
        creation: wire::body::TaskCreationContext::unavailable(
            wire::body::TaskCreationUnavailable::SpiceActor,
        ),
        revision: seq(1),
    }
}

fn lease() -> LeaseHead {
    LeaseHead {
        claimed: seq(2),
        start: instant("2026-08-01T06:00:00.000Z"),
        renewal: instant("2026-08-01T06:00:00.000Z"),
        expiry: instant("2026-08-01T06:10:00.000Z"),
        maximum: instant("2026-08-01T06:30:00.000Z"),
        epoch: uuid("019fbbde-5105-75a3-a76a-722afde1e410"),
    }
}

fn placed(head: TaskHead, current: &str) -> Entry {
    Entry::new(
        head,
        workflow(current),
        wire::CompletionHead::opened(seq(1)),
        Vec::new(),
    )
    .expect("placed candidate")
}

fn placed_entry(name: &str, topic: &str, current: &str, topic_origin: u64) -> Entry {
    let mut head = statement(name);
    head.topic = topic.parse().expect("fixture topic");
    head.topic_origin = seq(topic_origin);
    placed(head, current)
}

/// A peer lane's active claim on a row of its own, held in `topic`/`current`.
fn peer_entry(name: &str, topic: &str, current: &str) -> Entry {
    let mut head = statement(name);
    head.topic = topic.parse().expect("fixture topic");
    let claim = ClaimHead {
        actor: Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e499")),
        fact: seq(2),
        claimed: seq(2),
        cell: wire::ClaimCell {
            topic: Some(topic.parse().expect("fixture topic")),
            phase: phase(current),
        },
        lane: uuid("019fbbde-5105-75a3-a76a-722afde1e408"),
        occupant: uuid("019fbbde-5105-75a3-a76a-722afde1e409"),
        epoch: uuid("019fbbde-5105-75a3-a76a-722afde1e410"),
        recovery: None,
        disposition: ClaimDisposition::Active,
        acknowledged: 0,
    };
    Entry::new(
        head,
        workflow(current),
        wire::CompletionHead::opened(seq(1)),
        vec![(claim, Some(lease()))],
    )
    .expect("active peer")
}

/// Every row scores the same, as a claimant whose profile matches nothing
/// apart from the rest does.
fn tied(_: &Entry) -> f64 {
    0.5
}

/// What the allocator hands `Actor::Spice` under a tied score and under none.
fn selected(entries: &[Entry]) -> [&Entry; 2] {
    let subscription = all_topics();
    let environment = environment();
    let scored = board::select_with_satisfied_at(
        entries,
        board::Claimant {
            actor: &Actor::Spice,
            subscription: &subscription,
            environment: &environment,
            match_score: Some(&tied),
        },
        board::NextConfig::default(),
        &board::resolved_tasks(entries),
        instant("2026-08-01T06:00:00.000Z"),
    )
    .expect("a scored candidate is selectable");
    let unscored = board::select(entries, &Actor::Spice, &subscription, &environment)
        .expect("an unscored candidate is selectable");
    [scored.entry(), unscored.entry()]
}

#[test]
fn among_score_ties_the_older_topic_origin_wins_regardless_of_cell_or_peer_claim() {
    // The older row sits in a different topic and phase, in a cell a peer
    // occupies; the younger row is alone in its cell.
    let older = placed_entry("BOARD-z1", "/board/alpha", "review", 1);
    let crowding = peer_entry("BOARD-p1", "/board/alpha", "review");
    let younger = placed_entry("BOARD-a1", "/board/beta", "todo", 2);

    for entries in [
        [older.clone(), younger.clone(), crowding.clone()],
        [crowding.clone(), younger.clone(), older.clone()],
    ] {
        for choice in selected(&entries) {
            assert_eq!(choice.born(), older.born());
        }
    }
}

#[test]
fn with_no_score_the_ladder_then_the_oldest_then_the_handle_decides() {
    let mut urgent = statement("BOARD-h1");
    urgent.topic = "/board/gamma".parse().expect("fixture topic");
    urgent.topic_origin = seq(9);
    urgent.priority = Some(TaskPriority::High);
    let urgent = placed(urgent, "todo");
    let first = placed_entry("BOARD-a1", "/board/alpha", "todo", 1);
    let second = placed_entry("BOARD-b1", "/board/beta", "review", 1);
    let lower_handle = if first.handle() < second.handle() {
        first.born()
    } else {
        second.born()
    };

    for entries in [
        [first.clone(), second.clone(), urgent.clone()],
        [urgent.clone(), second.clone(), first.clone()],
    ] {
        assert_eq!(selected(&entries)[1].born(), urgent.born());
    }
    for entries in [
        [first.clone(), second.clone()],
        [second.clone(), first.clone()],
    ] {
        assert_eq!(selected(&entries)[1].born(), lower_handle);
    }
}

#[test]
fn offer_order_is_select_repeated_and_never_skips_an_age_gap() {
    let actor = Actor::Spice;
    let subscription = all_topics();
    let environment = environment();
    let entries = vec![
        placed_entry("BOARD-c1", "/board/beta", "review", 3),
        placed_entry("BOARD-a1", "/board/alpha", "todo", 1),
        placed_entry("BOARD-e1", "/board/gamma", "review", 40),
        placed_entry("BOARD-b1", "/board/beta", "todo", 2),
        placed_entry("BOARD-d1", "/board/gamma", "todo", 9),
        peer_entry("BOARD-p1", "/board/alpha", "todo"),
    ];

    let order = board::offer_order(&entries, &actor, &subscription, &environment)
        .into_iter()
        .map(|selection| selection.entry().born().clone())
        .collect::<Vec<_>>();

    let mut remaining = entries.clone();
    let mut repeated = Vec::new();
    while let Some(next) = board::select(&remaining, &actor, &subscription, &environment) {
        let born = next.entry().born().clone();
        remaining.retain(|entry| entry.born() != &born);
        repeated.push(born);
    }
    assert_eq!(order, repeated);
    assert_eq!(
        order,
        [
            stamp("BOARD-a1"),
            stamp("BOARD-b1"),
            stamp("BOARD-c1"),
            stamp("BOARD-d1"),
            stamp("BOARD-e1"),
        ],
    );
}
