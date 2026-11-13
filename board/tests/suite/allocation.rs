use super::support;

use board::Snapshot;
use board::allocate::{Mint, mint};
use journal::{FactCause, Journal, JournalWrite};
use wire::{
    Actor, CanonicalUuid, FactKind, Instant, LaneResidency, Origin, Phase, Producer, ResidencyHead,
    ResidentState, TaskHandle, TaskHead, TaskStamp, WorkflowHead, body,
};

fn uuid(last: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
        .parse()
        .expect("fixture uuid")
}

fn instant(minute: u8) -> Instant {
    format!("2026-08-01T06:{minute:02}:00.000Z")
        .parse()
        .expect("fixture instant")
}

fn phase(text: &str) -> Phase {
    text.parse().expect("fixture phase")
}

fn stamp(text: &str) -> TaskStamp {
    let value = text.bytes().fold(0_i64, |value, byte| {
        (value * 131 + i64::from(byte)) % 10_000_000_000_000
    });
    TaskStamp::for_micros(value).expect("fixture name fits the task stamp codec")
}

fn handle(text: &str) -> TaskHandle {
    TaskHandle::for_topic(
        &"/board/allocate"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &stamp(text),
    )
}

fn snapshot(journal: &Journal) -> Snapshot {
    support::bounded_snapshot(journal, &[stamp("BOARD-b2"), stamp("BOARD-a1")])
        .expect("fixture board entries")
}

fn workflow() -> WorkflowHead {
    WorkflowHead {
        flow: vec![phase("draft"), phase("todo"), phase("review")],
        phase: phase("todo"),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    }
}

fn residency() -> ResidencyHead {
    ResidencyHead {
        epoch: uuid(22),
        state: ResidentState::Seated,
        topology_revision: 0,
        lanes: vec![LaneResidency {
            lane: uuid(20),
            team: uuid(20),
            name: None,
            worktree: "/worktrees/lane-a".to_owned(),
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: Some(uuid(21)),
            lineage: Vec::new(),
            capabilities: Vec::new(),
            session: None,
            disposition: "occupied".to_owned(),
        }],
        subscriptions: vec![wire::TeamSubscription {
            team: uuid(20),
            formed_at: None,
            drive: true,
            drain: true,
            name: None,
            fast_mode: false,
        }],
        renewals: Vec::new(),
    }
}

fn fixture() -> Journal {
    let mut journal = Journal::in_memory().expect("in-memory allocation journal");
    let origin = "task:BOARD-root".parse::<Origin>().expect("fixture origin");
    let mut write = JournalWrite::new(
        Actor::Spice,
        origin.clone(),
        instant(0),
        Producer::CommandDoor,
    );
    let tasks = [
        ("BOARD-b2", wire::TaskPriority::Medium),
        ("BOARD-a1", wire::TaskPriority::High),
    ];
    for (index, (name, priority)) in tasks.into_iter().enumerate() {
        let task = stamp(name);
        let flow = workflow();
        let fact = write
            .fact(
                body::TaskAdded {
                    preparation: None,
                    intent: None,
                    task: task.clone(),
                    title: format!("Allocate {name}"),
                    pitch: "Allocation must find this work.".to_owned(),
                    description: None,
                    term: Vec::new(),
                    requirements: wire::EnvironmentRequirements::default(),
                    topic: "/board/allocate".parse().expect("fixture topic"),
                    submitted: false,
                    priority: Some(priority),
                    taste: None,
                    depends: Vec::new(),
                    flow: flow.flow.clone(),
                    phase: flow.phase,
                    revision: 1,
                    creation: body::TaskCreationContext::unavailable(
                        body::TaskCreationUnavailable::SpiceActor,
                    ),
                },
                FactCause::Root,
            )
            .expect("registered task fact");
        let statement_task = task.clone();
        let statement_origin = wire::TaskProvenance::Task(task.clone());
        write.task_head(fact, move |position| TaskHead {
            preparation: None,
            intent: None,
            born: statement_task,
            topic_origin: position,
            title: format!("Allocate {name}"),
            pitch: "Allocation must find this work.".to_owned(),
            description: None,
            term: Vec::new(),
            requirements: wire::EnvironmentRequirements::default(),
            resources: wire::ResourceRequirements::default(),
            topic: "/board/allocate".parse().expect("fixture topic"),
            priority: Some(priority),
            taste: None,
            dependencies: Vec::new(),
            provenance: statement_origin,
            creation: body::TaskCreationContext::unavailable(
                body::TaskCreationUnavailable::SpiceActor,
            ),
            revision: position,
        });
        write.workflow_head(task, fact, move |_| flow);
        if index == 0 {
            write.residency_head(fact, |_| residency());
        }
    }
    journal.apply(write).expect("seed allocation authority");
    journal
}

