use super::trust_ice::held_claim_write;
use super::*;

/// A row a lane explicitly freed is that lane's last resort in the wake as it
/// is in allocation: a vacant teammate that can take it wakes first, and the
/// freeing lane wakes for it only when no other lane can.
#[test]
fn a_freed_row_wakes_a_vacant_teammate_before_the_lane_that_freed_it() {
    let (bus, mut projection) = policy_fixture(true, false);
    committed(
        bus.commit(held_claim_write(&bus, instant(2), |_, _| {}))
            .expect("claim commits"),
    );
    committed(bus.commit(free_write(&bus)).expect("free commits"));

    let alone = bus.task_snapshot().expect("freed snapshot reads");
    projection
        .advance(&alone)
        .expect("freed projection advances");
    let targets = targets_at(&alone, &projection);
    assert!(
        targets.iter().all(|target| target.last_resort),
        "the freed row is its freeing lane's last resort"
    );
    assert_eq!(
        resolve(&targets, &Backoff::default(), instant(50)),
        Resolution::Attempt {
            trigger: Trigger::DrainQueue,
            lane: lane(),
        },
        "with no other lane, the freeing lane still takes its row"
    );

    add_vacant_org_members(&bus, &[lane_at(2)]);
    let joined = bus.task_snapshot().expect("joined snapshot reads");
    projection
        .advance(&joined)
        .expect("joined projection advances");
    let targets = targets_at(&joined, &projection);
    assert_eq!(
        resolve(&targets, &Backoff::default(), instant(50)),
        Resolution::Attempt {
            trigger: Trigger::DrainQueue,
            lane: lane_at(2),
        },
        "a vacant teammate takes the freed row before its freeing lane"
    );
    bus.shutdown().expect("freed fixture stops");
}

fn targets_at<'a>(snapshot: &bus::TaskSnapshot, projection: &'a Projection) -> Vec<Target<'a>> {
    project_targets(
        snapshot,
        projection,
        instant(50),
        &Holds::new(3),
        &board::NextConfig::default(),
        None,
        &owners(snapshot),
    )
    .expect("freed targets project")
}

/// `lane()` frees its claim on the seeded task, as `spice task free` does.
fn free_write(bus: &Bus) -> JournalWrite {
    let snapshot = bus.task_snapshot().expect("claimed snapshot reads");
    let heads = &snapshot.tasks()[0];
    let task = heads.statement().body().born.clone();
    let claim = heads
        .latest_claim()
        .map(|held| held.claim())
        .expect("active claim")
        .body()
        .clone();
    let mut write = JournalWrite::new(
        Actor::Lane(lane()),
        format!("task:{}", handle(TASK_MOMENT))
            .parse()
            .expect("task origin"),
        instant(3),
        Producer::CommandDoor,
    );
    let freed = write
        .fact(
            body::ClaimFreed {
                task: task.clone(),
                actor: claim.actor.clone(),
                parked: None,
            },
            FactCause::Root,
        )
        .expect("free fact builds");
    write.claim_head(task, freed, move |position| wire::ClaimHead {
        fact: position,
        disposition: wire::ClaimDisposition::Freed,
        ..claim
    });
    write
}
