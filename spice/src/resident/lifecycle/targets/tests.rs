use super::*;

fn uuid(value: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131164{value:02x}")
        .parse()
        .expect("fixture UUID")
}

fn instant(second: u8) -> Instant {
    format!("2026-11-01T00:00:{second:02}.000Z")
        .parse()
        .expect("fixture instant")
}

fn subscription(team: CanonicalUuid) -> wire::TeamSubscription {
    wire::TeamSubscription {
        team,
        formed_at: None,
        drive: true,
        drain: true,
        name: None,
        fast_mode: false,
    }
}

fn lane(lane: CanonicalUuid, team: CanonicalUuid) -> wire::LaneResidency {
    wire::LaneResidency {
        lane,
        team,
        name: None,
        worktree: "/fixture".to_owned(),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: None,
        lineage: Vec::new(),
        capabilities: Vec::new(),
        session: None,
        disposition: "vacant".to_owned(),
    }
}

fn statement() -> wire::TaskHead {
    let born = TaskStamp::for_micros(1).expect("fixture stamp");
    wire::TaskHead {
        preparation: None,
        intent: None,
        born: born.clone(),
        topic_origin: Seq::new(1).expect("fixture sequence"),
        title: "Review the handoff".to_owned(),
        pitch: "A peer should review this work.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: "/peer/handoff".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: wire::TaskProvenance::Task(born),
        creation: wire::body::TaskCreationContext::unavailable(
            wire::body::TaskCreationUnavailable::SpiceActor,
        ),
        revision: Seq::new(1).expect("fixture sequence"),
    }
}

fn handoff(author: CanonicalUuid, creator_team: CanonicalUuid) -> Entry {
    let statement = statement();
    let workflow = wire::WorkflowHead {
        flow: vec![
            wire::Phase::Draft,
            "todo".parse().expect("fixture phase"),
            "review".parse().expect("fixture phase"),
        ],
        phase: "review".parse().expect("fixture phase"),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    };
    Entry::new(
        statement,
        workflow,
        wire::CompletionHead::opened(wire::Seq::new(1).expect("fixture round")),
        Vec::new(),
    )
    .expect("fixture entry")
    .with_creator_team(Some(creator_team))
    .with_phase_handoff(Some(board::PhaseHandoff::new(
        Actor::Lane(author),
        instant(3),
        instant(30),
    )))
    .with_topic_origin_at(Some(instant(0)))
}

/// A todo row needing `target` lanes, held by `holder` when named.
fn todo_row(target: u32, holder: Option<CanonicalUuid>, team: CanonicalUuid) -> Entry {
    let todo: wire::Phase = "todo".parse().expect("fixture phase");
    let mut completion = wire::CompletionHead::opened(Seq::new(1).expect("fixture round"));
    completion.set_target(
        todo,
        std::num::NonZeroU32::new(target).expect("positive target"),
    );
    let epoch = uuid(8);
    let claims = holder
        .map(|lane| {
            let claimed = Seq::new(2).expect("fixture claim");
            let claim = wire::ClaimHead {
                actor: Actor::Lane(lane),
                fact: claimed,
                claimed,
                cell: wire::ClaimCell {
                    topic: Some("/peer/handoff".parse().expect("fixture topic")),
                    phase: todo,
                },
                lane,
                occupant: uuid(7),
                epoch,
                recovery: None,
                disposition: wire::ClaimDisposition::Active,
                acknowledged: 0,
            };
            let lease = wire::LeaseHead {
                claimed,
                start: instant(1),
                renewal: instant(1),
                expiry: instant(50),
                maximum: instant(59),
                epoch,
            };
            (claim, Some(lease))
        })
        .into_iter()
        .collect();
    let workflow = wire::WorkflowHead {
        flow: vec![
            wire::Phase::Draft,
            todo,
            "review".parse().expect("fixture phase"),
        ],
        phase: todo,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    };
    Entry::new(statement(), workflow, completion, claims)
        .expect("fixture entry")
        .with_creator_team(Some(team))
        .with_topic_origin_at(Some(instant(0)))
}

