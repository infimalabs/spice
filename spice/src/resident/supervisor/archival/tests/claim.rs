use super::*;

fn write(fixture: &Fixture) -> JournalWrite {
    JournalWrite::new(
        Actor::Lane(fixture.request.lane),
        "task:archival-claim".parse().expect("origin"),
        wire::Instant::new(jiff::Timestamp::now()).expect("instant"),
        Producer::CommandDoor,
    )
}

fn claim(fixture: &mut Fixture) -> wire::TaskStamp {
    let task = wire::TaskStamp::for_micros(1_000_000).expect("task stamp");
    let mut write = write(fixture);
    let creation = wire::body::TaskCreationContext::unavailable(
        wire::body::TaskCreationUnavailable::PrincipalActor,
    );
    let phase = "todo".parse::<wire::Phase>().expect("phase");
    let flow = vec![phase];
    let added = write
        .fact(
            wire::body::TaskAdded {
                task: task.clone(),
                title: "Owned work".to_owned(),
                pitch: "Pending work holds archival.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: Default::default(),
                topic: "/tape".parse().expect("fixture topic"),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: flow.clone(),
                phase,
                preparation: None,
                intent: None,
                creation: creation.clone(),
                revision: 1,
            },
            FactCause::Root,
        )
        .expect("task fact");
    let born = task.clone();
    write.task_head(added, move |revision| wire::TaskHead {
        born,
        topic_origin: revision,
        title: "Owned work".to_owned(),
        pitch: "Pending work holds archival.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: Default::default(),
        resources: Default::default(),
        topic: "/tape".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        intent: None,
        preparation: None,
        provenance: wire::TaskProvenance::OperatorDraft,
        creation,
        revision,
    });
    let workflow_phase = phase;
    write.workflow_head(task.clone(), added, move |_| wire::WorkflowHead {
        flow,
        phase: workflow_phase,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    let (lane, occupant, epoch) = (
        fixture.request.lane,
        fixture.request.occupant,
        fixture.request.epoch,
    );
    let actor = Actor::Lane(lane);
    let cell = wire::ClaimCell {
        topic: Some("/tape".parse().expect("fixture topic")),
        phase,
    };
    let won = write
        .fact(
            wire::body::ClaimWon {
                task: task.clone(),
                actor: actor.clone(),
                lane,
                occupant,
                epoch,
                cell: cell.clone(),
                until: wire::Instant::new(jiff::Timestamp::now()).expect("instant"),
                git: None,
                recovery: None,
                source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Pending(added),
        )
        .expect("claim fact");
    write.claim_head(task.clone(), won, move |position| wire::ClaimHead {
        actor,
        fact: position,
        claimed: position,
        cell,
        lane,
        occupant,
        epoch,
        recovery: None,
        disposition: wire::ClaimDisposition::Active,
        acknowledged: 0,
    });
    fixture
        .journal
        .as_mut()
        .expect("journal")
        .apply(write)
        .expect("claim publishes");
    task
}

#[test]
fn an_authoritative_claim_holds_archival_until_released() {
    let mut fixture = Fixture::new();
    fixture.fill(MINIMUM);
    let task = claim(&mut fixture);
    assert!(
        !authority(&fixture.paths, &fixture.request, Boundary::Shutdown)
            .expect("claim authority reads")
            .expect("lane exists")
            .eligible
    );
    assert!(shutdown(&fixture.paths, &fixture.request).is_none());
    let mut head = fixture
        .journal
        .as_ref()
        .expect("journal")
        .read(|read| read.task_heads_for(&task))
        .expect("claim reads")
        .and_then(|heads| heads.latest_claim().map(|held| held.claim().clone()))
        .expect("claim exists")
        .into_body();
    let mut write = write(&fixture);
    let freed = write
        .fact(
            wire::body::ClaimFreed {
                task: task.clone(),
                actor: Actor::Lane(fixture.request.lane),
                parked: None,
            },
            FactCause::Existing(head.claimed),
        )
        .expect("release fact");
    head.disposition = wire::ClaimDisposition::Freed;
    write.claim_head(task, freed, move |position| {
        head.fact = position;
        head
    });
    fixture
        .journal
        .as_mut()
        .expect("journal")
        .apply(write)
        .expect("claim releases");
    assert!(shutdown(&fixture.paths, &fixture.request).is_some());
}

#[test]
fn historical_conversion_names_a_vacant_lanes_claim() {
    let mut fixture = Fixture::new();
    fixture.predecessor(true);
    fixture.seat(false);
    claim(&mut fixture);

    assert_eq!(
        historical_authority(&fixture.paths, fixture.request.lane)
            .expect("historical authority reads"),
        HistoricalAuthority::Claimed
    );
}
