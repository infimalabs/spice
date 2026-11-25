use super::support;

use board::claim::ClaimRefusal;
use board::phase::PhaseRefusal;
use board::{Active, Entry, Snapshot, Transition, Urgency, View, graph, handout};
use journal::{FactCause, Journal, JournalWrite};
use wire::JournalCursor as Cursor;
use wire::{
    Actor, CanonicalUuid, ClaimDisposition, ClaimHead, Digest, Instant, LeaseHead, Phase, Producer,
    Seq, TaskHandle, TaskHead, TaskStamp, WorkflowHead, body,
};

fn stamp(text: &str) -> TaskStamp {
    let value = text.bytes().fold(0_i64, |value, byte| {
        (value * 131 + i64::from(byte)) % 10_000_000_000_000
    });
    TaskStamp::for_micros(value).expect("fixture name fits the task stamp codec")
}

fn handle(text: &str) -> TaskHandle {
    TaskHandle::for_topic(
        &"/board/kernel"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &stamp(text),
    )
}

fn phase(text: &str) -> Phase {
    text.parse().expect("phase")
}

fn uuid(text: &str) -> CanonicalUuid {
    text.parse().expect("canonical UUID")
}

fn instant(text: &str) -> Instant {
    text.parse().expect("canonical instant")
}

fn seq(value: u64) -> Seq {
    Seq::new(value).expect("non-zero sequence")
}

fn environment() -> wire::Environment {
    wire::Environment::journal_schema(journal::SCHEMA_VERSION)
        .expect("the compiled journal schema is positive")
}

fn all_topics() -> board::Allocation {
    support::allocation(uuid("019fbbde-5105-75a3-a76a-722afde1e408"), true)
}

fn select<'a>(
    entries: &'a [Entry],
    actor: &Actor,
    subscription: &board::Allocation,
) -> Option<board::Selection<'a>> {
    board::select(entries, actor, subscription, &environment())
}

fn snapshot_for(journal: &Journal, task: &TaskStamp) -> Result<Snapshot, board::TaskError> {
    support::bounded_snapshot(journal, std::slice::from_ref(task))
}

/// Admit heads with at most one claim, whose lease rides with it.
fn admitted(
    statement: TaskHead,
    workflow: WorkflowHead,
    claim: Option<ClaimHead>,
    lease: Option<LeaseHead>,
) -> Result<Entry, board::TaskError> {
    Entry::new(
        statement,
        workflow,
        wire::CompletionHead::opened(seq(1)),
        claim.map(|claim| (claim, lease)).into_iter().collect(),
    )
}

fn statement(name: &str) -> TaskHead {
    let born = stamp(name);
    TaskHead {
        preparation: None,
        intent: None,
        born: born.clone(),
        topic_origin: seq(1),
        title: format!("Implement {name}"),
        pitch: "Coordinated work must land once.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: "/board/kernel".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: wire::TaskProvenance::Task(born.clone()),
        creation: body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::SpiceActor),
        revision: seq(1),
    }
}

fn workflow(current: &str) -> WorkflowHead {
    WorkflowHead {
        flow: vec![phase("draft"), phase("todo"), phase("review")],
        phase: phase(current),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    }
}

fn claim(disposition: ClaimDisposition) -> ClaimHead {
    ClaimHead {
        actor: Actor::Spice,
        fact: seq(2),
        claimed: seq(2),
        cell: wire::ClaimCell {
            topic: Some("/board/kernel".parse().expect("fixture topic")),
            phase: phase("todo"),
        },
        lane: uuid("019fbbde-5105-75a3-a76a-722afde1e408"),
        occupant: uuid("019fbbde-5105-75a3-a76a-722afde1e409"),
        epoch: uuid("019fbbde-5105-75a3-a76a-722afde1e410"),
        recovery: None,
        disposition,
        acknowledged: 0,
    }
}

fn lease() -> LeaseHead {
    LeaseHead {
        claimed: seq(2),
        start: instant("2026-08-01T06:00:00.000Z"),
        renewal: instant("2026-08-01T06:01:00.000Z"),
        expiry: instant("2026-08-01T06:10:00.000Z"),
        maximum: instant("2026-08-01T06:20:00.000Z"),
        epoch: uuid("019fbbde-5105-75a3-a76a-722afde1e410"),
    }
}

fn entry(name: &str) -> Entry {
    admitted(statement(name), workflow("todo"), None, None).expect("valid heads")
}

fn claimed_entry(name: &str, current: &str) -> Entry {
    admitted(
        statement(name),
        workflow(current),
        Some(claim(ClaimDisposition::Active)),
        Some(lease()),
    )
    .expect("valid claimed heads")
}

fn completed_entry(name: &str) -> Entry {
    let mut completed = workflow("review");
    completed.status = wire::WorkflowStatus::completed(instant("2026-08-01T06:06:00.000Z"));
    admitted(
        statement(name),
        completed,
        Some(claim(ClaimDisposition::Active)),
        Some(lease()),
    )
    .expect("valid completed heads")
}

