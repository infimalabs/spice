use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};

use bus::{Bus, CommitOutcome};
use journal::crossing::{
    Actor, CanonicalUuid, ClaimCell, ClaimDisposition, ClaimHead, Command, CommandKind, Digest,
    EnvironmentRequirements, Origin, Phase, Producer, Seq, TaskHandle, TaskHead, Topic, TrustHead,
    TrustState, WireVersion, Witness, WorkflowHead, WorkflowStatus, body, intent,
};
use journal::{FactCause, GuardError, Journal, JournalGuard, JournalWrite};

const COMMAND_ONE: &str = "018f82d4-6f59-7d06-92e3-bce9131163c4";
const COMMAND_TWO: &str = "018f82d4-6f59-7d06-92e3-bce9131163c5";
/// How many retained tasks make a fleet whose snapshot spans several pages.
const RETAINED_FACTS: usize = 300;
const INITIAL_SEQUENCE: u64 = Seq::FIRST.get();

#[test]
fn steering_presentation_batches_refuse_before_crossing_the_fixed_key_ceiling() {
    let bus = Bus::start(Journal::in_memory().expect("in-memory authority"))
        .expect("dedicated writer starts");
    let admitted = bus
        .reader()
        .steering_presentations(vec![
            "unresolved".to_owned();
            bus::MAX_STEERING_PRESENTATION_KEYS
        ])
        .expect("the exact presentation ceiling is admitted");
    assert!(admitted.is_empty());
    assert!(
        bus.reader()
            .steering_presentations(vec![
                "unresolved".to_owned();
                bus::MAX_STEERING_PRESENTATION_KEYS + 1
            ])
            .is_err()
    );
    bus.shutdown().expect("writer shuts down");
}

#[test]
fn one_writer_commits_then_broadcasts_monotone_cursors() {
    let caller = thread::current().id();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let bus = Bus::start(Journal::in_memory().expect("in-memory authority"))
        .expect("dedicated writer starts");
    let commits = bus.subscribe().expect("subscription registers");
    let empty = bus
        .task_snapshot()
        .expect("empty task snapshot initializes");
    assert!(empty.facts().is_empty());
    assert!(empty.tasks().is_empty());

    let first = committed(
        bus.commit(initial_write(uuid(COMMAND_ONE), Arc::clone(&observed)))
            .expect("writer answers"),
    );
    assert_eq!(
        (first.first(), first.last()),
        (INITIAL_SEQUENCE, INITIAL_SEQUENCE)
    );
    assert_eq!(
        first.cursor().position().map(Seq::get),
        Some(INITIAL_SEQUENCE)
    );
    assert_eq!(commits.recv().expect("post-durability signal"), first);
    let tasks = bus.task_snapshot().expect("incremental task head reads");
    assert_eq!(tasks.facts().len(), 1);
    assert_eq!(tasks.tasks().len(), 1);
    assert_eq!(tasks.tasks()[0].statement().body().born, stamp());
    assert_eq!(
        bus.transact(|journal| bus::JournalTransaction::settled(
            journal.fact_snapshot().expect("journal snapshot")
        ))
        .expect("durable snapshot")
        .facts()
        .len(),
        1
    );

    let second = committed(
        bus.commit(note_write(
            uuid(COMMAND_TWO),
            INITIAL_SEQUENCE,
            Arc::clone(&observed),
        ))
        .expect("writer answers"),
    );
    assert_eq!(
        (second.first(), second.last()),
        (INITIAL_SEQUENCE + 1, INITIAL_SEQUENCE + 1)
    );
    assert!(second.cursor() > first.cursor());
    assert_eq!(commits.recv().expect("second durable signal"), second);

    let verification = bus.verify().expect("authority verifies");
    assert_eq!(verification.facts, 2);
    assert_eq!(verification.receipts, 2);
    assert_eq!(verification.heads, 4, "task heads plus its topic node");
    assert_eq!(verification.threads, 2);

    let writer_threads = observed.lock().expect("thread observations");
    assert_eq!(writer_threads.len(), 2);
    assert!(writer_threads.iter().all(|thread| *thread != caller));
    assert!(
        writer_threads
            .iter()
            .all(|thread| *thread == writer_threads[0])
    );
    drop(writer_threads);
    bus.shutdown().expect("writer shuts down");
}

