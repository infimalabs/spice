use super::*;

pub(super) fn scoped(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    read: BoardRead,
) -> BoardAnswer {
    match issue(bus, epoch, actor, CommandKind::BoardRead, Vec::new(), &read) {
        Outcome::Reply(Reply {
            body: CommandAnswer::Board(answer),
            ..
        }) => *answer,
        outcome => panic!("scoped board read failed: {outcome:?}"),
    }
}
fn read(bus: &Bus, epoch: CanonicalUuid, read: &BoardRead) -> Outcome<CommandAnswer> {
    issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardRead,
        Vec::new(),
        read,
    )
}

pub(super) fn append_terminal_tasks(
    bus: &Bus,
    count: usize,
) -> std::collections::BTreeSet<TaskStamp> {
    append_terminal(bus, count, None)
}

fn append_terminal(
    bus: &Bus,
    count: usize,
    intent: Option<wire::TaskIntent>,
) -> std::collections::BTreeSet<TaskStamp> {
    let origin: wire::Origin = "fixture:paged-history"
        .parse()
        .expect("history fixture origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("current fixture instant");
    let creation =
        body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::PrincipalActor);
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let tasks = (0..count)
        .map(|index| {
            let micros = 100_000_000_000_i64
                .checked_add(i64::try_from(index).expect("fixture count fits task stamp"))
                .expect("fixture task stamp stays bounded");
            push_task(
                &mut write,
                &wire::TaskProvenance::OperatorDraft,
                &creation,
                TerminalTaskFixture {
                    micros,
                    title: format!("Terminal history {index}"),
                    description: None,
                    topic: "/board/test".parse().expect("fixture topic"),
                    completion: at,
                },
                WorkflowFixtureStatus::Completed,
                intent,
            )
        })
        .collect();
    assert!(matches!(
        bus.commit(write).expect("history fixture reaches writer"),
        CommitOutcome::Committed(_)
    ));
    tasks
}

pub(super) struct TerminalTaskFixture {
    pub(super) micros: i64,
    pub(super) title: String,
    pub(super) description: Option<String>,
    pub(super) topic: wire::TaskTopic,
    pub(super) completion: Instant,
}

#[derive(Clone, Copy)]
enum WorkflowFixtureStatus {
    Pending,
    Completed,
    Deleted,
}

pub(super) fn push_terminal_task(
    write: &mut JournalWrite,
    provenance: &wire::TaskProvenance,
    creation: &body::TaskCreationContext,
    fixture: TerminalTaskFixture,
) -> TaskStamp {
    push_task(
        write,
        provenance,
        creation,
        fixture,
        WorkflowFixtureStatus::Completed,
        None,
    )
}

/// One task head whose workflow has `status`; only `Pending` stays unsettled.
fn push_task(
    write: &mut JournalWrite,
    provenance: &wire::TaskProvenance,
    creation: &body::TaskCreationContext,
    fixture: TerminalTaskFixture,
    status: WorkflowFixtureStatus,
    intent: Option<wire::TaskIntent>,
) -> TaskStamp {
    let task = TaskStamp::for_micros(fixture.micros).expect("fixture task stamp");
    let phase: wire::Phase = "todo".parse().expect("fixture phase");
    let flow = vec![wire::Phase::Draft, phase];
    let added = write
        .fact(
            body::TaskAdded {
                preparation: None,
                intent,
                task: task.clone(),
                title: fixture.title.clone(),
                pitch: "This work keeps the fixture honest.".to_owned(),
                description: fixture.description.clone(),
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: fixture.topic.clone(),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: flow.clone(),
                phase,
                revision: 1,
                creation: creation.clone(),
            },
            FactCause::Root,
        )
        .expect("terminal fixture fact builds");
    let statement_task = task.clone();
    let statement_origin = provenance.clone();
    let statement_creation = creation.clone();
    write.task_head(added, move |position| TaskHead {
        preparation: None,
        intent,
        born: statement_task,
        topic_origin: position,
        title: fixture.title,
        pitch: "This work keeps the fixture honest.".to_owned(),
        description: fixture.description,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: fixture.topic,
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: statement_origin,
        creation: statement_creation,
        revision: position,
    });
    let workflow_task = task.clone();
    write.workflow_head(workflow_task, added, move |_| WorkflowHead {
        flow,
        phase,
        status: match status {
            WorkflowFixtureStatus::Pending => wire::WorkflowStatus::pending(),
            WorkflowFixtureStatus::Completed => wire::WorkflowStatus::reviewed(fixture.completion),
            WorkflowFixtureStatus::Deleted => wire::WorkflowStatus::deleted(fixture.completion),
        },
        deferral: None,
        submitted: false,
        generation: 0,
    });
    task
}

