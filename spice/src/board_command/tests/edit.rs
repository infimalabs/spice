use super::*;

mod environment;

/// Drive one correction through the door the CLI leaf reaches.
///
/// The witnesses are read from the board immediately before the command, so
/// a scenario that edits twice in a row observes the head the first edit
/// published rather than a stale one.
pub(super) fn correct(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    edit: &TaskEdit,
) -> Outcome<CommandAnswer> {
    let visible = board(bus, epoch, actor.clone());
    let heads = at(&visible, &edit.task);
    let mut expected = vec![statement_witness(heads)];
    if edit.flow.is_some() {
        expected.extend([workflow_witness(heads), completion_witness(heads)]);
    }
    issue(bus, epoch, actor, CommandKind::TaskEdit, expected, edit)
}

fn edited_status(outcome: Outcome<CommandAnswer>) -> String {
    let CommandAnswer::Task(answer) = assert_committed(outcome) else {
        panic!("task edit answers with a task status");
    };
    answer.status
}

/// Facts recording a correction of this task, in journal order.
fn edit_facts(bus: &Bus, task: &TaskHandle) -> Vec<(Actor, u64)> {
    let born = task.born().expect("fixture handle carries a birth stamp");
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("edit facts read")
    .facts()
    .iter()
    .filter(|fact| {
        fact.fact.kind() == wire::FactKind::TaskEdited
            && fact
                .fact
                .body()
                .get("task")
                .and_then(serde_json::Value::as_str)
                == Some(born.as_str())
    })
    .map(|fact| {
        (
            fact.actor.clone(),
            fact.fact
                .body()
                .get("revision")
                .and_then(serde_json::Value::as_u64)
                .expect("an edit fact carries its revision"),
        )
    })
    .collect()
}

/// Shape CASE 146 / INVARIANTS ID-01: re-homing changes only the derived name,
/// so the handle a task was filed under keeps naming it by birth stamp.
#[test]
fn rehoming_rerenders_the_handle_without_migrating_identity() {
    let (_root, bus, epoch, _seats) = fixture("task-edit-rehome-identity", 0);
    let original = add_task_in_topic(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "ALPHA-1kKfYW09x",
        "Move between topics",
        "/board/alpha",
        Some("todo"),
    );
    let born = original
        .born()
        .expect("filed handle carries its sole identity");

    let expected = TaskHandle::for_topic(
        &"/board/beta".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    );
    let CommandAnswer::Task(edited) = assert_committed(correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            topic: Some("/board/beta".parse().expect("fixture topic")),
            ..only(&original)
        },
    )) else {
        panic!("task edit answers with a task status");
    };
    assert_eq!(edited.status, "edited");
    assert_eq!(edited.task, expected);

    let answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let moved = at(&answer, &expected);
    assert_eq!(moved.statement.body.born, born);
    assert_eq!(moved.statement.body.handle, expected);
    let stale = task_heads(&answer, &original).expect("the pre-move handle resolves by stamp");
    assert_eq!(stale.statement.body.handle, expected);
    let CommandAnswer::Task(noted) = assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskNote,
        vec![Witness::Existence { task: born.clone() }],
        &TaskNote {
            task: original.clone(),
            note: "named through the pre-move handle".to_owned(),
        },
    )) else {
        panic!("task note answers with a task status");
    };
    assert_eq!(noted.task, expected, "answers render the current handle");
    let CommandAnswer::Task(retitled) = assert_committed(correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            title: Some("Moved and still addressable".to_owned()),
            ..only(&original)
        },
    )) else {
        panic!("task edit answers with a task status");
    };
    assert_eq!(retitled.task, expected);
    assert_eq!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &expected).presentation_title(),
        "Moved and still addressable",
    );
    let stored = statement_head(&bus, &expected);
    let stored = serde_json::to_value(stored.body).expect("stored head JSON");
    let stored = stored.as_object().expect("stored head object");
    assert!(!stored.contains_key("handle"));
    assert!(!stored.contains_key("uuid"));
}

