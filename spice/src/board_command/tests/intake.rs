//! Crude unplaced intake is submitted-draft triage: a bare `task add` rests
//! submitted in draft, the global teams claim it as ordinary work, and only
//! the phase-zero validator moves it.

use super::*;

/// A title and a pitch nothing on the corpus explains: no topic, no acceptance.
fn crude_add(title: &str) -> TaskAdd {
    let mut add = authored_add(title, "/unused", None, &[]);
    add.pitch = "Nothing here names a subject.".to_owned();
    add.topic = None;
    add.flow = None;
    add
}

fn crude_intake(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, name: &str) -> TaskHandle {
    let CommandAnswer::Task(answer) = assert_committed(issue(
        bus,
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: stamp(name) }],
        &crude_add("Something went wrong somewhere"),
    )) else {
        panic!("creation returns a task receipt")
    };
    assert_eq!(answer.status, "draft");
    assert!(
        answer
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("task::term-missing")),
        "crude intake carries its construction diagnostic: {:?}",
        answer.diagnostic
    );
    answer.task
}

fn done(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) -> Outcome<CommandAnswer> {
    let lane = Actor::Lane(seat.lane);
    let held = board(bus, epoch, lane.clone());
    issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        progress_witnesses(&held, at(&held, task), seat.lane, seat.occupant, false)
            .expect("the claimant has progress witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(task, "the triaged statement is complete")
        },
    )
}

/// The refused advance named `reason`, returned the diagnostic in the typed
/// answer, and left the row's statement, workflow, claims and the journal as
/// they were.
fn assert_refused_unmoved(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &TaskHandle,
    reason: &str,
    advance: impl FnOnce() -> Outcome<CommandAnswer>,
) {
    let before = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let cursor = bus.cursor().expect("cursor reads");
    let outcome = advance();
    let Outcome::Refusal(refusal) = &outcome else {
        panic!("the advance refuses: {outcome:?}")
    };
    assert_eq!(refusal.reason(), reason);
    assert!(
        refusal.diagnostic().is_some(),
        "the refusal names what to repair"
    );
    assert_eq!(
        bus.cursor().expect("cursor reads"),
        cursor,
        "a refused advance writes nothing"
    );
    let after = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let (before, after) = (at(&before, task), at(&after, task));
    assert_eq!(after.statement, before.statement, "authored content stays");
    assert_eq!(
        after.workflow, before.workflow,
        "phase and the submitted mark stay"
    );
    assert_eq!(after.claims, before.claims, "every claim stays");
    assert!(after.workflow.body.is_submitted_draft());
}

#[test]
fn crude_intake_is_triage_for_unconstrained_teams_and_moves_only_through_the_validator() {
    let (root, bus, epoch, seats) = fixture("intake-triage", 2);
    let (global, scoped) = (&seats[0], &seats[1]);
    subscription::name_fixture_team(&bus, epoch, scoped, "scoped");
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = true\ndrain = true\n\
         [team.scoped]\ndrive = true\ndrain = [{ topic = [\"/went\"] }]\n",
    )
    .unwrap();
    authorize_lane_creation(&bus, epoch, global);
    let task = crude_intake(&bus, epoch, global, "intake-crude");

    let created = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&created, &task);
    assert!(row.workflow.body.is_submitted_draft());
    assert_eq!(row.statement.body.topic.path, None);
    assert_eq!(
        row.workflow
            .body
            .flow
            .iter()
            .map(|phase| phase.as_str())
            .collect::<Vec<_>>(),
        ["draft", "todo", "review"],
        "no plan phase is inserted for contractless intake"
    );
    assert_eq!(
        allocated(&bus, epoch, &Actor::Lane(global.lane)).map(|(handle, _)| handle),
        Some(task.to_string()),
        "a team whose drain rule names no topic triages unplaced intake"
    );
    assert_eq!(
        allocated(&bus, epoch, &Actor::Lane(scoped.lane)),
        None,
        "a topic-constrained team never matches absence"
    );

    claim_task(&bus, epoch, global, &Actor::Lane(global.lane), &task);
    assert_refused_unmoved(&bus, epoch, &task, "task::term-missing", || {
        done(&bus, epoch, global, &task)
    });

    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        vec![statement_witness(at(&current, &task))],
        &TaskEdit {
            term: Some(vec!["Record what went wrong and verify the fix".to_owned()]),
            ..only(&task)
        },
    ));
    refresh_held(&bus, epoch, &Actor::Lane(global.lane), global, &task);
    assert_refused_unmoved(&bus, epoch, &task, "task::placement-required", || {
        done(&bus, epoch, global, &task)
    });

    let _trees = warm_topics(&root, &bus);
    let CommandAnswer::Task(answer) = assert_committed(done(&bus, epoch, global, &task)) else {
        panic!("the advance returns a task receipt")
    };
    let placed = TaskHandle::for_topic(
        &"/went".parse::<wire::Topic>().expect("public topic"),
        &task.born().expect("task stamp"),
    );
    assert_eq!(answer.task, placed, "the answer names the placed handle");
    let advanced = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&advanced, &task);
    assert_eq!(
        row.statement
            .body
            .topic
            .path
            .as_ref()
            .map(wire::Topic::as_str),
        Some("/went")
    );
    assert_eq!(row.workflow.body.phase, wire::Phase::Todo);
    assert!(row.workflow.body.submitted, "the mark is never cleared");
    assert_committed(issue(
        &bus,
        epoch,
        Actor::Lane(global.lane),
        CommandKind::ClaimFree,
        vec![
            held_claim_witness(row),
            held_lease_witness(row),
            residency(&advanced, global),
        ],
        &TaskRef {
            task: placed.clone(),
        },
    ));
    assert_eq!(
        allocated(&bus, epoch, &Actor::Lane(scoped.lane)).map(|(handle, _)| handle),
        Some(placed.to_string()),
        "the placed work enters the destination team's drain after handoff"
    );
    clean(&root, bus);
}

