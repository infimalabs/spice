use super::tests::lane;
use super::*;

/// The held claim one vacant lane would launch, minted either provisional
/// or ordinary, after the lane bore `lineage`.
fn reservation(
    lineage: Vec<CanonicalUuid>,
    provisional: bool,
) -> Option<crate::board_command::AutomaticClaim> {
    let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    let (seat, occupant, epoch) = (lane(1), lane(2), lane(3));
    let row = wire::LaneResidency {
        lane: seat,
        team: seat,
        name: None,
        worktree: "/fixture".to_owned(),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: None,
        lineage,
        capabilities: Vec::new(),
        session: None,
        disposition: "vacant".to_owned(),
    };
    let start = Instant::new(jiff::Timestamp::now()).expect("fixture instant");
    let after = |seconds| {
        Instant::new(
            start
                .get()
                .checked_add(jiff::SignedDuration::from_secs(seconds))
                .expect("fixture instant shifts"),
        )
        .expect("shifted fixture instant")
    };
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("resident origin");
    let mut write = journal::JournalWrite::new(Actor::Spice, origin, start, Producer::CommandDoor);
    let seated = write
        .fact(wire::body::UpSeated { epoch }, FactCause::Root)
        .expect("resident seating fact builds");
    let lanes = vec![row.clone()];
    write.residency_head(seated, move |_| wire::ResidencyHead {
        epoch,
        state: wire::ResidentState::Seated,
        topology_revision: 1,
        lanes,
        subscriptions: Vec::new(),
        renewals: Vec::new(),
    });
    assert!(matches!(
        bus.commit(write).expect("resident seating commits"),
        bus::CommitOutcome::Committed(_)
    ));
    let task: wire::TaskStamp = "1kKRRsDQx".parse().expect("task stamp");
    crate::resident::lane_surface::seed_task_claim(
        &bus,
        &task,
        &wire::LaneResidency {
            occupant: Some(occupant),
            ..row
        },
        epoch,
        start,
        after(if provisional { 60 } else { 3_600 }),
        after(3_600),
    );
    let snapshot = bus.task_snapshot().expect("claimed snapshot reads");
    let reserved = unlaunched_claim(&snapshot, seat);
    bus.shutdown().expect("fixture bus stops");
    reserved
}

#[test]
fn only_an_automatic_claim_for_a_never_seated_occupant_is_a_launch_reservation() {
    assert_eq!(
        reservation(Vec::new(), true),
        Some(crate::board_command::AutomaticClaim {
            task: "1kKRRsDQx".parse().expect("task stamp"),
            lane: lane(1),
            occupant: lane(2),
        })
    );
    assert_eq!(
        reservation(vec![lane(2)], true),
        None,
        "a claim its occupant already worked survives a refused relaunch"
    );
    assert_eq!(
        reservation(Vec::new(), false),
        None,
        "an ordinary held claim is never released by a refused launch"
    );
}