fn handles(answer: &BoardAnswer) -> std::collections::BTreeSet<TaskHandle> {
    answer
        .tasks
        .iter()
        .map(|task| task.statement.body.handle.clone())
        .collect()
}

#[test]
fn scoped_reads_preserve_completed_dependencies_and_claim_conflicts_without_unrelated_rows() {
    let (root, bus, epoch, seats) = fixture("scoped-board", 1);
    let actor = Actor::Lane(seats[0].lane);
    let completed = todo_task(&bus, epoch, "completed", "Completed prerequisite");
    claim_task(&bus, epoch, &seats[0], &actor, &completed);
    let held = board(&bus, epoch, actor.clone());
    let heads = at(&held, &completed);
    assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::TaskDone,
        progress_witnesses(&held, heads, seats[0].lane, seats[0].occupant, false)
            .expect("held task has completion witnesses"),
        &TaskDone {
            operation: Some(TaskDone::operation_for_claim(
                heads
                    .latest_claim()
                    .map(|held| &held.claim)
                    .expect("claim")
                    .body
                    .claimed,
            )),
            ..finished(&completed, "scoped history fixture")
        },
    ));
    let claimed = todo_task(&bus, epoch, "claimed", "Conflicting active claim");
    claim_task(&bus, epoch, &seats[0], &actor, &claimed);
    let selected = todo_task(&bus, epoch, "selected", "Selected task");
    let middle = todo_task(&bus, epoch, "middle", "Intermediate prerequisite");
    let unrelated = todo_task(&bus, epoch, "unrelated", "Unrelated history");
    connect_child(&bus, epoch, &selected, &middle);
    connect_child(&bus, epoch, &middle, &completed);

    let context = scoped(
        &bus,
        epoch,
        actor.clone(),
        BoardRead::Tasks {
            tasks: vec![selected.born().expect("task stamp")],
            dependencies: true,
            claims: true,
            history: false,
        },
    );
    assert_eq!(
        handles(&context),
        [
            selected.clone(),
            middle.clone(),
            completed.clone(),
            claimed.clone()
        ]
        .into()
    );
    assert!(
        at(&context, &completed)
            .workflow
            .body
            .status
            .completion()
            .is_some()
    );
    assert!(matches!(recovery_preflight(&context, &selected),
        Err(Outcome::Refusal(refusal)) if refusal.reason() == "claim::active-exists"));

    let exact = scoped(
        &bus,
        epoch,
        actor.clone(),
        BoardRead::Tasks {
            tasks: vec![completed.born().expect("task stamp")],
            dependencies: false,
            claims: false,
            history: false,
        },
    );
    assert_eq!(handles(&exact), [completed.clone()].into());
    assert!(
        at(&exact, &completed)
            .workflow
            .body
            .status
            .completion()
            .is_some()
    );

    let trust = scoped(&bus, epoch, actor.clone(), BoardRead::Trust);
    assert!(trust.tasks.is_empty());
    assert_eq!(
        handles(&board(&bus, epoch, actor)),
        [selected, middle, completed, claimed, unrelated].into()
    );
    clean(&root, bus);
}

