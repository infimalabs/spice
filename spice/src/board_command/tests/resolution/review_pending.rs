use super::*;

struct AssignedReview {
    bus: Bus,
    root: FixtureRoot,
    epoch: CanonicalUuid,
    seats: Vec<Seat>,
    implementer: Actor,
    reviewer: Actor,
    reviewed: TaskHandle,
}

impl AssignedReview {
    fn new(name: &str) -> Self {
        let (root, bus, epoch, seats) = fixture(name, 3);
        authorize_lane_creation(&bus, epoch, &seats[1]);
        let implementer = Actor::Lane(seats[0].lane);
        let reviewer = Actor::Lane(seats[1].lane);
        let reviewed = add(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            "REVIEW-target1",
            "Reviewed target",
            None,
        );
        advance_task_to_review(&bus, epoch, &seats[0], &implementer, &reviewed);
        let reviewable = board(&bus, epoch, reviewer.clone());
        assert_committed(issue(
            &bus,
            epoch,
            reviewer.clone(),
            CommandKind::ClaimWin,
            claim_expected(&reviewable, &reviewed, &seats[1]),
            &next_rule(),
        ));
        Self {
            root,
            bus,
            epoch,
            seats,
            implementer,
            reviewer,
            reviewed,
        }
    }

    fn expected(&self) -> Vec<Witness> {
        let assigned = board(&self.bus, self.epoch, self.reviewer.clone());
        progress_witnesses(
            &assigned,
            at(&assigned, &self.reviewed),
            self.seats[1].lane,
            self.seats[1].occupant,
            true,
        )
        .expect("assigned review has exact witnesses")
    }

    fn finish(self) {
        clean(&self.root, self.bus);
    }
}

fn existing_correction(review: &AssignedReview, handle: &str) -> TaskHandle {
    add(
        &review.bus,
        review.epoch,
        Actor::TEST_PRINCIPAL,
        handle,
        "Existing correction",
        Some("todo"),
    )
}

fn existing_expected(
    review: &AssignedReview,
    correction: &TaskHandle,
    mut expected: Vec<Witness>,
) -> Vec<Witness> {
    let snapshot = board(&review.bus, review.epoch, review.reviewer.clone());
    let correction_heads = at(&snapshot, correction);
    expected.push(statement_witness(correction_heads));
    expected.push(workflow_witness(correction_heads));
    expected.push(Witness::Edge {
        task: at(&snapshot, &review.reviewed).statement.body.born.clone(),
        after: correction_heads.statement.body.born.clone(),
    });
    append_dependency_reachability_witnesses(
        &snapshot,
        std::slice::from_ref(&correction_heads.statement.body.born),
        &mut expected,
    )
    .expect("correction reachability is visible");
    expected
}

#[test]
fn correction_that_reaches_the_reviewed_task_refuses_atomically() {
    let review = AssignedReview::new("review-correction-cycle");
    let correction = existing_correction(&review, "REVIEW-cycle1");
    connect_child(&review.bus, review.epoch, &correction, &review.reviewed);
    let expected = existing_expected(&review, &correction, review.expected());
    let before = review.bus.cursor().expect("cursor before refusal");
    let outcome = issue(
        &review.bus,
        review.epoch,
        review.reviewer.clone(),
        CommandKind::TaskReview,
        expected,
        &TaskReview {
            followup: vec![correction],
            ..verdict(&review.reviewed, "cycle must refuse")
        },
    );
    assert!(
        matches!(outcome, Outcome::Refusal(ref refusal) if refusal.reason() == "task::dependency-refused"),
        "correction cycle must refuse: {outcome:?}",
    );
    assert_eq!(
        review.bus.cursor().expect("cursor after refusal"),
        before,
        "cycle refusal must publish no partial finding or correction edge",
    );
    review.finish();
}

