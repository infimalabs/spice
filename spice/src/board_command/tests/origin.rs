use super::*;
fn seed_steering_origin(bus: &Bus, key: &str) {
    seed_steering_directive(bus, key, uuid(16_201));
}

/// Words every seeded directive carries unless a test names its own.
pub(super) const DIRECTIVE_TEXT: &str = "Preserve this directive\nverbatim across hydration.";

pub(super) fn seed_steering_directive(bus: &Bus, key: &str, directive: CanonicalUuid) {
    send_directive(
        bus,
        Actor::TEST_PRINCIPAL,
        key,
        directive,
        uuid(16_202),
        DIRECTIVE_TEXT,
    );
}

/// Commit one live directive `author` addressed to `lane` with `text`.
pub(super) fn send_directive(
    bus: &Bus,
    author: Actor,
    key: &str,
    directive: CanonicalUuid,
    lane: CanonicalUuid,
    text: &str,
) {
    let published: Instant = "2030-04-12T18:44:09.271Z"
        .parse()
        .expect("fixture publication instant");
    let expires: Instant = "2030-04-12T19:44:09.271Z"
        .parse()
        .expect("fixture expiry instant");
    let sent = body::SteerSent {
        sender: author.clone(),
        room: None,
        directive,
        fanout: None,
        recipients: Vec::new(),
        key: key.to_owned(),
        recipient: Actor::Lane(lane),
        text: text.to_owned(),
        control: None,
        attachments: vec![wire::Media {
            name: "operator capture.png".to_owned(),
            content_type: "image/png".to_owned(),
            data_url: "data:image/png;base64,iVBORw0KGgo=".to_owned(),
        }],
        expires,
    };
    let minted_micros = key
        .parse::<steer::Key>()
        .expect("current steering key")
        .minted_micros();
    let origin: wire::Origin = format!("ack:{key}")
        .parse()
        .expect("fixture steering origin");
    let mut write = JournalWrite::new(author, origin.clone(), published, Producer::CommandDoor);
    let pending = write
        .fact(sent.clone(), FactCause::Root)
        .expect("steering fact builds");
    write.steering_head(pending, move |created| wire::SteeringHead {
        sender: sent.sender,
        room: sent.room.clone(),
        directive: sent.directive,
        fanout: sent.fanout,
        recipients: Vec::new(),
        key: sent.key,
        recipient: sent.recipient,
        text: sent.text,
        control: sent.control,
        attachments: sent.attachments,
        published,
        expires: sent.expires,
        stage: body::SteeringStage::Published,
        escalations: Vec::new(),
        disposition: wire::Disposition::Live,
        origin,
        created,
        minted_micros,
    });
    assert!(matches!(
        bus.commit(write).expect("steering origin commits"),
        CommitOutcome::Committed(_)
    ));
}

fn add_and_claim_causal_parent(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
) -> (TaskHandle, Actor, wire::Origin) {
    let parent_stamp = stamp("ORIGIN-parent1");
    let parent = TaskHandle::for_topic(
        &"/board/origin"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &parent_stamp,
    );
    seed_steering_origin(bus, "1kLGp2NJx");
    let ack: wire::Origin = "ack:1kLGp2NJx".parse().expect("steering origin");
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: parent_stamp }],
        &TaskAdd {
            topic: Some("/board/origin".parse().expect("fixture topic")),
            term: vec!["the causal root is durable".to_owned()],
            origin: Some(ack.clone()),
            flow: Some("todo,review".to_owned()),
            ..adding(
                "Materialize one steering directive",
                "This work keeps the fixture honest.",
            )
        },
    ));
    let lane = Actor::Lane(seat.lane);
    let before = board(bus, epoch, lane.clone());
    let parent_heads = at(&before, &parent);
    let capture: Name = "capture".parse().expect("capture rule");
    let CommandAnswer::Claim(claim) = assert_committed(issue(
        bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimWin,
        vec![
            statement_witness(parent_heads),
            workflow_witness(parent_heads),
            claim_witness(parent_heads),
            Witness::Eligibility {
                rule: capture.clone(),
            },
            residency(&before, seat),
        ],
        &ClaimWin {
            rule: capture,
            recovery: None,
        },
    )) else {
        panic!("claim win must hydrate the claimed task");
    };
    let expected_origin = format!(
        "origin_steering key 1kLGp2NJx\n\
         origin_steering_directive {} lane={} author={} published=2030-04-12T18:44:09.271Z\n\
         origin_steering_text:\n\
         Preserve this directive\nverbatim across hydration.\n\
         origin_steering_attachments operator capture.png\n\
         origin_steering_answer unavailable live",
        uuid(16_201),
        Actor::Lane(uuid(16_202)),
        Actor::TEST_PRINCIPAL,
    );
    assert_eq!(
        claim.origin_context.as_deref(),
        Some(expected_origin.as_str())
    );
    let claimed = board(bus, epoch, lane.clone());
    let shown = task_show_with_origin(
        &claimed,
        at(&claimed, &parent),
        &[],
        TaskShowHistory::default(),
        claim.origin_context.as_deref(),
    );
    assert!(
        shown.contains(&expected_origin),
        "task show prints the same resolved origin hydration"
    );
    (parent, lane, ack)
}