#[test]
fn trust_only_commits_refresh_immutable_task_snapshots_at_the_commit_cursor() {
    let bus = Bus::start(Journal::in_memory().expect("in-memory authority"))
        .expect("dedicated writer starts");
    let digest: Digest = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        .parse()
        .expect("fixture digest");
    let mut pended = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "repo:genesis".parse().expect("origin"),
        instant("2030-04-12T18:44:09.271Z"),
        Producer::CommandDoor,
    );
    let fact = pended
        .fact(
            body::TrustPended {
                surface: "spice".to_owned(),
                digest: digest.clone(),
                worktree: Some("/producer/worktree".to_owned()),
                previous: None,
                documents: None,
            },
            FactCause::Root,
        )
        .expect("trust pend fact builds");
    let head_digest = digest.clone();
    pended.trust_head(fact, move |_| TrustHead {
        digest: head_digest,
        state: TrustState::Pended,
        worktree: Some("/producer/worktree".to_owned()),
    });
    let task = journal::crossing::TaskStamp::for_micros(999).expect("trust task stamp");
    let task_phase = phase("todo");
    let added = pended
        .fact(
            retained_added(task.clone(), 999, task_phase),
            FactCause::Root,
        )
        .expect("trust task fact builds");
    let head_task = task.clone();
    let required = digest.clone();
    pended.task_head(added, move |seq| {
        let mut head = retained_task_head(head_task, 999, seq);
        head.requirements = EnvironmentRequirements::one(
            journal::crossing::EnvironmentRequirement::trust_grant(required),
        );
        head
    });
    pended.workflow_head(task, added, move |_| WorkflowHead {
        flow: vec![Phase::Draft, task_phase],
        phase: task_phase,
        status: WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    append_trust_fleet(&mut pended);
    let first = committed(bus.commit(pended).expect("trust pend commits"));
    let before = bus.task_snapshot().expect("pended snapshot reads");
    assert_eq!(before.cursor(), first.cursor().position());
    assert_eq!(
        before.trust().len(),
        1,
        "retained exact-digest authority does not grow the live task snapshot"
    );
    let before_target = trust_head(&before, &digest);
    assert_eq!(before_target.body().state, TrustState::Pended);
    assert_eq!(before.facts().latest_trust_digest("spice"), Some(&digest));
    let requirements = EnvironmentRequirements::one(
        journal::crossing::EnvironmentRequirement::trust_grant(digest.clone()),
    );
    assert!(!requirements.are_satisfied_by(&before.environment()));

    let mut granted = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "repo:genesis".parse().expect("origin"),
        instant("2030-04-12T18:44:10.271Z"),
        Producer::CommandDoor,
    );
    let fact = granted
        .fact(
            body::TrustGranted {
                surface: "spice".to_owned(),
                digest: digest.clone(),
            },
            FactCause::Existing(before_target.position()),
        )
        .expect("trust grant fact builds");
    let worktree = before_target.body().worktree.clone();
    let granted_digest = digest.clone();
    granted.trust_head(fact, move |_| TrustHead {
        digest: granted_digest,
        state: TrustState::Granted,
        worktree,
    });
    let second = committed(bus.commit(granted).expect("trust grant commits"));
    let after = bus.task_snapshot().expect("granted snapshot reads");
    assert_eq!(after.cursor(), second.cursor().position());
    assert_eq!(
        after.task_head_update_visits(),
        0,
        "trust-only authority movement reads and copies no task head"
    );
    let after_target = trust_head(&after, &digest);
    assert_eq!(after_target.body().state, TrustState::Granted);
    assert_eq!(after.facts().latest_trust_digest("spice"), Some(&digest));
    assert!(requirements.are_satisfied_by(&after.environment()));
    assert_eq!(
        before_target.body().state,
        TrustState::Pended,
        "the earlier snapshot remains immutable"
    );
    bus.shutdown().expect("writer shuts down");
}

#[test]
fn multi_page_task_snapshots_resume_then_decode_only_the_committed_tail() {
    let (bus, last) = retained_fleet(RETAINED_FACTS);
    let cold = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("cold fact snapshot reads");
    let before = bus.task_snapshot().expect("multi-page snapshot settles");
    let retained = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("retained fact snapshot reads");
    assert_eq!(retained, cold);
    assert_eq!(before.facts().len(), RETAINED_FACTS);
    assert_eq!(before.tasks().len(), RETAINED_FACTS);
    assert!(
        before.materialization_steps() > 1,
        "retained history was drained inside one materialization step"
    );
    let mut tail = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:BUS-a1".parse().expect("origin"),
        instant("2030-04-12T18:44:11.271Z"),
        Producer::ResidentRecognition,
    );
    tail.fact(
        body::JournalBacked {
            bundle: "one proportional tail".to_owned(),
            manifest: Vec::new(),
        },
        FactCause::Existing(last),
    )
    .expect("tail fact builds");
    committed(bus.commit(tail).expect("tail commit answers"));

    let after = bus.task_snapshot().expect("advanced snapshot reads");
    let current_facts = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("advanced fact snapshot reads");
    assert_eq!(current_facts.horizon(), after.cursor());
    assert_eq!(
        current_facts.facts().iter().collect::<Vec<_>>(),
        after.facts().iter().collect::<Vec<_>>()
    );
    assert_eq!(retained.horizon(), before.cursor());
    assert_eq!(retained.facts().len(), RETAINED_FACTS);
    assert_eq!(after.facts().len(), RETAINED_FACTS + 1);
    assert_eq!(
        after.facts().horizon().map(Seq::get),
        Some(INITIAL_SEQUENCE + RETAINED_FACTS as u64)
    );
    assert_eq!(
        after.materialization_steps(),
        3,
        "one fact page, the one moved task bundle, and one trust-head page complete the generation"
    );
    let committed_tail = after.facts().after(before.cursor()).collect::<Vec<_>>();
    assert_eq!(
        committed_tail.len(),
        1,
        "a caught-up lifecycle consumer touches only the one-fact suffix"
    );
    assert_eq!(
        committed_tail[0].seq.get(),
        INITIAL_SEQUENCE + RETAINED_FACTS as u64
    );
    assert_shared_fact_prefix(&before, &after);
    assert!(
        std::ptr::eq(
            before
                .facts()
                .at(Seq::new(INITIAL_SEQUENCE).expect("first"))
                .expect("first fact is retained"),
            after
                .facts()
                .at(Seq::new(INITIAL_SEQUENCE).expect("first"))
                .expect("first fact stays retained"),
        ),
        "a position inside the shared prefix is answered from the shared chunk"
    );
    assert!(
        std::ptr::eq(
            after.facts().at(last).expect("last bulk fact"),
            before
                .facts()
                .iter()
                .last()
                .expect("bulk history is nonempty"),
        ),
        "the last position of a shared chunk is answered from that chunk"
    );
    assert_eq!(
        after
            .facts()
            .at(committed_tail[0].seq)
            .and_then(|fact| fact.fact.read::<body::JournalBacked>())
            .map(|backup| backup.bundle),
        Some("one proportional tail".to_owned()),
        "a position in the newest chunk is answered from that chunk"
    );
    assert!(
        after
            .facts()
            .at(Seq::new(INITIAL_SEQUENCE + RETAINED_FACTS as u64 + 1).expect("beyond"),)
            .is_none(),
        "a position past the horizon is not retained"
    );
    bus.shutdown().expect("writer shuts down");
}