fn journal_with_heads(mut statement: TaskHead, workflow: WorkflowHead) -> Journal {
    let mut journal = Journal::in_memory().expect("board journal fixture");
    let task = statement.born.clone();
    let mut write = JournalWrite::new(
        Actor::Spice,
        statement.provenance.origin(),
        instant("2026-08-01T06:00:00.000Z"),
        Producer::CommandDoor,
    );
    let fact = write
        .fact(
            body::TaskAdded {
                preparation: None,
                intent: None,
                task: task.clone(),
                title: statement.title.clone(),
                pitch: statement.pitch.clone(),
                description: statement.description.clone(),
                term: statement.term.clone(),
                requirements: statement.requirements.clone(),
                topic: statement.topic.clone(),
                submitted: false,
                priority: statement.priority,
                taste: statement.taste.clone(),
                depends: statement.dependencies.clone(),
                flow: workflow.flow.clone(),
                phase: workflow.phase,
                revision: 1,
                creation: statement.creation.clone(),
            },
            FactCause::Root,
        )
        .expect("registered board fixture fact");
    write.task_head(fact, move |position| {
        statement.topic_origin = position;
        statement.revision = position;
        statement
    });
    write.workflow_head(task, fact, move |_| workflow);
    journal.apply(write).expect("commit board fixture heads");
    journal
}

#[test]
fn statement_views_preserve_topic_family_handle_and_dependency_heads() {
    let mut head = statement("BOARD-a1");
    head.dependencies = vec![stamp("WIRE-b2"), stamp("JOURNAL-c3")];
    let entry = admitted(head, workflow("todo"), None, None).expect("valid heads");

    assert_eq!(
        entry.topic().map(wire::Topic::as_str),
        Some("/board/kernel")
    );
    assert_eq!(entry.family().as_str(), "KERNEL");
    assert_eq!(
        entry.dependencies().collect::<Vec<_>>(),
        vec![
            board::Dependency {
                task: stamp("BOARD-a1"),
                after: stamp("WIRE-b2"),
            },
            board::Dependency {
                task: stamp("BOARD-a1"),
                after: stamp("JOURNAL-c3"),
            },
        ]
    );

    let mut self_dependent = statement("BOARD-a1");
    self_dependent.dependencies = vec![stamp("BOARD-a1")];
    assert!(matches!(
        admitted(self_dependent, workflow("todo"), None, None),
        Err(board::TaskError::SelfDependency(_))
    ));

    let mut duplicate = statement("BOARD-a1");
    duplicate.dependencies = vec![stamp("WIRE-b2"), stamp("WIRE-b2")];
    assert!(matches!(
        admitted(duplicate, workflow("todo"), None, None),
        Err(board::TaskError::DuplicateDependency { .. })
    ));

    let mut malformed = workflow("todo");
    malformed.phase = phase("design");
    assert!(matches!(
        admitted(statement("BOARD-a1"), malformed, None, None),
        Err(board::TaskError::PhaseOutsideFlow(_))
    ));

    let mut empty = workflow("todo");
    empty.flow.clear();
    assert!(matches!(
        admitted(statement("BOARD-a1"), empty, None, None),
        Err(board::TaskError::EmptyFlow)
    ));

    let mut duplicate_phase = workflow("todo");
    duplicate_phase.flow.push(phase("todo"));
    assert!(matches!(
        admitted(statement("BOARD-a1"), duplicate_phase, None, None),
        Err(board::TaskError::DuplicatePhase(_))
    ));
}