#[test]
fn open_read_offers_the_full_board_allocation_order_from_its_bounded_rows() {
    let (root, bus, epoch, seats) = fixture("offered-board", 1);
    let actor = Actor::Lane(seats[0].lane);
    let completed = todo_task(&bus, epoch, "completed", "Completed prerequisite");
    claim_task(&bus, epoch, &seats[0], &actor, &completed);
    let held = board(&bus, epoch, actor.clone());
    let heads = at(&held, &completed);
    assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::TaskDone,
        progress_witnesses(&held, heads, seats[0].lane, seats[0].occupant, false)
            .expect("held task has completion witnesses"),
        &TaskDone {
            operation: Some(TaskDone::operation_for_claim(
                heads
                    .latest_claim()
                    .map(|held| &held.claim)
                    .expect("claim")
                    .body
                    .claimed,
            )),
            ..finished(&completed, "offer order fixture")
        },
    ));
    append_terminal_tasks(&bus, 3);
    let claimed = todo_task(&bus, epoch, "claimed", "Held by a lane");
    claim_task(&bus, epoch, &seats[0], &actor, &claimed);
    let unblocked = todo_task(&bus, epoch, "unblocked", "Ready after its prerequisite");
    let waiting = todo_task(&bus, epoch, "waiting", "Waits on open work");
    let middle = todo_task(&bus, epoch, "middle", "Open prerequisite");
    connect_child(&bus, epoch, &unblocked, &completed);
    connect_child(&bus, epoch, &waiting, &middle);

    let open = scoped(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        BoardRead::Open {
            recent_finished: 0,
            after: None,
        },
    );
    assert_eq!(
        handles(&open),
        [
            unblocked.clone(),
            waiting.clone(),
            middle.clone(),
            claimed,
            completed
        ]
        .into(),
        "a settled prerequisite rides along as evidence; unrelated history stays home"
    );
    let full = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let entries = actor_entries(&full).expect("full board entries validate");
    assert_eq!(
        open.offer,
        crate::board_command::read_guard::board_offer(&entries, &full, &policy_owners(&full))
    );
    assert!(open.blocked.is_empty());
    assert_eq!(
        open.workflow_waiting,
        [waiting.born().expect("waiting stamp")],
        "an ordinary unsettled dependency waits behind advancing work"
    );
    // The open answer alone judges readiness: a row whose prerequisite
    // settled is workable without reading retained history.
    let open_entries = actor_entries(&open).expect("open entries validate");
    let satisfied = board::resolved_tasks(&open_entries);
    let eligible = |task: &TaskHandle| {
        let entry = open_entries
            .iter()
            .find(|entry| entry.handle() == task)
            .expect("open row is answered");
        board::eligible_entry(
            &satisfied,
            entry,
            &Actor::TEST_PRINCIPAL,
            &open.environment(),
        )
    };
    assert!(eligible(&unblocked));
    assert!(!eligible(&waiting));
    let offered = open
        .offer
        .iter()
        .map(|born| at_born(&open, born))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(offered, [unblocked, middle].into());
    clean(&root, bus);
}

fn at_born(answer: &BoardAnswer, born: &TaskStamp) -> TaskHandle {
    answer
        .tasks
        .iter()
        .find(|task| &task.statement.body.born == born)
        .expect("offered stamps name answered rows")
        .statement
        .body
        .handle
        .clone()
}