/// A proven boundary carrying one accounted head.
///
/// Deliberately not an empty `Proven`: an empty one is byte-identical for every
/// lane, so a claim fact that dropped the caller's boundary entirely would
/// still compare equal to this fixture and the loss would not show.
fn source_boundary(lane: CanonicalUuid) -> body::SourceBoundary {
    body::SourceBoundary::Proven {
        heads: vec![wire::TranscriptSourceHead {
            lane,
            vendor: "omp".parse().expect("fixture vendor"),
            stream: "session".to_owned(),
            source: uuid(23),
            next: body::TranscriptPosition {
                member: 4,
                offset: 512,
            },
        }],
    }
}

/// Open a transaction on the fixture's task and mint one claim into it.
fn minted(
    journal: &mut Journal,
    git: Option<body::GitBoundary>,
    recovery: Option<String>,
    source: body::SourceBoundary,
) {
    let task = stamp("BOARD-a1");
    let actor = Actor::Lane(uuid(20));
    let mut write = JournalWrite::new(
        actor.clone(),
        "task:BOARD-a1".parse().expect("claim origin"),
        instant(5),
        Producer::CommandDoor,
    );
    mint(
        &mut write,
        Mint {
            task: &task,
            actor: &actor,
            workflow: &workflow(),
            topic: Some(&"/board/allocate".parse().expect("fixture topic")),
            lane: uuid(20),
            occupant: uuid(21),
            epoch: uuid(22),
            at: instant(5),
            expiry: instant(20),
            maximum: instant(35),
            git,
            recovery,
            source,
            cause: None,
        },
    )
    .expect("the claim mints");
    journal.apply(write).expect("the minted claim commits");
}

/// A boundary the caller supplies survives the minter it is handed to.
///
/// `git` is the field the two minting sites actually disagreed about: the
/// allocator hardcoded it empty while the resident threaded a real worktree
/// through, which is why it is the one worth proving reaches the journal from a
/// caller rather than from the writer's own defaults.
///
/// `source` is proven beside it for the same reason and one more: the minter is
/// handed an already-open transaction, so it cannot read a transcript boundary
/// itself and has no default to fall back to. Whatever the caller passes is the
/// only boundary the claim can ever open at.
#[test]
fn a_caller_supplied_boundary_reaches_the_claim() {
    let mut journal = fixture();
    let boundary = body::GitBoundary {
        head: "a-recorded-commit".to_owned(),
        integrated: "an-integrated-commit".to_owned(),
        upstream: Some("origin/main".to_owned()),
        upstream_head: Some("a-baseline-commit".to_owned()),
        local_commits: 2,
    };
    let recovery = "a".repeat(40);
    minted(
        &mut journal,
        Some(boundary),
        Some(recovery.clone()),
        source_boundary(uuid(20)),
    );

    let facts = journal.fact_snapshot().expect("claim fact stream");
    let claim = facts
        .facts()
        .iter()
        .find(|envelope| envelope.fact.kind() == FactKind::ClaimWon)
        .expect("minting writes a claim fact");
    let git = claim
        .fact
        .body()
        .get("git")
        .expect("the claim carries the boundary it was minted with");
    // Read back field by field rather than compared against the fixture struct,
    // so the assertion cannot pass by echoing its own input through the same
    // serializer that wrote it.
    assert_eq!(
        git.get("head").and_then(|value| value.as_str()),
        Some("a-recorded-commit")
    );
    assert_eq!(
        git.get("integrated").and_then(|value| value.as_str()),
        Some("an-integrated-commit")
    );
    assert_eq!(
        git.get("upstream").and_then(|value| value.as_str()),
        Some("origin/main")
    );
    assert_eq!(
        git.get("upstream_head").and_then(|value| value.as_str()),
        Some("a-baseline-commit")
    );
    assert_eq!(
        git.get("local_commits").and_then(|value| value.as_u64()),
        Some(2)
    );
    assert_eq!(
        claim
            .fact
            .body()
            .get("recovery")
            .and_then(|value| value.as_str()),
        Some(recovery.as_str()),
        "the claim fact binds the explicit dirty-handoff transaction",
    );

    let snapshot = snapshot(&journal);
    let entry = snapshot
        .entries()
        .iter()
        .find(|entry| entry.handle() == &handle("BOARD-a1"))
        .expect("the claimed task is on the board");
    assert_eq!(
        entry
            .latest_claim()
            .map(|held| held.claim())
            .and_then(|head| head.recovery.as_deref()),
        Some(recovery.as_str()),
        "restart-visible claim authority retains the recovery transaction",
    );

    let head = claim
        .fact
        .body()
        .get("source")
        .and_then(|source| source.get("proven"))
        .and_then(|proven| proven.get("heads"))
        .and_then(|heads| heads.get(0))
        .expect("the claim carries the accounted head it was minted with");
    assert_eq!(
        head.get("lane").and_then(|value| value.as_str()),
        Some(uuid(20).to_string().as_str())
    );
    assert_eq!(
        head.get("stream").and_then(|value| value.as_str()),
        Some("session")
    );
    assert_eq!(
        head.get("next")
            .and_then(|next| next.get("offset"))
            .and_then(|value| value.as_u64()),
        Some(512)
    );
}

