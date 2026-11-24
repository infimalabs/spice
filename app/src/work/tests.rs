use super::*;
use wire::intent::{BoardStatement, Positioned};
use wire::{Actor, CanonicalUuid, ClaimCell, ClaimHead, Environment, EnvironmentRequirement};

fn seq(value: u64) -> wire::Seq {
    wire::Seq::new(value).expect("fixture sequence is positive")
}

pub(crate) fn stamp(value: i64) -> TaskStamp {
    TaskStamp::for_micros(value).expect("fixture stamp is representable")
}

pub(crate) fn task(value: i64, topic: &str) -> BoardTask {
    let born = stamp(value);
    BoardTask {
        ownership: None,
        origin_thread: "operator".to_owned(),
        creator_team: None,
        presentation_title: format!("task {value}"),
        statement: Positioned {
            position: seq(1),
            body: BoardStatement {
                preparation: None,
                intent: None,
                handle: wire::TaskHandle::for_topic(
                    &topic.parse::<wire::Topic>().expect("fixture topic"),
                    &born,
                ),
                born: born.clone(),
                topic_origin: seq(value.unsigned_abs() + 1),
                pitch: "This work keeps the fixture honest.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                resources: wire::ResourceRequirements::default(),
                topic: topic.parse().expect("fixture topic"),
                priority: None,
                taste: None,
                dependencies: Vec::new(),
                provenance: format!("task:{born}").parse().expect("fixture origin"),
                creation: wire::body::TaskCreationContext::unavailable(
                    wire::body::TaskCreationUnavailable::PrincipalActor,
                ),
                revision: seq(1),
            },
        },
        workflow: Positioned {
            position: seq(2),
            body: wire::WorkflowHead {
                flow: vec![wire::Phase::Draft, "todo".parse().expect("fixture phase")],
                phase: "todo".parse().expect("fixture phase"),
                status: wire::WorkflowStatus::pending(),
                deferral: None,
                submitted: false,
                generation: 0,
            },
        },
        completion: Positioned {
            position: seq(2),
            body: wire::CompletionHead::opened(seq(2)),
        },
        claims: Vec::new(),
        original_claimed_at: None,
        invitations: Vec::new(),
        explicit_release: false,
        design_cycle: wire::rooms::DesignCycleFacts::default(),
        collapse_reason: None,
        controlled: Vec::new(),
        claim_context: None,
        git_context: None,
        parked_commits: None,
        design_coverage: None,
    }
}

/// An open Design: Design intent in its pending `design` phase.
pub(crate) fn design(value: i64, topic: &str) -> BoardTask {
    let mut row = task(value, topic);
    row.statement.body.intent = Some(wire::TaskIntent::Design);
    row.workflow.body.flow = vec![wire::Phase::Draft, wire::Phase::Design];
    row.workflow.body.phase = wire::Phase::Design;
    row
}

// Principal claim fixture.
pub(crate) fn claim(task: &BoardTask, disposition: ClaimDisposition) -> Positioned<ClaimHead> {
    Positioned {
        position: seq(3),
        body: ClaimHead {
            actor: Actor::TEST_PRINCIPAL,
            fact: seq(3),
            claimed: seq(3),
            cell: ClaimCell {
                topic: task.statement.body.topic.path.clone(),
                phase: task.workflow.body.phase,
            },
            lane: "11111111-1111-4111-8111-111111111111"
                .parse::<CanonicalUuid>()
                .expect("fixture lane"),
            occupant: "22222222-2222-4222-8222-222222222222"
                .parse::<CanonicalUuid>()
                .expect("fixture occupant"),
            epoch: "33333333-3333-4333-8333-333333333333"
                .parse::<CanonicalUuid>()
                .expect("fixture epoch"),
            recovery: None,
            disposition,
            acknowledged: 0,
        },
    }
}

/// One retained claim without a lease, as a board row carries it.
pub(crate) fn retained(claim: Positioned<ClaimHead>) -> wire::intent::BoardClaim {
    wire::intent::BoardClaim { claim, lease: None }
}

pub(crate) fn board(tasks: Vec<BoardTask>) -> BoardView {
    BoardView {
        schema: 1,
        cursor: seq(100),
        tasks,
        allocation_evidence: Vec::new(),
        finished: Vec::new(),
        offer: Vec::new(),
        blocked: std::collections::BTreeSet::new(),
        unallocated: std::collections::BTreeMap::new(),
        workflow_waiting: std::collections::BTreeSet::new(),
        design_cycles: std::collections::BTreeMap::new(),
        quote_tasks: std::rc::Rc::default(),
        environment: Environment::journal_schema(1).expect("fixture schema"),
    }
}

