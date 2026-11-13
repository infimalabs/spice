use super::*;

/// Shape CASE 147 / INVARIANTS ID-02: lexical identity order is birth order
/// within the current generation.
#[test]
fn plain_string_stamp_and_handle_order_is_birth_order() {
    let micros = [2_705_i64, 51, 2_704, 52, 53];
    let mut stamps = micros
        .into_iter()
        .map(|micros| wire::TaskStamp::for_micros(micros).expect("fixture birth stamp"))
        .collect::<Vec<_>>();
    stamps.sort();
    let expected = [51_i64, 52, 53, 2_704, 2_705]
        .into_iter()
        .map(|micros| wire::TaskStamp::for_micros(micros).expect("fixture birth stamp"))
        .collect::<Vec<_>>();
    assert_eq!(stamps, expected);

    let mut handles = micros
        .into_iter()
        .map(|micros| {
            TaskHandle::for_topic(
                &"/task/identity"
                    .parse::<wire::Topic>()
                    .expect("fixture topic"),
                &wire::TaskStamp::for_micros(micros).expect("fixture birth stamp"),
            )
        })
        .collect::<Vec<_>>();
    handles.sort();
    assert_eq!(
        handles
            .iter()
            .map(|handle| handle.born().expect("sorted handle stamp"))
            .collect::<Vec<_>>(),
        expected
    );
}

/// Every creation route advances same-microsecond collisions.
#[test]
fn same_microsecond_adds_and_followups_receive_ordered_distinct_stamps() {
    let alphabet = (0..52_i64)
        .map(|micros| {
            wire::TaskStamp::for_micros(micros)
                .expect("alphabet fixture birth stamp")
                .as_str()
                .as_bytes()[wire::TaskStamp::WIDTH - 1]
        })
        .collect::<Vec<_>>();
    assert!(alphabet.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(
        alphabet.iter().all(|byte| !b"AEIOUaeiou".contains(byte)),
        "task stamp alphabet must remain vowel-free"
    );

    let (root, bus, epoch, _seats) = fixture("identity-collision", 0);
    let candidate_micros = 90_000_i64;
    let first_stamp =
        wire::TaskStamp::for_micros(candidate_micros).expect("first fixture birth stamp");
    add_born(
        &bus,
        epoch,
        first_stamp.clone(),
        "First same-microsecond task",
    );

    let after_first = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let second_stamp =
        allocate_task_stamp(&after_first, candidate_micros).expect("collision advances");
    assert_eq!(
        second_stamp,
        wire::TaskStamp::for_micros(candidate_micros + 1).expect("second fixture birth stamp")
    );
    add_born(
        &bus,
        epoch,
        second_stamp.clone(),
        "Second same-microsecond task",
    );

    let after_second = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let followups =
        allocate_task_stamps(&after_second, candidate_micros, 3).expect("batch advances");
    let expected = (2..5)
        .map(|offset| {
            wire::TaskStamp::for_micros(candidate_micros + offset)
                .expect("advanced fixture birth stamp")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        followups, expected,
        "review follow-ups retain authored order"
    );
    clean(&root, bus);
}

#[test]
fn occupied_last_stamp_refuses_beyond_the_task_stamp_clock() {
    let (root, bus, epoch, _seats) = fixture("identity-clock-overflow", 0);
    let last_micros = 52_i64.pow(wire::TaskStamp::WIDTH as u32) - 1;
    let last = wire::TaskStamp::for_micros(last_micros).expect("last stamp is representable");
    add_born(&bus, epoch, last, "Last-microsecond task");

    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let error =
        allocate_task_stamp(&current, last_micros).expect_err("occupied last stamp cannot advance");
    let Outcome::Fault(fault) = error else {
        panic!("clock overflow must be an environment fault");
    };
    assert_eq!(fault.reason, "task::stamp-clock-invalid");
    clean(&root, bus);
}

#[test]
fn a_won_claim_carries_its_whole_statement() {
    let (root, bus, epoch, seats) = fixture("claim-whole-statement", 1);
    let lane = Actor::Lane(seats[0].lane);
    let born = wire::TaskStamp::for_micros(8_002).expect("fixture task stamp");
    let task = TaskHandle::for_topic(
        &"/task/cli".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    );
    let term = vec![
        "First criterion".to_owned(),
        "Second criterion\n  continued".to_owned(),
    ];
    let statement = wire::intent::ClaimStatement {
        title: "Hand the holder its contract".to_owned(),
        pitch: "The holder works from the whole statement.".to_owned(),
        description: Some(format!("Why it matters.\n\n{}", "context ".repeat(400))),
        term: statement_items_with_handles(&term),
        completion_contract: None,
        design_research: None,
    };
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born }],
        &TaskAdd {
            topic: Some("/task/cli".parse().expect("fixture topic")),
            description: statement.description.clone(),
            term,
            origin: Some(fixture_origin()),
            flow: Some("todo".to_owned()),
            ..adding(statement.title.clone(), statement.pitch.clone())
        },
    ));
    let available = board(&bus, epoch, lane.clone());
    let answer = assert_committed(issue(
        &bus,
        epoch,
        lane,
        CommandKind::ClaimWin,
        claim_expected(&available, &task, &seats[0]),
        &next_rule(),
    ));
    let CommandAnswer::Claim(claim) = answer else {
        panic!("claim win returned another answer shape");
    };
    assert_eq!(claim.task, task);
    let mut delivered = *claim.statement.expect("a won claim carries its statement");
    assert!(
        delivered.completion_contract.take().is_some(),
        "acquisition discloses the completion contract"
    );
    assert_eq!(delivered, statement);
    clean(&root, bus);
}