fn assert_invalid_creation_origins_refuse(bus: &Bus, epoch: CanonicalUuid) {
    let ungrounded_stamp = stamp("ORIGIN-ground00");
    let ungrounded = TaskHandle::for_topic(
        &"/board/origin"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &ungrounded_stamp,
    );
    let refused = issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: ungrounded_stamp,
        }],
        &TaskAdd {
            topic: Some("/board/origin".parse().expect("fixture topic")),
            flow: Some("todo,review".to_owned()),
            ..adding(
                "Refuse synthetic team",
                "This work keeps the fixture honest.",
            )
        },
    );
    let Outcome::Refusal(ref refusal) = refused else {
        panic!("ungrounded task creation must refuse: {refused:?}")
    };
    assert_eq!(refusal.reason(), "task::origin-required");
    assert!(refusal.diagnostic().is_some());
    assert!(
        refusal
            .explanation()
            .contains("--origin ack:<steering-key>")
    );
    assert!(
        refusal.explanation().contains("task:<parent-born-stamp>"),
        "causal guidance names durable task identity rather than a rendered handle"
    );
    assert!(
        board(bus, epoch, Actor::TEST_PRINCIPAL)
            .tasks
            .iter()
            .all(|task| task.statement.body.handle != ungrounded)
    );

    let self_referential_stamp = stamp("ORIGIN-self000");
    let self_referential = TaskHandle::for_topic(
        &"/board/origin"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &self_referential_stamp,
    );
    let self_origin = task_origin(&self_referential_stamp).expect("task forms its own origin");
    let refused = issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence {
            task: self_referential_stamp,
        }],
        &TaskAdd {
            topic: Some("/board/origin".parse().expect("fixture topic")),
            origin: Some(self_origin.clone()),
            flow: Some("todo,review".to_owned()),
            ..adding(
                "Refuse a caller-authored self edge",
                "This work keeps the fixture honest.",
            )
        },
    );
    let self_origin_text = self_origin.to_string();
    let Outcome::Refusal(ref refusal) = refused else {
        panic!("self-originating task creation must refuse: {refused:?}")
    };
    assert_eq!(refusal.reason(), "task::origin-self");
    assert_eq!(
        refusal
            .diagnostic()
            .and_then(wire::RefusalDiagnostic::observed),
        Some(self_origin_text.as_str())
    );
    assert!(
        board(bus, epoch, Actor::TEST_PRINCIPAL)
            .tasks
            .iter()
            .all(|task| task.statement.body.handle != self_referential)
    );
}