/// A caller naming a task by its pre-move handle is told the current one; the
/// current handle itself earns no notice.
#[test]
fn a_renamed_handle_tells_the_caller_its_current_name() {
    let born = wire::TaskStamp::for_micros(1_788_810_000_000_000).expect("fixture stamp");
    let named = TaskHandle::for_topic(
        &"/canvas/work"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &born,
    );
    let current = TaskHandle::for_topic(
        &"/canvas/chrome"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &born,
    );
    let notice = rename_notice(&named, &current).expect("a renamed handle earns a notice");
    assert!(
        notice.contains(&format!("{named} is now {current}")),
        "{notice}"
    );
    assert_eq!(rename_notice(&current, &current), None);
}

/// A filed statement is correctable, and the correction is durable and
/// attributable.
///
/// Two different principals correct the same statement in turn, because one
/// editor cannot separate recorded attribution from a constant: a fact that
/// always spelled `Operator` would pass a single-editor test. The second
/// edit is also what makes the first one durable in the interesting sense --
/// a later revision must carry forward text it did not name, not silently
/// drop it.
#[test]
fn a_correction_publishes_a_new_revision_and_names_each_editor() {
    let (_root, bus, epoch, seats) = fixture("task-edit", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add_task_with_term(
        &bus,
        epoch,
        "EDITED-1kKfYW01x",
        "Original title",
        "/board/test",
        Some("todo"),
        &["the clause as first filed"],
    );
    let filed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let before = at(&filed, &task).statement.clone();
    assert_eq!(before.body.description, None);

    assert_eq!(
        edited_status(correct(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            &TaskEdit {
                description: Some("what the task actually means".to_owned()),
                ..only(&task)
            },
        )),
        "edited"
    );
    let replacement = vec![
        "a clause a reviewer can check".to_owned(),
        "and a second the first one missed".to_owned(),
    ];
    let items = wire::StatementItem::each(&replacement).collect::<Vec<_>>();
    assert_eq!(
        edited_status(correct(
            &bus,
            epoch,
            lane.clone(),
            &TaskEdit {
                term: Some(replacement),
                ..only(&task)
            },
        )),
        format!(
            "edited; item handles {},{}",
            items[0].handle(&items),
            items[1].handle(&items),
        )
    );

    let corrected = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&corrected, &task).clone();
    let after = row.statement.clone();
    assert_eq!(
        after.body.description.as_deref(),
        Some("what the task actually means"),
        "the term edit must carry the earlier description forward",
    );
    assert_eq!(
        after.body.term,
        vec![
            "a clause a reviewer can check".to_owned(),
            "and a second the first one missed".to_owned(),
        ],
        "a named term replaces the filed clauses outright",
    );
    assert_eq!(
        row.presentation_title(),
        "Original title",
        "a field no correction named must not move",
    );
    assert_eq!(after.body.topic, before.body.topic);
    assert_eq!(
        after.body.born, before.body.born,
        "a correction edits the task rather than replacing it",
    );
    assert!(
        after.position > before.position,
        "a correction publishes a new statement head: {:?} then {:?}",
        before.position,
        after.position,
    );

    let editors = edit_facts(&bus, &task);
    assert_eq!(
        editors.iter().map(|(actor, _)| actor).collect::<Vec<_>>(),
        vec![&Actor::TEST_PRINCIPAL, &lane],
        "each correction records the principal that made it",
    );
    assert!(
        editors[0].1 < editors[1].1,
        "the recorded revision advances with each correction: {editors:?}",
    );
}

#[test]
fn a_correction_cannot_overwrite_a_competing_statement_head() {
    let (_root, bus, epoch, seats) = fixture("task-edit-stale-head", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "EDITED-1kKfYW09x",
        "Original title",
        Some("todo"),
    );
    let filed = board(&bus, epoch, lane.clone());
    let stale = statement_witness(at(&filed, &task));
    assert_eq!(
        edited_status(correct(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            &TaskEdit {
                title: Some("Competing title".to_owned()),
                ..only(&task)
            },
        )),
        "edited"
    );

    let outcome = issue(
        &bus,
        epoch,
        lane,
        CommandKind::TaskEdit,
        vec![stale],
        &TaskEdit {
            description: Some("The stale editor's description".to_owned()),
            ..only(&task)
        },
    );
    assert!(
        matches!(
            &outcome,
            Outcome::Refusal(refusal) if refusal.reason() == "board::witness-moved"
        ),
        "a stale TaskEdit must refuse: {outcome:?}"
    );
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let current = at(&current, &task);
    assert_eq!(current.presentation_title(), "Competing title");
    assert_eq!(
        current.statement.body.description, None,
        "the stale edit cannot overwrite any statement field"
    );
    assert_eq!(
        edit_facts(&bus, &task).len(),
        1,
        "only the competing edit was recorded"
    );
}

