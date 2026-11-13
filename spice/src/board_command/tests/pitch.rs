use super::*;

/// One add whose pitch is the only field under test.
fn add_with_pitch(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &str,
    pitch: &str,
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
            ..adding("Lead with the reason", pitch.to_owned())
        },
    );
    (handle, outcome)
}

/// Each mechanical rule refuses by its own reason before anything is written,
/// and the bound counts scalar values rather than bytes.
#[test]
fn an_add_refuses_an_empty_multiline_or_overlong_pitch_by_name() {
    let (_root, bus, epoch, _seats) = fixture("task-pitch-refusals", 0);
    let long = "é".repeat(wire::pitch::PITCH_CHARS);
    let longer = "é".repeat(wire::pitch::PITCH_CHARS + 1);
    for (index, (pitch, reason)) in [
        ("   ", "task::pitch-empty"),
        ("Why it matters.\nAnd more.", "task::pitch-multiline"),
        (longer.as_str(), "task::pitch-too-long"),
    ]
    .into_iter()
    .enumerate()
    {
        let (task, outcome) = add_with_pitch(&bus, epoch, &format!("PITCH-1kKfYW0{index}"), pitch);
        assert!(
            matches!(&outcome, Outcome::Refusal(refusal) if refusal.reason() == reason),
            "{pitch:?}: {outcome:?}"
        );
        let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        assert!(
            task_heads(&visible, &task).is_err(),
            "a refused pitch records no statement"
        );
    }

    let (task, outcome) = add_with_pitch(&bus, epoch, "PITCH-1kKfYW09x", &long);
    assert_committed(outcome);
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&visible, &task).statement.body.pitch, long);
}

/// Code inside a pitch records the statement and holds it, naming each
/// matched token on the pitch field, exactly as a taste word does.
#[test]
fn an_identifier_in_a_pitch_records_and_holds_the_statement_naming_the_text() {
    let (_root, bus, epoch, _seats) = fixture("task-pitch-hold", 0);
    let (task, outcome) = add_with_pitch(
        &bus,
        epoch,
        "PITCH-1kKfYW10x",
        "Fix `wire::pitch` so BOARD-1kKfYW11x lands.",
    );
    assert_committed(outcome);

    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let held = at(&visible, &task);
    let hold = held
        .statement
        .body
        .taste
        .as_ref()
        .expect("a pitch naming code is held");
    let matched = hold
        .matches
        .iter()
        .map(|found| {
            (
                found.field,
                found.taste.as_str(),
                found
                    .finding
                    .as_ref()
                    .map(|finding| finding.trigger.as_str())
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matched,
        [
            (
                wire::StatementField::Pitch,
                wire::pitch::PITCH_IDENTIFIER_TASTE,
                "`wire::pitch`",
            ),
            (
                wire::StatementField::Pitch,
                wire::pitch::PITCH_IDENTIFIER_TASTE,
                "BOARD-1kKfYW11x",
            ),
        ]
    );
}

/// An authored pitch change is a statement correction: it advances the
/// revision, replaces the stored pitch, and admits nothing the add refuses.
#[test]
fn a_pitch_edit_advances_the_statement_revision() {
    let (_root, bus, epoch, _seats) = fixture("task-pitch-edit", 0);
    let (task, outcome) = add_with_pitch(
        &bus,
        epoch,
        "PITCH-1kKfYW20x",
        "Operators read the reason before the title.",
    );
    assert_committed(outcome);
    let before = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
        .statement
        .position;

    let outcome = correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            pitch: Some("  Operators read the reason first.  ".to_owned()),
            ..only(&task)
        },
    );
    assert_committed(outcome);
    let after = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
        .statement
        .clone();
    assert!(after.position > before, "the pitch edit is a new revision");
    assert_eq!(after.body.pitch, "Operators read the reason first.");

    let outcome = correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            pitch: Some("One line.\nTwo lines.".to_owned()),
            ..only(&task)
        },
    );
    assert!(
        matches!(&outcome, Outcome::Refusal(refusal) if refusal.reason() == "task::pitch-multiline"),
        "{outcome:?}"
    );
}
