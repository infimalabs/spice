use super::*;

fn uuid(last: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131164{last:02}")
        .parse()
        .expect("fixture UUID")
}

fn shifted(at: wire::Instant, seconds: i64) -> wire::Instant {
    wire::Instant::new(
        at.get()
            .checked_add(jiff::SignedDuration::from_secs(seconds))
            .expect("fixture instant remains in range"),
    )
    .expect("shifted fixture instant is canonical")
}

#[test]
fn adoption_rebinds_claim_epoch_without_restarting_the_occupant() {
    let lane = uuid(20);
    let occupant = uuid(21);
    let old_epoch = uuid(22);
    let new_epoch = uuid(23);
    let task: wire::TaskStamp = "1kQpV9Gtx".parse().expect("fixture task");
    let now = wire::Instant::new(jiff::Timestamp::now()).expect("fixture clock is canonical");
    let start = shifted(now, -60);
    let expiry = shifted(now, 60 * 60);
    let maximum = shifted(now, 2 * 60 * 60);
    let durable = wire::LaneResidency {
        lane,
        team: lane,
        name: Some("Adopted lane".to_owned()),
        worktree: "/worktrees/adopted".to_owned(),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: Some(occupant),
        lineage: vec![occupant],
        capabilities: vec![uuid(24)],
        session: None,
        disposition: OCCUPIED.to_owned(),
    };
    let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    let born = seed_birth(&bus, &durable, old_epoch, start);
    seed_task_claim(&bus, &task, &durable, old_epoch, start, expiry, maximum);
    seed_restart_residency(&bus, &durable, new_epoch, start);
    let residency_position = bus
        .residency_head()
        .expect("residency reads")
        .expect("residency exists")
        .position();

    commit_adopted(&bus, new_epoch, &durable).expect("adopted claim commits");

    let snapshot = bus.task_snapshot().expect("adopted heads read");
    let heads = snapshot.tasks().first().expect("fixture task remains");
    let claim = heads
        .latest_claim()
        .map(|held| held.claim())
        .expect("claim remains active");
    let lease = heads
        .latest_claim()
        .and_then(|held| held.lease())
        .expect("lease remains present");
    let residency = snapshot.residency().expect("residency remains present");
    assert_eq!(claim.position(), lease.position());
    assert_ne!(claim.position(), residency.position());
    assert_eq!(residency.position(), residency_position);
    assert_eq!(claim.body().occupant, occupant);
    assert_eq!(claim.body().epoch, new_epoch);
    assert_eq!(lease.body().epoch, new_epoch);
    assert_eq!(residency.body().lanes[0], durable);
    let carried = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("adopted facts read")
        .facts()
        .iter()
        .find(|fact| fact.fact.kind() == wire::FactKind::ClaimCarried)
        .expect("adoption records the authority carry")
        .fact
        .body()
        .clone();
    assert_eq!(carried.get("occupant"), carried.get("ancestor"));
    assert_eq!(
        snapshot.facts().occupant_address(lane, occupant),
        Ok(born),
        "adoption keeps the occupant's original birth address"
    );
    assert_eq!(
        bus.resolve_occupant(born)
            .expect("resolver answers")
            .map(|birth| (birth.lane, birth.occupant, birth.epoch)),
        Ok((lane, occupant, old_epoch))
    );
    bus.verify().expect("adopted authority verifies");
    bus.shutdown().expect("fixture bus stops");
}

