use super::*;

fn capture_reason(result: Result<TaskCreationCapture, Outcome<CommandAnswer>>) -> String {
    match result {
        Err(Outcome::Refusal(refusal)) => refusal.reason().to_owned(),
        Err(Outcome::Fault(fault)) => fault.reason,
        Err(Outcome::Reply(_)) => panic!("creation capture cannot reply"),
        Ok(_) => panic!("creation capture unexpectedly admitted"),
    }
}

fn add_command(epoch: CanonicalUuid, lane: CanonicalUuid) -> wire::AdmittedCommand {
    fixture_command(
        epoch,
        Actor::Lane(lane),
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: stamp("creation-context-command"),
        }],
        &TaskAdd {
            topic: Some("/task/provenance".parse().expect("fixture topic")),
            origin: Some(fixture_origin()),
            flow: Some("todo".to_owned()),
            ..adding(
                "Capture exact creation authority",
                "This work keeps the fixture honest.",
            )
        },
    )
}

#[test]
fn lane_creation_refuses_missing_session_and_missing_accounted_source() {
    let (root, bus, epoch, seats) = fixture("creation-context-missing", 1);
    let lane = seats[0].lane;
    let command = add_command(epoch, lane);
    let snapshot =
        validated_snapshot(&bus, None, Actor::Lane(lane)).expect("fixture snapshot validates");
    assert_eq!(
        capture_reason(task_creation_capture(&bus, &command, &snapshot)),
        "task::creation-context-session-missing"
    );

    declare_session(&bus, epoch, &seats[0], uuid(9_301), 301);
    let snapshot =
        validated_snapshot(&bus, None, Actor::Lane(lane)).expect("session snapshot validates");
    assert_eq!(
        capture_reason(task_creation_capture(&bus, &command, &snapshot)),
        "task::creation-context-source-ambiguous"
    );
    clean(&root, bus);
}

#[test]
fn lane_creation_refuses_a_causal_session_that_disagrees_with_its_occupant() {
    let (root, bus, epoch, seats) = fixture("creation-context-mismatch", 1);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let lane = seats[0].lane;
    let command = add_command(epoch, lane);
    let mut snapshot =
        validated_snapshot(&bus, None, Actor::Lane(lane)).expect("fixture snapshot validates");
    snapshot.answer.residency.body.lanes[0]
        .session
        .as_mut()
        .expect("fixture session")
        .occupant = uuid(9_399);
    assert_eq!(
        capture_reason(task_creation_capture(&bus, &command, &snapshot)),
        "task::creation-context-session-mismatch"
    );
    clean(&root, bus);
}

#[test]
fn lane_creation_refuses_a_sole_unregistered_stream() {
    let (root, bus, epoch, seats) = fixture("creation-context-unregistered-stream", 1);
    let lane = seats[0].lane;
    declare_session(&bus, epoch, &seats[0], uuid(9_302), 302);
    seed_creation_stream(&bus, lane, "stdout");
    let snapshot =
        validated_snapshot(&bus, None, Actor::Lane(lane)).expect("session snapshot validates");
    assert_eq!(
        capture_reason(task_creation_capture(
            &bus,
            &add_command(epoch, lane),
            &snapshot,
        )),
        "task::creation-context-stream-unregistered"
    );
    clean(&root, bus);
}

#[test]
fn lane_creation_guard_refuses_when_the_accounted_source_moves_before_commit() {
    let (root, bus, epoch, seats) = fixture("creation-context-source-race", 1);
    let expected = authorize_lane_creation(&bus, epoch, &seats[0]);
    let lane = seats[0].lane;
    let command = add_command(epoch, lane);
    let snapshot =
        validated_snapshot(&bus, None, Actor::Lane(lane)).expect("fixture snapshot validates");
    let capture = task_creation_capture(&bus, &command, &snapshot)
        .expect("exact lane creation context admits");
    assert_eq!(
        capture.creator_team(),
        Some(seats[0].team),
        "the same guarded residency observation captures creator-team ownership",
    );
    let body::TaskCreationContext::Transcript { boundary, .. } = expected else {
        unreachable!("lane authorization returns transcript context")
    };
    let moved = body::TranscriptPosition {
        member: boundary.next.member,
        offset: boundary.next.offset + 128,
    };
    let mut accounting = JournalWrite::new(
        Actor::Spice,
        format!("lane:{lane}").parse().expect("fixture origin"),
        Instant::new(jiff::Timestamp::now()).expect("current fixture instant"),
        Producer::ResidentRecognition,
    );
    let ingested = accounting
        .fact(
            body::JournalIngested {
                lane,
                vendor: boundary.vendor.clone(),
                stream: boundary.stream.clone(),
                source: boundary.source,
                supersedes: None,
                from: boundary.next,
                to: moved,
                usage: Vec::new(),
                non_usage_records: 1,
                residue_records: 0,
            },
            FactCause::Root,
        )
        .expect("source continuation builds");
    accounting.transcript_source_head(ingested, move |_| wire::TranscriptSourceHead {
        next: moved,
        ..boundary
    });
    assert!(matches!(
        bus.commit(accounting)
            .expect("source continuation reaches writer"),
        CommitOutcome::Committed(_)
    ));

    let mut guarded = JournalWrite::new(
        Actor::Lane(lane),
        "fixture:creation-context-guard"
            .parse()
            .expect("fixture origin"),
        Instant::new(jiff::Timestamp::now()).expect("current fixture instant"),
        Producer::CommandDoor,
    );
    capture.guard(&mut guarded);
    guarded
        .fact(
            body::TaskWoken {
                task: stamp("creation-context-guard"),
            },
            FactCause::Root,
        )
        .expect("guarded proposal builds");
    let outcome = bus.commit(guarded).expect("guarded write reaches writer");
    let CommitOutcome::Refused(refusal) = outcome else {
        panic!("moved creation source must refuse, found {outcome:?}");
    };
    assert_eq!(refusal.reason(), "task::creation-context-source-moved");
    clean(&root, bus);
}

#[test]
fn lane_creation_guard_refuses_when_creator_team_moves_before_commit() {
    let (root, bus, epoch, seats) = fixture("creation-context-team-race", 1);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let lane = seats[0].lane;
    let command = add_command(epoch, lane);
    let snapshot =
        validated_snapshot(&bus, None, Actor::Lane(lane)).expect("fixture snapshot validates");
    let capture = task_creation_capture(&bus, &command, &snapshot)
        .expect("exact lane creation context admits");
    assert_eq!(capture.creator_team(), Some(seats[0].team));

    super::subscription::move_fixture_lane_to_team(&bus, epoch, &seats[0], uuid(9_999));

    let mut guarded = JournalWrite::new(
        Actor::Lane(lane),
        "fixture:creation-team-guard"
            .parse()
            .expect("fixture origin"),
        Instant::new(jiff::Timestamp::now()).expect("current fixture instant"),
        Producer::CommandDoor,
    );
    capture.guard(&mut guarded);
    guarded
        .fact(
            body::TaskWoken {
                task: stamp("creation-team-guard"),
            },
            FactCause::Root,
        )
        .expect("guarded proposal builds");
    let outcome = bus.commit(guarded).expect("guarded write reaches writer");
    let CommitOutcome::Refused(refusal) = outcome else {
        panic!("moved creator team must refuse, found {outcome:?}");
    };
    assert_eq!(refusal.reason(), "task::creation-context-residency-moved");
    clean(&root, bus);
}