#[test]
fn active_todo_task_show_prints_the_parser_owned_done_command() {
    let (root, bus, epoch, seats) = fixture("todo-show-next-action", 1);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "SHOW-todo0001",
        "Render the active todo next action",
        Some("todo"),
    );
    let claim = capture_for(&bus, epoch, &seats[0], &task);
    assert_eq!(claim.task, task);
    assert_eq!(
        claim.creation_context.as_deref(),
        Some("unavailable principal-actor"),
        "assignment projects an explicit unavailable creation reason"
    );

    let claimed = board(&bus, epoch, Actor::Lane(seats[0].lane));
    let shown = task_show(&claimed, at(&claimed, &task), &[]);
    assert!(shown.contains("creation_context unavailable principal-actor"));
    assert!(shown.contains("claim_context unavailable registered-session-stream-missing"));
    assert!(shown.contains(&format!("spice doc ledger {task}")));
    assert!(
        shown.contains("next: spice task done --validation \"...\""),
        "the detail packet exposes the parser-owned task done form: {shown}"
    );
    assert!(
        !shown.contains(&format!("next: spice task done {task}")),
        "the detail packet must not advertise the rejected handle form: {shown}"
    );
    clean(&root, bus);
}

#[test]
fn task_show_prints_creation_context_as_one_shell_executable_current_command() {
    let (root, bus, epoch, _) = fixture("task-show-creation-command", 0);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "SHOW-context01",
        "Render exact creation context",
        Some("todo"),
    );
    let answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let mut row = at(&answer, &task).clone();
    let lane = uuid(9_401);
    let source = uuid(9_402);
    row.statement.body.creation = body::TaskCreationContext::Transcript {
        session: "session with ' quote".to_owned(),
        boundary: wire::TranscriptSourceHead {
            lane,
            vendor: "omp".parse().expect("fixture vendor"),
            stream: "session".to_owned(),
            source,
            next: body::TranscriptPosition {
                member: 4,
                offset: 512,
            },
        },
        creator_team: None,
    };
    let shown = task_show(&answer, &row, &[]);
    let command = shown
        .lines()
        .find(|line| line.starts_with("spice session context "))
        .expect("task show carries one executable context command");
    assert_eq!(
        shell_words::split(command).expect("rendered command is valid shell words"),
        vec![
            "spice".to_owned(),
            "session".to_owned(),
            "context".to_owned(),
            "session with ' quote".to_owned(),
            "--lane".to_owned(),
            lane.to_string(),
            "--vendor".to_owned(),
            "omp".to_owned(),
            "--stream".to_owned(),
            "session".to_owned(),
            "--source".to_owned(),
            source.to_string(),
            "--member".to_owned(),
            "4".to_owned(),
            "--offset".to_owned(),
            "512".to_owned(),
        ]
    );
    clean(&root, bus);
}