#[test]
fn shape_case_162_builds_a_causal_tree_and_refuses_ungrounded_creation() {
    let (root, bus, epoch, seats) = fixture("task-causal-origin", 1);
    let expected_creation = authorize_lane_creation(&bus, epoch, &seats[0]);
    let dependency = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "ORIGIN-order01",
        "Independent scheduling predecessor",
        Some("todo,review"),
    );
    let (parent, lane, ack) = add_and_claim_causal_parent(&bus, epoch, &seats[0]);
    let child_stamp = stamp("ORIGIN-child01");
    let child = TaskHandle::for_topic(
        &"/board/origin"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &child_stamp,
    );
    assert_committed(issue(
        &bus,
        epoch,
        lane,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: child_stamp }],
        &TaskAdd {
            topic: Some("/board/origin".parse().expect("fixture topic")),
            term: vec!["origin and dependency remain independent".to_owned()],
            flow: Some("todo,review".to_owned()),
            depends: vec![dependency.clone()],
            ..adding("Create causal child", "This work keeps the fixture honest.")
        },
    ));
    let causal = task_origin(&parent.born().expect("parent handle carries a birth stamp"))
        .expect("parent origin");
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&current, &parent).statement.body.provenance.origin(),
        ack
    );
    assert_eq!(creation_fact_origin(&bus, &parent), ack);
    assert_eq!(
        at(&current, &child).statement.body.provenance.origin(),
        causal.clone()
    );
    assert_eq!(
        at(&current, &child).statement.body.creation,
        expected_creation
    );
    assert_eq!(creation_fact_origin(&bus, &child), causal);
    assert_eq!(
        at(&current, &child).statement.body.dependencies,
        [dependency
            .born()
            .expect("dependency handle carries a birth stamp")],
        "causal provenance was inferred from a scheduling edge"
    );
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        vec![statement_witness(at(&current, &child))],
        &TaskEdit {
            title: Some("Create the same causal child after correction".to_owned()),
            ..only(&child)
        },
    ));
    assert_eq!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &child)
            .statement
            .body
            .creation,
        expected_creation,
        "later statement revisions cannot rewrite creation authority"
    );
    assert_invalid_creation_origins_refuse(&bus, epoch);
    clean(&root, bus);
}

/// Commit `declared` sessions on one tape, then retire `key` by an
/// acknowledgment at member 1 offset 40 of that tape.
fn answer_steering_origin(bus: &Bus, key: &str, source: CanonicalUuid, declared: &[(&str, u64)]) {
    let lane = uuid(16_202);
    let epoch = uuid(16_204);
    let origin: wire::Origin = format!("resident:{epoch}")
        .parse()
        .expect("resident fixture origin");
    let at = fixture_instant("acknowledgment fixture instant");
    for (identity, offset) in declared {
        let mut write = JournalWrite::new(
            Actor::Spice,
            origin.clone(),
            at,
            Producer::ResidentRecognition,
        );
        write
            .fact(
                body::LaneSessionDeclared {
                    lane,
                    occupant: uuid(16_205),
                    worktree: "/fixture/worktree".to_owned(),
                    vendor: "omp".parse().expect("fixture vendor"),
                    launch: body::LaneLaunchIdentity::new("sonnet", "high"),
                    epoch,
                    stream: "stdout".to_owned(),
                    source,
                    identity: (*identity).to_owned(),
                    member: 1,
                    offset: *offset,
                    length: 5,
                },
                FactCause::Root,
            )
            .expect("session declaration builds");
        assert!(matches!(
            bus.commit(write).expect("session declaration commits"),
            CommitOutcome::Committed(_)
        ));
    }
    acknowledge(bus, key, lane, source);
}

/// Retire live directive `key` by `lane`'s acknowledgment at member 1 offset
/// 40 of `source`.
fn acknowledge(bus: &Bus, key: &str, lane: CanonicalUuid, source: CanonicalUuid) {
    let epoch = uuid(16_204);
    let lookup = key.to_owned();
    let head = bus
        .read(move |read| read.steering_key(&lookup))
        .expect("steering index reads")
        .expect("seeded directive");
    let mut write = JournalWrite::new(
        Actor::Lane(lane),
        head.body().origin.clone(),
        fixture_instant("acknowledgment fixture instant"),
        Producer::AckScanner,
    );
    let pending = write
        .fact(
            body::SteerAcked {
                directive: head.body().directive,
                key: key.to_owned(),
                recipient: Actor::Lane(lane),
                evidence: body::SteeringEvidence::Transcript {
                    epoch,
                    locus: body::TranscriptLocus {
                        source: Some(source),
                        member: 1,
                        offset: 40,
                        length: 7,
                        event: 0,
                        line: 1,
                        column: 1,
                    },
                },
            },
            FactCause::Existing(head.position()),
        )
        .expect("acknowledgment builds");
    let mut acked = head.body().clone();
    acked.disposition = wire::Disposition::Acked;
    write.steering_head(pending, move |_| acked);
    let outcome = bus.commit(write).expect("acknowledgment commits");
    assert!(
        matches!(outcome, CommitOutcome::Committed(_)),
        "{outcome:?}"
    );
}