#[test]
fn board_graph_views_render_independent_relations_in_stable_order() {
    let reviewer = Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e411"));
    let author = Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e412"));
    let mut alpha_head = statement("BOARD-a1");
    alpha_head.provenance = wire::TaskProvenance::OperatorDraft;
    alpha_head.dependencies.push(stamp("BOARD-b2"));
    let alpha_workflow = workflow("review");
    let alpha = admitted(
        alpha_head,
        alpha_workflow,
        Some(claim(ClaimDisposition::Active)),
        Some(lease()),
    )
    .expect("valid reviewed and claimed entry");
    let beta = entry("BOARD-b2");
    let entries = vec![beta.clone(), alpha.clone()];
    let stable_entries = vec![alpha.clone(), beta.clone()];
    let reviews = vec![graph::ReviewEdge::new(
        alpha.handle().clone(),
        Some((phase("todo"), author.clone())),
        reviewer.clone(),
        body::Verdict::Clean,
        seq(50),
    )];

    for view in View::ALL {
        assert_eq!(
            graph::render(view, &entries, &reviews).text(),
            graph::render(view, &stable_entries, &reviews).text(),
            "{} graph depends on input order",
            view.as_str()
        );
    }
    assert_eq!(
        View::ALL.map(View::as_str),
        ["origin", "dependencies", "review", "workflow", "claims"]
    );

    let origin = graph::render(View::Origin, &entries, &reviews)
        .text()
        .to_owned();
    assert!(origin.contains(&format!(
        "{} -> {}",
        alpha.statement().provenance,
        alpha.handle()
    )));
    assert!(origin.contains(&format!(
        "{} -> {}",
        beta.statement().provenance,
        beta.handle()
    )));
    assert!(!origin.contains("[assigned]"));

    let dependencies = graph::render(View::Dependencies, &entries, &reviews)
        .text()
        .to_owned();
    assert_eq!(
        dependencies,
        format!(
            "graph dependencies\n{} -> {}\n",
            alpha.handle(),
            beta.handle()
        )
    );

    let review = graph::render(View::Review, &entries, &reviews)
        .text()
        .to_owned();
    assert_eq!(
        review,
        format!(
            "graph review\n{} -> {} [{} todo clean]\n",
            reviewer,
            author,
            alpha.handle(),
        )
    );
    assert!(!review.contains("active"));
    assert!(!review.contains("assigned"));

    let workflow = graph::render(View::Workflow, &entries, &reviews)
        .text()
        .to_owned();
    assert!(workflow.contains(&format!("{}: review [pending]\n", alpha.handle())));
    assert!(workflow.contains(&format!("{}: todo [pending]\n", beta.handle())));
    assert!(!workflow.contains(&reviewer.to_string()));

    let claims = graph::render(View::Claims, &entries, &reviews)
        .text()
        .to_owned();
    assert!(claims.contains(&format!("{}: {} [active]\n", alpha.handle(), Actor::Spice)));
    assert!(!claims.contains("assigned"));

    let handout = handout::compose(&entries, &reviews);
    let sections = [
        "## origin",
        "## dependencies",
        "## review",
        "## workflow",
        "## claims",
    ];
    let positions = sections.map(|section| handout.find(section).expect("section is present"));
    assert!(positions.windows(2).all(|window| window[0] < window[1]));
}

#[test]
fn dependency_graph_does_not_project_design_release() {
    let design = planning(
        "BOARD-design-graph",
        wire::TaskIntent::Design,
        Standing::Open,
    );
    let output = released(&named_output(
        "BOARD-output-graph",
        "BOARD-design-graph",
        "BOARD-design-graph",
    ));

    assert_eq!(
        graph::render(View::Dependencies, &[design, output], &[]).text(),
        "graph dependencies\n"
    );
}

#[test]
fn live_graphs_exclude_settled_history_but_name_direct_dependencies() {
    let reviewer = Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e411"));
    let author = Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e412"));
    let settled_head = statement("BOARD-settled");
    let mut live_head = statement("BOARD-live");
    live_head.dependencies.push(settled_head.born.clone());
    let live = admitted(
        live_head,
        workflow("todo"),
        Some(claim(ClaimDisposition::Active)),
        Some(lease()),
    )
    .expect("live entry");
    let mut settled_workflow = workflow("review");
    settled_workflow.status = wire::WorkflowStatus::completed(instant("2026-08-01T07:00:00.000Z"));
    let settled = admitted(
        settled_head,
        settled_workflow,
        Some(claim(ClaimDisposition::Freed)),
        None,
    )
    .expect("settled entry");
    let reviews = [
        graph::ReviewEdge::new(
            live.handle().clone(),
            Some((phase("todo"), author.clone())),
            reviewer.clone(),
            body::Verdict::Clean,
            seq(50),
        ),
        graph::ReviewEdge::new(
            settled.handle().clone(),
            Some((phase("todo"), author.clone())),
            reviewer.clone(),
            body::Verdict::Clean,
            seq(51),
        ),
    ];
    let entries = [settled.clone(), live.clone()];

    assert_eq!(
        graph::render_live(View::Origin, &entries, &reviews).text(),
        format!(
            "graph origin\n{} -> {}\n",
            live.statement().provenance,
            live.handle()
        )
    );
    assert_eq!(
        graph::render_live(View::Dependencies, &entries, &reviews).text(),
        format!(
            "graph dependencies\n{} -> {}\n",
            live.handle(),
            settled.handle()
        )
    );
    assert_eq!(
        graph::render_live(View::Review, &entries, &reviews).text(),
        format!(
            "graph review\n{reviewer} -> {author} [{} todo clean]\n",
            live.handle()
        )
    );
    assert_eq!(
        graph::render_live(View::Workflow, &entries, &reviews).text(),
        format!("graph workflow\n{}: todo [pending]\n", live.handle())
    );
    assert_eq!(
        graph::render_live(View::Claims, &entries, &reviews).text(),
        format!("graph claims\n{}: spice [active]\n", live.handle())
    );

    for view in View::ALL {
        assert_eq!(
            graph::render_live(view, std::slice::from_ref(&settled), &reviews).text(),
            format!("graph {}\n", view.as_str()),
            "{} retained settled history",
            view.as_str()
        );
    }
}