#[test]
fn lifecycle_precedence_follows_task_status_standings() {
    let deferral = || {
        Some(
            "2026-09-24T06:00:00.000Z"
                .parse()
                .expect("fixture deferral"),
        )
    };
    let upgrade = || {
        wire::EnvironmentRequirements::one(
            EnvironmentRequirement::journal_schema(2).expect("fixture requirement"),
        )
    };
    let review = || "review".parse().expect("review phase");

    let mut terminal = task(1, "/alpha/lifecycle");
    terminal.workflow.body.status = wire::WorkflowStatus::completed(
        "2026-09-23T06:00:00.000Z"
            .parse()
            .expect("fixture completion"),
    );
    terminal.claims = vec![retained(claim(&terminal, ClaimDisposition::Active))];

    let mut in_flight = task(2, "/alpha/lifecycle");
    in_flight.workflow.body.deferral = deferral();
    in_flight.statement.body.requirements = upgrade();
    in_flight.claims = vec![retained(claim(&in_flight, ClaimDisposition::Active))];

    let mut stale = task(3, "/alpha/lifecycle");
    stale.workflow.body.deferral = deferral();
    stale.claims = vec![retained(claim(&stale, ClaimDisposition::Lapsed))];

    let mut deferred = task(4, "/alpha/lifecycle");
    deferred.workflow.body.deferral = deferral();
    deferred.statement.body.requirements = upgrade();

    let mut gated_review = task(5, "/alpha/lifecycle");
    gated_review.workflow.body.phase = review();
    gated_review.statement.body.requirements = upgrade();

    let mut held_review = task(6, "/alpha/lifecycle");
    held_review.workflow.body.phase = review();
    held_review.statement.body.requirements = upgrade();

    let mut held = task(7, "/alpha/lifecycle");
    held.statement.body.requirements = upgrade();

    let ready = task(8, "/alpha/lifecycle");
    let workflow_waiting = task(9, "/alpha/lifecycle");
    let selection = task(10, "/alpha/lifecycle");
    let mut board = board(vec![
        terminal,
        in_flight,
        stale,
        deferred,
        gated_review,
        held_review,
        held,
        ready,
        workflow_waiting,
        selection,
    ]);
    // Distinct lifecycle evidence always wins if a malformed snapshot also
    // repeats those rows in the generic blocker set.
    board.blocked.extend([3, 4, 5, 9, 10].map(stamp));
    board.workflow_waiting.insert(stamp(9));
    board
        .design_cycles
        .insert(stamp(10), wire::rooms::DesignCycle::AwaitingDecision);
    let projection = WorkProjection::from_board_and_lanes(&board, None);

    assert_eq!(
        projection
            .tasks
            .iter()
            .map(|task| task.availability)
            .collect::<Vec<_>>(),
        [
            WorkAvailability::Terminal,
            WorkAvailability::InFlight,
            WorkAvailability::Stale,
            WorkAvailability::Deferred,
            WorkAvailability::Blocked,
            WorkAvailability::Review,
            WorkAvailability::Held,
            WorkAvailability::Ready,
            WorkAvailability::Waiting,
            WorkAvailability::Selection,
        ]
    );
    let counts = projection.counts();
    assert_eq!(
        [
            counts.ready(),
            counts.review(),
            counts.stale(),
            counts.blocked(),
            counts.waiting(),
        ],
        [1, 1, 1, 1, 4],
        "each open category counts exactly its own rows"
    );
    assert_eq!(projection.tasks[0].working_claimant(), None);
    assert_eq!(
        projection.tasks[1].working_claimant(),
        Some(&Actor::TEST_PRINCIPAL),
    );
    assert!(projection.tasks[1].environment_blocked);
    assert!(projection.tasks[6].environment_blocked);
}

#[test]
fn configured_resource_scarcity_is_scheduler_waiting_not_operator_attention() {
    let name: wire::ResourcePoolName = "windows-guest".parse().expect("fixture resource name");
    let mut waiting = task(11, "/alpha/resources");
    waiting.statement.body.requirements = wire::EnvironmentRequirements::one(
        wire::EnvironmentRequirement::resource_pool(name.clone(), 1).expect("fixture shard count"),
    );
    waiting.statement.body.resources =
        wire::ResourceRequirements::new([(name.clone(), 1)]).expect("fixture declaration");
    let mut board = board(vec![waiting]);
    board.environment = board.environment.with_resource_pool(name, 0);
    let projection = WorkProjection::from_board_and_lanes(&board, None);

    assert_eq!(
        projection.tasks()[0].availability(),
        WorkAvailability::Waiting,
    );
    assert!(!projection.tasks()[0].environment_blocked());
    assert!(operator::OperatorWaits::new(&projection).waits().is_empty());
}

#[test]
fn missing_declared_resource_is_operator_actionable_configuration_work() {
    let name: wire::ResourcePoolName = "windows-guest".parse().expect("fixture resource name");
    let mut blocked = task(11, "/alpha/resources");
    blocked.statement.body.requirements = wire::EnvironmentRequirements::one(
        wire::EnvironmentRequirement::resource_pool(name.clone(), 1).expect("fixture shard count"),
    );
    blocked.statement.body.resources =
        wire::ResourceRequirements::new([(name, 1)]).expect("fixture declaration");
    let board = board(vec![blocked]);
    let projection = WorkProjection::from_board_and_lanes(&board, None);

    assert_eq!(projection.tasks()[0].availability(), WorkAvailability::Held,);
    assert!(projection.tasks()[0].environment_blocked());
    assert_eq!(operator::OperatorWaits::new(&projection).waits().len(), 1);
}