#[test]
fn a_live_claimed_correction_moves_neither_claim_nor_lease() {
    let (_root, bus, epoch, seats) = fixture("task-edit-claim-notice", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add_task_with_term(
        &bus,
        epoch,
        "EDITED-1kKfYW11x",
        "Original title",
        "/board/test",
        Some("todo"),
        &["original acceptance"],
    );
    claim_task(&bus, epoch, &seats[0], &lane, &task);
    let held = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let before = at(&held, &task).clone();

    let corrected_item = wire::StatementItem::of("corrected acceptance");
    assert_eq!(
        edited_status(correct(
            &bus,
            epoch,
            lane.clone(),
            &TaskEdit {
                title: Some("Corrected title".to_owned()),
                description: Some("Corrected description".to_owned()),
                term: Some(vec!["corrected acceptance".to_owned()]),
                priority: Some("high".to_owned()),
                topic: Some("/board/identity".parse().expect("fixture topic")),
                ..only(&task)
            },
        )),
        format!(
            "edited; item handles {}",
            corrected_item.handle(std::slice::from_ref(&corrected_item))
        ),
    );

    let handle = TaskHandle::for_topic(
        &"/board/identity"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &task.born().expect("task stamp"),
    );
    let corrected = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let moved = at(&corrected, &handle);
    assert_holder_kept(
        moved,
        &before
            .latest_claim()
            .map(|held| &held.claim)
            .expect("claim head")
            .body,
    );
    assert_eq!(
        moved.latest_claim().and_then(|held| held.lease.as_ref()),
        before.latest_claim().and_then(|held| held.lease.as_ref()),
        "the edit preserves the lease head"
    );
}

/// A settled statement is history, so correcting it is refused.
#[test]
fn a_settled_task_refuses_a_correction() {
    let (_root, bus, epoch, seats) = fixture("task-edit-settled", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "EDITED-1kKfYW02x",
        "Settled work",
        Some("todo"),
    );
    claim_and_complete_task(&bus, epoch, &seats[0], &lane, &task);
    let settled = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&settled, &task).workflow.body.status.is_completed());
    let before = at(&settled, &task).statement.clone();

    let outcome = correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            title: Some("Rewrite what already shipped".to_owned()),
            ..only(&task)
        },
    );
    assert!(
        matches!(
            outcome,
            Outcome::Refusal(ref refusal) if refusal.reason() == "task::settled"
        ),
        "{outcome:?}"
    );
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&after, &task).presentation_title(), "Settled work");
    assert_eq!(at(&after, &task).statement.position, before.position);
    assert!(edit_facts(&bus, &task).is_empty());
}

/// A correction naming no field is refused rather than published.
///
/// Publishing it would cost a revision and an entry in a record other lanes
/// read, for a statement nobody changed, so the position must not move.
#[test]
fn a_correction_naming_no_field_is_refused() {
    let (_root, bus, epoch, _seats) = fixture("task-edit-empty", 1);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "EDITED-1kKfYW03x",
        "Filed correctly the first time",
        Some("todo"),
    );
    let filed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let before = at(&filed, &task).statement.clone();

    let outcome = correct(&bus, epoch, Actor::TEST_PRINCIPAL, &only(&task));
    assert!(
        matches!(
            outcome,
            Outcome::Refusal(ref refusal) if refusal.reason() == "task::edit-empty"
        ),
        "{outcome:?}"
    );
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&after, &task).statement.position,
        before.position,
        "a refused correction must not cost a revision",
    );
    assert!(edit_facts(&bus, &task).is_empty());
}

/// A handle no board carries is refused before any head is read.
#[test]
fn a_correction_to_an_unknown_task_is_refused() {
    let (_root, bus, epoch, _seats) = fixture("task-edit-unknown", 1);
    let known = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "EDITED-1kKfYW04x",
        "The only filed task",
        Some("todo"),
    );
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&visible, &known);
    let missing: TaskHandle = "EDITED-1kKfYW05x".parse().expect("absent fixture handle");
    let outcome = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        vec![statement_witness(heads)],
        &TaskEdit {
            title: Some("Correct a task that was never filed".to_owned()),
            ..only(&missing)
        },
    );
    assert!(
        matches!(
            outcome,
            Outcome::Refusal(ref refusal) if refusal.reason() == "task::unknown"
        ),
        "{outcome:?}"
    );
    assert!(edit_facts(&bus, &missing).is_empty());
}