#[test]
fn stealing_waives_possession_alone_and_waits_on_no_lease() {
    let actor = Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e408"));
    let dependent = {
        let mut head = statement("BOARD-blocked");
        head.dependencies.push(stamp("BOARD-absent"));
        head
    };
    let deferred = {
        let mut head = workflow("todo");
        head.deferral = Some(instant("2026-08-01T07:00:00.000Z"));
        head
    };
    let entries = vec![
        claimed_entry("BOARD-held", "todo"),
        entry("BOARD-free"),
        admitted(
            dependent,
            workflow("todo"),
            Some(claim(ClaimDisposition::Active)),
            Some(lease()),
        )
        .expect("claimed heads awaiting a dependency"),
        admitted(
            statement("BOARD-deferred"),
            deferred,
            Some(claim(ClaimDisposition::Active)),
            Some(lease()),
        )
        .expect("claimed heads sleeping until later"),
    ];
    let [held, free, blocked, sleeping] = &entries[..] else {
        panic!("four possession fixtures");
    };
    let satisfied = board::resolved_tasks(&entries);

    // lease() runs to 06:10 and is nowhere near spent here.
    assert!(!board::eligible_entry(
        &satisfied,
        held,
        &actor,
        &environment()
    ));
    assert!(board::stealable_entry(
        &satisfied,
        held,
        &actor,
        &environment()
    ));

    // Possession is the only clause a steal waives.
    assert!(board::eligible_entry(
        &satisfied,
        free,
        &actor,
        &environment()
    ));
    assert!(board::stealable_entry(
        &satisfied,
        free,
        &actor,
        &environment()
    ));
    assert!(!board::stealable_entry(
        &satisfied,
        blocked,
        &actor,
        &environment()
    ));
    assert!(!board::stealable_entry(
        &satisfied,
        sleeping,
        &actor,
        &environment()
    ));
}

#[test]
fn environment_is_the_final_eligibility_filter_and_never_ranking_or_task_state() {
    let actor = Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e408"));
    let current_schema = journal::SCHEMA_VERSION;
    let required_schema = current_schema
        .checked_add(1)
        .expect("fixture schema advances");
    let mut blocked_head = statement("BOARD-environment-blocked");
    blocked_head.priority = Some(wire::TaskPriority::Critical);
    blocked_head.requirements = wire::EnvironmentRequirements::one(
        wire::EnvironmentRequirement::journal_schema(required_schema)
            .expect("future schema is positive"),
    );
    let blocked = admitted(blocked_head, workflow("todo"), None, None).expect("blocked entry");
    let ordinary = entry("BOARD-environment-ready");
    let entries = vec![blocked.clone(), ordinary.clone()];
    let satisfied = board::resolved_tasks(&entries);

    let observed_before =
        wire::Environment::journal_schema(current_schema).expect("current schema is positive");
    assert!(!board::environment_satisfied(&blocked, &observed_before));
    assert!(!board::eligible_entry(
        &satisfied,
        &blocked,
        &actor,
        &observed_before
    ));
    assert!(!board::selectable_entry(
        &satisfied,
        &blocked,
        &actor,
        &all_topics(),
        &observed_before
    ));
    assert!(!board::stealable_entry(
        &satisfied,
        &blocked,
        &actor,
        &observed_before
    ));
    assert_eq!(
        select(&entries, &actor, &all_topics())
            .expect("ordinary eligible work remains")
            .entry()
            .handle(),
        ordinary.handle(),
        "an environment-blocked critical task never enters ranking"
    );

    let observed_after =
        wire::Environment::journal_schema(required_schema).expect("future schema is positive");
    assert!(!board::environment_satisfied(&blocked, &observed_before));
    assert!(board::environment_satisfied(&blocked, &observed_after));
    assert_eq!(
        blocked.statement().requirements.as_slice(),
        entries[0].statement().requirements.as_slice(),
        "observation movement does not mutate task authority"
    );
}

