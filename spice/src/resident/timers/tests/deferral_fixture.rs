use super::*;

pub(super) fn deferred_task(topic: &str, stamp: &str) -> TaskHandle {
    TaskHandle::for_topic(
        &topic.parse::<wire::Topic>().expect("fixture topic"),
        &stamp.parse().expect("fixture task stamp"),
    )
}

pub(super) fn seed_timer_task(
    bus: &Bus,
    task: &TaskHandle,
    topic: &str,
    deferral: Option<Instant>,
    dependencies: Vec<wire::TaskStamp>,
) {
    let stamp = task.born().expect("fixture task has a stamp");
    let phase = "todo".parse::<Phase>().expect("fixture phase");
    let at = Instant::new(jiff::Timestamp::now()).expect("fixture clock is canonical");
    let origin = format!("task:{stamp}")
        .parse::<Origin>()
        .expect("fixture task origin");
    let mut write = JournalWrite::new(Actor::TEST_PRINCIPAL, origin, at, Producer::CommandDoor);
    let added = write
        .fact(
            body::TaskAdded {
                preparation: None,
                intent: None,
                task: stamp.clone(),
                title: format!("Timer fixture {stamp}"),
                pitch: "This work keeps the fixture honest.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: topic.parse().expect("fixture topic"),
                submitted: false,
                priority: None,
                taste: None,
                depends: dependencies.clone(),
                flow: vec![wire::Phase::Draft, phase],
                phase,
                creation: body::TaskCreationContext::unavailable(
                    body::TaskCreationUnavailable::SpiceActor,
                ),
                revision: 1,
            },
            FactCause::Root,
        )
        .expect("fixture task fact builds");
    let workflow_fact = match deferral {
        Some(until) => write
            .fact(
                body::TaskDeferred {
                    task: stamp.clone(),
                    until,
                },
                FactCause::Pending(added),
            )
            .expect("fixture deferral fact builds"),
        None => added,
    };
    let statement_stamp = stamp.clone();
    let statement_provenance = wire::TaskProvenance::Task(stamp.clone());
    let statement_phase = phase;
    let statement_topic = topic.to_owned();
    write.task_head(added, move |position| TaskHead {
        preparation: None,
        intent: None,
        born: statement_stamp,
        topic_origin: position,
        title: "Timer deferral fixture".to_owned(),
        pitch: "This work keeps the fixture honest.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: statement_topic.parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies,
        provenance: statement_provenance,
        creation: body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::SpiceActor),
        revision: position,
    });
    write.workflow_head(stamp, workflow_fact, move |_| WorkflowHead {
        flow: vec![wire::Phase::Draft, statement_phase],
        phase: statement_phase,
        status: wire::WorkflowStatus::pending(),
        deferral,
        submitted: false,
        generation: 0,
    });
    assert!(matches!(
        bus.commit(write).expect("fixture task commits"),
        bus::CommitOutcome::Committed(_)
    ));
}

pub(super) fn timer_task_heads(bus: &Bus, task: &TaskHandle) -> TaskHeads {
    let stamp = task.born().expect("fixture task has a stamp");
    bus.task_snapshot()
        .expect("fixture task snapshot reads")
        .tasks()
        .iter()
        .find(|heads| heads.statement().body().born == stamp)
        .expect("fixture task heads exist")
        .clone()
}

pub(super) fn task_woken_count(bus: &Bus) -> usize {
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("fixture task facts read")
    .facts()
    .iter()
    .filter(|fact| fact.fact.kind() == wire::FactKind::TaskWoken)
    .count()
}