/// Add one task under `ack:1kLGp2NJx`, note it under `ack:1kLGp2NKx`, and read
/// it back with its notes.
pub(super) fn add_task_noted_under_a_second_ask(
    bus: &Bus,
    epoch: CanonicalUuid,
    born: &wire::TaskStamp,
) -> BoardAnswer {
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born.clone() }],
        &TaskAdd {
            topic: Some("/board/origin".parse().expect("fixture topic")),
            term: vec!["the ask is traceable".to_owned()],
            origin: Some("ack:1kLGp2NJx".parse().expect("steering origin")),
            flow: Some("todo,review".to_owned()),
            ..adding(
                "Follow an acknowledged ask",
                "This work keeps the fixture honest.",
            )
        },
    ));
    // An observation written under another directive carries that directive's
    // key.
    let at = fixture_instant("observation fixture instant");
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "ack:1kLGp2NKx".parse().expect("observation origin"),
        at,
        Producer::CommandDoor,
    );
    write
        .fact(
            wire::rooms::RoomPublished {
                room: born.clone(),
                publication: wire::rooms::RoomAuthorityStamp::for_micros(at.get().as_microsecond())
                    .expect("fixture publication"),
                content: wire::rooms::RoomPublicationContent::Authored {
                    text: "observed under a second ask".to_owned(),
                },
            },
            FactCause::Root,
        )
        .expect("observation builds");
    assert!(matches!(
        bus.commit(write).expect("observation commits"),
        CommitOutcome::Committed(_)
    ));
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
            tasks: vec![born.clone()],
            dependencies: false,
            claims: false,
            history: true,
        },
    )
    else {
        panic!("task read answers a board");
    };
    *answer
}

#[test]
fn task_show_follows_every_acknowledged_key_to_the_session_that_answered_it() {
    let (root, bus, epoch, _seats) = fixture("task-acknowledgment-provenance", 0);
    let source = uuid(16_206);
    seed_steering_directive(&bus, "1kLGp2NJx", uuid(16_201));
    seed_steering_directive(&bus, "1kLGp2NKx", uuid(16_203));
    // The answer names the session declared at or before its record, not a
    // later declaration on the same tape.
    answer_steering_origin(
        &bus,
        "1kLGp2NJx",
        source,
        &[("creator-session", 10), ("later-session", 50)],
    );
    let born = stamp("ORIGIN-ackshow");
    let task = TaskHandle::for_topic(
        &"/board/origin"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &born,
    );
    let answer = add_task_noted_under_a_second_ask(&bus, epoch, &born);
    let acknowledgments = &answer.acknowledgments[&born];
    assert_eq!(
        acknowledgments
            .iter()
            .map(|row| (row.kind, row.key.as_str()))
            .collect::<Vec<_>>(),
        [
            (wire::FactKind::TaskAdded, "1kLGp2NJx"),
            (wire::FactKind::TaskPhased, "1kLGp2NJx"),
            (wire::FactKind::RoomPublished, "1kLGp2NKx"),
        ]
    );
    let keys = task_acknowledgment_keys(
        &at(&answer, &task).statement.body.provenance,
        acknowledgments,
    );
    let context = bus
        .read(move |read| {
            ["1kLGp2NJx", "1kLGp2NKx"]
                .iter()
                .map(|key| {
                    read_steering_provenance(read, key)
                        .map(|provenance| render_task_steering_origin(key, provenance.as_ref()))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("steering provenance reads")
        .join("\n");
    assert_eq!(keys, ["1kLGp2NJx", "1kLGp2NKx"]);
    let shown = task_show_with_origin(
        &answer,
        at(&answer, &task),
        &[],
        TaskShowHistory {
            acknowledgments,
            ..TaskShowHistory::default()
        },
        Some(&context),
    );
    let (added, phased, observed) = (
        acknowledgments[0].position,
        acknowledgments[1].position,
        acknowledgments[2].position,
    );
    assert!(shown.contains(&format!(
        "acknowledgment ack:1kLGp2NJx task::added@{added} task::phased@{phased}\n\
         acknowledgment ack:1kLGp2NKx room::published@{observed}\n\
         origin_steering key 1kLGp2NJx\n"
    )));
    assert!(shown.contains(&format!(
        "origin_steering_answer acked lane={lane} at=2030-04-12T18:44:09.271Z\n\
         spice session context creator-session --lane {lane} --vendor omp --stream session \
         --source {source} --member 1 --offset 40\n\
         origin_steering key 1kLGp2NKx\n",
        lane = uuid(16_202),
    )));
    assert!(shown.contains("origin_steering_answer unavailable live\n"));
    clean(&root, bus);
}

#[test]
fn unresolved_steering_origin_is_explicit() {
    assert_eq!(
        render_task_steering_origin("1kLGp2NZx", None).as_ref(),
        "origin_steering key 1kLGp2NZx\norigin_steering unavailable unresolved-key"
    );
}

#[test]
fn relations_carry_the_request_a_steering_origin_names() {
    let (root, bus, epoch, _seats) = fixture("task-origin-request", 0);
    seed_steering_directive(&bus, "1kLGp2NJx", uuid(16_201));
    seed_steering_directive(&bus, "1kLGp2NKx", uuid(16_203));
    let born = stamp("ORIGIN-request");
    add_task_noted_under_a_second_ask(&bus, epoch, &born);
    let request = Some(wire::intent::OriginRequest {
        text: "Preserve this directive\nverbatim across hydration.".to_owned(),
        truncated: false,
    });
    let Outcome::Reply(Reply {
        body: CommandAnswer::TaskRelations(relations),
        ..
    }) = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskRelations,
        Vec::new(),
        &wire::intent::TaskRelations {
            roots: vec![born.clone()],
            relations: vec![wire::intent::TaskRelationKind::Origin],
            depth: 1,
            node_limit: 4,
        },
    )
    else {
        panic!("task relations answer a projection");
    };
    assert!(
        relations
            .nodes
            .contains(&wire::intent::TaskRelationNode::Origin {
                origin: "ack:1kLGp2NJx".parse().expect("steering origin"),
                request: request.clone(),
            }),
        "the lineage origin node names its request: {:?}",
        relations.nodes,
    );
    clean(&root, bus);
}

/// Win `task` for `seat` under the named capture rule.
fn capture(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
) -> Box<wire::intent::ClaimAnswer> {
    let lane = Actor::Lane(seat.lane);
    let before = board(bus, epoch, lane.clone());
    let heads = at(&before, task);
    let capture: Name = "capture".parse().expect("capture rule");
    let CommandAnswer::Claim(claim) = assert_committed(issue(
        bus,
        epoch,
        lane,
        CommandKind::ClaimWin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility {
                rule: capture.clone(),
            },
            residency(&before, seat),
        ],
        &ClaimWin {
            rule: capture,
            recovery: None,
        },
    )) else {
        panic!("claim win must hydrate the claimed task");
    };
    claim
}

fn add_separate_steering_task(bus: &Bus, epoch: CanonicalUuid) -> (wire::TaskStamp, TaskHandle) {
    let born = stamp("STEER-captured");
    let task = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    );
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born.clone() }],
        &TaskAdd {
            topic: Some("/board/test".parse().expect("fixture topic")),
            term: vec!["the ask is its own work".to_owned()],
            origin: Some("ack:1kLGp2N3x".parse().expect("steering origin")),
            ..adding("Separate ask", "This work keeps the fixture honest.")
        },
    ));
    (born, task)
}