#[test]
fn trust_grant_transition_changes_every_allocator_admission_without_rewriting_the_task() {
    let actor = Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e408"));
    let digest: Digest = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        .parse()
        .expect("fixture digest");
    let mut head = statement("BOARD-trust-blocked");
    head.requirements = wire::EnvironmentRequirements::one(
        wire::EnvironmentRequirement::trust_grant(digest.clone()),
    );
    let entry = admitted(head, workflow("todo"), None, None).expect("trust-gated entry");
    let entries = vec![entry.clone()];
    let satisfied = board::resolved_tasks(&entries);
    let observed = |state, worktree: Option<&str>| {
        environment().with_trust_heads([wire::TrustHead {
            digest: digest.clone(),
            state,
            worktree: worktree.map(str::to_owned),
        }])
    };
    let missing = environment();
    let pended = observed(wire::TrustState::Pended, Some("/producer/worktree"));
    let revoked = observed(wire::TrustState::Revoked, Some("/producer/worktree"));
    let unbound = observed(wire::TrustState::Granted, None);
    for blocked in [&missing, &pended, &revoked, &unbound] {
        assert!(!board::environment_satisfied(&entry, blocked));
        assert!(!board::eligible_entry(&satisfied, &entry, &actor, blocked));
        assert!(!board::selectable_entry(
            &satisfied,
            &entry,
            &actor,
            &all_topics(),
            blocked,
        ));
        assert!(!board::stealable_entry(&satisfied, &entry, &actor, blocked));
    }

    let granted = observed(wire::TrustState::Granted, Some("/different/worktree"));
    assert!(board::environment_satisfied(&entry, &granted));
    assert!(board::eligible_entry(&satisfied, &entry, &actor, &granted));
    assert!(board::selectable_entry(
        &satisfied,
        &entry,
        &actor,
        &all_topics(),
        &granted,
    ));
    assert!(board::stealable_entry(&satisfied, &entry, &actor, &granted));
    assert_eq!(
        entry.statement().requirements,
        entries[0].statement().requirements,
        "grant movement never rewrites task authority"
    );
}

#[test]
fn snapshot_is_a_schema_and_cursor_bounded_head_answer() {
    let entries = [entry("BOARD-a1")];
    let cursor = Cursor::at(seq(9));
    let snapshot =
        Snapshot::new(journal::SCHEMA_VERSION, cursor, entries.to_vec()).expect("current schema");

    assert_eq!(snapshot.schema_version(), journal::SCHEMA_VERSION);
    assert_eq!(snapshot.cursor(), cursor);
    assert_eq!(snapshot.entries(), entries.as_slice());
    assert!(Snapshot::new(journal::SCHEMA_VERSION + 1, cursor, entries.to_vec()).is_err());
}

#[test]
fn durable_snapshot_owns_one_validated_journal_answer() {
    let snapshot = {
        let journal = journal_with_heads(statement("BOARD-a1"), workflow("todo"));
        snapshot_for(&journal, &stamp("BOARD-a1")).expect("authoritative board snapshot")
    };

    assert_eq!(snapshot.schema_version(), journal::SCHEMA_VERSION);
    assert_eq!(snapshot.cursor(), Cursor::at(Seq::FIRST));
    assert_eq!(snapshot.entries().len(), 1);
    assert_eq!(snapshot.entries()[0].handle(), &handle("BOARD-a1"));
}

#[test]
fn durable_snapshot_names_the_task_whose_heads_violate_board_policy() {
    let mut invalid = workflow("todo");
    invalid.flow.clear();
    let journal = journal_with_heads(statement("BOARD-a1"), invalid);

    assert_eq!(
        snapshot_for(&journal, &stamp("BOARD-a1")),
        Err(board::TaskError::EmptyFlow)
    );
}

#[test]
fn active_claim_is_only_admitted_from_matching_coherent_live_heads() {
    let entry = claimed_entry("BOARD-a1", "todo");
    let active = Active::admit(&entry, &Actor::Spice, instant("2026-08-01T06:05:00.000Z"))
        .expect("live lease proves possession");

    assert_eq!(active.handle(), &handle("BOARD-a1"));
    assert_eq!(active.actor(), &Actor::Spice);
    assert_eq!(active.epoch(), lease().epoch);
    assert!(matches!(
        Active::admit(
            &entry,
            &Actor::TEST_PRINCIPAL,
            instant("2026-08-01T06:05:00.000Z")
        ),
        Err(ClaimRefusal::WrongActor { .. })
    ));
    assert!(matches!(
        Active::admit(&entry, &Actor::Spice, instant("2026-08-01T06:10:00.000Z")),
        Err(ClaimRefusal::Expired { .. })
    ));

    let freed = admitted(
        statement("BOARD-a1"),
        workflow("todo"),
        Some(claim(ClaimDisposition::Freed)),
        Some(lease()),
    )
    .expect("valid heads");
    assert!(matches!(
        Active::admit(&freed, &Actor::Spice, instant("2026-08-01T06:05:00.000Z")),
        Err(ClaimRefusal::Inactive(ClaimDisposition::Freed))
    ));

    // Possession is two heads, and admission names which one is absent rather
    // than reporting the pair as merely incoherent. Both arms are reachable
    // from the resident, which admits through this predicate before it renews
    // or frees, so a half-written pair has to refuse by name here.
    let unclaimed =
        admitted(statement("BOARD-a1"), workflow("todo"), None, None).expect("valid heads");
    assert!(matches!(
        Active::admit(
            &unclaimed,
            &Actor::Spice,
            instant("2026-08-01T06:05:00.000Z")
        ),
        Err(ClaimRefusal::MissingClaim)
    ));

    let leaseless = admitted(
        statement("BOARD-a1"),
        workflow("todo"),
        Some(claim(ClaimDisposition::Active)),
        None,
    )
    .expect("valid heads");
    assert!(matches!(
        Active::admit(
            &leaseless,
            &Actor::Spice,
            instant("2026-08-01T06:05:00.000Z")
        ),
        Err(ClaimRefusal::MissingLease)
    ));
}

