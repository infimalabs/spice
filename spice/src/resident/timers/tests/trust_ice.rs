use super::*;

#[test]
fn iced_automatic_claim_arms_only_its_distant_lapse() {
    let fixture = automatic_fixture();
    ice_claim(&fixture);
    let mut timers = start_timers().expect("timer runtime starts");

    timers
        .synchronize(&fixture.bus)
        .expect("iced timers synchronize");

    assert!(timers.renewals.is_empty());
    assert_eq!(
        timers
            .claims
            .get(&fixture.key())
            .expect("distant lapse remains armed")
            .authority
            .expiry,
        wire::Instant::indefinite()
    );
    assert_eq!(
        fixture
            .bus
            .transact(|journal| bus::JournalTransaction::settled(
                journal.fact_snapshot().expect("journal snapshot")
            ))
            .expect("iced facts read")
            .facts()
            .iter()
            .filter(|fact| fact.fact.kind() == wire::FactKind::ClaimRenewed)
            .count(),
        0
    );

    drop(timers);
    fixture.bus.shutdown().expect("fixture bus stops");
}

fn ice_claim(fixture: &Fixture) {
    let snapshot = fixture.bus.task_snapshot().expect("pre-ice snapshot reads");
    let heads = &snapshot.tasks()[0];
    let stamp = fixture.authority.born();
    let mut statement = heads.statement().body().clone();
    statement.requirements =
        wire::EnvironmentRequirements::one(wire::EnvironmentRequirement::trust_grant(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse()
                .expect("fixture digest"),
        ));
    let observed = heads
        .latest_claim()
        .and_then(|held| held.lease())
        .expect("lease head exists")
        .body()
        .clone();
    let at = Instant::new(jiff::Timestamp::now()).expect("fixture clock is canonical");
    let mut write = JournalWrite::new(
        Actor::Lane(fixture.authority.lane),
        format!("task:{}", fixture.authority.task)
            .parse()
            .expect("fixture origin"),
        at,
        Producer::CommandDoor,
    );
    let edited = write
        .fact(
            body::TaskEdited {
                intent: None,
                task: stamp.clone(),
                revision: 2,
                dependencies: None,
                flow: None,
            },
            FactCause::Root,
        )
        .expect("requirement edit builds");
    write.task_head(edited, move |position| TaskHead {
        revision: position,
        ..statement
    });
    let horizon = wire::Instant::indefinite();
    write.lease_head(stamp, edited, move |_| LeaseHead {
        expiry: horizon,
        maximum: horizon,
        ..observed
    });
    assert!(matches!(
        fixture.bus.commit(write).expect("ice commits"),
        bus::CommitOutcome::Committed(_)
    ));
}