#[test]
fn claim_context_recovery_requires_one_exact_session_authority() {
    let lane = uuid(9_410);
    let other_lane = uuid(9_411);
    let occupant = uuid(9_412);
    let epoch = uuid(9_413);
    let vendor: Name = "omp".parse().expect("fixture vendor");
    let stream = body::TASK_CREATION_CONTEXT_STREAM.to_owned();
    let mut won = body::ClaimWon {
        task: stamp("claim-context-authority"),
        actor: Actor::Lane(lane),
        lane,
        occupant,
        epoch,
        cell: wire::ClaimCell {
            topic: Some("/board/context".parse().expect("fixture topic")),
            phase: "todo".parse().expect("fixture phase"),
        },
        until: Instant::new(jiff::Timestamp::now()).expect("fixture instant"),
        git: None,
        recovery: None,
        source: body::SourceBoundary::Proven {
            heads: vec![wire::TranscriptSourceHead {
                lane: other_lane,
                vendor: vendor.clone(),
                stream: stream.clone(),
                source: uuid(9_414),
                next: body::TranscriptPosition {
                    member: 7,
                    offset: 1_024,
                },
            }],
        },
    };
    let mut sessions = std::collections::BTreeMap::new();
    assert_eq!(
        claim_transcript_recovery(&won, &sessions),
        wire::intent::TaskTranscriptRecovery::Unavailable {
            reason: "registered-session-lane-mismatch".to_owned(),
        }
    );

    let body::SourceBoundary::Proven { heads } = &mut won.source;
    heads[0].lane = lane;
    sessions.insert(
        (lane, occupant, vendor.clone(), uuid(9_415)),
        "wrong-epoch".to_owned(),
    );
    assert_eq!(
        claim_transcript_recovery(&won, &sessions),
        wire::intent::TaskTranscriptRecovery::Unavailable {
            reason: "session-declaration-missing".to_owned(),
        }
    );

    let key = (lane, occupant, vendor, epoch);
    sessions.insert(key.clone(), String::new());
    assert_eq!(
        claim_transcript_recovery(&won, &sessions),
        wire::intent::TaskTranscriptRecovery::Unavailable {
            reason: "session-identity-empty".to_owned(),
        }
    );
    sessions.insert(key, "exact-session".to_owned());
    assert_eq!(
        claim_transcript_recovery(&won, &sessions),
        wire::intent::TaskTranscriptRecovery::Transcript {
            session: "exact-session".to_owned(),
        }
    );
}

#[test]
fn allocator_assignment_prints_the_lane_tasks_exact_creation_command() {
    let (root, bus, epoch, seats) = fixture("task-next-creation-command", 1);
    let expected = authorize_lane_creation(&bus, epoch, &seats[0]);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        lane.clone(),
        "NEXT-context01",
        "Assign exact creation context",
        Some("todo"),
    );
    let available = board(&bus, epoch, lane.clone());
    let answer = assert_committed(issue(
        &bus,
        epoch,
        lane,
        CommandKind::ClaimWin,
        claim_expected(&available, &task, &seats[0]),
        &next_rule(),
    ));
    let CommandAnswer::Claim(claim) = answer else {
        panic!("claim win returned another answer shape");
    };
    let output = claim
        .creation_context
        .as_deref()
        .expect("fresh assignment carries creation context");
    assert_eq!(output, task_creation_context_output(&expected).as_ref());
    assert_eq!(
        shell_words::split(output)
            .expect("assignment context is shell executable")
            .first()
            .map(String::as_str),
        Some("spice")
    );
    let claimed = board(&bus, epoch, Actor::Lane(seats[0].lane));
    let row = at(&claimed, &task);
    let (context, boundary) = assert_claim_context(row, &expected);
    let shown = task_show(&claimed, row, &[]);
    assert!(shown.contains(&format!(
        "claim_context fact={} lane={} occupant={} epoch={}",
        context.claimed, context.lane, context.occupant, context.epoch
    )));
    assert_eq!(
        shown
            .lines()
            .filter(|line| *line == task_creation_context_output(&expected).as_ref())
            .count(),
        2,
        "creation and claim coordinates independently recover the same unchanged source"
    );
    continue_claim_transcript(&bus, &context, &boundary);
    carry_claim_and_assert_context(&bus, epoch, &task, &context);
    clean(&root, bus);
}