/// A direction's Work outlives the open board: open reads index every task an
/// acknowledgement caused, open or settled, until that task is deleted.
#[test]
fn open_reads_index_the_work_each_direction_caused_until_it_is_deleted() {
    let (root, bus, epoch, _seats) = fixture("open-origin-index", 0);
    let ack: wire::TaskProvenance = "ack:dTVQdQfLw".parse().expect("ack fixture provenance");
    let unrelated: wire::Origin = "fixture:unrelated".parse().expect("fixture origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("current fixture instant");
    let creation =
        body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::PrincipalActor);
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        unrelated.clone(),
        at,
        Producer::CommandDoor,
    );
    let mut push = |micros: i64, provenance: &wire::TaskProvenance, status| {
        let fixture = TerminalTaskFixture {
            micros,
            title: format!("Origin fixture {micros}"),
            description: None,
            topic: "/board/test".parse().expect("fixture topic"),
            completion: at,
        };
        push_task(&mut write, provenance, &creation, fixture, status, None)
    };
    let open = push(300_000_000_000, &ack, WorkflowFixtureStatus::Pending);
    let completed = push(300_000_000_001, &ack, WorkflowFixtureStatus::Completed);
    push(300_000_000_002, &ack, WorkflowFixtureStatus::Deleted);
    push(
        300_000_000_003,
        &wire::TaskProvenance::OperatorDraft,
        WorkflowFixtureStatus::Completed,
    );
    assert!(matches!(
        bus.commit(write).expect("origin fixture reaches writer"),
        CommitOutcome::Committed(_)
    ));

    let answer = scoped(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        BoardRead::Open {
            recent_finished: 0,
            after: None,
        },
    );
    let mut caused = [open, completed].map(|task| {
        TaskHandle::for_topic(
            &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
            &task,
        )
    });
    caused.sort();
    assert_eq!(
        answer.origins,
        std::collections::BTreeMap::from([("dTVQdQfLw".to_owned(), caused.to_vec())])
    );
    clean(&root, bus);
}
#[test]
fn open_read_excludes_large_history_and_cursor_pages_cover_it_once() {
    const HISTORY_TASKS: usize = 129;
    const PAGE_LIMIT: u16 = 64;

    let (root, bus, epoch, _seats) = fixture("paged-board-history", 0);
    let expected = append_terminal_tasks(&bus, HISTORY_TASKS);
    let open_task = todo_task(&bus, epoch, "only-open", "Only open task");
    let recent_prerequisite = expected
        .iter()
        .next_back()
        .expect("terminal fixture is nonempty")
        .clone();
    let recent_prerequisite = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &recent_prerequisite,
    );
    connect_child(&bus, epoch, &open_task, &recent_prerequisite);
    let open = scoped(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        BoardRead::Open {
            recent_finished: 0,
            after: None,
        },
    );
    assert_eq!(
        handles(&open),
        [open_task, recent_prerequisite].into(),
        "only the open row's settled prerequisite leaves history"
    );
    assert_eq!(open.history, None);
    let recent = scoped(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        BoardRead::Open {
            recent_finished: 20,
            after: None,
        },
    );
    let first_history = scoped(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        BoardRead::History {
            before: None,
            query: None,
            completed_before: None,
            team: None,
            limit: 20,
        },
    );
    let first_history = first_history
        .tasks
        .iter()
        .map(|task| task.statement.body.born.clone())
        .collect::<Vec<_>>();
    assert_eq!(recent.finished, first_history);
    assert_eq!(recent.finished.len(), 20);
    assert_eq!(recent.tasks.len(), 21);
    assert!(recent.finished.iter().all(|finished| {
        recent
            .tasks
            .iter()
            .any(|task| task.statement.body.born == *finished)
    }));

    let mut before = None;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let page = scoped(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            BoardRead::History {
                before: before.clone(),
                query: None,
                completed_before: None,
                team: None,
                limit: PAGE_LIMIT,
            },
        );
        let metadata = page.history.expect("history read carries page metadata");
        assert_eq!(metadata.before, before);
        assert_eq!(metadata.limit, PAGE_LIMIT);
        assert_eq!(
            metadata.total,
            u64::try_from(HISTORY_TASKS).expect("fixture count fits wire total")
        );
        assert!(page.tasks.len() <= usize::from(PAGE_LIMIT));
        for pair in page.tasks.windows(2) {
            assert!(pair[0].workflow.position > pair[1].workflow.position);
        }
        for task in page.tasks {
            assert!(
                seen.insert(task.statement.body.born),
                "a durable history identity appeared on more than one page"
            );
        }
        let Some(next) = metadata.next else {
            break;
        };
        before = Some(next);
    }
    assert_eq!(seen, expected);
    clean(&root, bus);
}

struct FilteredHistoryFixture {
    first: Instant,
    second: Instant,
    alpha: TaskStamp,
    topic_match: TaskStamp,
    description_match: TaskStamp,
    title_match: TaskStamp,
}

fn append_filtered_history(bus: &Bus) -> FilteredHistoryFixture {
    let origin: wire::Origin = "fixture:filtered-history"
        .parse()
        .expect("filtered history origin");
    let first: Instant = "2030-04-12T18:44:01.000Z"
        .parse()
        .expect("first completion instant");
    let second: Instant = "2030-04-12T18:44:02.000Z"
        .parse()
        .expect("second completion instant");
    let third: Instant = "2030-04-12T18:44:03.000Z"
        .parse()
        .expect("third completion instant");
    let creation =
        body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::PrincipalActor);
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin.clone(),
        third,
        Producer::CommandDoor,
    );
    let alpha = push_terminal_task(
        &mut write,
        &wire::TaskProvenance::OperatorDraft,
        &creation,
        TerminalTaskFixture {
            micros: 200_000_000_000,
            title: "Alpha title".to_owned(),
            description: None,
            topic: "/board/alpha".parse().expect("fixture topic"),
            completion: first,
        },
    );
    let topic_match = push_terminal_task(
        &mut write,
        &wire::TaskProvenance::OperatorDraft,
        &creation,
        TerminalTaskFixture {
            micros: 200_000_000_001,
            title: "Topic match".to_owned(),
            description: None,
            topic: "/board/needle".parse().expect("fixture topic"),
            completion: second,
        },
    );
    let description_match = push_terminal_task(
        &mut write,
        &wire::TaskProvenance::OperatorDraft,
        &creation,
        TerminalTaskFixture {
            micros: 200_000_000_002,
            title: "Description match".to_owned(),
            description: Some("Needle context".to_owned()),
            topic: "/board/context".parse().expect("fixture topic"),
            completion: second,
        },
    );
    let title_match = push_terminal_task(
        &mut write,
        &wire::TaskProvenance::OperatorDraft,
        &creation,
        TerminalTaskFixture {
            micros: 200_000_000_003,
            title: "UPPERCASE NEEDLE TITLE".to_owned(),
            description: None,
            topic: "/board/title".parse().expect("fixture topic"),
            completion: third,
        },
    );
    assert!(matches!(
        bus.commit(write).expect("filtered history reaches writer"),
        CommitOutcome::Committed(_)
    ));
    FilteredHistoryFixture {
        first,
        second,
        alpha,
        topic_match,
        description_match,
        title_match,
    }
}