#[test]
fn gathering_designs_are_passive_while_selected_work_waits() {
    use wire::rooms::DesignCycle;

    let mut rows = (1..=7)
        .map(|value| design(value, "/app/design"))
        .collect::<Vec<_>>();
    rows[4].claims = vec![retained(claim(&rows[4], ClaimDisposition::Active))];
    rows[5].workflow.body.deferral = Some(
        "2026-09-24T06:00:00.000Z"
            .parse()
            .expect("fixture deferral"),
    );
    rows[6].claims = vec![retained(claim(&rows[6], ClaimDisposition::Lapsed))];
    rows.push(task(8, "/app/work"));
    rows.push(design(9, "/app/design"));
    let mut held = design(10, "/app/design");
    held.statement.body.requirements = wire::EnvironmentRequirements::one(
        EnvironmentRequirement::journal_schema(2).expect("fixture requirement"),
    );
    rows.push(held);
    let mut board = board(rows);
    board
        .workflow_waiting
        .extend([1, 2, 4, 5, 6, 7, 8].map(stamp));
    board.blocked.insert(stamp(9));
    board.design_cycles.extend([
        (stamp(1), DesignCycle::GatheringInput),
        (stamp(2), DesignCycle::WaitingOnSelectedWork),
        (stamp(3), DesignCycle::ReadyForPass),
        (stamp(4), DesignCycle::AwaitingDecision),
        (stamp(5), DesignCycle::GatheringInput),
        (stamp(6), DesignCycle::GatheringInput),
        (stamp(7), DesignCycle::GatheringInput),
        (stamp(9), DesignCycle::GatheringInput),
        (stamp(10), DesignCycle::GatheringInput),
    ]);
    let projection = WorkProjection::from_board_and_lanes(&board, None);

    assert_eq!(
        projection
            .tasks()
            .iter()
            .map(WorkTask::availability)
            .collect::<Vec<_>>(),
        [
            WorkAvailability::Passive,
            WorkAvailability::Waiting,
            WorkAvailability::Ready,
            WorkAvailability::Selection,
            WorkAvailability::InFlight,
            WorkAvailability::Deferred,
            WorkAvailability::Stale,
            WorkAvailability::Waiting,
            WorkAvailability::Blocked,
            WorkAvailability::Held,
        ]
    );
    assert_eq!(projection.counts().workflow_waiting(), 2);
    assert_eq!(projection.counts().open(), 10);
    assert!(projection.tasks()[2].pass_ready());
}

#[test]
fn a_design_depends_on_the_outputs_it_released_while_gated_outputs_stay_its_dependents() {
    let design = design(1, "/alpha/design");
    let output = |value: i64, released: bool| {
        let mut row = task(value, "/alpha/work");
        row.statement.body.preparation = Some(wire::TaskPreparation {
            parent: stamp(1),
            controller: stamp(1),
            construction: stamp(value),
        });
        if !released {
            row.workflow.body.phase = wire::Phase::Draft;
        }
        row
    };
    let mut settled = output(4, true);
    settled.workflow.body.status = wire::WorkflowStatus::completed(
        "2026-09-23T06:00:00.000Z"
            .parse()
            .expect("fixture completion"),
    );
    let board = board(vec![design, output(2, false), output(3, true), settled]);
    let projection = WorkProjection::from_board_and_lanes(&board, None);
    let [design, gated, released, settled] = projection.tasks.as_slice() else {
        panic!("the projection keeps every row")
    };

    assert_eq!(
        design
            .dependencies()
            .iter()
            .map(|dependency| (dependency.born().clone(), dependency.satisfied()))
            .collect::<Vec<_>>(),
        vec![(stamp(3), false), (stamp(4), true)],
        "the Design waits on each released root until it settles"
    );
    assert!(
        released
            .dependents()
            .iter()
            .any(|dependent| dependent.task() == 0),
        "the released root counts the Design among its dependents"
    );
    assert!(
        gated.belongs_to_design(),
        "gated work is placed with the Design"
    );
    assert!(
        !released.belongs_to_design() && !settled.belongs_to_design(),
        "released work keeps execution placement rather than joining the Design namespace"
    );
}

#[test]
fn design_availability_tracks_claims_deferral_and_pass_cycle() {
    let design = |value| design(value, "/alpha/design");
    let mut held = design(1);
    held.claims = vec![retained(claim(&held, ClaimDisposition::Active))];
    let mut lapsed = design(2);
    lapsed.claims = vec![retained(claim(&lapsed, ClaimDisposition::Lapsed))];
    let mut deferred = design(3);
    deferred.workflow.body.deferral = Some(wire::Instant::indefinite());
    let mut board = board(vec![held, lapsed, deferred, design(6), design(7)]);
    board.blocked.insert(stamp(6));
    board
        .design_cycles
        .insert(stamp(7), wire::rooms::DesignCycle::AwaitingDecision);
    let projection = WorkProjection::from_board_and_lanes(&board, None);

    assert_eq!(
        projection
            .tasks
            .iter()
            .map(|task| task.availability)
            .collect::<Vec<_>>(),
        [
            WorkAvailability::InFlight,
            WorkAvailability::Stale,
            WorkAvailability::Deferred,
            WorkAvailability::Blocked,
            WorkAvailability::Selection,
        ],
        "a deferred Design waits for explicit wake; other Designs follow claims and pass state"
    );
    let counts = projection.counts();
    assert_eq!([counts.deferred(), counts.waiting()], [1, 2]);
    assert_eq!(
        projection
            .scope_strip(&WorkScope::planning())
            .iter()
            .map(|task| task.availability)
            .collect::<Vec<_>>(),
        [
            WorkAvailability::InFlight,
            WorkAvailability::Stale,
            WorkAvailability::Blocked,
            WorkAvailability::Selection,
            WorkAvailability::Deferred,
        ],
        "a deferred Design stays on the Design rail after every Design an agent can move"
    );
}