/// A bus over one initial task and a fleet of retained tasks behind it, with
/// the position of the fleet's last fact.
fn retained_fleet(retained: usize) -> (Bus, Seq) {
    let mut journal = Journal::in_memory().expect("in-memory authority");
    journal
        .apply(initial_write(
            uuid(COMMAND_ONE),
            Arc::new(Mutex::new(Vec::new())),
        ))
        .expect("initial task commits");
    let mut bulk = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:BUS-a1".parse().expect("origin"),
        instant("2030-04-12T18:44:10.271Z"),
        Producer::CommandDoor,
    );
    let mut cause = FactCause::Existing(Seq::new(INITIAL_SEQUENCE).expect("initial cause"));
    for index in 1..retained {
        cause = FactCause::Pending(append_retained_task(&mut bulk, index, cause));
    }
    let applied = journal.apply(bulk).expect("fleet history commits");
    assert_eq!(applied.last().get(), INITIAL_SEQUENCE + retained as u64 - 1);
    let bus = Bus::start(journal).expect("dedicated writer starts");
    (bus, applied.last())
}

#[test]
fn allocation_work_and_one_head_update_ignore_unrelated_terminal_history() {
    const TERMINAL_TASKS: usize = 1_301;
    let observe = |terminal_tasks: usize| {
        let mut journal = Journal::in_memory().expect("in-memory authority");
        let initial = journal
            .apply(initial_write(
                uuid(COMMAND_ONE),
                Arc::new(Mutex::new(Vec::new())),
            ))
            .expect("live task commits");
        if terminal_tasks > 0 {
            let mut bulk = JournalWrite::new(
                Actor::TEST_PRINCIPAL,
                "task:BUS-a1".parse().expect("origin"),
                instant("2030-04-12T18:44:10.271Z"),
                Producer::CommandDoor,
            );
            let mut cause =
                FactCause::Pending(append_retained_dependency(&mut bulk, initial.last()));
            for index in 0..terminal_tasks {
                cause = FactCause::Pending(append_retained_terminal(&mut bulk, index, cause));
            }
            journal.apply(bulk).expect("terminal history commits");
        } else {
            let mut dependency = JournalWrite::new(
                Actor::TEST_PRINCIPAL,
                "task:BUS-a1".parse().expect("origin"),
                instant("2030-04-12T18:44:10.271Z"),
                Producer::CommandDoor,
            );
            append_retained_dependency(&mut dependency, initial.last());
            journal
                .apply(dependency)
                .expect("dependency-bearing task commits");
        }
        let bus = Bus::start(journal).expect("dedicated writer starts");
        let snapshot = bus.task_snapshot().expect("indexed snapshot reads");
        let allocation = snapshot.allocation_read();
        for heads in allocation.frontier() {
            for dependency in &heads.statement().body().dependencies {
                let _ = allocation.dependency_standing(dependency);
            }
        }
        let work = allocation.work();
        let retained_heads = snapshot.tasks().len();

        let mut growth = JournalWrite::new(
            Actor::TEST_PRINCIPAL,
            "task:BUS-a1".parse().expect("origin"),
            instant("2030-04-12T18:44:12.271Z"),
            Producer::CommandDoor,
        );
        append_retained_task(
            &mut growth,
            10_000,
            FactCause::Existing(snapshot.cursor().expect("snapshot cursor")),
        );
        committed(bus.commit(growth).expect("one task head changes"));
        let advanced = bus.task_snapshot().expect("delta snapshot reads");
        let update_visits = advanced.task_head_update_visits();
        bus.shutdown().expect("writer shuts down");
        (work, retained_heads, update_visits)
    };

    let baseline = observe(0);
    let retained = observe(TERMINAL_TASKS);
    assert_eq!(
        retained.0, baseline.0,
        "the instrumented next-work traversal depends only on its fixed live graph"
    );
    assert_eq!(baseline.0.task_visits, 2);
    assert_eq!(baseline.0.standing_lookups, 1);
    assert_eq!(baseline.1, 2);
    assert_eq!(retained.1, TERMINAL_TASKS + 2);
    assert_eq!(
        (baseline.2, retained.2),
        (1, 1),
        "one changed head stays one visited head despite retained terminal history"
    );
}