fn history_page(
    bus: &Bus,
    epoch: CanonicalUuid,
    before: Option<TaskHistoryCursor>,
    query: Option<&str>,
    completed_before: Option<Instant>,
    limit: u16,
) -> BoardAnswer {
    scoped(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        BoardRead::History {
            before,
            query: query.map(str::to_owned),
            completed_before,
            team: None,
            limit,
        },
    )
}

#[test]
fn history_filters_before_paging_with_case_insensitive_fields_and_inclusive_completion() {
    let (root, bus, epoch, _seats) = fixture("filtered-board-history", 0);
    let filtered = append_filtered_history(&bus);
    let open = todo_task(&bus, epoch, "needle-open", "Needle open task");

    let first_page = history_page(&bus, epoch, None, Some("NeEdLe"), Some(filtered.second), 1);
    assert_eq!(
        first_page.tasks[0].statement.body.born,
        filtered.description_match
    );
    let first_metadata = first_page.history.expect("filtered first page metadata");
    assert_eq!(first_metadata.total, 2);
    let second_page = history_page(
        &bus,
        epoch,
        first_metadata.next,
        Some("needle"),
        Some(filtered.second),
        1,
    );
    assert_eq!(
        second_page.tasks[0].statement.body.born,
        filtered.topic_match
    );
    let second_metadata = second_page.history.expect("filtered second page metadata");
    assert_eq!(second_metadata.total, 2);
    assert_eq!(second_metadata.next, None);

    let all_matches = history_page(&bus, epoch, None, Some("needle"), None, 10);
    assert_eq!(
        all_matches.history.as_ref().expect("match metadata").total,
        3
    );
    assert!(!handles(&all_matches).contains(&open));
    assert!(
        handles(&all_matches).contains(&TaskHandle::for_topic(
            &"/board/title"
                .parse::<wire::Topic>()
                .expect("fixture topic"),
            &filtered.title_match
        ))
    );

    let handle = TaskHandle::for_topic(
        &"/board/alpha"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &filtered.alpha,
    )
    .to_string()
    .to_lowercase();
    let handle_match = history_page(&bus, epoch, None, Some(&handle), None, 10);
    assert_eq!(
        handles(&handle_match),
        [TaskHandle::for_topic(
            &"/board/alpha"
                .parse::<wire::Topic>()
                .expect("fixture topic"),
            &filtered.alpha
        )]
        .into()
    );

    for (cutoff, expected) in [
        (
            filtered.first,
            [filtered.alpha.clone()].into_iter().collect(),
        ),
        (
            filtered.second,
            [
                filtered.alpha,
                filtered.topic_match,
                filtered.description_match,
            ]
            .into_iter()
            .collect(),
        ),
    ] {
        let page = history_page(&bus, epoch, None, None, Some(cutoff), 10);
        let found = page
            .tasks
            .iter()
            .map(|task| task.statement.body.born.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(found, expected);
    }

    let miss = history_page(&bus, epoch, None, Some("missing"), None, 10);
    assert!(miss.tasks.is_empty());
    assert_eq!(miss.history.expect("miss metadata").total, 0);
    let blank = history_page(&bus, epoch, None, Some(" \t "), None, 10);
    assert_eq!(
        blank.history.expect("blank query metadata").total,
        4,
        "blank query is the unfiltered terminal history"
    );
    clean(&root, bus);
}

#[test]
fn history_bounds_empty_page_and_stale_cursor_are_explicit() {
    let (root, bus, epoch, _seats) = fixture("paged-board-history-errors", 0);
    let empty = scoped(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        BoardRead::History {
            before: None,
            query: None,
            completed_before: None,
            team: None,
            limit: 10,
        },
    );
    assert!(empty.tasks.is_empty());
    assert_eq!(
        empty.history,
        Some(TaskHistoryPage {
            before: None,
            limit: 10,
            total: 0,
            next: None,
        })
    );

    for limit in [0, TASK_HISTORY_PAGE_LIMIT_MAX.saturating_add(1)] {
        assert!(matches!(
            read(
                &bus,
                epoch,
                &BoardRead::History {
                    before: None,
                    query: None,
                    completed_before: None,
                    team: None,
                    limit,
                },
            ),
            Outcome::Refusal(refusal) if refusal.reason() == "board::history-limit-invalid"
        ));
    }
    let stale = TaskHistoryCursor {
        workflow: Seq::new(1).expect("fixture sequence"),
        task: stamp("absent-history-cursor"),
    };
    for query in [
        "x".repeat(TASK_HISTORY_QUERY_BYTES_MAX.saturating_add(1)),
        "bad\0query".to_owned(),
    ] {
        assert!(matches!(
            read(
                &bus,
                epoch,
                &BoardRead::History {
                    before: None,
                    query: Some(query),
                    completed_before: None,
                    team: None,
                    limit: 10,
                },
            ),
            Outcome::Refusal(refusal) if refusal.reason() == "board::history-query-invalid"
        ));
    }
    assert!(matches!(
        read(
            &bus,
            epoch,
            &BoardRead::History {
                before: Some(stale),
                query: None,
                completed_before: None,
                team: None,
                limit: 10,
            },
        ),
        Outcome::Refusal(refusal) if refusal.reason() == "board::history-cursor-invalid"
    ));
    clean(&root, bus);
}

#[test]
fn history_cursor_orders_equal_workflow_heads_by_durable_task_identity() {
    let workflow = Seq::new(7).expect("fixture workflow position");
    let first = TaskHistoryCursor {
        workflow,
        task: TaskStamp::for_micros(10).expect("first fixture task"),
    };
    let second = TaskHistoryCursor {
        workflow,
        task: TaskStamp::for_micros(11).expect("second fixture task"),
    };
    let newer = TaskHistoryCursor {
        workflow: Seq::new(8).expect("newer fixture workflow position"),
        task: TaskStamp::for_micros(12).expect("newer fixture task"),
    };
    let mut cursors = vec![second.clone(), first.clone(), newer.clone()];
    cursors.sort_by(history_cursor_order);
    assert_eq!(cursors, [newer, first, second]);
}
#[test]
fn implicit_completion_reads_completed_prerequisites_before_advancing_a_plan() {
    let (root, bus, epoch, seats) = fixture("scoped-plan-completion", 1);
    let seat = &seats[0];
    let actor = Actor::Lane(seat.lane);
    let completed = todo_task(&bus, epoch, "completed", "Completed prerequisite");
    claim_and_complete_task(&bus, epoch, seat, &actor, &completed);
    let plan = add_task_with_term(
        &bus,
        epoch,
        "plan",
        "Reconcile completed work",
        "/task/read",
        Some("plan,todo,review"),
        &["The completed work is reconciled"],
    );
    connect_child(&bus, epoch, &plan, &completed);
    claim_task(&bus, epoch, seat, &actor, &plan);
    let _unrelated = todo_task(&bus, epoch, "unrelated", "Unrelated work");
    let matches = crate::cli::command()
        .try_get_matches_from(["spice", "task", "done"])
        .expect("implicit completion parses");
    let contract = crate::cli::selected_contract(&matches).expect("completion contract");
    let (_, read) = mutation_read(contract, crate::cli::selected_matches(&matches))
        .expect("completion selects its read");
    let held = scoped(&bus, epoch, actor.clone(), read);
    let heads = at(&held, &plan);
    let expected = progress_witnesses(&held, heads, seat.lane, seat.occupant, false)
        .expect("completed prerequisites remain visible to plan witnesses");
    assert_eq!(handles(&held), [plan.clone(), completed].into());
    assert_committed(issue(
        &bus,
        epoch,
        actor.clone(),
        CommandKind::TaskDone,
        expected,
        &TaskDone {
            operation: Some(TaskDone::operation_for_claim(
                heads
                    .latest_claim()
                    .map(|held| &held.claim)
                    .expect("claim")
                    .body
                    .claimed,
            )),
            ..finished(&plan, "Completed work reconciled")
        },
    ));
    let advanced = board(&bus, epoch, actor);
    assert_eq!(at(&advanced, &plan).workflow.body.phase.as_str(), "todo");
    clean(&root, bus);
}