#[test]
fn live_frontier_selection_distinguishes_settled_open_and_missing_dependencies() {
    let completed_stamp = stamp("BOARD-completed");
    let deleted_stamp = stamp("BOARD-deleted");
    let missing_stamp = stamp("BOARD-missing");
    let mut completed_workflow = workflow("todo");
    completed_workflow.status =
        wire::WorkflowStatus::completed(instant("2026-08-01T06:10:00.000Z"));
    let mut deleted_workflow = workflow("todo");
    deleted_workflow.status = wire::WorkflowStatus::deleted(instant("2026-08-01T06:10:00.000Z"));
    let completed =
        admitted(statement("BOARD-completed"), completed_workflow, None, None).expect("completed");
    let deleted =
        admitted(statement("BOARD-deleted"), deleted_workflow, None, None).expect("deleted");
    let mut ready_statement = statement("BOARD-ready");
    ready_statement.dependencies = vec![completed_stamp.clone(), deleted_stamp.clone()];
    let ready = admitted(ready_statement, workflow("todo"), None, None).expect("ready");
    let full = [completed, deleted, ready.clone()];
    let reference = select(&full, &Actor::Spice, &all_topics())
        .expect("both terminal forms satisfy dependency edges");

    let mut settled = board::ResolvedTasks::new();
    settled.settle(completed_stamp, true);
    settled.settle(deleted_stamp, false);
    let frontier = [ready.clone()];
    let indexed = board::select_with_satisfied(
        &frontier,
        &Actor::Spice,
        &all_topics(),
        &environment(),
        &settled,
    )
    .expect("compact standing preserves the complete-board selection");
    assert_eq!(indexed.entry().born(), reference.entry().born());

    let mut missing_statement = statement("BOARD-missing-dependent");
    missing_statement.dependencies = vec![missing_stamp];
    let missing =
        admitted(missing_statement, workflow("todo"), None, None).expect("missing dependency");
    assert!(
        board::select_with_satisfied(
            &[missing],
            &Actor::Spice,
            &all_topics(),
            &environment(),
            &settled,
        )
        .is_none(),
        "a stamp omitted from both frontier and standing never becomes implicit success"
    );

    let mut first_statement = statement("BOARD-cycle-a");
    first_statement.dependencies = vec![stamp("BOARD-cycle-b")];
    let mut second_statement = statement("BOARD-cycle-b");
    second_statement.dependencies = vec![stamp("BOARD-cycle-a")];
    let cycle = [
        admitted(first_statement, workflow("todo"), None, None).expect("cycle first"),
        admitted(second_statement, workflow("todo"), None, None).expect("cycle second"),
    ];
    assert!(
        board::select_with_satisfied(
            &cycle,
            &Actor::Spice,
            &all_topics(),
            &environment(),
            &settled,
        )
        .is_none(),
        "an open dependency cycle terminates and remains blocked"
    );
}

#[derive(Clone, Copy, Debug)]
enum Standing {
    Open,
    Positive,
    Closed,
    Deleted,
}

fn planning(name: &str, intent: wire::TaskIntent, standing: Standing) -> Entry {
    let at = instant("2026-08-01T06:00:00.000Z");
    let workflow = match standing {
        Standing::Open if intent == wire::TaskIntent::Design => WorkflowHead {
            flow: vec![phase("draft"), phase("design")],
            ..workflow("design")
        },
        Standing::Open => workflow("todo"),
        Standing::Positive => WorkflowHead {
            status: wire::WorkflowStatus::reviewed(at),
            ..workflow("review")
        },
        Standing::Closed => WorkflowHead {
            status: wire::WorkflowStatus::closed(at),
            ..workflow("review")
        },
        Standing::Deleted => WorkflowHead {
            status: wire::WorkflowStatus::deleted(at),
            ..workflow("review")
        },
    };
    let head = TaskHead {
        intent: Some(intent),
        ..statement(name)
    };
    admitted(head, workflow, None, None).expect("planning heads")
}

fn constructed(
    parent: &str,
    controller: &str,
    intent: Option<wire::TaskIntent>,
    selected_by: Option<&str>,
    dependencies: Vec<TaskStamp>,
) -> Entry {
    let parent = stamp(parent);
    let controller = stamp(controller);
    let selected_by = selected_by.map(stamp);
    let phase = if selected_by.as_ref() == Some(&controller) {
        "todo"
    } else {
        "draft"
    };
    let head = TaskHead {
        preparation: Some(wire::TaskPreparation {
            parent,
            controller,
            construction: stamp("BOARD-output"),
        }),
        intent,
        dependencies,
        ..statement("BOARD-output")
    };
    admitted(head, workflow(phase), None, None).expect("output heads")
}