#[test]
fn complete_head_materialization_replays_delta_updates_for_snapshot_clones() {
    const UPDATES: usize = 70;
    let bus = Bus::start(Journal::in_memory().expect("in-memory authority"))
        .expect("dedicated writer starts");
    committed(
        bus.commit(initial_write(
            uuid(COMMAND_ONE),
            Arc::new(Mutex::new(Vec::new())),
        ))
        .expect("initial task commits"),
    );
    let initial = bus.task_snapshot().expect("initial snapshot reads");
    let mut current = initial.tasks()[0].statement().body().clone();
    for revision in 1..=UPDATES {
        let snapshot = bus.task_snapshot().expect("prior generation reads");
        let mut write = JournalWrite::new(
            Actor::TEST_PRINCIPAL,
            "task:BUS-a1".parse().expect("origin"),
            instant("2030-04-12T18:44:11.271Z"),
            Producer::CommandDoor,
        );
        let edited = write
            .fact(
                body::TaskEdited {
                    intent: None,
                    task: current.born.clone(),
                    revision: u32::try_from(revision).expect("fixture revision"),
                    dependencies: None,
                    flow: None,
                },
                FactCause::Existing(snapshot.cursor().expect("prior cursor")),
            )
            .expect("task edit builds");
        current.title = format!("Bounded revision {revision}");
        let next = current.clone();
        write.task_head(edited, move |position| TaskHead {
            revision: position,
            ..next
        });
        committed(bus.commit(write).expect("task edit commits"));
    }

    let snapshot = bus.task_snapshot().expect("delta generation reads");
    let cloned = snapshot.clone();
    assert_eq!(
        snapshot.tasks()[0].statement().body().title,
        format!("Bounded revision {UPDATES}")
    );
    assert_eq!(cloned.tasks(), snapshot.tasks());
    bus.shutdown().expect("writer shuts down");
}

