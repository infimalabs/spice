//! Creation refuses or warns on an independent sibling with the same work.

use super::*;

/// A sibling's birth: every creation below is born this long after it.
const SIBLING_MICROS: i64 = 1_760_000_000_000_000;

fn born_after(seconds: i64) -> TaskStamp {
    TaskStamp::for_micros(SIBLING_MICROS + seconds * 1_000_000).expect("fixture task stamp")
}

fn duplicated() -> TaskAdd {
    authored_add(
        "Refuse racing duplicate task creation",
        "/board/test",
        Some("todo"),
        &["a racing paraphrase is refused before it commits"],
    )
}

fn create(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    born: &TaskStamp,
    add: &TaskAdd,
) -> Outcome<CommandAnswer> {
    issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born.clone() }],
        add,
    )
}

fn handle(born: &TaskStamp) -> TaskHandle {
    TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        born,
    )
}

fn admitted(
    epoch: CanonicalUuid,
    actor: Actor,
    born: &TaskStamp,
    add: &TaskAdd,
) -> wire::AdmittedCommand {
    fixture_command(
        epoch,
        actor,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born.clone() }],
        add,
    )
}

/// The board as one read naming `task` answers it, relations included.
fn named(bus: &Bus, epoch: CanonicalUuid, task: &TaskStamp) -> BoardTask {
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(answer),
        ..
    }) = issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Tasks {
            tasks: vec![task.clone()],
            dependencies: false,
            claims: false,
            history: false,
        },
    )
    else {
        panic!("a named read answers the board");
    };
    answer
        .tasks
        .into_iter()
        .find(|row| row.statement.body.born == *task)
        .expect("the named task is read")
}

/// Board a sibling as a lane, so the operator's creations are independent.
fn sibling(bus: &Bus, epoch: CanonicalUuid, seat: &Seat) -> TaskStamp {
    authorize_lane_creation(bus, epoch, seat);
    let born = born_after(0);
    assert_committed(create(
        bus,
        epoch,
        Actor::Lane(seat.lane),
        &born,
        &duplicated(),
    ));
    born
}

#[test]
fn a_fresh_independent_duplicate_refuses_whole_even_when_related() {
    let (root, bus, epoch, seats) = fixture("duplicate-fresh", 1);
    let first = sibling(&bus, epoch, &seats[0]);
    let facts = fact_count(&bus);
    for related in [Vec::new(), vec![handle(&first)]] {
        let outcome = create(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            &born_after(60),
            &TaskAdd {
                related,
                ..duplicated()
            },
        );
        let Outcome::Refusal(wire::Refusal::Bare { diagnostic }) = outcome else {
            panic!("a fresh duplicate must refuse: {outcome:?}");
        };
        assert_eq!(diagnostic.reason(), "task::duplicate-fresh");
        let observed = diagnostic
            .observed()
            .expect("the refusal names its sibling");
        for named in [
            handle(&first).to_string(),
            "\"Refuse racing duplicate task creation\"".to_owned(),
            Actor::Lane(seats[0].lane).to_string(),
            "cosine 1.00".to_owned(),
            "age 60s".to_owned(),
        ] {
            assert!(observed.contains(&named), "{named} missing from {observed}");
        }
        assert_eq!(fact_count(&bus), facts, "a refused creation writes nothing");
    }
    clean(&root, bus);
}

#[test]
fn a_recent_duplicate_commits_only_with_a_relation_both_tasks_list() {
    let (root, bus, epoch, seats) = fixture("duplicate-related", 1);
    let first = sibling(&bus, epoch, &seats[0]);
    let born = born_after(61);
    let outcome = create(&bus, epoch, Actor::TEST_PRINCIPAL, &born, &duplicated());
    assert!(
        matches!(&outcome, Outcome::Refusal(refusal) if refusal.reason() == "task::duplicate-unrelated"),
        "an unrelated recent duplicate must refuse: {outcome:?}"
    );
    let CommandAnswer::Task(answer) = assert_committed(create(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &born,
        &TaskAdd {
            related: vec![handle(&first)],
            ..duplicated()
        },
    )) else {
        panic!("task add answers its task");
    };
    assert!(
        answer.duplicates.is_empty(),
        "a related sibling is not a duplicate"
    );
    let created = named(&bus, epoch, &born);
    let earlier = named(&bus, epoch, &first);
    assert_eq!(
        created
            .related
            .iter()
            .map(|row| &row.task)
            .collect::<Vec<_>>(),
        [&handle(&first)]
    );
    assert_eq!(
        earlier
            .related
            .iter()
            .map(|row| &row.task)
            .collect::<Vec<_>>(),
        [&handle(&born)]
    );
    assert!(
        created.statement.body.dependencies.is_empty()
            && earlier.statement.body.dependencies.is_empty(),
        "a relation adds no dependency"
    );
    clean(&root, bus);
}