fn assert_claim_context(
    row: &BoardTask,
    expected: &body::TaskCreationContext,
) -> (wire::intent::TaskClaimContext, wire::TranscriptSourceHead) {
    let context = row
        .claim_context
        .as_ref()
        .expect("claimed task carries compact claim provenance");
    let wire::intent::TaskTranscriptRecovery::Transcript { session } = &context.recovery else {
        panic!("accounted claim context must be executable: {context:?}");
    };
    let body::TaskCreationContext::Transcript {
        session: expected_session,
        boundary: expected_boundary,
        ..
    } = expected
    else {
        unreachable!("authorized lane creation has transcript context")
    };
    assert_eq!(session, expected_session);
    let body::SourceBoundary::Proven { heads } = &context.source;
    let boundary = heads
        .iter()
        .find(|head| head.stream == body::TASK_CREATION_CONTEXT_STREAM)
        .expect("claim source retains its registered session boundary");
    assert_eq!(boundary, expected_boundary);
    (context.clone(), boundary.clone())
}

fn continue_claim_transcript(
    bus: &Bus,
    context: &wire::intent::TaskClaimContext,
    boundary: &wire::TranscriptSourceHead,
) {
    let moved_position = body::TranscriptPosition {
        member: boundary.next.member,
        offset: boundary.next.offset + 64,
    };
    let moved_boundary = wire::TranscriptSourceHead {
        next: moved_position,
        ..boundary.clone()
    };
    let mut continuation = JournalWrite::new(
        Actor::Spice,
        format!("lane:{}", context.lane)
            .parse()
            .expect("fixture origin"),
        Instant::new(jiff::Timestamp::now()).expect("fixture instant"),
        Producer::ResidentRecognition,
    );
    let ingested = continuation
        .fact(
            body::JournalIngested {
                lane: moved_boundary.lane,
                vendor: moved_boundary.vendor.clone(),
                stream: moved_boundary.stream.clone(),
                source: moved_boundary.source,
                supersedes: None,
                from: boundary.next,
                to: moved_position,
                usage: Vec::new(),
                non_usage_records: 1,
                residue_records: 0,
            },
            FactCause::Root,
        )
        .expect("transcript continuation builds");
    continuation.transcript_source_head(ingested, move |_| moved_boundary.clone());
    assert!(matches!(
        bus.commit(continuation)
            .expect("transcript continuation commits"),
        CommitOutcome::Committed(_)
    ));
}

fn carry_claim_and_assert_context(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &TaskHandle,
    context: &wire::intent::TaskClaimContext,
) {
    let before_carry = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let prior_claim = at(&before_carry, task)
        .latest_claim()
        .map(|held| &held.claim)
        .expect("fixture claim")
        .body
        .clone();
    let task_stamp = at(&before_carry, task).statement.body.born.clone();
    let source = bus
        .source_boundary(&context.lane)
        .expect("continued source boundary reads");
    let mut carry = JournalWrite::new(
        Actor::Spice,
        format!("lane:{}", context.lane)
            .parse()
            .expect("fixture carry origin"),
        Instant::new(jiff::Timestamp::now()).expect("fixture carry instant"),
        Producer::ResidentRecognition,
    );
    let carried = carry
        .fact(
            body::ClaimCarried {
                task: task_stamp.clone(),
                lane: context.lane,
                occupant: prior_claim.occupant,
                ancestor: prior_claim.occupant,
                epoch: prior_claim.epoch,
                source,
            },
            FactCause::Root,
        )
        .expect("claim carry builds");
    carry.claim_head(task_stamp, carried, move |fact| wire::ClaimHead {
        fact,
        ..prior_claim
    });
    assert!(matches!(
        bus.commit(carry).expect("claim carry commits"),
        CommitOutcome::Committed(_)
    ));
    assert_eq!(
        bus.task_snapshot()
            .expect("carried snapshot reads")
            .task_head_update_visits(),
        1,
        "claim carry updates one live head"
    );
    let after_carry = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let carried_row = at(&after_carry, task);
    assert_ne!(
        carried_row
            .latest_claim()
            .map(|held| &held.claim)
            .expect("carried claim")
            .position,
        context.claimed,
        "the claim head moved to the carry fact"
    );
    assert_eq!(
        carried_row.claim_context.as_ref(),
        Some(context),
        "claim recovery remains anchored to ClaimWon across continuation and carry"
    );
}