fn complete_claimed_task(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) {
    let lane = Actor::Lane(seat.lane);
    let held = board(bus, epoch, lane.clone());
    assert_committed(issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        progress_witnesses(&held, at(&held, task), seat.lane, seat.occupant, false)
            .expect("held task has completion witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(task, "claim steering fixture complete")
        },
    ));
    assert_eq!(
        bus.task_snapshot()
            .expect("advanced snapshot reads")
            .task_head_update_visits(),
        1,
        "phase completion updates one live head"
    );
}

fn read_task(bus: &Bus, epoch: CanonicalUuid, task: &wire::TaskStamp) -> BoardAnswer {
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
            history: true,
        },
    )
    else {
        panic!("task read answers a board");
    };
    *answer
}

fn rendered_task(answer: &BoardAnswer, task: &TaskHandle, origin_context: Option<&str>) -> String {
    let born = task.born().expect("fixture identity");
    task_show_with_origin(
        answer,
        at(answer, task),
        &[],
        TaskShowHistory {
            acknowledgments: &answer.acknowledgments[&born],
            ..TaskShowHistory::default()
        },
        origin_context,
    )
}

fn assert_rendered_claim_steering(rendered: &str, holder: CanonicalUuid) {
    let keys = rendered
        .lines()
        .filter_map(|line| line.strip_prefix("origin_steering key "))
        .collect::<Vec<_>>();
    assert_eq!(keys, ["1kJK6N3Rx", "1kLGp2N2x", "1kLGp2N7x"]);
    assert_eq!(
        rendered
            .matches("origin_steering_text:\nPreserve this directive\n")
            .count(),
        2
    );
    assert_eq!(
        rendered
            .matches(&format!("origin_steering_answer acked lane={holder}"))
            .count(),
        2
    );
    assert!(!rendered.contains("1kLGp2N3x"));
}