#[test]
fn exact_topics_never_merge_through_a_shared_prefix() {
    let mut active = task(1, "/app/one");
    active.claims = vec![retained(claim(&active, ClaimDisposition::Active))];
    let ready = task(2, "/app/two");
    let parent = task(3, "/app");
    let mut terminal = task(4, "/app/one");
    terminal.workflow.body.status = wire::WorkflowStatus::completed(
        "2026-09-23T06:00:00.000Z"
            .parse()
            .expect("fixture completion"),
    );
    let board = board(vec![active, ready, parent, terminal]);
    let projection = WorkProjection::from_board_and_lanes(&board, None);

    assert_eq!(
        projection
            .topics
            .iter()
            .map(|topic| topic.topic)
            .collect::<Vec<_>>(),
        [Some("/app"), Some("/app/one"), Some("/app/two")]
    );
    assert_eq!(
        projection.topic("/app/one").map(WorkTopic::counts),
        Some(WorkCounts {
            in_flight: 1,
            terminal: 1,
            ..WorkCounts::default()
        })
    );
    assert!(
        !WorkCounts {
            terminal: 1,
            ..WorkCounts::default()
        }
        .has_open()
    );
    for topic in ["/app", "/app/one", "/app/two"] {
        let scope = WorkScope::Topic(topic.to_owned());
        assert_eq!(scope.topic(), Some(topic));
        assert_eq!(projection.contains_scope(&scope), Some(true), "{topic}");
        assert_eq!(projection.scope_counts(&scope).open(), 1, "{topic}");
    }

    let absent = WorkScope::Topic("app.three".to_owned());
    assert_eq!(projection.contains_scope(&absent), Some(false));
    assert_eq!(projection.scope_counts(&absent), projection.counts());
}

/// An open task no first phase has placed, carrying `tail` for its placement.
fn unplaced(value: i64, tail: Option<&str>) -> BoardTask {
    let mut row = task(value, "/placeholder");
    let born = row.statement.body.born.clone();
    let tail = tail.map(|tail| match tail.parse() {
        Ok(wire::TopicInput::Tail(tail)) => tail,
        _ => panic!("fixture tail"),
    });
    row.statement.body.topic = wire::TaskTopic::unplaced(tail);
    row.statement.body.handle = wire::TaskHandle::for_topic(None, &born);
    row
}

#[test]
fn unplaced_work_groups_before_every_topic_and_under_no_topic_scope() {
    let placed = task(1, "/app");
    let bare = unplaced(2, None);
    let tailed = unplaced(3, Some("core"));
    let board = board(vec![placed, bare, tailed]);
    let projection = WorkProjection::from_board_and_lanes(&board, None);

    assert_eq!(
        projection
            .topics
            .iter()
            .map(|topic| (topic.topic, topic.tasks.len()))
            .collect::<Vec<_>>(),
        [(None, 2), (Some("/app"), 1)]
    );
    assert_eq!(projection.contains_scope(&WorkScope::Unplaced), Some(true));
    assert_eq!(projection.scope_counts(&WorkScope::Unplaced).open(), 2);
    assert_eq!(
        projection
            .scope_counts(&WorkScope::Topic("/app".to_owned()))
            .open(),
        1
    );
    for task in &projection.tasks {
        let is_unplaced = task.row.statement.body.topic.path.is_none();
        assert_eq!(projection.in_scope(task, &WorkScope::Unplaced), is_unplaced);
        assert_eq!(
            projection.in_scope(task, &WorkScope::Topic("/app".to_owned())),
            !is_unplaced
        );
        assert_eq!(projection.task_topic(task).is_none(), is_unplaced);
    }
    assert_eq!(
        resolve_work_route(&WorkRoute::Unplaced, None),
        WorkRouteResolution::Resolved(WorkScope::Unplaced)
    );
}