#[test]
fn terminal_workflow_with_an_active_claim_is_an_allocation_fault() {
    let mut journal = Journal::in_memory().expect("in-memory authority");
    let task = journal::crossing::TaskStamp::for_micros(30_000).expect("terminal task stamp");
    let task_phase = phase("todo");
    let lane = uuid("018f82d4-6f59-7d06-92e3-bce9131163d1");
    let occupant = uuid("018f82d4-6f59-7d06-92e3-bce9131163d2");
    let mut write = bare_write();
    let added = write
        .fact(retained_added(task.clone(), 0, task_phase), FactCause::Root)
        .expect("task addition builds");
    let head_task = task.clone();
    write.task_head(added, move |seq| retained_task_head(head_task, 0, seq));
    let done = write
        .fact(
            body::TaskDone {
                task: task.clone(),
                validation: "malformed terminal possession fixture".to_owned(),
                disposition: None,
                terminal: None,
                git: None,
                source: body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Pending(added),
        )
        .expect("completion builds");
    write.workflow_head(task.clone(), done, move |_| WorkflowHead {
        flow: vec![Phase::Draft, task_phase],
        phase: task_phase,
        status: WorkflowStatus::completed(instant("2030-04-12T18:44:10.271Z")),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    let won = write
        .fact(
            body::ClaimWon {
                task: task.clone(),
                actor: Actor::TEST_PRINCIPAL,
                lane,
                occupant,
                epoch: uuid(COMMAND_ONE),
                cell: ClaimCell {
                    topic: Some("/bus".parse().expect("fixture topic")),
                    phase: task_phase,
                },
                until: instant("2030-04-12T18:54:10.271Z"),
                git: None,
                recovery: None,
                source: body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Pending(done),
        )
        .expect("claim win builds");
    write.claim_head(task.clone(), won, move |seq| ClaimHead {
        actor: Actor::TEST_PRINCIPAL,
        fact: seq,
        claimed: seq,
        cell: ClaimCell {
            topic: Some("/bus".parse().expect("fixture topic")),
            phase: task_phase,
        },
        lane,
        occupant,
        epoch: uuid(COMMAND_ONE),
        recovery: None,
        disposition: ClaimDisposition::Active,
        acknowledged: 0,
    });
    journal.apply(write).expect("malformed authority commits");
    let bus = Bus::start(journal).expect("dedicated writer starts");

    let snapshot = bus.task_snapshot().expect("snapshot remains inspectable");
    assert!(matches!(
        snapshot.allocation_fault(),
        Some(bus::AllocationFault::ActiveTerminal { task: observed }) if observed == &task
    ));
    assert_eq!(
        snapshot.allocation_frontier().count(),
        0,
        "terminal possession is never returned as live work"
    );
    bus.shutdown().expect("writer shuts down");
}

#[test]
fn a_new_task_among_a_retained_fleet_costs_one_moved_bundle() {
    let (bus, last) = retained_fleet(RETAINED_FACTS);
    let settled = bus.task_snapshot().expect("multi-page snapshot settles");
    assert_eq!(settled.tasks().len(), RETAINED_FACTS);
    let mut growth = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:BUS-a1".parse().expect("origin"),
        instant("2030-04-12T18:44:12.271Z"),
        Producer::CommandDoor,
    );
    append_retained_task(&mut growth, 0, FactCause::Existing(last));
    committed(bus.commit(growth).expect("a new task commits"));
    let grown = bus.task_snapshot().expect("grown snapshot reads");
    assert_eq!(
        grown.materialization_steps(),
        3,
        "a new task among hundreds costs one fact page, one moved bundle, and one trust page"
    );
    assert_eq!(grown.tasks().len(), RETAINED_FACTS + 1);
    let stamps = grown
        .tasks()
        .iter()
        .map(|heads| heads.statement().body().born.clone())
        .collect::<Vec<_>>();
    let mut ordered = stamps.clone();
    ordered.sort();
    assert_eq!(stamps, ordered, "the grown snapshot keeps stamp order");
    assert_eq!(
        stamps[0],
        journal::crossing::TaskStamp::for_micros(1_000).expect("newborn stamp"),
        "the newborn takes its place ahead of the fleet it sorts before"
    );
    bus.shutdown().expect("writer shuts down");
}

#[test]
fn refusals_and_faults_leave_every_durable_effect_and_signal_absent() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let bus = Bus::start(Journal::in_memory().expect("in-memory authority"))
        .expect("dedicated writer starts");
    let commits = bus.subscribe().expect("subscription registers");

    committed(
        bus.commit(initial_write(uuid(COMMAND_ONE), Arc::clone(&observed)))
            .expect("first commit answers"),
    );
    commits.recv().expect("first signal");

    let mut refused = note_write(uuid(COMMAND_TWO), INITIAL_SEQUENCE, Arc::clone(&observed));
    refused.guard(JournalGuard::new(|read| {
        if read.cursor()?.is_none() {
            Ok(())
        } else {
            Err(GuardError::refused("expected an empty authority"))
        }
    }));
    match bus.commit(refused).expect("refusal answers") {
        CommitOutcome::Refused(refusal) => {
            assert_eq!(refusal.reason(), "expected an empty authority");
        }
        other => panic!("expected refusal, found {other:?}"),
    }
    assert!(matches!(
        commits.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    assert_eq!(
        bus.transact(|journal| bus::JournalTransaction::settled(
            journal.fact_snapshot().expect("journal snapshot")
        ))
        .expect("snapshot after refusal")
        .facts()
        .len(),
        1
    );

    match bus
        .commit(cross_origin_write())
        .expect("causal refusal answers")
    {
        CommitOutcome::Refused(refusal) => {
            assert_eq!(
                refusal.reason(),
                "a fact cause cannot cross origin boundaries"
            );
        }
        other => panic!("expected causal refusal, found {other:?}"),
    }
    assert!(matches!(
        commits.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    assert_eq!(
        bus.transact(|journal| bus::JournalTransaction::settled(
            journal.fact_snapshot().expect("journal snapshot")
        ))
        .expect("snapshot after causal refusal")
        .facts()
        .len(),
        1
    );

    let faulted = note_write(uuid(COMMAND_ONE), INITIAL_SEQUENCE, Arc::clone(&observed));
    match bus.commit(faulted).expect("fault answers") {
        CommitOutcome::Fault(_) => {}
        other => panic!("expected fault, found {other:?}"),
    }
    assert!(matches!(
        commits.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    assert_eq!(
        bus.transact(|journal| bus::JournalTransaction::settled(
            journal.fact_snapshot().expect("journal snapshot")
        ))
        .expect("snapshot after fault")
        .facts()
        .len(),
        1
    );
    let verification = bus.verify().expect("rolled-back authority verifies");
    assert_eq!(verification.facts, 1);
    assert_eq!(verification.receipts, 1);
    assert_eq!(verification.heads, 4, "task heads plus its topic node");
    bus.shutdown().expect("writer shuts down");
}

#[test]
fn command_receipts_refuse_body_and_answer_shapes_from_another_binding() {
    let task = task();
    let mut write = bare_write();
    write
        .command_result(
            Command {
                v: WireVersion::CURRENT,
                id: uuid(COMMAND_ONE),
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
                kind: CommandKind::TaskAdd,
                expected: vec![Witness::Absence { task: stamp() }],
                body: intent::TaskNote {
                    task: task.clone(),
                    note: "not an add body".to_owned(),
                },
            },
            uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
            result(&task, "todo"),
        )
        .expect_err("task::add must refuse a task::note body");

    write
        .command_result(
            Command {
                v: WireVersion::CURRENT,
                id: uuid(COMMAND_ONE),
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
                kind: CommandKind::TaskAdd,
                expected: vec![Witness::Absence { task: stamp() }],
                body: intent::TaskAdd {
                    preparation: None,
                    intent: None,
                    title: "Implement bus writer".to_owned(),
                    pitch: "One ordered write must land atomically.".to_owned(),
                    topic: Some("/bus".parse().expect("fixture topic")),
                    description: None,
                    term: Vec::new(),
                    requirements: EnvironmentRequirements::default(),
                    origin: None,
                    flow: None,
                    depends: Vec::new(),
                    related: Vec::new(),
                    claim: false,
                    draft: false,
                },
            },
            uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
            BTreeMap::from([("unexpected".to_owned(), "answer".to_owned())]),
        )
        .expect_err("task::add must refuse another answer shape");
}

fn append_trust_fleet(write: &mut JournalWrite) {
    for index in 1..300_u16 {
        let digest: Digest = format!("{index:064x}").parse().expect("fleet digest");
        let fact = write
            .fact(
                body::TrustPended {
                    surface: format!("fleet-{index}"),
                    digest: digest.clone(),
                    worktree: Some("/producer/worktree".to_owned()),
                    previous: None,
                    documents: None,
                },
                FactCause::Root,
            )
            .expect("fleet trust fact builds");
        write.trust_head(fact, move |_| TrustHead {
            digest,
            state: TrustState::Pended,
            worktree: Some("/producer/worktree".to_owned()),
        });
    }
}

fn trust_head<'a>(
    snapshot: &'a bus::TaskSnapshot,
    digest: &Digest,
) -> &'a journal::JournalHead<TrustHead> {
    snapshot
        .trust()
        .iter()
        .find(|head| &head.body().digest == digest)
        .expect("target trust head remains present")
}

fn assert_shared_fact_prefix(before: &bus::TaskSnapshot, after: &bus::TaskSnapshot) {
    assert!(
        std::ptr::eq(
            before.facts().iter().next().expect("history is nonempty"),
            after
                .facts()
                .iter()
                .next()
                .expect("history remains nonempty")
        ),
        "the one-fact commit rebuilt rather than shared retained facts"
    );
    assert_eq!(
        after
            .facts()
            .iter()
            .last()
            .and_then(|fact| fact.fact.read::<body::JournalBacked>())
            .map(|backup| backup.bundle),
        Some("one proportional tail".to_owned())
    );
}

fn append_retained_task(
    write: &mut JournalWrite,
    index: usize,
    cause: FactCause,
) -> journal::PendingFact {
    let task = journal::crossing::TaskStamp::for_micros(
        i64::try_from(index + 1_000).expect("fleet moment"),
    )
    .expect("fleet stamp");
    let task_phase = phase("todo");
    let pending = write
        .fact(retained_added(task.clone(), index, task_phase), cause)
        .expect("fleet fact builds");
    let head_task = task.clone();
    write.task_head(pending, move |seq| {
        retained_task_head(head_task, index, seq)
    });
    write.workflow_head(task, pending, move |_| WorkflowHead {
        flow: vec![Phase::Draft, task_phase],
        phase: task_phase,
        status: WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    pending
}

fn append_retained_dependency(write: &mut JournalWrite, cause: Seq) -> journal::PendingFact {
    let task = journal::crossing::TaskStamp::for_micros(15_000).expect("dependent task stamp");
    let missing =
        journal::crossing::TaskStamp::for_micros(19_000).expect("missing dependency stamp");
    let task_phase = phase("todo");
    let mut added = retained_added(task.clone(), 15_000, task_phase);
    added.depends = vec![missing.clone()];
    let pending = write
        .fact(added, FactCause::Existing(cause))
        .expect("dependent task addition builds");
    let head_task = task.clone();
    write.task_head(pending, move |seq| {
        let mut head = retained_task_head(head_task, 15_000, seq);
        head.dependencies = vec![missing];
        head
    });
    write.workflow_head(task, pending, move |_| WorkflowHead {
        flow: vec![Phase::Draft, task_phase],
        phase: task_phase,
        status: WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    pending
}

fn append_retained_terminal(
    write: &mut JournalWrite,
    index: usize,
    cause: FactCause,
) -> journal::PendingFact {
    let task = journal::crossing::TaskStamp::for_micros(
        i64::try_from(index + 20_000).expect("terminal fleet moment"),
    )
    .expect("terminal fleet stamp");
    let task_phase = phase("todo");
    let added = write
        .fact(retained_added(task.clone(), index, task_phase), cause)
        .expect("terminal task addition builds");
    let head_task = task.clone();
    write.task_head(added, move |seq| retained_task_head(head_task, index, seq));
    let done = write
        .fact(
            body::TaskDone {
                task: task.clone(),
                validation: "terminal fleet fixture".to_owned(),
                disposition: None,
                terminal: None,
                git: None,
                source: body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Pending(added),
        )
        .expect("terminal completion builds");
    write.workflow_head(task, done, move |_| WorkflowHead {
        flow: vec![Phase::Draft, task_phase],
        phase: task_phase,
        status: WorkflowStatus::completed(instant("2030-04-12T18:44:10.271Z")),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    done
}

fn retained_added(
    task: journal::crossing::TaskStamp,
    index: usize,
    phase: Phase,
) -> body::TaskAdded {
    body::TaskAdded {
        preparation: None,
        intent: None,
        task,
        title: format!("Retained task {index}"),
        pitch: "Retained work must survive a restart.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: EnvironmentRequirements::default(),
        topic: "/bus".parse().expect("fixture topic"),
        submitted: false,
        priority: None,
        taste: None,
        depends: Vec::new(),
        flow: vec![Phase::Draft, phase],
        phase,
        revision: u32::try_from(index + 1).expect("fleet revision"),
        creation: body::TaskCreationContext::unavailable(
            body::TaskCreationUnavailable::PrincipalActor,
        ),
    }
}

fn retained_task_head(task: journal::crossing::TaskStamp, index: usize, seq: Seq) -> TaskHead {
    TaskHead {
        preparation: None,
        intent: None,
        born: task,
        topic_origin: seq,
        title: format!("Retained task {index}"),
        pitch: "Retained work must survive a restart.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: EnvironmentRequirements::default(),
        resources: Default::default(),
        topic: "/bus".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: journal::crossing::TaskProvenance::OperatorDraft,
        creation: body::TaskCreationContext::unavailable(
            body::TaskCreationUnavailable::PrincipalActor,
        ),
        revision: seq,
    }
}

fn initial_write(command_id: CanonicalUuid, observed: Arc<Mutex<Vec<ThreadId>>>) -> JournalWrite {
    let task = task();
    let at = instant("2030-04-12T18:44:09.271Z");
    let origin: Origin = "task:BUS-a1".parse().expect("origin");
    let phase = phase("todo");
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    write.guard(JournalGuard::new(move |read| {
        observed
            .lock()
            .expect("thread observations")
            .push(thread::current().id());
        if read.cursor()?.is_some() {
            return Err(GuardError::refused(
                "task creation requires an empty fixture",
            ));
        }
        Ok(())
    }));
    let fact = write
        .fact(
            body::TaskAdded {
                preparation: None,
                intent: None,
                task: stamp(),
                title: "Implement bus writer".to_owned(),
                pitch: "One ordered write must land atomically.".to_owned(),
                description: Some("Persist one ordered write".to_owned()),
                term: vec!["The journal commits atomically".to_owned()],
                requirements: EnvironmentRequirements::default(),
                topic: "/bus".parse().expect("fixture topic"),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: vec![Phase::Draft, phase],
                phase,
                revision: 1,
                creation: body::TaskCreationContext::unavailable(
                    body::TaskCreationUnavailable::PrincipalActor,
                ),
            },
            FactCause::Root,
        )
        .expect("registered command-door fact");

    let task_for_head = stamp();
    write.task_head(fact, move |seq| TaskHead {
        preparation: None,
        intent: None,
        born: task_for_head,
        topic_origin: seq,
        title: "Implement bus writer".to_owned(),
        pitch: "One ordered write must land atomically.".to_owned(),
        description: Some("Persist one ordered write".to_owned()),
        term: vec!["The journal commits atomically".to_owned()],
        requirements: EnvironmentRequirements::default(),
        resources: Default::default(),
        topic: "/bus".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: journal::crossing::TaskProvenance::Task(stamp()),
        creation: body::TaskCreationContext::unavailable(
            body::TaskCreationUnavailable::PrincipalActor,
        ),
        revision: seq,
    });
    let phase_for_head = phase;
    write.workflow_head(stamp(), fact, move |_| WorkflowHead {
        flow: vec![Phase::Draft, phase_for_head],
        phase: phase_for_head,
        status: WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });

    write
        .command_result(
            Command {
                v: WireVersion::CURRENT,
                id: command_id,
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
                kind: CommandKind::TaskAdd,
                expected: vec![Witness::Absence { task: stamp() }],
                body: intent::TaskAdd {
                    preparation: None,
                    intent: None,
                    title: "Implement bus writer".to_owned(),
                    pitch: "One ordered write must land atomically.".to_owned(),
                    topic: Some("/bus".parse().expect("fixture topic")),
                    description: None,
                    term: Vec::new(),
                    requirements: EnvironmentRequirements::default(),
                    origin: Some(origin),
                    flow: None,
                    depends: Vec::new(),
                    related: Vec::new(),
                    claim: false,
                    draft: false,
                },
            },
            uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
            result(&task, "todo"),
        )
        .expect("canonical command result");
    write
}

#[test]
fn task_snapshot_indexes_the_newest_claim_per_actor_and_each_occupants_context_anchor() {
    let bus = Bus::start(Journal::in_memory().expect("in-memory authority"))
        .expect("dedicated writer starts");
    let lane = uuid("018f82d4-6f59-7d06-92e3-bce9131163d1");
    let first = uuid("018f82d4-6f59-7d06-92e3-bce9131163d2");
    let successor = uuid("018f82d4-6f59-7d06-92e3-bce9131163d3");

    let mut write = recognition_write();
    write
        .fact(
            body::LaneStarted {
                lane,
                occupant: first,
                epoch: uuid(COMMAND_ONE),
                group: 7,
                fresh_session: true,
            },
            FactCause::Root,
        )
        .expect("lane start fact");
    write
        .fact(
            body::LaneRenewed {
                lane,
                occupant: successor,
                ancestor: first,
                epoch: uuid(COMMAND_ONE),
                group: 8,
            },
            FactCause::Root,
        )
        .expect("lane renewal fact");
    let landed = committed(bus.commit(write).expect("writer answers"));
    assert_commit_range(&landed, INITIAL_SEQUENCE, INITIAL_SEQUENCE + 1);

    let mut write = bare_write();
    claim_won(&mut write, "/bus/first", phase("todo"), lane, first);
    claim_won(&mut write, "/bus/second", phase("todo"), lane, first);
    claim_won(&mut write, "/bus/third", phase("review"), lane, successor);
    let landed = committed(bus.commit(write).expect("writer answers"));
    assert_commit_range(&landed, INITIAL_SEQUENCE + 2, INITIAL_SEQUENCE + 4);

    let snapshot = bus.task_snapshot().expect("task snapshot reads");
    let facts = snapshot.facts();
    assert_eq!(
        facts.latest_claim(&Actor::TEST_PRINCIPAL),
        Some((
            Seq::new(INITIAL_SEQUENCE + 4).expect("position"),
            ClaimCell {
                topic: Some("/bus/third".parse().expect("fixture topic")),
                phase: phase("review"),
            }
        )),
        "the newest win wins, whatever cells the older ones carried"
    );
    assert_eq!(facts.latest_claim(&Actor::Spice), None);
    assert_eq!(
        facts.context_anchor(lane, first),
        Seq::new(INITIAL_SEQUENCE)
    );
    assert_eq!(
        facts.context_anchor(lane, successor),
        Seq::new(INITIAL_SEQUENCE + 1)
    );
    assert_eq!(facts.context_anchor(first, lane), None);

    let mut write = recognition_write();
    write
        .fact(
            body::LaneCompacted {
                lane,
                occupant: successor,
                stream: "session".to_owned(),
                member: 1,
                offset: 0,
                length: 1,
            },
            FactCause::Root,
        )
        .expect("lane compaction fact");
    let landed = committed(bus.commit(write).expect("writer answers"));
    assert_commit_range(&landed, INITIAL_SEQUENCE + 5, INITIAL_SEQUENCE + 5);
    let mut write = bare_write();
    claim_won(&mut write, "/bus/fourth", phase("todo"), lane, successor);
    let landed = committed(bus.commit(write).expect("writer answers"));
    assert_commit_range(&landed, INITIAL_SEQUENCE + 6, INITIAL_SEQUENCE + 6);

    let snapshot = bus.task_snapshot().expect("task snapshot advances");
    let facts = snapshot.facts();
    assert_eq!(
        facts.context_anchor(lane, first),
        Seq::new(INITIAL_SEQUENCE)
    );
    assert_eq!(
        facts.context_anchor(lane, successor),
        Seq::new(INITIAL_SEQUENCE + 5)
    );
    assert_eq!(
        facts.latest_claim(&Actor::TEST_PRINCIPAL),
        Some((
            Seq::new(INITIAL_SEQUENCE + 6).expect("position"),
            ClaimCell {
                topic: Some("/bus/fourth".parse().expect("fixture topic")),
                phase: phase("todo"),
            }
        )),
        "a later commit's win supersedes the cell before it"
    );
    bus.shutdown().expect("writer shuts down");
}

fn recognition_write() -> JournalWrite {
    JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:BUS-a1".parse().expect("origin"),
        instant("2030-04-12T18:44:09.271Z"),
        Producer::ResidentRecognition,
    )
}

fn claim_won(
    write: &mut JournalWrite,
    topic: &str,
    phase: Phase,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) {
    write
        .fact(
            body::ClaimWon {
                task: stamp(),
                actor: Actor::TEST_PRINCIPAL,
                lane,
                occupant,
                epoch: uuid(COMMAND_ONE),
                cell: ClaimCell {
                    topic: Some(topic.parse().expect("fixture topic")),
                    phase,
                },
                until: instant("2030-04-12T18:54:10.271Z"),
                git: None,
                recovery: None,
                source: body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Root,
        )
        .expect("claim win fact");
}

fn note_write(
    command_id: CanonicalUuid,
    expected_cursor: u64,
    observed: Arc<Mutex<Vec<ThreadId>>>,
) -> JournalWrite {
    let task = task();
    let at = instant("2030-04-12T18:44:10.271Z");
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:BUS-a1".parse().expect("origin"),
        at,
        Producer::CommandDoor,
    );
    let guarded_task = stamp();
    write.guard(JournalGuard::new(move |read| {
        observed
            .lock()
            .expect("thread observations")
            .push(thread::current().id());
        let expected = Seq::new(expected_cursor).expect("expected cursor");
        let task_head = read.task_head(&guarded_task)?;
        let workflow_head = read.workflow_head(&guarded_task)?;
        if read.cursor()? != Some(expected)
            || task_head.as_ref().map(journal::JournalHead::position) != Some(expected)
            || workflow_head.as_ref().map(journal::JournalHead::position) != Some(expected)
        {
            return Err(GuardError::refused("task note witnesses changed"));
        }
        Ok(())
    }));
    write
        .fact(
            body::TaskWoken { task: stamp() },
            FactCause::Existing(Seq::new(expected_cursor).expect("existing cause")),
        )
        .expect("registered command-door fact");
    write
        .command_result(
            Command {
                v: WireVersion::CURRENT,
                id: command_id,
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
                kind: CommandKind::TaskNote,
                expected: vec![Witness::Existence { task: stamp() }],
                body: intent::TaskNote {
                    task: task.clone(),
                    note: "writer observation".to_owned(),
                },
            },
            uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
            result(&task, "noted"),
        )
        .expect("canonical command result");
    write
}

fn cross_origin_write() -> JournalWrite {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:OTHER-a1".parse().expect("other origin"),
        instant("2030-04-12T18:44:10.271Z"),
        Producer::CommandDoor,
    );
    write
        .fact(
            body::TaskWoken { task: stamp() },
            FactCause::Existing(Seq::new(INITIAL_SEQUENCE).expect("existing cause")),
        )
        .expect("registered command-door fact");
    write
}

fn bare_write() -> JournalWrite {
    JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:BUS-a1".parse().expect("origin"),
        instant("2030-04-12T18:44:09.271Z"),
        Producer::CommandDoor,
    )
}

fn assert_commit_range(commit: &bus::JournalCommit, first: u64, last: u64) {
    assert_eq!((commit.first(), commit.last()), (first, last));
}

fn committed(outcome: CommitOutcome) -> bus::JournalCommit {
    match outcome {
        CommitOutcome::Committed(commit) => commit,
        other => panic!("expected commit, found {other:?}"),
    }
}

fn result(task: &TaskHandle, status: &str) -> intent::TaskAnswer {
    intent::TaskAnswer {
        placement: None,
        created: BTreeMap::new(),
        ownership: None,
        taste: None,
        completion_contract: None,
        task: task.clone(),
        status: status.to_owned(),
        completion: None,
        claim: None,
        closed: Vec::new(),
        next: None,
        diagnostic: None,
        duplicates: Vec::new(),
        duplicates_omitted: 0,
    }
}

fn task() -> TaskHandle {
    TaskHandle::for_topic(&"/bus".parse::<Topic>().expect("fixture topic"), &stamp())
}

fn stamp() -> journal::crossing::TaskStamp {
    "1kKRRsDPx".parse().expect("task stamp")
}

fn phase(value: &str) -> Phase {
    value.parse().expect("phase")
}

fn uuid(value: &str) -> CanonicalUuid {
    value.parse().expect("canonical uuid")
}

fn instant(value: &str) -> journal::crossing::Instant {
    value.parse().expect("canonical instant")
}