/// Seat one lane holding `task` on a lease spanning `start..expiry`, restart
/// the resident through the atomic startup transaction, and return the
/// carried lease after checking the claim moved with residency in one step.
fn restart_with_lease(
    seed: u8,
    start: wire::Instant,
    expiry: wire::Instant,
    maximum: wire::Instant,
) -> wire::LeaseHead {
    let lane = uuid(seed);
    let occupant = uuid(seed + 1);
    let old_epoch = uuid(seed + 2);
    let new_epoch = uuid(seed + 3);
    let task: wire::TaskStamp = "1kQpV9Gtx".parse().expect("fixture task");
    let durable = wire::LaneResidency {
        lane,
        team: lane,
        name: Some("Atomic restart lane".to_owned()),
        worktree: "/worktrees/atomic-restart".to_owned(),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: Some(occupant),
        lineage: vec![occupant],
        capabilities: vec![uuid(seed + 4)],
        session: None,
        disposition: OCCUPIED.to_owned(),
    };
    let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    seed_task_claim(&bus, &task, &durable, old_epoch, start, expiry, maximum);
    let mut startup = JournalWrite::new(
        Actor::Spice,
        format!("resident:{new_epoch}")
            .parse()
            .expect("resident origin"),
        start,
        Producer::CommandDoor,
    );
    let seated = startup
        .fact(wire::body::UpSeated { epoch: new_epoch }, FactCause::Root)
        .expect("startup fact builds");
    let carried = super::super::restart_claim::append_startup_claims(
        &bus,
        std::slice::from_ref(&durable),
        new_epoch,
        &mut startup,
        seated,
    )
    .expect("startup claim appends");
    let row = durable.clone();
    startup.residency_head(carried, move |_| wire::ResidencyHead {
        epoch: new_epoch,
        state: wire::ResidentState::Seated,
        topology_revision: 1,
        lanes: vec![row],
        subscriptions: Vec::new(),
        renewals: Vec::new(),
    });
    assert!(matches!(
        bus.commit(startup).expect("atomic startup commits"),
        bus::CommitOutcome::Committed(_)
    ));

    let snapshot = bus.task_snapshot().expect("atomic heads read");
    let heads = snapshot.tasks().first().expect("fixture task remains");
    let claim = heads
        .latest_claim()
        .map(|held| held.claim())
        .expect("claim remains active");
    let lease = heads
        .latest_claim()
        .and_then(|held| held.lease())
        .expect("lease remains present");
    let residency = snapshot.residency().expect("residency remains present");
    assert_eq!(claim.body().disposition, wire::ClaimDisposition::Active);
    assert_eq!(claim.position(), lease.position());
    assert_eq!(claim.position(), residency.position());
    assert_eq!(claim.body().occupant, occupant);
    assert_eq!(claim.body().epoch, new_epoch);
    assert_eq!(lease.body().epoch, new_epoch);
    assert_eq!(residency.body().epoch, new_epoch);
    let lease = lease.body().clone();
    bus.verify().expect("atomic restart authority verifies");
    bus.shutdown().expect("fixture bus stops");
    lease
}

#[test]
fn startup_seating_carries_claim_without_an_observable_epoch_gap() {
    let now = wire::Instant::new(jiff::Timestamp::now()).expect("fixture clock is canonical");
    let expiry = shifted(now, 60 * 60);
    let maximum = shifted(now, 2 * 60 * 60);
    let lease = restart_with_lease(30, shifted(now, -60), expiry, maximum);
    assert_eq!(
        (lease.expiry, lease.maximum),
        (expiry, maximum),
        "a lease already reaching past one rolling window keeps its horizon"
    );
}

#[test]
fn startup_restores_a_claim_whose_lease_ran_out_while_no_resident_ran() {
    let before = wire::Instant::new(jiff::Timestamp::now()).expect("fixture clock is canonical");
    let lease = restart_with_lease(
        40,
        shifted(before, -60 * 60),
        shifted(before, -10 * 60),
        shifted(before, -5 * 60),
    );
    let window = shifted(before, crate::board_command::AUTOMATIC_ROLLING_SECONDS);
    assert!(
        lease.expiry >= window,
        "downtime must not count against the holder: {} < {window}",
        lease.expiry
    );
    assert!(lease.renewal >= before && lease.renewal <= lease.expiry);
    assert!(lease.expiry <= lease.maximum);
}