#[test]
fn relationships_claim_dispositions_and_ranking_are_deterministic() {
    let mut complete = task(1, "/graph/core");
    complete.workflow.body.status = wire::WorkflowStatus::completed(
        "2026-09-23T06:00:00.000Z"
            .parse()
            .expect("fixture completion"),
    );
    let mut prerequisite = task(2, "/graph/core");
    prerequisite.claims = vec![retained(claim(&prerequisite, ClaimDisposition::Lapsed))];
    // The open board omits only terminal rows, so an absent prerequisite settled.
    let settled = stamp(99);
    let mut blocked = task(3, "/graph/core");
    blocked.statement.body.dependencies = vec![
        complete.statement.body.born.clone(),
        prerequisite.statement.body.born.clone(),
        settled.clone(),
    ];
    let mut freed = task(4, "/graph/core");
    freed.claims = vec![retained(claim(&freed, ClaimDisposition::Freed))];

    let mut board = board(vec![complete, prerequisite, blocked, freed]);
    board
        .blocked
        .insert(board.tasks[2].statement.body.born.clone());
    let projection = WorkProjection::from_board_and_lanes(&board, None);
    assert_eq!(projection.tasks[1].availability, WorkAvailability::Stale);
    assert_eq!(projection.tasks[2].availability, WorkAvailability::Blocked);
    assert_eq!(projection.tasks[3].availability, WorkAvailability::Ready);
    assert_eq!(
        projection.tasks[2]
            .dependencies
            .iter()
            .map(|dependency| (dependency.born, dependency.task, dependency.satisfied))
            .collect::<Vec<_>>(),
        [
            (&projection.tasks[0].row.statement.body.born, Some(0), true),
            (&projection.tasks[1].row.statement.body.born, Some(1), false),
            (&settled, None, true),
        ]
    );
    assert_eq!(
        projection.tasks[1].dependents,
        [WorkDependent {
            task: 2,
            released: false
        }]
    );

    let lane = &projection.topics[0];
    assert_eq!(lane.topic, Some("/graph/core"));
    assert_eq!(
        lane.tasks
            .iter()
            .map(|&index| projection.tasks[index].availability)
            .collect::<Vec<_>>(),
        [
            WorkAvailability::Ready,
            WorkAvailability::Stale,
            WorkAvailability::Blocked,
            WorkAvailability::Terminal,
        ]
    );
    assert_eq!(lane.tasks, [3, 1, 2, 0]);
    assert_eq!(projection.tasks[2].topic, 0);
    assert_eq!(lane.counts.blocked(), 1);
}

#[test]
fn owners_read_as_agent_names_and_unknown_lanes_stay_distinguishable() {
    let lane = "11111111-1111-4111-8111-111111111111"
        .parse::<CanonicalUuid>()
        .expect("fixture lane");
    let other = "44444444-4444-4444-8444-444444444444"
        .parse::<CanonicalUuid>()
        .expect("fixture lane");
    let mut member = LaneMember {
        lane,
        name: None,
        worktree: "/work/spice-h".to_owned(),
        speech: wire::SpeechPlayback::Disabled {},
        life: "steer".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        seat_model: None,
        effort: None,
        seat_effort: None,
        fast_mode: false,
        fast_refusal: None,
        seat_fast_mode: false,
        occupant: None,
        session: None,
        disposition: "seated".to_owned(),
        liveness: wire::intent::LaneLiveness::Working,
        bound: true,
        wakeable: true,
        jobs: Vec::new(),
        parked: None,
    };
    let actor = Actor::Lane(lane);
    assert_eq!(owner_label(&actor, Some(&member), None), "spice-h");
    member.name = Some("builder".to_owned());
    assert_eq!(owner_label(&actor, Some(&member), None), "builder");
    assert_eq!(owner_label(&actor, None, None), "lane 11111111");
    assert_eq!(
        owner_label(&Actor::Lane(other), Some(&member), None),
        "lane 44444444",
        "a member of another lane never lends its name"
    );
    let principal = Actor::TEST_PRINCIPAL;
    assert_eq!(
        owner_label(&principal, None, None),
        principal.to_string(),
        "an unresolved principal keeps its durable actor spelling"
    );

    let mut held = task(1, "/alpha/core");
    held.claims = vec![retained(claim(&held, ClaimDisposition::Active))];
    let mut lapsed = task(2, "/alpha/core");
    lapsed.claims = vec![retained(claim(&lapsed, ClaimDisposition::Lapsed))];
    let board = board(vec![held, lapsed, task(3, "/alpha/core")]);
    let projection = WorkProjection::from_board_and_lanes(&board, None);
    assert_eq!(projection.tasks[0].owner(), Some(&Actor::TEST_PRINCIPAL));
    assert_eq!(
        projection.tasks[1].owner(),
        None,
        "a lapsed claim owns nothing"
    );
    assert_eq!(projection.tasks[2].owner(), None);
    assert_eq!(
        projection.tasks[0].working_claimant(),
        Some(&Actor::TEST_PRINCIPAL),
    );
    assert_eq!(projection.tasks[1].working_claimant(), None);
    assert_eq!(projection.tasks[2].working_claimant(), None);
}

