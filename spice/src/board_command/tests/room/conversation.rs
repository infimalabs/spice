//! A task's room read newest first under `task show`'s detail bounds.

use super::*;

/// The board `task show --history` reads for `room`: its row with notes.
fn shown_board(bus: &Bus, epoch: CanonicalUuid, room: &TaskHandle) -> BoardAnswer {
    match issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Tasks {
            tasks: vec![room.born().expect("fixture identity")],
            dependencies: true,
            claims: false,
            history: true,
        },
    ) {
        Outcome::Reply(Reply {
            body: CommandAnswer::Board(answer),
            ..
        }) => *answer,
        other => panic!("task show board read did not reply: {other:?}"),
    }
}

/// `room`'s history page of two rows, resuming at `after` when it names one.
fn page(
    bus: &Bus,
    root: &FixtureRoot,
    answer: &BoardAnswer,
    room: &TaskHandle,
    after: Option<&str>,
) -> String {
    let budget = wire::output::INTERACTIVE
        .budget(wire::output::Request {
            rows: Some(2),
            bytes: None,
            text: None,
        })
        .expect("fixture budget");
    let from = after.map(|cursor| {
        cursor
            .strip_prefix("room:")
            .expect("a room cursor")
            .parse::<RoomAuthorityStamp>()
            .expect("a publication cursor")
    });
    let anchor = room.born().expect("fixture identity");
    let read_anchor = anchor.clone();
    let retain = budget.limits().rows + 1;
    let rows = bus
        .read(move |read| {
            crate::room_command::newest_rows(read, &read_anchor, from.as_ref(), retain)
        })
        .expect("the room reads")
        .expect("the cursor names a publication of the room");
    let artifacts = artifact::Store::new(
        root.as_ref().join("artifacts"),
        root.as_ref().join("locks/task-artifacts.lock"),
    );
    let tape = root.as_ref().join("tape");
    let shown = TaskShowRoom::new(
        rows,
        crate::room_command::SourceOwners {
            tape: &tape,
            artifacts: &artifacts,
            git: &root.as_ref().join("repo"),
        },
        &anchor,
        budget,
    );
    let task = at(answer, room);
    task_show_bounded(
        answer,
        task,
        &[],
        TaskShowHistory::of(answer, task, None),
        None,
        TaskShowLocal {
            maturity: None,
            room: Some(shown),
        },
        &TaskShowOptions {
            full: false,
            context: false,
            history: true,
            limit: Some(2),
            max_bytes: None,
            max_text: None,
            after: after.map(str::to_owned),
        },
    )
    .expect("a room page renders")
}

/// The observation numbers one page shows, in its order.
fn observations(text: &str) -> Vec<char> {
    text.split("| observation-")
        .skip(1)
        .filter_map(|rest| rest.chars().next())
        .collect()
}

/// The cursor a page's continuation resumes at.
fn continuation(text: &str) -> &str {
    text.split("--after ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("the page continues: {text}"))
}

#[test]
fn task_show_pages_its_room_newest_first_from_a_room_cursor() {
    let (root, bus, epoch, _) = fixture("task-show-room-newest", 0);
    let room = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "task-show-room-newest",
        "Room",
        None,
    );
    for index in 1..=5 {
        let command = post_command(&bus, epoch, &room, &format!("observation-{index}"));
        assert_committed(dispatch(&bus, &command).expect("room post dispatches"));
    }
    let answer = shown_board(&bus, epoch, &room);

    // The creation's advance out of draft is the oldest history row, so the
    // first page spends one of its two rows on it.
    let first = page(&bus, &root, &answer, &room, None);
    assert_eq!(observations(&first), ['5'], "{first}");
    assert!(first.contains("withheld 4 of 6 rows"), "{first}");

    let second = page(&bus, &root, &answer, &room, Some(continuation(&first)));
    assert_eq!(observations(&second), ['4', '3'], "{second}");
    assert!(second.contains("withheld 2 of 4 rows"), "{second}");

    let last = page(&bus, &root, &answer, &room, Some(continuation(&second)));
    assert_eq!(observations(&last), ['2', '1'], "{last}");
    assert!(!last.contains("continue:"), "{last}");

    let shown = [first, second, last].concat();
    for index in 1..=5 {
        assert_eq!(
            shown.matches(&format!("observation-{index}")).count(),
            1,
            "each authored observation shows once, under its room: {shown}",
        );
    }
    clean(&root, bus);
}