/// The task this actor's allocator would hand out now, and what it scored.
///
/// This is the ranking itself rather than a field read back off the row, so
/// a correction that stored a priority the allocator never scores would
/// leave the answer unmoved.
pub(super) fn allocated(bus: &Bus, epoch: CanonicalUuid, actor: &Actor) -> Option<(String, u64)> {
    let answer = board(bus, epoch, actor.clone());
    let entries = actor_entries(&answer).expect("allocation board entries validate");
    let subscription = allocation_for(&answer, actor);
    board::select(&entries, actor, &subscription, &answer.environment()).map(|selection| {
        (
            selection.entry().handle().to_string(),
            selection.urgency().value(),
        )
    })
}

/// The statement head the journal carries, read without a board answer.
///
/// A recorded revision points at a durable head, so a test proving the
/// record covers a field has to read the record rather than the rendered
/// row that a board answer would hand back.
pub(super) fn statement_head(bus: &Bus, task: &TaskHandle) -> Positioned<TaskHead> {
    let snapshot = bus.task_snapshot().expect("statement head reads");
    let heads = snapshot
        .tasks()
        .iter()
        .find(|heads| {
            task.born()
                .is_ok_and(|stamp| heads.statement().body().born == stamp)
        })
        .expect("the journal carries this task");
    positioned(heads.statement())
}

/// Priority the current statement declares.
fn declared_priority(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &TaskHandle,
) -> Option<wire::TaskPriority> {
    at(&board(bus, epoch, Actor::TEST_PRINCIPAL), task)
        .statement
        .body
        .priority
}

/// A corrected priority moves the allocator's ranking of the task.
///
/// Both tasks are filed without a priority, so the allocator ranks them by
/// handle alone and hands out the lower one. That tie is what makes the
/// test able to fail: a correction the allocator never reads would leave
/// the same handle selected every time, and the filed order would stand
/// through all three corrections.
#[test]
fn a_corrected_priority_moves_the_allocator_ranking_off_the_filed_order() {
    let (_root, bus, epoch, seats) = fixture("task-edit-priority", 1);
    let lane = Actor::Lane(seats[0].lane);
    // The handles decide the filed order: EDITED-1kKg6RB1x sorts first, so
    // the allocator hands it out until a priority says otherwise.
    let first = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "EDITED-1kKg6RB1x",
        "Filed first and ranked first",
        Some("todo"),
    );
    let second = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "EDITED-1kKg6RB2x",
        "Filed second and ranked second",
        Some("todo"),
    );
    let filed = allocated(&bus, epoch, &lane).expect("a filed task is allocatable");
    assert_eq!(
        filed.0,
        first.to_string(),
        "with no priority declared the allocator ranks by handle",
    );

    assert_eq!(
        edited_status(correct(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            &TaskEdit {
                priority: Some("high".to_owned()),
                ..only(&second)
            },
        )),
        "edited"
    );
    let raised = allocated(&bus, epoch, &lane).expect("a corrected task is allocatable");
    assert_eq!(
        raised.0,
        second.to_string(),
        "the correction has to outrank the filed order, not sit beside it",
    );
    assert!(
        filed.1 > raised.1,
        "effective priority outranks the newer task's lower topic age without being folded into it",
    );

    // The first task overtakes it, so the ranking is following each
    // correction rather than having latched onto the second task once.
    assert_eq!(
        edited_status(correct(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            &TaskEdit {
                priority: Some("critical".to_owned()),
                ..only(&first)
            },
        )),
        "edited"
    );
    assert_eq!(
        allocated(&bus, epoch, &lane)
            .expect("a corrected task is allocatable")
            .0,
        first.to_string(),
    );

    // A second correction of the same task replaces its declaration.
    assert_eq!(
        edited_status(correct(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            &TaskEdit {
                priority: Some("none".to_owned()),
                ..only(&first)
            },
        )),
        "edited"
    );
    assert_eq!(
        declared_priority(&bus, epoch, &first),
        Some(wire::TaskPriority::None),
    );
    assert_eq!(
        allocated(&bus, epoch, &lane)
            .expect("a corrected task is allocatable")
            .0,
        second.to_string(),
        "lowering a priority has to give the ranking back",
    );
}