#[test]
fn task_show_keeps_git_provenance_compact_and_prints_its_exact_lookup() {
    let (root, bus, epoch, _) = fixture("task-show-git-context", 0);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "SHOW-git00001",
        "Recover one Git boundary",
        Some("todo"),
    );
    let task_stamp = task.born().expect("fixture handle has a stamp");
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        format!("task:{task_stamp}")
            .parse()
            .expect("fixture task origin"),
        Instant::new(jiff::Timestamp::now()).expect("fixture instant"),
        Producer::CommandDoor,
    );
    write
        .fact(
            body::TaskDone {
                task: task_stamp,
                validation: "Git context fixture".to_owned(),
                disposition: None,
                terminal: None,
                git: Some(body::GitBoundary {
                    head: "agent-head".to_owned(),
                    integrated: "missing-merge-object".to_owned(),
                    upstream: Some("origin/main".to_owned()),
                    upstream_head: Some("baseline-head".to_owned()),
                    local_commits: 2,
                }),
                source: body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Root,
        )
        .expect("completion provenance builds");
    let committed = match bus.commit(write).expect("completion provenance commits") {
        CommitOutcome::Committed(committed) => committed,
        outcome => panic!("completion provenance did not commit: {outcome:?}"),
    };
    let completed = Seq::new(committed.first()).expect("committed fact position");
    let answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&answer, &task);
    assert_eq!(
        row.git_context,
        Some(wire::intent::TaskGitContext {
            fact: completed,
            kind: wire::intent::TaskGitContextKind::Completion,
            boundary: body::GitBoundary {
                head: "agent-head".to_owned(),
                integrated: "missing-merge-object".to_owned(),
                upstream: Some("origin/main".to_owned()),
                upstream_head: Some("baseline-head".to_owned()),
                local_commits: 2,
            },
        })
    );

    let shown = task_show(&answer, row, &[]);
    assert!(shown.contains(&format!(
        "git_context kind=completion fact={} head=agent-head integrated=missing-merge-object local_commits=2",
        row.git_context.as_ref().expect("Git context").fact
    )));
    assert!(shown.contains("git_upstream origin/main"));
    assert!(shown.contains("git_upstream_head baseline-head"));
    let command = shown
        .lines()
        .find(|line| line.starts_with("git show "))
        .expect("Git context carries one executable object lookup");
    assert_eq!(
        shell_words::split(command).expect("Git lookup is valid shell words"),
        [
            "git",
            "show",
            "-m",
            "--first-parent",
            "--stat",
            "--patch",
            "--end-of-options",
            "missing-merge-object",
        ]
    );

    let mut claimed = row.clone();
    claimed.git_context.as_mut().expect("Git context").kind =
        wire::intent::TaskGitContextKind::Claim;
    let claimed_show = task_show(&answer, &claimed, &[]);
    let claim_command = claimed_show
        .lines()
        .find(|line| line.starts_with("git show "))
        .expect("claim Git context carries an executable object lookup");
    assert_eq!(
        shell_words::split(claim_command).expect("claim Git lookup is valid shell words"),
        [
            "git",
            "show",
            "--no-patch",
            "--format=fuller",
            "--end-of-options",
            "agent-head",
        ]
    );
    clean(&root, bus);
}

#[test]
fn task_status_counts_one_snapshot_and_task_add_retains_authored_context() {
    let (root, bus, epoch, _) = fixture("task-bootstrap", 1);
    let born = wire::TaskStamp::for_micros(8_001).expect("fixture task stamp");
    let task = TaskHandle::for_topic(
        &"/task/cli".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    );
    let origin: wire::Origin = "ack:1kLGp2NJx".parse().expect("fixture origin");
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born }],
        &TaskAdd {
            topic: Some("/task/cli".parse().expect("fixture topic")),
            description: Some("The agent needs the complete handoff".to_owned()),
            term: vec![
                "Status reports board counts".to_owned(),
                "Creation provenance survives".to_owned(),
            ],
            origin: Some(origin.clone()),
            flow: Some("todo".to_owned()),
            ..adding(
                "Carry native bootstrap context",
                "This work keeps the fixture honest.",
            )
        },
    ));
    add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-review001",
        "Review native bootstrap",
        Some("review"),
    );

    let answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let statement = &at(&answer, &task).statement.body;
    assert_eq!(
        statement.description.as_deref(),
        Some("The agent needs the complete handoff")
    );
    assert_eq!(
        statement.term,
        [
            "Status reports board counts",
            "Creation provenance survives"
        ]
    );
    assert_eq!(statement.provenance.origin(), origin.clone());
    assert_eq!(creation_fact_origin(&bus, &task), origin);
    assert_eq!(
        at(&answer, &task).origin_thread,
        Actor::TEST_PRINCIPAL.to_string()
    );

    let status = task_status(&answer).expect("validated board yields status");
    for expected in [
        "claim -",
        "taste-review -",
        "actor principal:00000000-0000-0000-0000-000000000000",
        "active 0",
        "ready 1",
        "review 1",
        "blocked 0",
        "waiting 0",
        "stale 0",
    ] {
        assert!(status.lines().any(|line| line == expected), "{status}");
    }
    clean(&root, bus);
}