#[test]
fn seating_appends_the_lane_without_reordering_teams_or_members() {
    let epoch = uuid(60);
    let lead = uuid(50);
    let joiner = uuid(40);
    let lone = uuid(30);
    let seated = uuid(10);
    let subscription = |team: CanonicalUuid| wire::TeamSubscription {
        team,
        formed_at: None,
        drive: true,
        drain: false,
        name: None,
        fast_mode: false,
    };
    let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    let mut resident = JournalWrite::new(
        Actor::Spice,
        format!("resident:{epoch}")
            .parse()
            .expect("resident origin"),
        wire::Instant::new(jiff::Timestamp::now()).expect("fixture clock is canonical"),
        Producer::CommandDoor,
    );
    let up = resident
        .fact(wire::body::UpSeated { epoch }, FactCause::Root)
        .expect("resident fact builds");
    let lanes = vec![
        vacant_team_lane(lead, lead),
        vacant_team_lane(joiner, lead),
        vacant_team_lane(lone, lone),
    ];
    resident.residency_head(up, move |_| wire::ResidencyHead {
        epoch,
        state: wire::ResidentState::Seated,
        topology_revision: 1,
        lanes,
        subscriptions: vec![subscription(lone), subscription(lead)],
        renewals: Vec::new(),
    });
    assert!(matches!(
        bus.commit(resident).expect("fixture residency commits"),
        bus::CommitOutcome::Committed(_)
    ));
    let observed = bus
        .task_snapshot()
        .expect("residency reads")
        .residency()
        .expect("residency is seated")
        .clone();
    let request = LaneSeat {
        topology_revision: 1,
        lane: seated,
        team: seated,
        name: None,
        worktree: "/worktrees/seated".to_owned(),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        resume: None,
    };
    let operator = wire::Accepted {
        wire: wire::WireVersion::CURRENT,
        actor: Actor::TEST_PRINCIPAL,
        epoch,
        release: "1.2.3".parse().expect("fixture release"),
    };
    let command = wire::Command {
        v: wire::WireVersion::CURRENT,
        id: uuid(70),
        epoch,
        kind: wire::CommandKind::LaneSeat,
        expected: Vec::new(),
        body: serde_json::to_value(&request).expect("lane seat body encodes"),
    }
    .admit(&operator, &wire::principal::PrincipalRoles::default())
    .expect("operator lane seat admits");
    let answer = LaneChangeAnswer {
        lane: seated,
        topology_revision: 2,
        from: "absent".to_owned(),
        to: VACANT.to_owned(),
        git: None,
        rtk: None,
    };
    let proposal = propose_seat(&command, &request, &observed, epoch, uuid(71), answer)
        .expect("seat proposes");
    assert!(matches!(
        bus.commit(proposal).expect("seat commits"),
        bus::CommitOutcome::Committed(_)
    ));

    let head = bus.task_snapshot().expect("residency reads");
    let placed = head
        .residency()
        .expect("residency remains")
        .body()
        .lanes
        .iter()
        .map(|row| (row.lane, row.team))
        .collect::<Vec<_>>();
    assert_eq!(
        placed,
        vec![(lead, lead), (joiner, lead), (lone, lone), (seated, seated)],
        "a seat whose lane id sorts first still stands after every existing row"
    );
    bus.shutdown().expect("fixture bus stops");
}

/// A drain closes no context, whatever ending it observes: the resident may
/// have terminated the occupant mid-turn, so only accounting's own clean turn
/// stops and compactions write checkpoints.
#[test]
fn a_drain_closes_no_context_whatever_its_ending() {
    let (epoch, lane, occupant) = (uuid(80), uuid(81), uuid(82));
    for evidence in [
        StopEvidence::Drained,
        StopEvidence::Exited(1),
        StopEvidence::Killed,
    ] {
        let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
            .expect("fixture bus starts");
        let mut resident = JournalWrite::new(
            Actor::Spice,
            format!("resident:{epoch}")
                .parse()
                .expect("resident origin"),
            wire::Instant::new(jiff::Timestamp::now()).expect("fixture clock is canonical"),
            Producer::CommandDoor,
        );
        let up = resident
            .fact(wire::body::UpSeated { epoch }, FactCause::Root)
            .expect("resident fact builds");
        let mut row = vacant_team_lane(lane, lane);
        row.occupant = Some(occupant);
        row.lineage.push(occupant);
        row.disposition = OCCUPIED.to_owned();
        resident.residency_head(up, move |_| wire::ResidencyHead {
            epoch,
            state: wire::ResidentState::Seated,
            topology_revision: 1,
            lanes: vec![row],
            subscriptions: Vec::new(),
            renewals: Vec::new(),
        });
        assert!(matches!(
            bus.commit(resident).expect("fixture residency commits"),
            bus::CommitOutcome::Committed(_)
        ));
        let observed = bus
            .task_snapshot()
            .expect("residency reads")
            .residency()
            .expect("residency is seated")
            .clone();
        let request = LaneState {
            lane,
            action: LaneAction::Drain,
            worktree: None,
        };
        let command = wire::Command {
            v: wire::WireVersion::CURRENT,
            id: uuid(84),
            epoch,
            kind: wire::CommandKind::LaneState,
            expected: Vec::new(),
            body: serde_json::to_value(&request).expect("lane state body encodes"),
        }
        .admit(
            &wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor: Actor::TEST_PRINCIPAL,
                epoch,
                release: "1.2.3".parse().expect("fixture release"),
            },
            &wire::principal::PrincipalRoles::default(),
        )
        .expect("operator lane drain admits");
        let answer = LaneChangeAnswer {
            lane,
            topology_revision: 1,
            from: OCCUPIED.to_owned(),
            to: evidence.disposition().to_owned(),
            git: None,
            rtk: None,
        };
        let label = evidence.disposition();
        let proposal = propose_drain(
            &command, &request, &observed, epoch, occupant, evidence, answer,
        )
        .expect("drain proposes");
        assert!(matches!(
            bus.commit(proposal).expect("drain commits"),
            bus::CommitOutcome::Committed(_)
        ));
        let closed = bus
            .read(move |read| read.lane_checkpoint(lane))
            .expect("the checkpoint index reads")
            .map(|head| head.body);
        assert_eq!(closed, None, "{label}");
        bus.shutdown().expect("fixture bus stops");
    }
}

