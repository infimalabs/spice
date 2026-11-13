//! Recognition transaction races.

use super::*;

#[test]
fn transaction_guard_retries_after_both_key_and_directive_command_races() {
    let fixture = Fixture::new("guard-races");
    let bag = bag(&[("polling", &["polling"], "Wait.")]);
    let record = prose_record(fixture.lane, fixture.occupant, "polling now");
    let attempts = Cell::new(0_u8);

    recognize_with(
        &fixture.bus,
        fixture.epoch,
        &bag,
        &steer::Config::default(),
        None,
        &record,
        |bus, openings, proposal| {
            let attempt = attempts.get();
            attempts.set(attempt + 1);
            if attempt < 2 {
                let opening = &openings[0];
                let (directive, key) = if attempt == 0 {
                    (uuid(80), opening.key.to_string())
                } else {
                    (opening.directive, "00000000x".to_owned())
                };
                append_send(
                    bus,
                    fixture.epoch,
                    fixture.lane,
                    directive,
                    &key,
                    fixture.anchor,
                );
            }
            commit(bus, proposal)
        },
    )
    .expect("guard retries from current authority");

    assert_eq!(attempts.get(), 3, "both collision guards forced a rebuild");
    assert_eq!(fixture.reminders().len(), 1);
    let snapshot = fixture
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("snapshot");
    assert_eq!(
        snapshot
            .facts()
            .iter()
            .filter(|fact| fact.fact.kind() == wire::FactKind::SteerSent)
            .count(),
        2
    );
    steer::Ledger::from_snapshot(&snapshot).expect("all three directives replay distinctly");
    fixture.finish();
}

#[test]
fn transaction_guard_rebuilds_when_compaction_moves_after_recognition() {
    let fixture = Fixture::new("epoch-guard");
    let bag = bag(&[("polling", &["polling"], "Wait.")]);
    let record = prose_record(fixture.lane, fixture.occupant, "polling now");
    let attempts = Cell::new(0_u8);
    let moved = Cell::new(0_u64);

    recognize_with(
        &fixture.bus,
        fixture.epoch,
        &bag,
        &steer::Config::default(),
        None,
        &record,
        |bus, _, proposal| {
            let attempt = attempts.get();
            attempts.set(attempt + 1);
            if attempt == 0 {
                moved.set(
                    append_compaction(
                        bus,
                        fixture.epoch,
                        fixture.lane,
                        fixture.occupant,
                        fixture.anchor,
                    )
                    .get(),
                );
            }
            commit(bus, proposal)
        },
    )
    .expect("epoch movement rebuilds");

    assert_eq!(attempts.get(), 2, "the stale epoch proposal was refused");
    assert_eq!(fixture.reminders()[0].compaction, moved.get());
    fixture.finish();
}

#[test]
fn transaction_guard_treats_a_concurrent_same_content_winner_as_success() {
    let fixture = Fixture::new("content-guard");
    let bag = bag(&[("polling", &["polling"], "Wait.")]);
    let record = prose_record(fixture.lane, fixture.occupant, "polling now");
    let attempts = Cell::new(0_u8);

    recognize_with(
        &fixture.bus,
        fixture.epoch,
        &bag,
        &steer::Config::default(),
        None,
        &record,
        |bus, openings, proposal| {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                append_reminder(
                    bus,
                    fixture.epoch,
                    fixture.lane,
                    fixture.occupant,
                    fixture.anchor,
                    &openings[0].candidate,
                );
            }
            commit(bus, proposal)
        },
    )
    .expect("the concurrent winner suppresses retry work");

    assert_eq!(attempts.get(), 1);
    assert_eq!(
        fixture.reminders().len(),
        1,
        "same content and epoch has one durable winner"
    );
    fixture.finish();
}

#[test]
fn concurrent_adjudication_replay_records_one_judgment_and_one_directive() {
    let fixture = Fixture::new("judgment-content-guard");
    let bag = bag(&[("polling", &["polling"], "Wait.")]);
    let record = prose_record(fixture.lane, fixture.occupant, "polling now");
    let judge_calls = Cell::new(0_u8);
    let commit_attempts = Cell::new(0_u8);
    let mut judge = FunctionAdjudicator(|_: &Candidate| {
        judge_calls.set(judge_calls.get() + 1);
        Ok(judgment(
            true,
            vec![attempt(0, "YES", wire::body::TasteJudgeReply::Yes)],
        ))
    });

    recognize_with(
        &fixture.bus,
        fixture.epoch,
        &bag,
        &steer::Config::default(),
        Some(&mut judge),
        &record,
        |bus, openings, proposal| {
            commit_attempts.set(commit_attempts.get() + 1);
            if commit_attempts.get() == 1 {
                append_judgment(
                    bus,
                    fixture.epoch,
                    fixture.lane,
                    fixture.occupant,
                    fixture.anchor,
                    &openings[0],
                );
            }
            commit(bus, proposal)
        },
    )
    .expect("the concurrent judgment winner suppresses replay work");

    assert_eq!(judge_calls.get(), 1);
    assert_eq!(commit_attempts.get(), 1);
    assert_eq!(fixture.judgments().len(), 1);
    let snapshot = fixture
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("snapshot");
    assert_eq!(
        steer::Ledger::from_snapshot(&snapshot)
            .expect("winner judgment replays")
            .pending(fixture.lane)
            .len(),
        1
    );
    fixture.finish();
}
