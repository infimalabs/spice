//! The claimant's match score ranks right after effective priority and
//! downstream weight: it decides among that group unless a row is overdue, and
//! only the oldest topic origin and the handle follow it.

use board::Entry;
use wire::{
    Actor, CanonicalUuid, ClaimHead, Instant, Phase, Seq, TaskHead, TaskPriority, TaskStamp,
    WorkflowHead,
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

fn environment() -> wire::Environment {
    wire::Environment::journal_schema(journal::SCHEMA_VERSION)
        .expect("the compiled journal schema is positive")
}

fn all_topics() -> board::Allocation {
    super::support::allocation(uuid("019fbbde-5105-75a3-a76a-722afde1e408"), true)
}

fn statement(name: &str, topic: &str, topic_origin: u64) -> TaskHead {
    TaskHead {
        preparation: None,
        intent: None,
        born: stamp(name),
        topic_origin: Seq::new(topic_origin).expect("non-zero sequence"),
        title: format!("Implement {name}"),
        pitch: "Selection must follow the ladder.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: topic.parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: wire::TaskProvenance::Task(stamp(name)),
        creation: wire::body::TaskCreationContext::unavailable(
            wire::body::TaskCreationUnavailable::SpiceActor,
        ),
        revision: Seq::new(1).expect("non-zero sequence"),
    }
}

fn workflow() -> WorkflowHead {
    WorkflowHead {
        flow: vec![phase("draft"), phase("todo"), phase("review")],
        phase: phase("todo"),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    }
}

fn admitted(head: TaskHead, claim: Option<ClaimHead>) -> Entry {
    Entry::new(
        head,
        workflow(),
        wire::CompletionHead::opened(Seq::new(1).expect("non-zero sequence")),
        claim.map(|claim| (claim, None)).into_iter().collect(),
    )
    .expect("placed candidate")
}

fn entry(name: &str, topic: &str, topic_origin: u64, priority: Option<TaskPriority>) -> Entry {
    let mut head = statement(name, topic, topic_origin);
    head.priority = priority;
    admitted(head, None)
}

fn handoff(entry: Entry, until: &str) -> Entry {
    let until = instant(until);
    entry.with_phase_handoff(Some(board::PhaseHandoff::new(Actor::Spice, until, until)))
}

fn now() -> Instant {
    instant("2026-08-01T06:00:00.000Z")
}

/// Select with a score that prefers the named handle and ranks every other row zero.
fn select<'a>(entries: &'a [Entry], favored: Option<&str>) -> &'a Entry {
    let subscription = all_topics();
    let environment = environment();
    let score = |entry: &Entry| f64::from(favored == Some(entry.handle().as_str()));
    let match_score: Option<&dyn Fn(&Entry) -> f64> = favored.is_some().then_some(&score);
    board::select_with_satisfied_at(
        entries,
        board::Claimant {
            actor: &Actor::Spice,
            subscription: &subscription,
            environment: &environment,
            match_score,
        },
        board::NextConfig::default(),
        &board::resolved_tasks(entries),
        now(),
    )
    .expect("one candidate is selectable")
    .entry()
}

/// The favored row wins over `rival` whichever order the slice lists them in.
fn favored_wins(favored: &Entry, rival: &Entry) {
    let handle = favored.handle().as_str().to_owned();
    for entries in [
        [favored.clone(), rival.clone()],
        [rival.clone(), favored.clone()],
    ] {
        let selected = select(&entries, Some(&handle));
        assert_eq!(selected.born(), favored.born(), "favored {handle}");
    }
}

#[test]
fn a_higher_score_beats_an_older_row_in_either_order() {
    let oldest = entry("BOARD-a1", "/board/alpha", 1, None);
    let favored_young = entry("BOARD-z1", "/board/alpha", 40, None);
    favored_wins(&favored_young, &oldest);
}