#[test]
fn correction_movement_after_validation_refuses_the_whole_review() {
    let review = AssignedReview::new("review-correction-race");
    let correction = existing_correction(&review, "REVIEW-race01");
    let moved_after = existing_correction(&review, "REVIEW-race02");
    let expected = existing_expected(&review, &correction, review.expected());
    let intent = TaskReview {
        followup: vec![correction.clone()],
        ..verdict(&review.reviewed, "correction moved")
    };
    let command = fixture_command(
        review.epoch,
        review.reviewer.clone(),
        CommandKind::TaskReview,
        expected,
        &intent,
    );
    let reviewed_before = at(
        &board(&review.bus, review.epoch, Actor::TEST_PRINCIPAL),
        &review.reviewed,
    )
    .clone();
    let outcome = task_review_with(&review.bus, &command, &intent, None, || {
        connect_child(&review.bus, review.epoch, &correction, &moved_after)
    });
    assert!(
        matches!(outcome, Outcome::Refusal(ref refusal) if refusal.reason() == "board::witness-moved"),
        "moved correction must lose the guarded commit: {outcome:?}",
    );
    let current = board(&review.bus, review.epoch, Actor::TEST_PRINCIPAL);
    let reviewed_after = at(&current, &review.reviewed);
    assert_eq!(reviewed_after.statement, reviewed_before.statement);
    assert_eq!(reviewed_after.workflow, reviewed_before.workflow);
    assert_eq!(reviewed_after.claims, reviewed_before.claims);
    assert!(
        !review
            .bus
            .transact(|journal| bus::JournalTransaction::settled(
                journal.fact_snapshot().expect("journal snapshot")
            ))
            .expect("race facts")
            .facts()
            .iter()
            .any(|fact| {
                fact.fact.kind() == wire::FactKind::TaskReviewed
                    && fact
                        .fact
                        .body()
                        .get("task")
                        .and_then(serde_json::Value::as_str)
                        == review.reviewed.born().ok().as_ref().map(TaskStamp::as_str)
            })
    );
    review.finish();
}

#[test]
fn finding_command_replays_once_and_allocator_selects_its_correction() {
    let review = AssignedReview::new("review-finding-replay");
    let correction_stamp: TaskStamp = "1kKRRsDTx".parse().expect("correction stamp");
    let mut expected = review.expected();
    expected.push(Witness::Absence {
        task: correction_stamp.clone(),
    });
    let intent = TaskReview {
        task: review.reviewed.clone(),
        finding: "one replayable correction".to_owned(),
        note: Some("keep the review pending".to_owned()),
        then: vec![ReviewFollowup {
            task: correction_stamp.clone(),
            add: TaskAdd {
                topic: Some("/board/test".parse().expect("fixture topic")),
                term: vec!["the fixture work is verified".to_owned()],
                flow: Some("todo".to_owned()),
                ..adding(
                    "Replay-safe correction",
                    "This work keeps the fixture honest.",
                )
            },
        }],
        followup: Vec::new(),
    };
    let command = fixture_command(
        review.epoch,
        review.reviewer.clone(),
        CommandKind::TaskReview,
        expected,
        &intent,
    );
    let first = dispatch(&review.bus, &command).expect("review command dispatches");
    let replay_command = command.clone();
    let replay = review
        .bus
        .read(move |read| read.command_replay(&replay_command))
        .expect("review receipt reads");
    assert_eq!(replay, Some(journal::CommandReplay::Exact(first)));
    let facts = review
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("review replay facts");
    assert_eq!(
        facts
            .facts()
            .iter()
            .filter(|fact| fact.fact.kind() == wire::FactKind::TaskReviewed)
            .count(),
        1,
    );
    let correction = board_test_handle(&correction_stamp);
    let available = board(&review.bus, review.epoch, review.implementer.clone());
    let entries = actor_entries(&available).expect("pending review projects to entries");
    let reviewed = entries
        .iter()
        .find(|entry| entry.handle() == &review.reviewed)
        .expect("reviewed task remains visible");
    assert!(!board::eligible_entry(
        &board::resolved_tasks(&entries),
        reviewed,
        &review.implementer,
        &available.environment(),
    ));
    let subscription = allocation_for(&available, &review.implementer);
    let selected = board::select(
        &entries,
        &review.implementer,
        &subscription,
        &available.environment(),
    )
    .expect("the correction is allocatable");
    assert_eq!(selected.entry().handle(), &correction);
    review.finish();
}

fn board_test_handle(stamp: &wire::TaskStamp) -> TaskHandle {
    TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        stamp,
    )
}

fn correction_add(title: &str, flow: Option<&str>) -> TaskAdd {
    TaskAdd {
        topic: Some("/board/test".parse().expect("fixture topic")),
        term: vec!["the fixture work is verified".to_owned()],
        flow: flow.map(str::to_owned),
        ..adding(title.to_owned(), "This work keeps the fixture honest.")
    }
}