/// Neither an author-held draft nor a controlled draft under an open Plan is
/// offered, while a submitted sibling is; neither carries the submitted mark.
#[test]
fn held_and_controlled_drafts_are_never_offered_beside_submitted_triage() {
    let (root, bus, epoch, seats) = fixture("intake-standings", 1);
    let seat = &seats[0];
    let lane = Actor::Lane(seat.lane);
    authorize_lane_creation(&bus, epoch, seat);

    let held_born = stamp("intake-held");
    let CommandAnswer::Task(held) = assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: held_born.clone(),
        }],
        &TaskAdd {
            draft: true,
            ..crude_add("Still being written")
        },
    )) else {
        panic!("creation returns a task receipt")
    };
    assert_eq!(held.status, "draft");
    assert_eq!(
        held.diagnostic, None,
        "an author-held draft hears no diagnostic"
    );

    let plan = issue_add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "intake-plan",
        TaskAdd {
            intent: Some(wire::TaskIntent::Plan),
            ..authored_add(
                "Controlling plan",
                "/board/test",
                Some("plan,review"),
                &["The planned outputs are accepted"],
            )
        },
    );
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let parent = at(&current, &plan);
    assert!(parent.workflow.body.is_activated());
    let controlled_born = stamp("intake-controlled");
    let CommandAnswer::Task(controlled) = assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![
            Witness::Absence {
                task: controlled_born.clone(),
            },
            statement_witness(parent),
            workflow_witness(parent),
        ],
        &TaskAdd {
            preparation: Some(plan.clone()),
            draft: true,
            ..crude_add("Planned output still being shaped")
        },
    )) else {
        panic!("creation returns a task receipt")
    };
    assert_eq!(controlled.status, "draft");

    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    for (born, standing) in [(&held_born, "held"), (&controlled_born, "controlled")] {
        let row = view
            .tasks
            .iter()
            .find(|row| row.statement.body.born == *born)
            .expect("the draft is visible to the operator");
        assert!(row.workflow.body.is_draft(), "{standing} rests in draft");
        assert!(
            !row.workflow.body.submitted,
            "{standing} draft never carries the submitted mark"
        );
        assert!(!row.workflow.body.is_submitted_draft());
    }
    assert_eq!(
        allocated(&bus, epoch, &lane).map(|(handle, _)| handle),
        Some(plan.to_string()),
        "only the activated plan is offered beside held and controlled drafts"
    );

    let submitted = crude_intake(&bus, epoch, seat, "intake-submitted");
    claim_task(&bus, epoch, seat, &lane, &plan);
    assert_eq!(
        allocated(&bus, epoch, &lane).map(|(handle, _)| handle),
        Some(submitted.to_string()),
        "a submitted uncontrolled draft is the only draft the allocator offers"
    );
    clean(&root, bus);
}