/// Whether the first entry is eligible against the readiness of all of them.
fn first_eligible(entries: &[Entry]) -> bool {
    let actor = Actor::Lane(uuid("019fbbde-5105-75a3-a76a-722afde1e408"));
    board::eligible_entry(
        &board::resolved_tasks(entries),
        &entries[0],
        &actor,
        &environment(),
    )
}

#[test]
fn a_design_output_is_eligible_only_under_that_designs_selection() {
    let design = |standing| planning("BOARD-design", wire::TaskIntent::Design, standing);
    for intent in [None, Some(wire::TaskIntent::Plan)] {
        let output = |selected_by, dependencies| {
            constructed(
                "BOARD-design",
                "BOARD-design",
                intent,
                selected_by,
                dependencies,
            )
        };
        let selected = Some("BOARD-design");
        for (reason, output, design) in [
            (
                "an unselected output",
                output(None, Vec::new()),
                design(Standing::Open),
            ),
            (
                "another controller's authorization",
                output(Some("BOARD-other"), Vec::new()),
                design(Standing::Open),
            ),
            (
                "an open ordinary dependency",
                output(selected, vec![stamp("BOARD-blocker")]),
                design(Standing::Open),
            ),
        ] {
            assert!(
                !first_eligible(&[output, design, entry("BOARD-blocker")]),
                "{reason} must leave a {intent:?} Design output gated"
            );
        }
        for standing in [
            Standing::Open,
            Standing::Positive,
            Standing::Closed,
            Standing::Deleted,
        ] {
            assert!(
                first_eligible(&[output(selected, Vec::new()), design(standing)]),
                "exact selection permanently releases a {intent:?} Design output"
            );
        }
    }
}

#[test]
fn speculative_design_work_ends_at_exact_execution_authorization() {
    let design = planning("BOARD-design", wire::TaskIntent::Design, Standing::Open);
    let plan = planning("BOARD-plan", wire::TaskIntent::Plan, Standing::Open);
    let resolved = board::resolved_tasks(&[design.clone(), plan.clone()]);
    let speculative = constructed("BOARD-design", "BOARD-design", None, None, Vec::new());
    let authorized = constructed(
        "BOARD-design",
        "BOARD-design",
        None,
        Some("BOARD-design"),
        Vec::new(),
    );
    let mismatched = constructed(
        "BOARD-design",
        "BOARD-design",
        None,
        Some("BOARD-other"),
        Vec::new(),
    );
    let plan_output = constructed("BOARD-plan", "BOARD-plan", None, None, Vec::new());
    let nested_output = constructed(
        "BOARD-plan",
        "BOARD-design",
        None,
        None,
        vec![stamp("BOARD-plan")],
    );
    let remains =
        |entry: &Entry| resolved.speculative_design_work(entry.statement(), entry.workflow());
    let ordinary = entry("BOARD-ordinary");
    assert!(remains(&design));
    assert!(remains(&speculative));
    assert!(remains(&mismatched));
    assert!(!remains(&authorized));
    assert!(!remains(&plan));
    assert!(!remains(&plan_output));
    assert!(!remains(&ordinary));
    assert!(
        remains(&nested_output),
        "a Draft under a Design's control stays speculative though a Plan prepared it"
    );

    let nested_output = released(&nested_output);

    let released_design = planning("BOARD-design", wire::TaskIntent::Design, Standing::Positive);
    let released = board::resolved_tasks(&[
        released_design.clone(),
        planning("BOARD-plan", wire::TaskIntent::Plan, Standing::Positive),
    ]);
    assert!(!released.speculative_design_work(nested_output.statement(), nested_output.workflow()));
    assert!(
        first_eligible(&[
            nested_output,
            released_design,
            planning("BOARD-plan", wire::TaskIntent::Plan, Standing::Positive),
        ]),
        "the summary boundary releases a nested output exactly when allocation does"
    );
}

#[test]
fn a_plan_output_waits_only_for_its_plans_positive_conclusion() {
    let plan = |standing| planning("BOARD-plan", wire::TaskIntent::Plan, standing);
    let task = |dependencies| constructed("BOARD-plan", "BOARD-plan", None, None, dependencies);
    let open = plan(Standing::Open);
    assert!(
        !first_eligible(&[task(Vec::new()), open]),
        "an open Plan must leave its task gated"
    );
    assert!(
        !first_eligible(&[
            released(&task(vec![stamp("BOARD-blocker")])),
            plan(Standing::Positive),
            entry("BOARD-blocker"),
        ]),
        "an open ordinary dependency still gates a Plan output"
    );
    assert!(
        !first_eligible(&[task(Vec::new())]),
        "an unknown preparation leaves its output gated"
    );
    assert!(
        first_eligible(&[released(&task(Vec::new())), plan(Standing::Positive)]),
        "a positively reviewed Plan releases its task without any selection"
    );

    let design = |standing| planning("BOARD-design", wire::TaskIntent::Design, standing);
    let selected_plan_task = released(&constructed(
        "BOARD-plan",
        "BOARD-design",
        None,
        None,
        Vec::new(),
    ));
    for standing in [
        Standing::Open,
        Standing::Positive,
        Standing::Closed,
        Standing::Deleted,
    ] {
        assert!(
            first_eligible(&[
                selected_plan_task.clone(),
                plan(Standing::Positive),
                design(standing),
            ]),
            "positive Plan review permanently releases its task from the outer Design"
        );
    }
    assert!(
        !first_eligible(&[
            constructed(
                "BOARD-plan",
                "BOARD-plan",
                Some(wire::TaskIntent::Plan),
                None,
                Vec::new(),
            ),
            plan(Standing::Open),
        ]),
        "a sub-Plan waits for its parent Plan"
    );
}

