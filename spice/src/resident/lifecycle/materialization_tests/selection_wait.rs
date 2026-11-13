use super::trust_ice::{held_claim_write, target};
use super::*;

#[test]
fn a_held_design_still_gathering_input_resumes_its_holders_pass() {
    let (bus, mut projection) = policy_fixture(true, false);
    commit_and_advance(&bus, &mut projection, held_design_write(&bus));
    // A lane captured this Design by name before it steeped, so its pass is
    // the holder's to finish, not the operator's to release.
    assert!(matches!(
        target(
            &bus.task_snapshot().expect("held snapshot reads"),
            &projection
        )
        .held_claim,
        Available::At(_)
    ));
    assert_eq!(
        resolution(&bus, &projection),
        Resolution::Attempt {
            trigger: Trigger::HeldClaim,
            lane: lane(),
        }
    );
    bus.shutdown().expect("gathering pass fixture stops");
}

#[test]
fn a_held_design_waiting_on_released_work_wakes_its_lane_only_for_operator_direction() {
    let (bus, mut projection) = policy_fixture(true, false);
    commit_and_advance(&bus, &mut projection, held_design_write(&bus));
    commit_and_advance(&bus, &mut projection, released_output_write());
    let parked = target(
        &bus.task_snapshot().expect("parked snapshot reads"),
        &projection,
    );
    assert_eq!(
        [parked.held_claim, parked.drain_queue],
        [Available::None; 2]
    );
    assert_eq!(resolution(&bus, &projection), Resolution::Idle);

    commit_and_advance(&bus, &mut projection, operator_direction_write());
    assert_eq!(
        resolution(&bus, &projection),
        Resolution::Attempt {
            trigger: Trigger::PendingInbox,
            lane: lane(),
        },
        "operator direction still wakes the lane"
    );
    bus.shutdown().expect("selection wait fixture stops");
}

/// Turn the seeded task into an open Design `lane()` holds.
fn held_design_write(bus: &Bus) -> JournalWrite {
    held_claim_write(bus, instant(2), |statement, workflow| {
        statement.intent = Some(wire::TaskIntent::Design);
        workflow.flow = vec![wire::Phase::Draft, "design".parse().expect("fixture phase")];
        workflow.phase = "design".parse().expect("fixture phase");
    })
}

/// Pending work the seeded Design selected and released: it carries the
/// Design's control but no longer depends on it.
fn released_output_write() -> JournalWrite {
    let output = task(NEWER_TASK_MOMENT);
    let design = task(TASK_MOMENT);
    let preparation = wire::TaskPreparation {
        parent: design.clone(),
        controller: design.clone(),
        construction: design,
    };
    let mut write = JournalWrite::new(
        Actor::Lane(lane()),
        format!("task:{}", handle(NEWER_TASK_MOMENT))
            .parse()
            .expect("released output origin"),
        instant(3),
        Producer::CommandDoor,
    );
    let added = write
        .fact(
            body::TaskAdded {
                preparation: Some(preparation.clone()),
                intent: None,
                task: output.clone(),
                title: "Released output".to_owned(),
                pitch: "This work keeps the fixture honest.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: TOPIC.parse().expect("fixture topic"),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: vec![wire::Phase::Draft, phase()],
                phase: phase(),
                creation: lane_creation(),
                revision: 1,
            },
            FactCause::Root,
        )
        .expect("released output fact builds");
    let (born, born_provenance) = (output.clone(), output.clone());
    write.task_head(added, move |position| TaskHead {
        preparation: Some(preparation),
        intent: None,
        born,
        topic_origin: position,
        title: "Released output".to_owned(),
        pitch: "This work keeps the fixture honest.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: TOPIC.parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: wire::TaskProvenance::Task(born_provenance),
        creation: lane_creation(),
        revision: position,
    });
    write.workflow_head(output, added, move |_| WorkflowHead {
        flow: vec![wire::Phase::Draft, phase()],
        phase: phase(),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    write
}

fn commit_and_advance(bus: &Bus, projection: &mut Projection, write: JournalWrite) {
    committed(bus.commit(write).expect("fixture write commits"));
    projection
        .advance(&bus.task_snapshot().expect("snapshot reads"))
        .expect("projection advances");
}

fn resolution(bus: &Bus, projection: &Projection) -> Resolution {
    resolve(
        &[target(
            &bus.task_snapshot().expect("snapshot reads"),
            projection,
        )],
        &Backoff::default(),
        instant(50),
    )
}