#[test]
fn shared_holders_list_earliest_claim_first_and_ended_claims_drop_out() {
    let lane = |suffix: &str| -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{suffix}")
            .parse()
            .expect("fixture lane")
    };
    let (first, ended, later) = (lane("01"), lane("02"), lane("03"));
    let mut shared = task(1, "/alpha/core");
    let won = |claimed: u64, holder: CanonicalUuid, disposition: ClaimDisposition| {
        let mut head = claim(&shared, disposition);
        head.body.claimed = seq(claimed);
        head.body.fact = seq(claimed);
        head.body.lane = holder;
        head.body.actor = Actor::Lane(holder);
        retained(head)
    };
    let claims = vec![
        won(3, first, ClaimDisposition::Active),
        won(4, ended, ClaimDisposition::Freed),
        won(5, later, ClaimDisposition::Active),
    ];
    shared.claims = claims.clone();
    let mut finished = task(2, "/alpha/core");
    finished.workflow.body.status = wire::WorkflowStatus::completed(
        "2026-09-23T06:00:00.000Z"
            .parse()
            .expect("fixture completion"),
    );
    finished.claims = claims;
    let board = board(vec![shared, finished]);
    let projection = WorkProjection::from_board_and_lanes(&board, None);

    assert_eq!(
        projection.tasks[0].working_lanes().collect::<Vec<_>>(),
        [first, later],
        "every active holder, in winning order; an ended claim is no holder"
    );
    assert_eq!(
        projection.tasks[0].working_lane(),
        Some(first),
        "the earliest holder leads the tab and supplies its accent"
    );
    assert_eq!(
        projection.tasks[1].working_lanes().count(),
        0,
        "a finished row's retained claims hold nothing"
    );
}
#[test]
fn principal_owners_read_as_their_short_name_alone() {
    let named_principal = "018f82d4-6f59-7d06-92e3-bce913116391"
        .parse::<CanonicalUuid>()
        .expect("fixture named principal");
    let unnamed_principal = "018f82d4-6f59-7d06-92e3-bce913116392"
        .parse::<CanonicalUuid>()
        .expect("fixture unnamed principal");
    let unresolved_principal = "018f82d4-6f59-7d06-92e3-bce913116393"
        .parse::<CanonicalUuid>()
        .expect("fixture unresolved principal");
    let named = wire::principal::PrincipalHead {
        principal: named_principal,
        born: seq(18_730),
        profile: wire::principal::PrincipalProfile {
            name: Some("Ada Example".to_owned()),
            ..wire::principal::PrincipalProfile::default()
        },
        roles: BTreeSet::new(),
        capability: None,
    };
    let unnamed = wire::principal::PrincipalHead {
        principal: unnamed_principal,
        born: seq(18_731),
        profile: wire::principal::PrincipalProfile::default(),
        roles: BTreeSet::new(),
        capability: None,
    };
    let unnamed_code = wire::principal::PrincipalCode::of(&unnamed);
    let principals = wire::principal::PrincipalDirectory::display(&[named, unnamed]);

    assert_eq!(
        owner_label(&Actor::Principal(named_principal), None, Some(&principals)),
        "AE",
    );
    assert_eq!(
        owner_label(
            &Actor::Principal(unnamed_principal),
            None,
            Some(&principals),
        ),
        unnamed_code.to_string(),
    );
    let unresolved = Actor::Principal(unresolved_principal);
    assert_eq!(
        owner_label(&unresolved, None, Some(&principals)),
        unresolved.to_string(),
    );
    assert_eq!(owner_label(&unresolved, None, None), unresolved.to_string(),);
}

#[test]
fn a_tab_marks_only_its_partitions_most_actionable_state() {
    let counts = |blocked, held, selection, pass_ready, deferred| WorkCounts {
        blocked,
        held,
        selection,
        pass_ready,
        deferred,
        ..WorkCounts::default()
    };
    assert_eq!(
        counts(1, 1, 3, 4, 5).mark(),
        Some((WorkMark::Blocked, 2)),
        "work only the operator can unblock outranks a decision, a ready pass, and deferral"
    );
    assert_eq!(
        counts(0, 0, 3, 4, 5).mark(),
        Some((WorkMark::Decision, 3)),
        "a pass awaiting decision outranks a Design ready for a pass"
    );
    assert_eq!(
        counts(0, 0, 0, 4, 5).mark(),
        Some((WorkMark::Pass, 4)),
        "a Design ready for a pass outranks deferred work"
    );
    assert_eq!(counts(0, 0, 0, 0, 5).mark(), Some((WorkMark::Deferred, 5)));
    assert_eq!(counts(0, 0, 0, 0, 0).mark(), None);
}

/// Each topic's mark inside one scope, read from the scoped strip the
/// topic routes open.
fn topic_marks(
    projection: &WorkProjection<'_>,
    scope: &WorkScope,
) -> BTreeMap<String, Option<(WorkMark, usize)>> {
    let mut by_topic = BTreeMap::<String, WorkCounts>::new();
    for task in projection.scope_strip(scope) {
        by_topic
            .entry(projection.task_topic(task).unwrap_or("unplaced").to_owned())
            .or_default()
            .record(task.availability, task.pass_ready);
    }
    by_topic
        .into_iter()
        .map(|(topic, counts)| (topic, counts.mark()))
        .collect()
}