/// A priority the allocator cannot score is refused rather than stored.
///
/// Storing it would rank the task as though it had declared nothing, which
/// is the silent form of the failure the correction exists to repair.
#[test]
fn a_priority_the_allocator_does_not_score_is_refused() {
    let (_root, bus, epoch, _seats) = fixture("task-edit-priority-unknown", 1);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "EDITED-1kKg6RB3x",
        "Filed without a priority",
        Some("todo"),
    );
    let filed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let before = at(&filed, &task).statement.clone();

    let outcome = correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            priority: Some("urgent".to_owned()),
            ..only(&task)
        },
    );
    assert!(
        matches!(
            outcome,
            Outcome::Refusal(ref refusal) if refusal.reason() == "task::edit-priority-unknown"
        ),
        "{outcome:?}"
    );
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&after, &task).statement.position,
        before.position,
        "a refused priority must not cost a revision",
    );
    assert_eq!(declared_priority(&bus, epoch, &task), None);
    assert!(edit_facts(&bus, &task).is_empty());

    // The same word with the case a caller is likely to type is admitted.
    assert_eq!(
        edited_status(correct(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            &TaskEdit {
                priority: Some("  High  ".to_owned()),
                ..only(&task)
            },
        )),
        "edited"
    );
    assert_eq!(
        declared_priority(&bus, epoch, &task),
        Some(wire::TaskPriority::High),
    );

    // Every letter the board renders is a spelling the door takes back, so
    // an agent correcting the row it just read does not have to translate.
    for (letter, stored) in [
        ("C", wire::TaskPriority::Critical),
        ("h", wire::TaskPriority::High),
        ("M", wire::TaskPriority::Medium),
        ("L", wire::TaskPriority::Low),
        ("N", wire::TaskPriority::None),
    ] {
        assert_eq!(
            edited_status(correct(
                &bus,
                epoch,
                Actor::TEST_PRINCIPAL,
                &TaskEdit {
                    priority: Some(letter.to_owned()),
                    ..only(&task)
                },
            )),
            "edited",
            "the board prints {letter}, so the door has to take it",
        );
        assert_eq!(declared_priority(&bus, epoch, &task), Some(stored));
    }
}

/// The revision and attribution a correction publishes cover the widened
/// fields, read off the durable facts rather than the board.
///
/// Each correction here names one widened field and nothing else, so a
/// revision or an editor recorded for prose alone would leave both facts
/// missing. Two different principals make them, because one editor cannot
/// separate recorded attribution from a constant.
#[test]
fn a_priority_or_topic_correction_records_its_revision_and_editor() {
    let (_root, bus, epoch, seats) = fixture("task-edit-widened-facts", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "EDITED-1kKg6RB6x",
        "Filed at the wrong priority and under the wrong topic",
        Some("todo"),
    );
    let filed = statement_head(&bus, &task);

    assert_eq!(
        edited_status(correct(
            &bus,
            epoch,
            lane.clone(),
            &TaskEdit {
                priority: Some("critical".to_owned()),
                ..only(&task)
            },
        )),
        "edited"
    );
    assert_eq!(
        edited_status(correct(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            &TaskEdit {
                topic: Some("/rim/elsewhere".parse().expect("fixture topic")),
                ..only(&task)
            },
        )),
        "edited"
    );

    let editors = edit_facts(&bus, &task);
    assert_eq!(
        editors.iter().map(|(actor, _)| actor).collect::<Vec<_>>(),
        vec![&lane, &Actor::TEST_PRINCIPAL],
        "a priority correction and a topic correction each record their editor",
    );
    assert!(
        editors[0].1 < editors[1].1,
        "the recorded revision advances with each correction: {editors:?}",
    );

    // The head the journal carries, not the board answer, is what the
    // recorded revisions point at.
    let corrected = statement_head(&bus, &task);
    assert!(corrected.position > filed.position);
    assert_eq!(corrected.body.priority, Some(wire::TaskPriority::Critical));
    assert_eq!(
        corrected.body.topic.path.as_ref().map(wire::Topic::as_str),
        Some("/rim/elsewhere")
    );
    assert_eq!(
        corrected.body.title, filed.body.title,
        "a widened correction carries the prose it did not name forward",
    );
    assert_eq!(corrected.body.born, filed.body.born);
}