#[test]
fn task_list_preserves_scope_status_topic_and_relevance_limit_semantics() {
    let (root, bus, epoch, _) = fixture("task-list", 0);
    let (answer, first, descendant, waiting) = task_list_fixture(&bus, epoch);
    assert_task_list_queries(&answer, &first, &descendant, &waiting);
    clean(&root, bus);
}

#[test]
fn shortened_task_list_names_original_and_withheld_row_counts() {
    let (root, bus, epoch, _) = fixture("task-list-shortened", 0);
    let (answer, _, _, _) = task_list_fixture(&bus, epoch);
    let listed = task_list(
        &answer,
        &TaskListOptions {
            all: true,
            topic: None,
            limit: Some(2),
            status: None,
        },
    );

    assert_eq!(listed.matches(" [todo]").count(), 2, "{listed}");
    assert!(listed.contains("withheld 2 of 4 rows"), "{listed}");
    assert!(listed.contains("continue: spice task list"), "{listed}");
    assert!(listed.contains("--after"), "{listed}");
    clean(&root, bus);
}

#[test]
fn complete_task_list_emits_no_withheld_notice() {
    let (root, bus, epoch, _) = fixture("task-list-complete", 0);
    let (answer, _, _, _) = task_list_fixture(&bus, epoch);
    let listed = task_list(
        &answer,
        &TaskListOptions {
            all: true,
            topic: None,
            limit: Some(4),
            status: None,
        },
    );

    assert!(!listed.contains("withheld"), "{listed}");
    clean(&root, bus);
}

#[test]
fn actor_route_projects_the_current_lane_session_identity() {
    let (root, bus, epoch, seats) = fixture("actor-route-session", 1);
    let answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let mut residency = answer.residency.body;
    let identity = uuid(9_001);
    let row = residency.lanes.first_mut().expect("fixture lane");
    let occupant = row.occupant.expect("fixture occupant");
    let unstarted = actor_route(&Actor::Lane(seats[0].lane), &residency)
        .expect("an occupied seat that has not started names its occupant route");
    assert_eq!(unstarted.actor, occupant.to_string());

    let row = residency.lanes.first_mut().expect("fixture lane");
    row.session = Some(wire::LaneSession {
        lane: row.lane,
        occupant,
        worktree: row.worktree.clone(),
        vendor: row.vendor.clone(),
        launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
        epoch,
        stream: "stdout".to_owned(),
        identity: identity.to_string(),
        locus: wire::TapeLocus {
            member: 0,
            offset: 0,
            length: 1,
        },
    });

    let route = actor_route(&Actor::Lane(seats[0].lane), &residency)
        .expect("current session identity names a route");
    let actor = identity.to_string();
    assert_eq!(route.actor, actor);
    assert_eq!(route.origin_thread, actor);

    residency.lanes[0]
        .session
        .as_mut()
        .expect("fixture session")
        .identity = "session-id-with-hyphens".to_owned();
    assert!(
        actor_route(&Actor::Lane(seats[0].lane), &residency).is_err(),
        "a current malformed session route must fail instead of falling back"
    );
    clean(&root, bus);
}

#[test]
fn lane_task_origin_thread_comes_from_its_durable_creation_session() {
    let (root, bus, epoch, seats) = fixture("task-origin-session", 1);
    let first_identity = uuid(9_101);
    declare_session(&bus, epoch, &seats[0], first_identity, 101);
    seed_creation_transcript(&bus, seats[0].lane);
    let first = add(
        &bus,
        epoch,
        Actor::Lane(seats[0].lane),
        "CLI-origin001",
        "Retain the creating session",
        Some("todo"),
    );
    let second_identity = uuid(9_102);
    declare_session(&bus, epoch, &seats[0], second_identity, 102);
    let second = add(
        &bus,
        epoch,
        Actor::Lane(seats[0].lane),
        "CLI-origin002",
        "Retain the successor session",
        Some("todo"),
    );
    let answer = board(&bus, epoch, Actor::Lane(seats[0].lane));
    let first_actor = first_identity.to_string();
    let second_actor = second_identity.to_string();
    assert_eq!(answer.actor_route.origin_thread, second_actor);
    assert_eq!(at(&answer, &first).origin_thread, first_actor);
    assert_eq!(at(&answer, &second).origin_thread, second_actor);
    clean(&root, bus);
}

