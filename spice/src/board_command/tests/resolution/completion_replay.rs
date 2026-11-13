use super::*;

#[test]
fn task_completion_terminal_replays_once_after_claim_release_and_restart() {
    let (root, bus, epoch, seats) = fixture("completion-terminal-replay", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DONE-replay001",
        "Replay one durable completion terminal",
        Some("todo"),
    );
    claim_task(&bus, epoch, &seats[0], &lane, &task);
    let held = board(&bus, epoch, lane.clone());
    let operation = TaskDone::operation_for_claim(
        at(&held, &task)
            .latest_claim()
            .map(|held| &held.claim)
            .expect("held task has a claim")
            .body
            .claimed,
    );
    let done = TaskDone {
        operation: Some(operation),
        ..finished(&task, "one focused evidence execution passed")
    };
    let expected = progress_witnesses(
        &held,
        at(&held, &task),
        seats[0].lane,
        seats[0].occupant,
        false,
    )
    .expect("held task has completion witnesses");
    assert_refusal(
        issue(
            &bus,
            epoch,
            lane.clone(),
            CommandKind::TaskDone,
            expected.clone(),
            &TaskDone {
                operation: None,
                ..done.clone()
            },
        ),
        "task::completion-operation-required",
    );
    let outcome = issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskDone,
        expected,
        &done,
    );
    let Outcome::Reply(Reply {
        position: terminal_position @ ReplyPosition::Committed(_),
        body: CommandAnswer::Task(terminal),
    }) = outcome
    else {
        panic!("completion did not return one committed task answer: {outcome:?}");
    };
    assert_eq!(terminal.status, "completed");
    assert!(terminal.completion.is_none());
    assert_eq!(terminal.next.as_deref(), Some("spice task next"));
    assert_eq!(
        at(&board(&bus, epoch, lane.clone()), &task)
            .latest_claim()
            .map(|held| &held.claim)
            .expect("freed claim remains durable")
            .body
            .disposition,
        ClaimDisposition::Freed
    );
    assert_eq!(terminal.ownership.as_ref().unwrap().teams, [seats[0].team]);
    std::fs::write(root.join("spice.toml"), "[team.default]\ndrain = []\n")
        .expect("current policy changes after the committed operation");
    let current = board(&bus, epoch, lane.clone());
    assert!(
        at(&current, &task)
            .ownership
            .as_ref()
            .unwrap()
            .teams
            .is_empty()
    );

    let replay = CompletionReplay {
        epoch,
        seat: &seats[0],
        lane,
        task: &task,
        done: &done,
        terminal_position: &terminal_position,
        terminal: &terminal,
    };
    assert_concurrent_terminal_retries(&bus, &replay);
    assert_terminal_replay_after_restart(&root, bus, &replay);
}

struct CompletionReplay<'a> {
    epoch: CanonicalUuid,
    seat: &'a Seat,
    lane: Actor,
    task: &'a TaskHandle,
    done: &'a TaskDone,
    terminal_position: &'a ReplyPosition,
    terminal: &'a wire::intent::TaskAnswer,
}

fn assert_concurrent_terminal_retries(bus: &Bus, replay: &CompletionReplay<'_>) {
    let settled = board(bus, replay.epoch, replay.lane.clone());
    let expected = progress_witnesses(
        &settled,
        at(&settled, replay.task),
        replay.seat.lane,
        replay.seat.occupant,
        false,
    )
    .expect("freed task retains replay witnesses");
    std::thread::scope(|scope| {
        let retries = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let outcome = issue(
                        bus,
                        replay.epoch,
                        replay.lane.clone(),
                        CommandKind::TaskDone,
                        expected.clone(),
                        replay.done,
                    );
                    let Outcome::Reply(Reply {
                        position,
                        body: CommandAnswer::Task(replayed),
                    }) = outcome
                    else {
                        panic!("terminal retry did not replay: {outcome:?}");
                    };
                    assert_eq!(&position, replay.terminal_position);
                    assert_eq!(&*replayed, replay.terminal);
                })
            })
            .collect::<Vec<_>>();
        for retry in retries {
            retry.join().expect("terminal retry joins");
        }
    });
    assert_eq!(task_done_fact_count(bus, replay.task), 1);
}

fn assert_terminal_replay_after_restart(
    root: &std::path::Path,
    bus: Bus,
    replay: &CompletionReplay<'_>,
) {
    bus.verify().expect("terminal replay authority verifies");
    bus.shutdown().expect("terminal replay bus stops");
    let reopened = journal::Journal::open(root.join("authority.sqlite3"))
        .expect("terminal replay journal reopens");
    let reopened = Bus::start(reopened).expect("terminal replay bus restarts");
    let settled = board(&reopened, replay.epoch, replay.lane.clone());
    let expected = progress_witnesses(
        &settled,
        at(&settled, replay.task),
        replay.seat.lane,
        replay.seat.occupant,
        false,
    )
    .expect("restarted task retains replay witnesses");
    let Outcome::Reply(Reply {
        position,
        body: CommandAnswer::Task(replayed),
    }) = issue(
        &reopened,
        replay.epoch,
        replay.lane.clone(),
        CommandKind::TaskDone,
        expected,
        replay.done,
    )
    else {
        panic!("restart retry did not replay its terminal answer");
    };
    assert_eq!(&position, replay.terminal_position);
    assert_eq!(&*replayed, replay.terminal);
    assert_eq!(task_done_fact_count(&reopened, replay.task), 1);
    clean(root, reopened);
}

fn task_done_fact_count(bus: &Bus, task: &TaskHandle) -> usize {
    let born = task.born().expect("fixture task has a stamp");
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("completion facts read")
    .facts()
    .iter()
    .filter_map(|fact| fact.fact.read::<body::TaskDone>())
    .filter(|done| done.task == born)
    .count()
}
