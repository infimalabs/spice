use super::*;

mod oops;
mod private;

fn policy(root: &Path, rules: &str) -> Repository {
    fs::write(
        root.join("spice.toml"),
        format!("[team.default]\ndrive = {rules}\n"),
    )
    .expect("team policy writes");
    Repository::discover(root).expect("fixture repository resolves")
}

fn add_command(
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    born: &wire::TaskStamp,
    add: &TaskAdd,
) -> wire::AdmittedCommand {
    fixture_command(
        epoch,
        Actor::Lane(lane),
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born.clone() }],
        add,
    )
}

fn denied(outcome: Outcome<CommandAnswer>) {
    let Outcome::Refusal(refusal) = outcome else {
        panic!("expected drive refusal, got {outcome:?}");
    };
    assert_eq!(refusal.reason(), "team::drive-refused");
}

#[test]
fn drive_admission_uses_final_topic_and_phase_but_drafts_confer_no_exemption() {
    let (root, bus, epoch, seats) = fixture("drive-publication", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let repository = policy(&root, r#"[{ topic = ["/allowed/**"], phase = ["todo"] }]"#);
    for (name, topic, phase) in [
        ("outside-topic", "/elsewhere/work", "todo"),
        ("outside-phase", "/allowed/work", "plan"),
    ] {
        let add = authored_add(
            "Admit only current policy",
            topic,
            Some(phase),
            &["Work is admitted"],
        );
        let born = stamp(name);
        let command = add_command(epoch, seat.lane, &born, &add);
        let CommandAnswer::Task(answer) = assert_committed(mutation::task_add(
            &bus,
            &command,
            &add,
            Some(host(&repository)),
        )) else {
            panic!("creation returns its task receipt")
        };
        let placement = answer.placement.expect("creation describes its placement");
        assert_eq!(placement.topic.as_str(), topic);
        assert!(placement.drive_refusal.is_some());
        assert_eq!(
            answer.next.as_deref(),
            Some(format!("spice task place {}", answer.task).as_str())
        );
        let snapshot = bus.task_snapshot().unwrap();
        let heads = snapshot
            .task_heads(&born)
            .expect("denied work remains saved");
        assert!(heads.workflow().body().is_draft());
        let saved = &heads.statement().body().topic.path;
        assert_eq!(saved, &Some(topic.parse().unwrap()));
        assert!(heads.statement().body().preparation.is_none());
        assert!(heads.claims().is_empty());
    }

    let mut draft = authored_add(
        "Keep authored work as a Draft",
        "/elsewhere/work",
        Some("todo"),
        &["Work is admitted"],
    );
    draft.draft = true;
    let born = stamp("draft-drive-admission");
    let command = add_command(epoch, seat.lane, &born, &draft);
    assert_committed(mutation::task_add(
        &bus,
        &command,
        &draft,
        Some(host(&repository)),
    ));
    let task = TaskHandle::for_phase(
        Some(&"/elsewhere/work".parse::<wire::Topic>().unwrap()),
        &born,
        wire::Phase::Draft,
    );
    let visible = board(&bus, epoch, Actor::Lane(seat.lane));
    let heads = at(&visible, &task);
    let expected = vec![
        statement_witness(heads),
        workflow_witness(heads),
        deferral_witness(heads),
        Witness::Eligibility {
            rule: "capture".parse().unwrap(),
        },
        residency(&visible, seat),
    ];
    let file = TaskFile {
        task: task.clone(),
        topic: None,
        claim: true,
    };
    let command = fixture_command(
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskFile,
        expected,
        &file,
    );
    let before = bus.cursor().unwrap();
    denied(task_file(&bus, &command, &file, Some(host(&repository))));
    assert_eq!(bus.cursor().unwrap(), before);
    let after = board(&bus, epoch, Actor::Lane(seat.lane));
    assert!(at(&after, &task).workflow.body.is_draft());
    assert!(at(&after, &task).claims.is_empty());
    clean(&root, bus);
}

#[test]
fn drive_denied_creation_advances_after_its_team_policy_changes() {
    let (root, bus, epoch, seats) = fixture("drive-denied-handoff", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let repository = policy(&root, "[]");
    let born = stamp("drive-denied-handoff");
    let add = authored_add(
        "Work follows current team authority",
        "/allowed/work",
        Some("todo"),
        &["The work follows its current policy"],
    );
    let CommandAnswer::Task(answer) = assert_committed(mutation::task_add(
        &bus,
        &add_command(epoch, seat.lane, &born, &add),
        &add,
        Some(host(&repository)),
    )) else {
        panic!("creation returns its saved task")
    };
    assert!(answer.placement.as_ref().unwrap().drive_refusal.is_some());
    let visible = board(&bus, epoch, Actor::Lane(seat.lane));
    let saved = at(&visible, &answer.task);
    assert!(saved.workflow.body.is_draft());
    assert!(!saved.workflow.body.submitted);
    assert_eq!(allocated(&bus, epoch, &Actor::Lane(seat.lane)), None);

    let repository = policy(&root, "true");
    let request = TaskFile {
        task: answer.task.clone(),
        topic: None,
        claim: false,
    };
    let command = fixture_command(
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskFile,
        vec![
            statement_witness(saved),
            workflow_witness(saved),
            deferral_witness(saved),
        ],
        &request,
    );
    assert_committed(task_file(&bus, &command, &request, Some(host(&repository))));
    let current = board(&bus, epoch, Actor::Lane(seat.lane));
    let active = at(&current, &answer.task);
    assert_eq!(active.workflow.body.phase, wire::Phase::Todo);
    assert_eq!(
        active.statement.body.topic.path,
        saved.statement.body.topic.path
    );
    assert!(active.claims.is_empty());
    clean(&root, bus);
}

/// A drive denial would leave a claiming creation author-held in draft, so the
/// creation refuses whole instead of committing work its author cannot hold.
#[test]
fn drive_denied_create_and_claim_refuses_without_a_draft() {
    let (root, bus, epoch, seats) = fixture("drive-denied-claim", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let repository = policy(&root, "[]");
    let born = stamp("drive-denied-claim");
    let add = TaskAdd {
        claim: true,
        ..authored_add(
            "Work the lane wants to hold at once",
            "/allowed/work",
            Some("todo"),
            &["The work is held"],
        )
    };
    let current = board(&bus, epoch, Actor::Lane(seat.lane));
    let command = fixture_command(
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskAdd,
        vec![
            Witness::Absence { task: born.clone() },
            Witness::Eligibility {
                rule: "capture".parse().unwrap(),
            },
            residency(&current, seat),
        ],
        &add,
    );
    let before = bus.cursor().unwrap();
    let outcome = mutation::task_add(&bus, &command, &add, Some(host(&repository)));
    let Outcome::Refusal(refusal) = &outcome else {
        panic!("a drive-denied create-and-claim must refuse: {outcome:?}");
    };
    assert_eq!(refusal.reason(), "task::claim-unadvanced");
    let observed = refusal
        .diagnostic()
        .and_then(wire::RefusalDiagnostic::observed)
        .unwrap_or_default();
    assert!(observed.contains("team::drive-refused"), "{observed}");
    assert_eq!(bus.cursor().unwrap(), before);
    assert!(
        board(&bus, epoch, Actor::Lane(seat.lane))
            .tasks
            .iter()
            .all(|task| task.statement.body.born != born)
    );
    clean(&root, bus);
}

#[test]
fn policy_movement_after_capture_refuses_publication_and_fresh_authority_can_retry() {
    let (root, bus, epoch, seats) = fixture("drive-policy-race", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let repository = policy(&root, "[]");
    let add = authored_add(
        "Publish under current drive authority",
        "/allowed/work",
        Some("todo"),
        &["Authority is current"],
    );
    let born = stamp("drive-policy-race");
    let command = add_command(epoch, seat.lane, &born, &add);
    let before = bus.cursor().unwrap();
    let outcome = mutation::task_add_with(&bus, &command, &add, Some(host(&repository)), || {
        policy(&root, "true");
    });
    assert!(matches!(outcome, Outcome::Refusal(_)), "{outcome:?}");
    assert_eq!(bus.cursor().unwrap(), before);
    let command = add_command(epoch, seat.lane, &born, &add);
    assert_committed(mutation::task_add(
        &bus,
        &command,
        &add,
        Some(host(&repository)),
    ));
    let snapshot = bus.task_snapshot().unwrap();
    let heads = snapshot
        .tasks()
        .iter()
        .find(|heads| heads.statement().body().born == born)
        .unwrap();
    assert_eq!(
        heads.statement().body().creation.creator_team(),
        Some(seat.team)
    );
    assert!(heads.workflow().body().is_activated());
    clean(&root, bus);
}

#[test]
fn membership_movement_refuses_captured_creation_and_retry_uses_the_new_team() {
    let (root, bus, epoch, seats) = fixture("drive-membership-race", 1);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let repository = policy(&root, r#"[{ team = ["self"] }]"#);
    let add = authored_add(
        "Publish with current team identity",
        "/allowed/work",
        Some("todo"),
        &["Identity is current"],
    );
    let born = stamp("drive-membership-race");
    let command = add_command(epoch, seat.lane, &born, &add);
    let moved_cursor = std::cell::Cell::new(None);
    let next_team = uuid(98_001);
    let outcome = mutation::task_add_with(&bus, &command, &add, Some(host(&repository)), || {
        subscription::move_fixture_lane_to_team(&bus, epoch, seat, next_team);
        moved_cursor.set(bus.cursor().unwrap());
    });
    assert!(matches!(outcome, Outcome::Refusal(_)), "{outcome:?}");
    assert_eq!(bus.cursor().unwrap(), moved_cursor.get());
    let command = add_command(epoch, seat.lane, &born, &add);
    assert_committed(mutation::task_add(
        &bus,
        &command,
        &add,
        Some(host(&repository)),
    ));
    let snapshot = bus.task_snapshot().unwrap();
    let heads = snapshot
        .tasks()
        .iter()
        .find(|heads| heads.statement().body().born == born)
        .unwrap();
    assert_eq!(
        heads.statement().body().creation.creator_team(),
        Some(next_team)
    );
    clean(&root, bus);
}

#[test]
fn completion_receipt_resolves_the_entered_phase_not_the_phase_just_finished() {
    let (root, bus, epoch, seats) = fixture("progress-owner-phase", 1);
    let seat = &seats[0];
    let actor = Actor::Lane(seat.lane);
    authorize_lane_creation(&bus, epoch, seat);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = true\ndrain = [{ phase = [\"todo\"], team = [\"self\"] }]\n",
    )
    .unwrap();
    subscription::configure_subscription(&bus, epoch, seat, true);
    let task = issue_add(
        &bus,
        epoch,
        actor.clone(),
        "progress-owner-phase",
        authored_add(
            "Complete work into review",
            "/allowed/work",
            Some("todo,review"),
            &["Work passes review"],
        ),
    );
    let available = board(&bus, epoch, actor.clone());
    let claim = assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::ClaimWin,
        claim_expected(&available, &task, seat),
        &next_rule(),
    ));
    let CommandAnswer::Claim(claim) = claim else {
        panic!("claim reply required");
    };
    assert_eq!(claim.ownership.as_ref().unwrap().teams, [seat.team]);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = true\ndrain = [{ phase = [\"review\"], team = [\"self\"] }]\n",
    )
    .unwrap();
    let held = board(&bus, epoch, actor.clone());
    let done = assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::TaskDone,
        progress_witnesses(&held, at(&held, &task), seat.lane, seat.occupant, false).unwrap(),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(&task, "Current work is ready for review")
        },
    ));
    let CommandAnswer::Task(done) = done else {
        panic!("completion reply required");
    };
    assert_eq!(done.ownership.as_ref().unwrap().teams, [seat.team]);
    let after = board(&bus, epoch, actor);
    assert_eq!(at(&after, &task).workflow.body.phase.as_str(), "review");
    assert_eq!(
        at(&after, &task).statement.body.creation.creator_team(),
        Some(seat.team)
    );
    clean(&root, bus);
}

#[test]
fn topic_ancestor_movement_refuses_stale_ownership_without_a_task_revision_change() {
    let (root, bus, epoch, seats) = fixture("owner-topic-race", 1);
    fs::write(
        root.join("spice.toml"),
        "[team.default]\ndrive = true\ndrain = [{ topic = [\"/owned/**\"] }]\n",
    )
    .unwrap();
    subscription::configure_subscription(&bus, epoch, &seats[0], true);
    let task = issue_add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "owner-topic-race",
        authored_add(
            "Keep ownership current across topic moves",
            "/owned/work",
            Some("todo"),
            &["Ownership is current"],
        ),
    );
    let captured = validated_snapshot(&bus, None, Actor::TEST_PRINCIPAL).unwrap();
    let entry = task_entry(&captured.entries, &task).unwrap();
    assert_eq!(captured.ownership(entry).teams, [seats[0].team]);
    let statement_position = at(&captured.answer, &task).statement.position;
    let topic_node = entry.statement().topic.nodes;
    let origin: wire::Origin = format!("task:{}", entry.born()).parse().unwrap();
    let mut moved = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin.clone(),
        current_instant().unwrap(),
        Producer::CommandDoor,
    );
    moved
        .fact(
            wire::topic::TopicMoved {
                from: "/owned".parse().unwrap(),
                to: "/outside".parse().unwrap(),
            },
            FactCause::Root,
        )
        .unwrap();
    assert!(matches!(
        bus.commit(moved).unwrap(),
        bus::CommitOutcome::Committed(_)
    ));
    let after_move = bus.cursor().unwrap();
    let proposal = || {
        let mut write = JournalWrite::new(
            Actor::TEST_PRINCIPAL,
            origin.clone(),
            current_instant().unwrap(),
            Producer::CommandDoor,
        );
        write
            .fact(
                wire::topic::TopicDescribed {
                    topic: "/outside/work".parse().unwrap(),
                    description: "Current allocation ownership".into(),
                },
                FactCause::Root,
            )
            .unwrap();
        write
    };
    let mut stale = proposal();
    captured.guard_ownership(&mut stale);
    let outcome = bus.commit(stale).unwrap();
    assert!(
        matches!(&outcome, bus::CommitOutcome::Refused(refusal)
        if refusal.reason() == "team::topic-moved"),
        "{outcome:?}"
    );
    assert_eq!(bus.cursor().unwrap(), after_move);
    let current = validated_snapshot(&bus, None, Actor::TEST_PRINCIPAL).unwrap();
    let moved_task = TaskHandle::for_topic(
        &"/outside/work".parse::<wire::Topic>().unwrap(),
        &task.born().unwrap(),
    );
    let entry = task_entry(&current.entries, &moved_task).unwrap();
    assert_eq!(
        at(&current.answer, &moved_task).statement.position,
        statement_position
    );
    assert_eq!(entry.statement().topic.nodes, topic_node);
    assert!(current.ownership(entry).teams.is_empty());
    let mut fresh = proposal();
    current.guard_ownership(&mut fresh);
    assert!(matches!(
        bus.commit(fresh).unwrap(),
        bus::CommitOutcome::Committed(_)
    ));
    clean(&root, bus);
}