#[test]
fn task_show_and_review_context_include_only_uncaptured_claim_steering() {
    let (root, bus, epoch, seats) = fixture("task-steering-during-claims", 2);
    let (holder, other) = (seats[0].lane, seats[1].lane);
    let held = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "STEER-held",
        "Hold steering",
        Some("todo,review"),
    );
    let held_born = held.born().expect("fixture identity");
    send_directive(
        &bus,
        Actor::TEST_PRINCIPAL,
        "1kLGp2N1x",
        uuid(16_301),
        holder,
        DIRECTIVE_TEXT,
    );
    acknowledge(&bus, "1kLGp2N1x", holder, uuid(16_401));
    capture(&bus, epoch, &seats[0], &held);
    let steer = |key: &str, author: Actor, to: CanonicalUuid, directive: u64| {
        send_directive(&bus, author, key, uuid(directive), to, DIRECTIVE_TEXT);
        acknowledge(&bus, key, to, uuid(directive + 100));
    };
    steer("1kLGp2N2x", Actor::TEST_PRINCIPAL, holder, 16_302);
    steer("1kLGp2N3x", Actor::TEST_PRINCIPAL, holder, 16_303);
    steer("1kLGp2N4x", Actor::Lane(other), holder, 16_304);
    steer("1kLGp2N5x", Actor::Spice, holder, 16_305);
    steer("1kLGp2N6x", Actor::TEST_PRINCIPAL, other, 16_306);
    steer("1kLGp2N7x", Actor::TEST_PRINCIPAL, holder, 16_307);

    // One ask became separate work, so it no longer joins the held task.
    let (captured_born, captured) = add_separate_steering_task(&bus, epoch);
    complete_claimed_task(&bus, epoch, &seats[0], &held);
    steer("1kLGp2N8x", Actor::TEST_PRINCIPAL, holder, 16_308);

    // The next phase's reviewer receives the steering that shaped the work.
    let review = capture(&bus, epoch, &seats[1], &held);
    let context = review
        .origin_context
        .as_deref()
        .expect("steered review carries context");
    assert_rendered_claim_steering(context, holder);

    let answer = read_task(&bus, epoch, &held_born);
    let acknowledgments = &answer.acknowledgments[&held_born];
    assert_eq!(
        acknowledgments
            .iter()
            .map(|row| (row.kind, row.key.as_str()))
            .collect::<Vec<_>>(),
        [
            (wire::FactKind::TaskAdded, "1kJK6N3Rx"),
            (wire::FactKind::TaskPhased, "1kJK6N3Rx"),
            (wire::FactKind::SteerAcked, "1kLGp2N2x"),
            (wire::FactKind::SteerAcked, "1kLGp2N7x")
        ],
        "only uncaptured operator steering acknowledged by the holder joins the task"
    );
    let task_shown = rendered_task(&answer, &held, Some(context));
    let (first, third) = (acknowledgments[2].position, acknowledgments[3].position);
    assert!(task_shown.contains(&format!(
        "acknowledgment ack:1kLGp2N2x steer::acked@{first}\n\
         acknowledgment ack:1kLGp2N7x steer::acked@{third}\n"
    )));
    assert_rendered_claim_steering(&task_shown, holder);

    let captured_answer = read_task(&bus, epoch, &captured_born);
    let captured_acknowledgments = &captured_answer.acknowledgments[&captured_born];
    assert_eq!(
        captured_acknowledgments
            .iter()
            .map(|row| (row.kind, row.key.as_str()))
            .collect::<Vec<_>>(),
        [
            (wire::FactKind::TaskAdded, "1kLGp2N3x"),
            (wire::FactKind::TaskPhased, "1kLGp2N3x")
        ],
        "the captured task retains its own provenance but no claim-linked steering"
    );
    let captured_shown = rendered_task(&captured_answer, &captured, None);
    assert!(captured_shown.contains("acknowledgment ack:1kLGp2N3x task::added@"));
    assert!(!captured_shown.contains("steer::acked"));
    clean(&root, bus);
}