#[test]
fn healthy_plan_sequences_and_planning_control_are_neutral_not_blocked() {
    let plan = planning("WAIT-plan", wire::TaskIntent::Plan, Standing::Open);
    let mut sequence = vec![
        plan.clone(),
        named_output_with("WAIT-step-1", "WAIT-plan", "WAIT-plan", Vec::new(), true),
    ];
    for step in 2..=6 {
        sequence.push(named_output_with(
            &format!("WAIT-step-{step}"),
            "WAIT-plan",
            "WAIT-plan",
            vec![stamp(&format!("WAIT-step-{}", step - 1))],
            false,
        ));
    }
    sequence.push(entry("WAIT-ready"));
    let mut ordinary_wait = statement("WAIT-ordinary");
    ordinary_wait.dependencies.push(stamp("WAIT-ready"));
    sequence.push(
        admitted(ordinary_wait, workflow("todo"), None, None).expect("ordinary dependency wait"),
    );
    let mut broken = statement("WAIT-broken");
    broken.dependencies.push(stamp("WAIT-missing"));
    sequence.push(admitted(broken, workflow("todo"), None, None).expect("blocked task"));

    let waits = board::work_waits(&sequence, &environment());
    assert_eq!(waits.get(&stamp("WAIT-step-1")), None);
    for step in 2..=6 {
        assert_eq!(
            waits.get(&stamp(&format!("WAIT-step-{step}"))),
            Some(&board::WorkWait::Workflow),
            "each Plan step behind progressing work is a neutral workflow wait"
        );
    }
    assert_eq!(
        waits.get(&stamp("WAIT-ordinary")),
        Some(&board::WorkWait::Workflow),
        "an ordinary dependency on ready work is waiting its turn"
    );
    assert_eq!(
        waits.get(&stamp("WAIT-broken")),
        Some(&board::WorkWait::Blocked),
        "an ordinary dependency wall is execution trouble"
    );

    let output = named_output("WAIT-plan-output", "WAIT-plan", "WAIT-plan");
    let entries = [plan, output];
    let waits = board::work_waits(&entries, &environment());
    assert_eq!(
        waits.get(&stamp("WAIT-plan-output")),
        Some(&board::WorkWait::Workflow),
        "an output waits neutrally for its Plan's positive review"
    );
}

#[test]
fn design_selection_wins_its_gate_and_its_output_waits_neutrally() {
    let design = planning("WAIT-design", wire::TaskIntent::Design, Standing::Open).with_design(
        wire::rooms::DesignCycleFacts {
            pass: Seq::new(1),
            ..wire::rooms::DesignCycleFacts::default()
        },
    );
    let output = named_output("WAIT-design-output", "WAIT-design", "WAIT-design");
    let entries = [design, output];
    let waits = board::work_waits(&entries, &environment());
    assert_eq!(
        waits.get(&stamp("WAIT-design")),
        Some(&board::WorkWait::Selection),
        "the open Design names the operator action rather than a generic block"
    );
    assert_eq!(
        waits.get(&stamp("WAIT-design-output")),
        Some(&board::WorkWait::Workflow),
        "an unselected output waits neutrally on its open Design"
    );
}

/// `output` after its controller's release moved it out of Draft.
fn released(output: &Entry) -> Entry {
    admitted(output.statement().clone(), workflow("todo"), None, None).expect("released output")
}

fn named_output(name: &str, parent: &str, controller: &str) -> Entry {
    named_output_with(name, parent, controller, Vec::new(), false)
}

fn named_output_with(
    name: &str,
    parent: &str,
    controller: &str,
    dependencies: Vec<TaskStamp>,
    claimed: bool,
) -> Entry {
    let parent = stamp(parent);
    let mut head = statement(name);
    head.preparation = Some(wire::TaskPreparation {
        parent,
        controller: stamp(controller),
        construction: head.born.clone(),
    });
    head.dependencies = dependencies;
    admitted(
        head,
        workflow(if claimed { "todo" } else { "draft" }),
        claimed.then(|| claim(ClaimDisposition::Active)),
        claimed.then(lease),
    )
    .expect("controlled output")
}

mod design_wait;
mod late;
mod work_wait;