#[test]
fn a_defaulted_correction_omits_review_while_an_explicit_flow_keeps_it() {
    let review = AssignedReview::new("review-correction-flow");
    let defaulted: TaskStamp = "1kKRRsDWx".parse().expect("defaulted stamp");
    let explicit: TaskStamp = "1kKRRsDXx".parse().expect("explicit stamp");
    let mut expected = review.expected();
    for task in [&defaulted, &explicit] {
        expected.push(Witness::Absence { task: task.clone() });
    }
    assert_committed(issue(
        &review.bus,
        review.epoch,
        review.reviewer.clone(),
        CommandKind::TaskReview,
        expected,
        &TaskReview {
            then: vec![
                ReviewFollowup {
                    task: defaulted.clone(),
                    add: correction_add("Defaulted correction", None),
                },
                ReviewFollowup {
                    task: explicit.clone(),
                    add: correction_add("Material correction", Some("todo,review")),
                },
            ],
            ..verdict(&review.reviewed, "two corrections")
        },
    ));
    let current = board(&review.bus, review.epoch, Actor::TEST_PRINCIPAL);
    let flow = |stamp: &TaskStamp| {
        at(&current, &board_test_handle(stamp))
            .workflow
            .body
            .flow
            .iter()
            .map(|phase| phase.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(flow(&defaulted), ["draft", "todo"]);
    assert_eq!(flow(&explicit), ["draft", "todo", "review"]);
    review.finish();
}

#[test]
fn manually_linked_review_origin_work_uses_the_parent_review() {
    let review = AssignedReview::new("review-manual-origin");
    let mut created = Vec::new();
    for (name, origin, flow, expected_flow) in [
        (
            "REVIEW-manual1",
            None,
            None,
            vec![wire::Phase::Draft, wire::Phase::Todo],
        ),
        (
            "REVIEW-manual2",
            Some(task_origin(&review.reviewed.born().unwrap()).unwrap()),
            None,
            vec![wire::Phase::Draft, wire::Phase::Todo],
        ),
        (
            "REVIEW-explicit",
            None,
            Some("todo,review"),
            vec![wire::Phase::Draft, wire::Phase::Todo, wire::Phase::Review],
        ),
        (
            "REVIEW-separate",
            Some(fixture_origin()),
            None,
            vec![wire::Phase::Draft, wire::Phase::Todo, wire::Phase::Review],
        ),
    ] {
        let born = stamp(name);
        let mut add = correction_add("Manual correction", flow);
        add.origin = origin;
        assert_committed(issue(
            &review.bus,
            review.epoch,
            review.reviewer.clone(),
            CommandKind::TaskAdd,
            vec![Witness::Absence { task: born.clone() }],
            &add,
        ));
        let current = board(&review.bus, review.epoch, review.reviewer.clone());
        let child = board_test_handle(&born);
        assert_eq!(at(&current, &child).workflow.body.flow, expected_flow);
        created.push(child);
    }
    let correction = &created[0];
    let current = board(&review.bus, review.epoch, review.reviewer.clone());
    assert_committed(issue(
        &review.bus,
        review.epoch,
        review.reviewer.clone(),
        CommandKind::TaskDepends,
        super::super::dependencies::dependency_witnesses(
            &current,
            &review.reviewed,
            &[correction],
            Some(&review.seats[1]),
            true,
        ),
        &TaskDepends {
            task: review.reviewed.clone(),
            after: vec![correction.clone()],
            remove: false,
        },
    ));
    let current = board(&review.bus, review.epoch, review.reviewer.clone());
    let mut expected = review.expected();
    expected.extend([
        statement_witness(at(&current, correction)),
        workflow_witness(at(&current, correction)),
    ]);
    assert_committed(issue(
        &review.bus,
        review.epoch,
        review.reviewer.clone(),
        CommandKind::TaskReview,
        expected,
        &TaskReview {
            followup: vec![correction.clone()],
            ..verdict(
                &review.reviewed,
                "The manually linked correction is required",
            )
        },
    ));
    claim_and_complete_task(
        &review.bus,
        review.epoch,
        &review.seats[0],
        &review.implementer,
        correction,
    );
    let current = board(&review.bus, review.epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&current, correction).workflow.body.status.is_completed());
    assert_eq!(
        at(&current, &review.reviewed).workflow.body.phase,
        wire::Phase::Review
    );
    assert!(
        at(&current, &review.reviewed)
            .workflow
            .body
            .status
            .is_pending()
    );
    review.finish();
}

#[test]
fn review_origin_flow_refuses_when_its_author_releases_the_review() {
    let review = AssignedReview::new("review-origin-race");
    let born = stamp("REVIEW-originrace");
    let mut add = correction_add("Racing correction", None);
    add.origin = None;
    let command = fixture_command(
        review.epoch,
        review.reviewer.clone(),
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born.clone() }],
        &add,
    );
    let outcome = task_add_with(&review.bus, &command, &add, None, || {
        let held = board(&review.bus, review.epoch, review.reviewer.clone());
        let heads = at(&held, &review.reviewed);
        assert_committed(issue(
            &review.bus,
            review.epoch,
            review.reviewer.clone(),
            CommandKind::ClaimFree,
            vec![
                held_claim_witness(heads),
                held_lease_witness(heads),
                residency(&held, &review.seats[1]),
            ],
            &TaskRef {
                task: review.reviewed.clone(),
            },
        ));
    });
    assert!(
        matches!(&outcome, Outcome::Refusal(refusal)
        if refusal.reason() == "board::witness-moved"),
        "{outcome:?}"
    );
    assert!(
        board(&review.bus, review.epoch, Actor::TEST_PRINCIPAL)
            .tasks
            .iter()
            .all(|task| task.statement.body.born != born)
    );
    review.finish();
}