fn vacant_team_lane(lane: CanonicalUuid, team: CanonicalUuid) -> wire::LaneResidency {
    wire::LaneResidency {
        lane,
        team,
        name: None,
        worktree: format!("/worktrees/{lane}"),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: None,
        lineage: Vec::new(),
        capabilities: vec![lane],
        session: None,
        disposition: VACANT.to_owned(),
    }
}

/// Record the occupant's one launch birth beneath the generation it started in.
pub(in crate::resident) fn seed_birth(
    bus: &Bus,
    durable: &wire::LaneResidency,
    epoch: CanonicalUuid,
    at: wire::Instant,
) -> wire::OccupantAddress {
    let origin: wire::Origin = format!("resident:{epoch}")
        .parse()
        .expect("resident origin");
    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::ResidentRecognition);
    write
        .fact(
            wire::body::LaneStarted {
                lane: durable.lane,
                occupant: durable.occupant.expect("durable occupant"),
                epoch,
                group: 7,
                fresh_session: true,
            },
            FactCause::Root,
        )
        .expect("birth fact builds");
    match bus.commit(write).expect("fixture birth commits") {
        bus::CommitOutcome::Committed(committed) => wire::OccupantAddress::new(
            committed
                .cursor()
                .position()
                .expect("a commit has a position"),
        ),
        other => panic!("fixture birth did not commit: {other:?}"),
    }
}

/// A successor owns a new executable address on the lane; its ancestor's
/// address stays readable history and refuses execution as expired.
pub(in crate::resident) fn assert_successor_address(
    bus: &Bus,
    lane: CanonicalUuid,
    (ancestor, ancestor_occupant): (wire::OccupantAddress, CanonicalUuid),
    successor_occupant: CanonicalUuid,
) {
    let snapshot = bus.task_snapshot().expect("successor heads read");
    let successor = snapshot
        .facts()
        .occupant_address(lane, successor_occupant)
        .expect("the successor's birth is recorded");
    assert_ne!(successor, ancestor);
    let resolved = bus.resolve_occupant(successor).expect("resolver answers");
    assert_eq!(resolved.map(|birth| birth.occupant), Ok(successor_occupant));
    let historical = bus.occupant_birth(ancestor).expect("resolver answers");
    let historical = historical.expect("the ancestor birth stays proved");
    assert_eq!(
        (historical.lane, historical.occupant),
        (lane, ancestor_occupant)
    );
    let expired = bus.resolve_occupant(ancestor).expect("resolver answers");
    assert_eq!(
        expired,
        Err(bus::AddressRefusal::Expired { birth: historical })
    );
}

pub(in crate::resident) fn seed_task_claim(
    bus: &Bus,
    task: &wire::TaskStamp,
    durable: &wire::LaneResidency,
    old_epoch: CanonicalUuid,
    start: wire::Instant,
    expiry: wire::Instant,
    maximum: wire::Instant,
) {
    seed_task_claim_in_topic(
        bus,
        task,
        durable,
        old_epoch,
        (start, expiry, maximum),
        "/task/claim".parse().expect("fixture topic"),
    );
}

