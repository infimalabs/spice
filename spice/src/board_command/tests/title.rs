use super::*;

fn add_with_title(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &str,
    title: &str,
) -> (TaskHandle, Outcome<CommandAnswer>) {
    let born = stamp(task);
    let handle = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    );
    let outcome = issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born }],
        &TaskAdd {
            topic: Some("/board/test".parse().expect("fixture topic")),
            term: vec!["the fixture work is verified".to_owned()],
            origin: Some(fixture_origin()),
            flow: Some("todo,review".to_owned()),
            ..adding(title, "Operators read the full name of the work.")
        },
    );
    (handle, outcome)
}

fn refused_as_too_long(outcome: &Outcome<CommandAnswer>) -> bool {
    matches!(
        outcome,
        Outcome::Refusal(refusal) if refusal.reason() == wire::title::TitleTooLong::REASON
    )
}

/// The bound counts scalar values: a full-width title is stored verbatim,
/// one more refuses before anything is written, and a correction is held to
/// the same bound.
#[test]
fn titles_admit_their_bound_verbatim_and_refuse_one_more_on_add_and_modify() {
    let (_root, bus, epoch, _seats) = fixture("task-title-bound", 0);
    let widest = "é".repeat(wire::title::TITLE_CHARS);
    let wider = "é".repeat(wire::title::TITLE_CHARS + 1);

    let (refused, outcome) = add_with_title(&bus, epoch, "TITLE-1kKfYW00x", &wider);
    assert!(refused_as_too_long(&outcome), "{outcome:?}");
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(
        task_heads(&visible, &refused).is_err(),
        "a refused title records no statement"
    );

    let (task, outcome) = add_with_title(&bus, epoch, "TITLE-1kKfYW01x", &widest);
    assert_committed(outcome);
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&visible, &task).presentation_title(), widest);

    let outcome = correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            title: Some(wider),
            ..only(&task)
        },
    );
    assert!(refused_as_too_long(&outcome), "{outcome:?}");
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&visible, &task).presentation_title(), widest);
}