#[test]
fn repeated_finding_reuses_one_completed_correction_without_a_duplicate_edge() {
    let review = AssignedReview::new("review-repeated-finding");
    let correction_stamp: TaskStamp = "1kKRRsDVx".parse().expect("correction stamp");
    let mut expected = review.expected();
    expected.push(Witness::Absence {
        task: correction_stamp.clone(),
    });
    assert_committed(issue(
        &review.bus,
        review.epoch,
        review.reviewer.clone(),
        CommandKind::TaskReview,
        expected,
        &TaskReview {
            task: review.reviewed.clone(),
            finding: "first finding".to_owned(),
            note: None,
            then: vec![ReviewFollowup {
                task: correction_stamp.clone(),
                add: TaskAdd {
                    topic: Some("/board/test".parse().expect("fixture topic")),
                    term: vec!["the fixture work is verified".to_owned()],
                    flow: Some("todo".to_owned()),
                    ..adding(
                        "One durable correction",
                        "This work keeps the fixture honest.",
                    )
                },
            }],
            followup: Vec::new(),
        },
    ));
    let correction = board_test_handle(&correction_stamp);
    claim_and_complete_task(
        &review.bus,
        review.epoch,
        &review.seats[0],
        &review.implementer,
        &correction,
    );

    // The finder hands the corrected revision's review to a peer, who finds
    // the same correction insufficient.
    let peer = &review.seats[2];
    let peer_actor = Actor::Lane(peer.lane);
    claim_task(
        &review.bus,
        review.epoch,
        peer,
        &peer_actor,
        &review.reviewed,
    );
    let assigned = board(&review.bus, review.epoch, peer_actor.clone());
    let mut expected = progress_witnesses(
        &assigned,
        at(&assigned, &review.reviewed),
        peer.lane,
        peer.occupant,
        true,
    )
    .expect("repeated review has exact witnesses");
    let correction_heads = at(&assigned, &correction);
    expected.push(statement_witness(correction_heads));
    expected.push(workflow_witness(correction_heads));
    assert_committed(issue(
        &review.bus,
        review.epoch,
        peer_actor,
        CommandKind::TaskReview,
        expected,
        &TaskReview {
            followup: vec![correction.clone(), correction],
            ..verdict(&review.reviewed, "the first correction was insufficient")
        },
    ));

    let pending = board(&review.bus, review.epoch, review.reviewer.clone());
    let reviewed = at(&pending, &review.reviewed);
    assert!(matches!(
        reviewed.workflow.body.status,
        wire::WorkflowStatus::Pending(wire::WorkflowReview::Unassigned)
    ));
    assert_eq!(reviewed.statement.body.dependencies, vec![correction_stamp]);
    assert_eq!(
        review
            .bus
            .transact(|journal| bus::JournalTransaction::settled(
                journal.fact_snapshot().expect("journal snapshot")
            ))
            .expect("repeated review facts")
            .facts()
            .iter()
            .filter(|fact| fact.fact.kind() == wire::FactKind::TaskReviewed)
            .count(),
        2,
    );
    let entries = actor_entries(&pending).expect("repeated review projects to entries");
    let entry = entries
        .iter()
        .find(|entry| entry.handle() == &review.reviewed)
        .expect("reviewed task remains visible");
    assert!(
        board::eligible_entry(
            &board::resolved_tasks(&entries),
            entry,
            &review.reviewer,
            &pending.environment()
        ),
        "a finding whose already-related correction is complete is reviewable again",
    );
    review.finish();
}