fn task_list_fixture(
    bus: &Bus,
    epoch: CanonicalUuid,
) -> (BoardAnswer, TaskHandle, TaskHandle, TaskHandle) {
    let first = add_task_in_topic(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-list0001",
        "First task route",
        "/task/cli",
        Some("todo"),
    );
    let descendant = add_task_in_topic(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-list0002",
        "Descendant task route",
        "/task/cli/deep",
        Some("todo"),
    );
    let waiting = add_task_in_topic(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-list0003",
        "Deferred serve route",
        "/serve/ops",
        Some("todo"),
    );
    let completed = add_task_in_topic(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-list0004",
        "Completed board route",
        "/board/test",
        Some("todo"),
    );
    let mut answer = board(bus, epoch, Actor::TEST_PRINCIPAL);
    answer
        .tasks
        .iter_mut()
        .find(|task| task.statement.body.handle == first)
        .expect("first row")
        .statement
        .body
        .priority = Some(wire::TaskPriority::Medium);
    answer
        .tasks
        .iter_mut()
        .find(|task| task.statement.body.handle == waiting)
        .expect("waiting row")
        .workflow
        .body
        .deferral = Some(fixture_instant("fixture deferral"));
    let completed_row = answer
        .tasks
        .iter_mut()
        .find(|task| task.statement.body.handle == completed)
        .expect("completed row");
    completed_row.workflow.body.status =
        wire::WorkflowStatus::completed(fixture_instant("fixture completion"));
    (answer, first, descendant, waiting)
}

fn assert_task_list_queries(
    answer: &BoardAnswer,
    first: &TaskHandle,
    descendant: &TaskHandle,
    waiting: &TaskHandle,
) {
    let default = task_list(
        answer,
        &TaskListOptions {
            all: false,
            topic: None,
            limit: None,
            status: None,
        },
    );
    assert!(default.contains(&format!("{first} [todo] P:M /task/cli First task route")));
    assert!(default.contains(&format!(
        "{descendant} [todo] /task/cli/deep Descendant task route"
    )));
    assert!(!default.contains(waiting.as_str()), "{default}");

    let topic = task_list(
        answer,
        &TaskListOptions {
            all: false,
            topic: Some("/task/cli".parse().expect("fixture topic")),
            limit: None,
            status: Some(TaskListStatus::Pending),
        },
    );
    assert!(topic.starts_with("scope explicit-topic topic:/task/cli\n"));
    assert!(topic.contains(first.as_str()));
    assert!(
        topic.contains(descendant.as_str()),
        "--topic /task/cli covers its subtree, including /task/cli/deep\n{topic}"
    );
    assert!(!topic.contains(waiting.as_str()));

    let waiting_only = task_list(
        answer,
        &TaskListOptions {
            all: false,
            topic: None,
            limit: None,
            status: Some(TaskListStatus::Waiting),
        },
    );
    assert!(waiting_only.contains(waiting.as_str()));
    assert!(!waiting_only.contains(first.as_str()));

    let completed_only = task_list(
        answer,
        &TaskListOptions {
            all: false,
            topic: None,
            limit: None,
            status: Some(TaskListStatus::Completed),
        },
    );
    assert!(completed_only.contains("/board/test Completed board route"));
    assert!(!completed_only.contains(first.as_str()));

    let relevant_global = task_list(
        answer,
        &TaskListOptions {
            all: true,
            topic: None,
            limit: Some(1),
            status: None,
        },
    );
    assert!(
        relevant_global.contains(descendant.as_str()),
        "{relevant_global}"
    );
    assert!(
        !relevant_global.contains("Completed board route"),
        "{relevant_global}"
    );
    assert_eq!(
        relevant_global.matches(" [todo]").count(),
        1,
        "{relevant_global}"
    );
    assert!(
        relevant_global.contains("withheld 3 of 4 rows"),
        "{relevant_global}"
    );
}