#[test]
fn allocation_respects_reserved_ownership_without_waking_a_paused_owner() {
    use board::team_policy::{Config, Team};
    use std::sync::Arc;

    let journal = fixture();
    let snapshot = snapshot(&journal);
    let entries: Vec<_> = snapshot
        .entries()
        .iter()
        .cloned()
        .map(|entry| entry.with_creator_team(Some(uuid(20))))
        .collect();
    let config: Config =
        wire::decode(br#"{"reserved":{"drive":[{"topic":["/board/**"],"exclusive":true}]}}"#)
            .expect("reservation policy");
    let mut teams = vec![
        Team {
            id: uuid(20),
            name: Some("reserved".into()),
            drive: true,
            drain: false,
            lanes: vec![uuid(20)],
        },
        Team {
            id: uuid(30),
            name: None,
            drive: true,
            drain: true,
            lanes: vec![uuid(30)],
        },
    ];
    let environment =
        wire::Environment::journal_schema(journal::SCHEMA_VERSION).expect("compiled schema");
    let selected = |owners: &Arc<board::OwnershipSnapshot>, team| {
        board::select(
            &entries,
            &Actor::Lane(team),
            &owners.for_team(team),
            &environment,
        )
        .map(|selection| selection.entry().handle().clone())
    };
    let paused = Arc::new(board::OwnershipSnapshot::new(config.clone(), teams.clone()));
    assert_eq!(selected(&paused, uuid(20)), None);
    assert_eq!(selected(&paused, uuid(30)), None);

    teams[0].drain = true;
    let resumed = Arc::new(board::OwnershipSnapshot::new(config.clone(), teams.clone()));
    assert_eq!(selected(&resumed, uuid(20)), Some(handle("BOARD-a1")));
    assert_eq!(selected(&resumed, uuid(30)), None);

    for (drain, admitted, reviewer) in [
        (r#"[]"#, false, uuid(30)),
        (r#"[{"topic":["/other"]}]"#, false, uuid(30)),
        (r#"[{"phase":["review"]}]"#, false, uuid(20)),
        (r#"[{"team":["unbound"]}]"#, false, uuid(30)),
        (
            r#"[{"topic":["/board/*"],"phase":["todo"],"team":["self"]}]"#,
            true,
            uuid(30),
        ),
    ] {
        let policy: Config = wire::decode(
            format!(
                r#"{{"reserved":{{"drive":[{{"topic":["/board/**"],"exclusive":true}}],"drain":{drain}}}}}"#
            )
            .as_bytes(),
        )
        .expect("independent drain rules");
        let owners = Arc::new(board::OwnershipSnapshot::new(policy, teams.clone()));
        assert_eq!(owners.owners(&entries[0]).teams, vec![uuid(20)]);
        assert!(owners.owners(&entries[0]).reserved);
        assert_eq!(
            selected(&owners, uuid(20)),
            admitted.then(|| handle("BOARD-a1")),
            "drain={drain}"
        );
        assert_eq!(selected(&owners, uuid(30)), None);
        if admitted {
            let other_creator = entries[0].clone().with_creator_team(Some(uuid(30)));
            assert!(!owners.for_team(uuid(20)).admits(&other_creator));
        }
        let review = owners.owners_in_phase(&entries[0], &phase("review"));
        assert!(!review.reserved);
        assert_eq!(review.teams, vec![reviewer]);
    }

    teams.remove(0);
    let ended = Arc::new(board::OwnershipSnapshot::new(config, teams));
    assert_eq!(selected(&ended, uuid(30)), Some(handle("BOARD-a1")));
}