#[test]
fn priority_weight_and_class_each_beat_any_score() {
    let urgent = entry("BOARD-z1", "/board/alpha", 1, Some(TaskPriority::High));
    let favored = entry("BOARD-a1", "/board/alpha", 1, None);
    let handle = favored.handle().as_str().to_owned();
    let entries = [favored.clone(), urgent.clone()];
    assert_eq!(select(&entries, Some(&handle)).born(), urgent.born());

    let weighted = entry("BOARD-z1", "/board/alpha", 1, None);
    let mut dependent = statement("BOARD-z2", "/board/alpha", 1);
    dependent.dependencies.push(stamp("BOARD-z1"));
    let entries = [favored.clone(), weighted.clone(), admitted(dependent, None)];
    assert_eq!(select(&entries, Some(&handle)).born(), weighted.born());

    let ordinary = entry("BOARD-z1", "/board/alpha", 1, None);
    let favored_last_resort = handoff(favored, "2026-08-01T05:00:00.000Z");
    let entries = [favored_last_resort, ordinary.clone()];
    assert_eq!(select(&entries, Some(&handle)).born(), ordinary.born());
}

#[test]
fn an_overdue_row_beats_any_score_and_overdue_rows_go_oldest_first() {
    let favored = entry("BOARD-a1", "/board/alpha", 1, None)
        .with_topic_origin_at(Some(instant("2026-08-01T05:00:00.000Z")));
    let handle = favored.handle().as_str().to_owned();
    let at_horizon = entry("BOARD-z1", "/board/alpha", 1, None)
        .with_topic_origin_at(Some(instant("2026-08-01T04:00:00.000Z")));
    let overdue = entry("BOARD-z2", "/board/alpha", 1, None)
        .with_topic_origin_at(Some(instant("2026-08-01T03:59:59.999Z")));
    let older_overdue = entry("BOARD-z3", "/board/alpha", 1, None)
        .with_topic_origin_at(Some(instant("2026-08-01T02:00:00.000Z")));

    // Exactly the horizon is not yet overdue: the score decides.
    let entries = [at_horizon, favored.clone()];
    assert_eq!(select(&entries, Some(&handle)).born(), favored.born());

    let entries = [favored.clone(), overdue.clone()];
    assert_eq!(select(&entries, Some(&handle)).born(), overdue.born());

    let entries = [overdue, favored, older_overdue.clone()];
    assert_eq!(select(&entries, Some(&handle)).born(), older_overdue.born());

    // The policy's horizon, not the installed one, draws the tier: under a
    // half-hour horizon the hour-old rival is overdue while the favored
    // half-hour-old row is not.
    let favored = entry("BOARD-a1", "/board/alpha", 1, None)
        .with_topic_origin_at(Some(instant("2026-08-01T05:30:00.000Z")));
    let rival = entry("BOARD-z1", "/board/alpha", 1, None)
        .with_topic_origin_at(Some(instant("2026-08-01T05:00:00.000Z")));
    let entries = [favored.clone(), rival.clone()];
    assert_eq!(select(&entries, Some(&handle)).born(), favored.born());
    let score = |entry: &Entry| f64::from(entry.born() == favored.born());
    let subscription = all_topics();
    let environment = environment();
    let selected = board::select_with_satisfied_at(
        &entries,
        board::Claimant {
            actor: &Actor::Spice,
            subscription: &subscription,
            environment: &environment,
            match_score: Some(&score),
        },
        board::NextConfig::new(std::time::Duration::from_secs(1800)),
        &board::resolved_tasks(&entries),
        now(),
    )
    .expect("one candidate is selectable");
    assert_eq!(selected.entry().born(), rival.born());
}

#[test]
fn a_below_floor_or_absent_score_selects_exactly_what_the_ladder_selects() {
    let handle_first = entry("BOARD-a1", "/board/alpha", 1, None);
    let handle_last = entry("BOARD-z1", "/board/alpha", 1, None);
    let oldest = entry("BOARD-m1", "/board/alpha", 1, None);
    let young = entry("BOARD-a2", "/board/alpha", 40, None);

    for (entries, expected) in [
        ([handle_last, handle_first.clone()], handle_first),
        ([young, oldest.clone()], oldest),
    ] {
        // Every row scoring zero is what a caller's floor hands the board.
        let floored = select(&entries, Some("never-a-handle"));
        let unscored = select(&entries, None);
        assert_eq!(floored.born(), expected.born());
        assert_eq!(unscored.born(), expected.born());
    }
}