#[test]
fn an_old_duplicate_commits_with_a_warning_naming_it() {
    let (root, bus, epoch, seats) = fixture("duplicate-old", 1);
    let first = sibling(&bus, epoch, &seats[0]);
    let CommandAnswer::Task(answer) = assert_committed(create(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &born_after(15 * 60 + 1),
        &duplicated(),
    )) else {
        panic!("task add answers its task");
    };
    let [warning] = answer.duplicates.as_slice() else {
        panic!("one warning names the old sibling: {:?}", answer.duplicates);
    };
    assert_eq!(warning.task, handle(&first));
    assert_eq!(&*warning.cosine, "1.00");
    assert_eq!(warning.creator, Some(Actor::Lane(seats[0].lane)));
    clean(&root, bus);
}

#[test]
fn a_post_screen_sibling_gets_the_same_duplicate_standing_after_retry() {
    enum Expected {
        Fresh,
        Unrelated,
        Warned,
    }

    for (fixture_name, age, expected) in [
        ("duplicate-race-fresh", 60, Expected::Fresh),
        ("duplicate-race-unrelated", 61, Expected::Unrelated),
        ("duplicate-race-warned", 15 * 60 + 1, Expected::Warned),
    ] {
        let (root, bus, epoch, seats) = fixture(fixture_name, 1);
        let seat = &seats[0];
        authorize_lane_creation(&bus, epoch, seat);
        let first = born_after(0);
        let born = born_after(age);
        let add = duplicated();
        let command = admitted(epoch, Actor::TEST_PRINCIPAL, &born, &add);
        let outcome = mutation::task_add_with(&bus, &command, &add, None, || {
            assert_committed(create(
                &bus,
                epoch,
                Actor::Lane(seat.lane),
                &first,
                &duplicated(),
            ));
        });

        match expected {
            Expected::Fresh => assert!(
                matches!(&outcome, Outcome::Refusal(refusal) if refusal.reason() == "task::duplicate-fresh"),
                "a post-screen fresh sibling must refuse after retry: {outcome:?}"
            ),
            Expected::Unrelated => assert!(
                matches!(&outcome, Outcome::Refusal(refusal) if refusal.reason() == "task::duplicate-unrelated"),
                "a post-screen recent sibling must require a relation after retry: {outcome:?}"
            ),
            Expected::Warned => {
                let CommandAnswer::Task(answer) = assert_committed(outcome) else {
                    panic!("task add answers its task");
                };
                let [warning] = answer.duplicates.as_slice() else {
                    panic!(
                        "one warning names the post-screen sibling: {:?}",
                        answer.duplicates
                    );
                };
                assert_eq!(warning.task, handle(&first));
                assert_eq!(&*warning.cosine, "1.00");
                assert_eq!(warning.creator, Some(Actor::Lane(seat.lane)));
            }
        }
        clean(&root, bus);
    }
}

#[test]
fn the_same_creator_or_distinct_work_never_matches() {
    let (root, bus, epoch, seats) = fixture("duplicate-exempt", 1);
    sibling(&bus, epoch, &seats[0]);
    let CommandAnswer::Task(again) = assert_committed(create(
        &bus,
        epoch,
        Actor::Lane(seats[0].lane),
        &born_after(1),
        &duplicated(),
    )) else {
        panic!("task add answers its task");
    };
    assert!(
        again.duplicates.is_empty(),
        "a lane never duplicates itself"
    );
    let CommandAnswer::Task(distinct) = assert_committed(create(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &born_after(2),
        &authored_add(
            "Render the release ledger as a table",
            "/board/test",
            Some("todo"),
            &["the ledger renders one row per release"],
        ),
    )) else {
        panic!("task add answers its task");
    };
    assert!(
        distinct.duplicates.is_empty(),
        "distinct work is no duplicate"
    );
    clean(&root, bus);
}

#[test]
fn modify_relates_tasks_without_revising_either_statement() {
    let (root, bus, epoch, seats) = fixture("duplicate-modify", 1);
    let first = sibling(&bus, epoch, &seats[0]);
    let other = born_after(30 * 60);
    assert_committed(create(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &other,
        &authored_add(
            "Render the release ledger as a table",
            "/board/test",
            Some("todo"),
            &["the ledger renders one row per release"],
        ),
    ));
    let current = named(&bus, epoch, &other);
    let before = current.statement.body.revision;
    let CommandAnswer::Task(answer) = assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        vec![statement_witness(&current)],
        &TaskEdit {
            related: vec![handle(&first)],
            ..only(&handle(&other))
        },
    )) else {
        panic!("task modify answers its task");
    };
    assert_eq!(answer.status, "related");
    let related = named(&bus, epoch, &other);
    assert_eq!(
        related.statement.body.revision, before,
        "relating revises nothing"
    );
    assert_eq!(
        related
            .related
            .iter()
            .map(|row| &row.task)
            .collect::<Vec<_>>(),
        [&handle(&first)]
    );
    assert_eq!(named(&bus, epoch, &first).related.len(), 1);
    let again = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        vec![statement_witness(&related)],
        &TaskEdit {
            related: vec![handle(&first)],
            ..only(&handle(&other))
        },
    );
    assert!(
        matches!(&again, Outcome::Refusal(refusal) if refusal.reason() == "task::related-unchanged"),
        "an existing relation is not recorded twice: {again:?}"
    );
    clean(&root, bus);
}