pub(in crate::resident) fn seed_task_claim_in_topic(
    bus: &Bus,
    task: &wire::TaskStamp,
    durable: &wire::LaneResidency,
    old_epoch: CanonicalUuid,
    window: (wire::Instant, wire::Instant, wire::Instant),
    topic: wire::TaskTopic,
) {
    let (start, expiry, maximum) = window;
    let phase: wire::Phase = "todo".parse().expect("fixture phase");
    let origin: wire::Origin = format!("task:{task}").parse().expect("fixture origin");
    let actor = Actor::Lane(durable.lane);
    let mut write = JournalWrite::new(actor.clone(), origin, start, Producer::CommandDoor);
    let (added, won) =
        append_seed_claim_facts(&mut write, &phase, task, durable, old_epoch, expiry, &topic);
    let statement_task = task.clone();
    let statement_provenance = wire::TaskProvenance::Task(task.clone());
    let statement_phase = phase;
    let statement_topic = topic.clone();
    write.task_head(added, move |position| wire::TaskHead {
        preparation: None,
        intent: None,
        born: statement_task,
        topic_origin: position,
        title: "Restore claimed work".to_owned(),
        pitch: "This work keeps the fixture honest.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: statement_topic,
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: statement_provenance,
        creation: wire::body::TaskCreationContext::unavailable(
            wire::body::TaskCreationUnavailable::SpiceActor,
        ),
        revision: position,
    });
    let workflow_task = task.clone();
    write.workflow_head(workflow_task, added, move |_| wire::WorkflowHead {
        flow: vec![wire::Phase::Draft, statement_phase],
        phase: statement_phase,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    let claim_task = task.clone();
    let lane = durable.lane;
    let occupant = durable.occupant.expect("durable occupant");
    let claim_topic = topic.path;
    write.claim_head(claim_task, won, move |position| wire::ClaimHead {
        actor,
        fact: position,
        claimed: position,
        cell: wire::ClaimCell {
            topic: claim_topic,
            phase,
        },
        lane,
        occupant,
        epoch: old_epoch,
        recovery: None,
        disposition: wire::ClaimDisposition::Active,
        acknowledged: 0,
    });
    let lease_task = task.clone();
    write.lease_head(lease_task, won, move |claimed| wire::LeaseHead {
        claimed,
        start,
        renewal: start,
        expiry,
        maximum,
        epoch: old_epoch,
    });
    assert!(matches!(
        bus.commit(write).expect("fixture task authority commits"),
        bus::CommitOutcome::Committed(_)
    ));
}

fn append_seed_claim_facts(
    write: &mut JournalWrite,
    phase: &wire::Phase,
    task: &wire::TaskStamp,
    durable: &wire::LaneResidency,
    old_epoch: CanonicalUuid,
    expiry: wire::Instant,
    topic: &wire::TaskTopic,
) -> (PendingFact, PendingFact) {
    let added = write
        .fact(
            wire::body::TaskAdded {
                preparation: None,
                intent: None,
                task: task.clone(),
                title: "Restore claimed work".to_owned(),
                pitch: "This work keeps the fixture honest.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: topic.clone(),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: vec![wire::Phase::Draft, *phase],
                phase: *phase,
                creation: wire::body::TaskCreationContext::unavailable(
                    wire::body::TaskCreationUnavailable::SpiceActor,
                ),
                revision: 1,
            },
            FactCause::Root,
        )
        .expect("task fact builds");
    let won = write
        .fact(
            wire::body::ClaimWon {
                task: task.clone(),
                actor: Actor::Lane(durable.lane),
                lane: durable.lane,
                occupant: durable.occupant.expect("durable occupant"),
                epoch: old_epoch,
                cell: wire::ClaimCell {
                    topic: topic.path.clone(),
                    phase: *phase,
                },
                until: expiry,
                git: None,
                recovery: None,
                source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Pending(added),
        )
        .expect("claim fact builds");
    (added, won)
}

fn seed_restart_residency(
    bus: &Bus,
    durable: &wire::LaneResidency,
    new_epoch: CanonicalUuid,
    start: wire::Instant,
) {
    let resident_origin: wire::Origin = format!("resident:{new_epoch}")
        .parse()
        .expect("resident origin");
    let mut resident =
        JournalWrite::new(Actor::Spice, resident_origin, start, Producer::CommandDoor);
    let seated = resident
        .fact(wire::body::UpSeated { epoch: new_epoch }, FactCause::Root)
        .expect("resident fact builds");
    let row = durable.clone();
    resident.residency_head(seated, move |_| wire::ResidencyHead {
        epoch: new_epoch,
        state: wire::ResidentState::Seated,
        topology_revision: 1,
        lanes: vec![row],
        subscriptions: Vec::new(),
        renewals: Vec::new(),
    });
    assert!(matches!(
        bus.commit(resident).expect("fixture residency commits"),
        bus::CommitOutcome::Committed(_)
    ));
}