#[test]
fn each_partition_marks_exactly_its_own_operator_transitions() {
    let mut in_flight = task(1, "/alpha/quiet");
    in_flight.claims = vec![retained(claim(&in_flight, ClaimDisposition::Active))];
    let mut stale = task(2, "/alpha/quiet");
    stale.claims = vec![retained(claim(&stale, ClaimDisposition::Lapsed))];
    let mut review = task(3, "/alpha/quiet");
    review.workflow.body.phase = "review".parse().expect("review phase");
    let ready = task(4, "/alpha/quiet");
    let workflow_waiting = task(5, "/alpha/quiet");
    let mut deferred = task(6, "/beta/later");
    deferred.workflow.body.deferral = Some(
        "2026-09-24T06:00:00.000Z"
            .parse()
            .expect("fixture deferral"),
    );
    let mut held = task(7, "/gamma/trust");
    held.statement.body.requirements = wire::EnvironmentRequirements::one(
        EnvironmentRequirement::journal_schema(2).expect("fixture requirement"),
    );
    let blocked = task(8, "/delta/wall");
    let selection = design(9, "/alpha/design");
    let ungrouped = design(10, "/beta/design");
    let decided = design(11, "/gamma/design");
    let proposing = design(12, "/alpha/design");
    let mut claimed = design(13, "/epsilon/design");
    claimed.claims = vec![retained(claim(&claimed, ClaimDisposition::Active))];
    let mut board = board(vec![
        in_flight,
        stale,
        review,
        ready,
        workflow_waiting,
        deferred,
        held,
        blocked,
        selection,
        ungrouped,
        decided,
        proposing,
        claimed,
    ]);
    board.workflow_waiting.insert(stamp(5));
    board.blocked.insert(stamp(8));
    board
        .design_cycles
        .insert(stamp(9), wire::rooms::DesignCycle::AwaitingDecision);
    for ready in [10, 11, 12, 13] {
        board
            .design_cycles
            .insert(stamp(ready), wire::rooms::DesignCycle::ReadyForPass);
    }
    let projection = WorkProjection::from_board_and_lanes(&board, None);
    let marks = |scope: &WorkScope| topic_marks(&projection, scope);

    assert_eq!(
        marks(&WorkScope::AllOpen),
        BTreeMap::from([
            ("/alpha/quiet".to_owned(), None),
            ("/beta/later".to_owned(), Some((WorkMark::Deferred, 1))),
            ("/delta/wall".to_owned(), Some((WorkMark::Blocked, 1))),
            ("/gamma/trust".to_owned(), Some((WorkMark::Blocked, 1))),
        ]),
        "in-flight, stale, review, ready, and neutral waiting need nothing now; a blocked \
         wall, an environment hold, and work intentionally set aside mark their own topic"
    );
    assert_eq!(
        projection.scope_counts(&WorkScope::AllOpen).mark(),
        Some((WorkMark::Blocked, 2)),
        "the execution total never counts a Design"
    );
    assert_eq!(
        marks(&WorkScope::planning()),
        BTreeMap::from([
            ("/alpha/design".to_owned(), Some((WorkMark::Decision, 1))),
            ("/beta/design".to_owned(), Some((WorkMark::Pass, 1))),
            ("/epsilon/design".to_owned(), None),
            ("/gamma/design".to_owned(), Some((WorkMark::Pass, 1))),
        ]),
        "a Design awaiting decision outranks a ready pass in its topic; an unpossessed \
         Design ready for a pass marks its topic; one a lane already holds \
         marks nothing"
    );
    assert_eq!(
        projection.scope_counts(&WorkScope::planning()).mark(),
        Some((WorkMark::Decision, 1)),
        "the Design total never counts execution work"
    );
    let rail = OperatorWaits::new(&projection)
        .waits()
        .iter()
        .map(|wait| {
            projection.tasks()[wait.task()]
                .row()
                .statement
                .body
                .born
                .clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rail,
        [stamp(7), stamp(8), stamp(9)],
        "the rail lists exactly the held, blocked, and selection work the blocked and \
         decision marks count, and never the deferral or a Design ready for a pass"
    );
}

#[test]
fn every_work_scope_writes_only_its_stable_route_identity() {
    let route = |topic: &str| WorkRouteTopic::new(topic.to_owned()).expect("fixture route");
    let team = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        .parse()
        .expect("fixture team");
    for (scope, route) in [
        (WorkScope::AllOpen, WorkRoute::AllOpen),
        (WorkScope::All, WorkRoute::All),
        (
            WorkScope::Topic("/oops".to_owned()),
            WorkRoute::Topic(route("/oops")),
        ),
        (
            WorkScope::Topic("/app/work/map".to_owned()),
            WorkRoute::Topic(route("/app/work/map")),
        ),
        (
            WorkScope::Planning { topic: None },
            WorkRoute::Planning { topic: None },
        ),
        (
            WorkScope::Planning {
                topic: Some("/oops/design".to_owned()),
            },
            WorkRoute::Planning {
                topic: Some(route("/oops/design")),
            },
        ),
        (WorkScope::Drafts, WorkRoute::Drafts),
        (WorkScope::Team { team }, WorkRoute::Team(team)),
    ] {
        assert_eq!(scope.route(), route);
    }
}

#[test]
fn the_drafts_scope_holds_exactly_the_open_drafts() {
    let filed = task(1, "/drafts/scope");
    let mut open = task(2, "/drafts/scope");
    open.workflow.body.phase = wire::Phase::Draft;
    let mut withdrawn = task(3, "/drafts/scope");
    withdrawn.workflow.body.phase = wire::Phase::Draft;
    withdrawn.workflow.body.status = wire::WorkflowStatus::deleted(
        "2026-09-23T06:00:00.000Z"
            .parse()
            .expect("fixture completion"),
    );
    let board = board(vec![filed, open, withdrawn]);
    let work = WorkProjection::from_board_and_lanes(&board, None);

    assert_eq!(
        work.overview_for_scope(WorkScope::Drafts).tasks(),
        &[stamp(2)]
    );
    assert_eq!(
        resolve_work_route(&WorkRoute::Drafts, None),
        WorkRouteResolution::Resolved(WorkScope::Drafts)
    );
}

#[test]
fn durable_work_identities_resolve_only_after_their_mutable_authority_settles() {
    let board = board(vec![task(1, "/oops/design"), task(2, "/private/two")]);
    let pending = WorkProjection::from_board_and_lanes(&board, None);
    let route = |topic: &str| WorkRouteTopic::new(topic.to_owned()).expect("fixture route");

    for scope in [WorkScope::AllOpen, WorkScope::All] {
        assert_eq!(
            resolve_work_route(&scope.route(), None),
            WorkRouteResolution::Resolved(scope)
        );
    }
    assert_eq!(
        resolve_work_route(&WorkRoute::Planning { topic: None }, None),
        WorkRouteResolution::Resolved(WorkScope::planning())
    );
    assert_eq!(
        resolve_work_route(&WorkRoute::Topic(route("/oops/design")), None),
        WorkRouteResolution::Pending
    );
    assert_eq!(
        resolve_work_route(&WorkRoute::Topic(route("/oops/design")), Some(&pending),),
        WorkRouteResolution::Resolved(WorkScope::Topic("/oops/design".to_owned()))
    );
    assert_eq!(
        resolve_work_route(
            &WorkRoute::Planning {
                topic: Some(route("/private/two")),
            },
            Some(&pending),
        ),
        WorkRouteResolution::Resolved(WorkScope::Planning {
            topic: Some("/private/two".to_owned()),
        })
    );
    for missing in ["/oops", "/private"] {
        assert_eq!(
            resolve_work_route(&WorkRoute::Topic(route(missing)), Some(&pending)),
            WorkRouteResolution::Missing,
            "{missing} names no exact topic"
        );
    }

    let team = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        .parse()
        .expect("fixture team");
    assert_eq!(
        resolve_work_route(&WorkRoute::Team(team), Some(&pending)),
        WorkRouteResolution::Pending
    );
    let with_team = WorkProjection::from_board_with_team_memberships(
        &board,
        Some(BTreeSet::from([team])),
        BTreeMap::new(),
    );
    assert_eq!(
        resolve_work_route(&WorkRoute::Team(team), Some(&with_team)),
        WorkRouteResolution::Resolved(WorkScope::Team { team })
    );
}

#[test]
fn a_direct_task_outside_the_bounded_board_keeps_a_retained_row() {
    let current = task(1, "/app/current");
    let current_stamp = current.statement.body.born.clone();
    let retained_stamp = stamp(2);
    let board = board(vec![current]);
    let work = WorkProjection::from_board_and_lanes(&board, None);

    let holders = work.holders();
    let current_selection = TaskOverviewSelection::task(current_stamp);
    let current_list = work.list(current_selection.tasks(), &holders, |_| true);
    assert_eq!(
        work.retained_direct_task(&current_selection, &current_list),
        None,
        "a listed task keeps its ordinary projected row"
    );
    let retained_selection = TaskOverviewSelection::task(retained_stamp.clone());
    let retained_list = work.list(retained_selection.tasks(), &holders, |_| true);
    assert_eq!(
        work.retained_direct_task(&retained_selection, &retained_list),
        Some(&retained_stamp),
        "an exact historical selection keeps a row for targeted detail"
    );
    assert_eq!(
        work.graph_selection(&retained_selection, &retained_list)
            .tasks(),
        retained_selection.tasks(),
        "an exact missing-board root remains selected for native enrichment"
    );
    assert_eq!(
        work.graph_selection(&current_selection, &current_list)
            .tasks(),
        current_selection.tasks()
    );
    let filtered = work.list(current_selection.tasks(), &holders, |_| false);
    assert!(
        work.graph_selection(&current_selection, &filtered)
            .tasks()
            .is_empty(),
        "a filter still excludes a current board row"
    );
    let scope_selection = TaskOverviewSelection::new(
        [retained_stamp.clone()],
        TaskOverviewContext::Scope(WorkScope::Topic("app.history".to_owned())),
    );
    assert!(
        work.graph_selection(&scope_selection, &retained_list)
            .tasks()
            .is_empty(),
        "scope selections do not invent historical graph roots"
    );
    assert_eq!(
        work.retained_direct_task(
            &TaskOverviewSelection::new(
                [retained_stamp],
                TaskOverviewContext::Scope(WorkScope::Topic("app.history".to_owned())),
            ),
            &retained_list,
        ),
        None,
        "only an exact task selection may extend the bounded board"
    );
}