#[test]
fn a_held_open_seat_wakes_at_once_while_unheld_rows_keep_their_age() {
    let holder = uuid(1);
    let vacant = uuid(2);
    let team = uuid(3);
    let environment =
        wire::Environment::journal_schema(journal::SCHEMA_VERSION).expect("fixture environment");
    let allocation = board::NextConfig::default();
    let residency = residency(holder, vacant, team);
    let owners = std::sync::Arc::new(board::OwnershipSnapshot::from_residency(
        board::team_policy::Config::default(),
        &residency,
    ));
    let starved = after(instant(0), STARVATION_SECONDS).expect("starvation deadline");
    let cases = [
        (
            "held open seat",
            todo_row(2, Some(holder), team),
            Available::Ready,
        ),
        (
            "full capacity",
            todo_row(1, Some(holder), team),
            Available::None,
        ),
        (
            "lone unclaimed",
            todo_row(1, None, team),
            Available::At(starved),
        ),
    ];
    for (label, entry, expected) in cases {
        let entries = [entry];
        let selection = board::SelectionIndex::new(&entries);
        let mut ready_at = BTreeMap::new();
        ready_at.insert(entries[0].born().clone(), instant(0));
        let view = BoardView {
            entries: &entries,
            selection: &selection,
            ready_at: &ready_at,
            allocation: &allocation,
            environment: &environment,
            owners: &owners,
            now: instant(4),
        };
        let work = work_availability(
            &view,
            &residency,
            &residency.lanes[1],
            Some(&residency.subscriptions[0]),
            false,
            None,
        )
        .expect("resident selection succeeds");
        assert_eq!(work.drain_queue, expected, "{label}");
    }
}

fn residency(
    author: CanonicalUuid,
    peer: CanonicalUuid,
    team: CanonicalUuid,
) -> wire::ResidencyHead {
    wire::ResidencyHead {
        epoch: uuid(9),
        state: wire::ResidentState::Seated,
        topology_revision: 1,
        lanes: vec![lane(author, team), lane(peer, team)],
        subscriptions: vec![subscription(team)],
        renewals: Vec::new(),
    }
}

#[test]
fn direct_pull_and_resident_target_share_the_peer_first_deadline() {
    let author = uuid(1);
    let peer = uuid(2);
    let team = uuid(3);
    let entries = [handoff(author, team)];
    let selection = board::SelectionIndex::new(&entries);
    let environment =
        wire::Environment::journal_schema(journal::SCHEMA_VERSION).expect("fixture environment");
    let residency = residency(author, peer, team);
    let owners = std::sync::Arc::new(board::OwnershipSnapshot::from_residency(
        board::team_policy::Config::default(),
        &residency,
    ));
    let mut ready_at = BTreeMap::new();
    ready_at.insert(entries[0].born().clone(), instant(0));
    let allocation = board::NextConfig::default();
    let now = instant(4);
    let view = BoardView {
        entries: &entries,
        selection: &selection,
        ready_at: &ready_at,
        allocation: &allocation,
        environment: &environment,
        owners: &owners,
        now,
    };

    let direct = crate::board_command::independent_handoff_awaits_peer(
        &entries,
        selection.resolved(),
        &entries[0],
        &Actor::Lane(author),
        (&residency, &owners),
        &environment,
        now,
    )
    .expect("peer evidence is unambiguous");
    let resident = work_availability(
        &view,
        &residency,
        &residency.lanes[0],
        Some(&residency.subscriptions[0]),
        true,
        None,
    )
    .expect("resident selection succeeds");
    assert_eq!(direct, Some(instant(30)));
    assert_eq!(resident.drain_queue, Available::At(instant(30)));
    assert_eq!(resident.task.map(Entry::born), Some(entries[0].born()));

    let expired_view = BoardView {
        now: instant(30),
        ..view
    };
    let expired = work_availability(
        &expired_view,
        &residency,
        &residency.lanes[0],
        Some(&residency.subscriptions[0]),
        true,
        None,
    )
    .expect("expired resident selection succeeds");
    assert_eq!(expired.drain_queue, Available::Ready);
}
