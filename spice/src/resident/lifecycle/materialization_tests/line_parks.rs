use super::trust_ice::held_claim_write;
use super::*;

const PARTNER: u8 = 12;
const OPENING: u8 = 20;

#[test]
fn an_open_armed_line_parks_only_continuation_and_reminders() {
    let (bus, mut projection) = caught_up_fixture(0);
    committed(
        bus.commit(held_claim_write(&bus, instant(2), |_, _| {}))
            .expect("caller claim commits"),
    );
    committed(
        bus.commit(partner_claim_write(&bus))
            .expect("partner claim commits"),
    );
    let claimed = bus.task_snapshot().expect("claimed snapshot reads");
    committed(
        bus.commit(open_and_arm_write(&claimed, lane_at(OPENING)))
            .expect("open line and arm commit"),
    );
    let snapshot = bus.task_snapshot().expect("armed snapshot reads");
    projection
        .advance(&snapshot)
        .expect("armed projection advances");

    let mut reminder = [target(true)];
    assert!(
        await_armed_lines(&bus, &snapshot, &mut reminder)
            .expect("line park projects")
            .is_empty()
    );
    assert!(!reminder[0].pending_inbox, "repeat-only presentation waits");
    assert_eq!(reminder[0].held_claim, Available::None);
    assert_eq!(reminder[0].park, Some(body::LanePark::Line));

    let mut fresh = [target(false)];
    await_armed_lines(&bus, &snapshot, &mut fresh).expect("fresh-input park projects");
    assert!(
        fresh[0].pending_inbox,
        "fresh input keeps ordinary priority"
    );
    assert_eq!(
        resolve(&fresh, &Backoff::default(), instant(5)),
        Resolution::Attempt {
            trigger: Trigger::PendingInbox,
            lane: lane(),
        }
    );

    committed(
        bus.commit(close_write(&bus, lane_at(OPENING)))
            .expect("line close commits"),
    );
    let closed = bus.task_snapshot().expect("closed snapshot reads");
    let mut resumed = [target(true)];
    await_armed_lines(&bus, &closed, &mut resumed).expect("closed line projects");
    assert_eq!(resumed[0].park, None);
    assert_eq!(resumed[0].held_claim, Available::Ready);
    assert!(resumed[0].pending_inbox);
    let second = lane_at(OPENING + 1);
    committed(
        bus.commit(open_and_arm_write(&closed, second))
            .expect("second open line and arm commit"),
    );
    let reopened = bus.task_snapshot().expect("reopened snapshot reads");
    let mut parked_again = [target(true)];
    await_armed_lines(&bus, &reopened, &mut parked_again).expect("second line parks");
    assert_eq!(parked_again[0].park, Some(body::LanePark::Line));

    committed(
        bus.commit(free_partner_write(&reopened))
            .expect("partner departure commits"),
    );
    let departed = bus.task_snapshot().expect("departed snapshot reads");
    let mut after_departure = [target(true)];
    await_armed_lines(&bus, &departed, &mut after_departure)
        .expect("counterpart departure projects");
    assert_eq!(after_departure[0].park, None);
    assert_eq!(after_departure[0].held_claim, Available::Ready);
    committed(
        bus.commit(mismatched_arm_write(&departed))
            .expect("mismatched arm fact commits"),
    );
    let malformed = bus.task_snapshot().expect("mismatched arm snapshot reads");
    let mut no_park = [target(true)];
    let invalid =
        await_armed_lines(&bus, &malformed, &mut no_park).expect("mismatched arm projects");
    assert_eq!(invalid.len(), 1);
    assert_eq!(invalid[0].0, lane());
    assert_eq!(no_park[0].park, None);

    bus.shutdown().expect("line park fixture stops");
}

fn partner_claim_write(bus: &Bus) -> JournalWrite {
    let snapshot = bus.task_snapshot().expect("caller claim snapshot reads");
    let heads = &snapshot.tasks()[0];
    let task = heads.statement().body().born.clone();
    let phase = heads.workflow().body().phase;
    let partner = lane_at(PARTNER);
    let occupant = lane_at(PARTNER + 1);
    let at = instant(3);
    let mut write = JournalWrite::new(
        Actor::Lane(partner),
        format!("task:{}", handle(TASK_MOMENT))
            .parse()
            .expect("fixture task origin"),
        at,
        Producer::CommandDoor,
    );
    let won = write
        .fact(
            body::ClaimWon {
                task: task.clone(),
                actor: Actor::Lane(partner),
                lane: partner,
                occupant,
                epoch: epoch(),
                cell: wire::ClaimCell {
                    topic: Some(TOPIC.parse().expect("fixture topic")),
                    phase,
                },
                until: wire::Instant::indefinite(),
                git: None,
                recovery: None,
                source: body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Root,
        )
        .expect("partner claim fact builds");
    let claim_task = task.clone();
    write.claim_head(claim_task, won, move |position| wire::ClaimHead {
        actor: Actor::Lane(partner),
        fact: position,
        claimed: position,
        cell: wire::ClaimCell {
            topic: Some(TOPIC.parse().expect("fixture topic")),
            phase,
        },
        lane: partner,
        occupant,
        epoch: epoch(),
        recovery: None,
        disposition: wire::ClaimDisposition::Active,
        acknowledged: 0,
    });
    write.lease_head(task, won, move |claimed| wire::LeaseHead {
        claimed,
        start: at,
        renewal: at,
        expiry: wire::Instant::indefinite(),
        maximum: wire::Instant::indefinite(),
        epoch: epoch(),
    });
    write
}

fn mismatched_arm_write(snapshot: &bus::TaskSnapshot) -> JournalWrite {
    let heads = &snapshot.tasks()[0];
    let mut write = JournalWrite::new(
        Actor::Lane(lane()),
        "steer:fixture".parse().expect("fixture steer origin"),
        instant(8),
        Producer::CommandDoor,
    );
    write
        .fact(
            body::LineArmed {
                lane: lane_at(PARTNER),
                task: heads.statement().body().born.clone(),
                phase: heads.workflow().body().phase,
                round: heads.completion().body().round,
                roots: Vec::new(),
            },
            FactCause::Root,
        )
        .expect("mismatched arm fact builds");
    write
}

fn open_and_arm_write(snapshot: &bus::TaskSnapshot, root: CanonicalUuid) -> JournalWrite {
    let heads = &snapshot.tasks()[0];
    let caller = lane();
    let partner = lane_at(PARTNER);
    let task = heads.statement().body().born.clone();
    let phase = heads.workflow().body().phase;
    let round = heads.completion().body().round;
    let line = body::PeerLineRef {
        root,
        parent: None,
        opening: root,
    };
    let at = instant(4);
    let origin = "steer:fixture"
        .parse::<Origin>()
        .expect("fixture steer origin");
    let direction = body::SteerSent {
        directive: root,
        fanout: None,
        key: steer::Key::for_identity(root).as_str().to_owned(),
        recipient: Actor::Lane(partner),
        text: "wait here".to_owned(),
        sender: Actor::Lane(caller),
        room: None,
        control: Some(wire::peer_line_control(line, body::PeerBodyKind::Open, &[])),
        attachments: Vec::new(),
        recipients: Vec::new(),
        expires: instant(50),
    };
    let mut write = JournalWrite::new(
        Actor::Lane(caller),
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let sent = write
        .fact(direction.clone(), FactCause::Root)
        .expect("open line fact builds");
    write.steering_head(sent, move |position| {
        Ledger::sent_head(&direction, origin, at, position).expect("open line head builds")
    });
    write
        .fact(
            body::LineArmed {
                lane: caller,
                task,
                phase,
                round,
                roots: vec![root],
            },
            FactCause::Root,
        )
        .expect("line arm fact builds");
    write
}

fn close_write(bus: &Bus, root: CanonicalUuid) -> JournalWrite {
    let partner = lane_at(PARTNER);
    let at = instant(6);
    let current = bus
        .read(move |read| read.steering_head(root))
        .expect("opening head reads")
        .expect("opening head exists");
    let acked = body::SteerAcked {
        directive: root,
        key: current.body().key.clone(),
        recipient: Actor::Lane(partner),
        evidence: body::SteeringEvidence::Transcript {
            epoch: epoch(),
            locus: body::TranscriptLocus {
                source: Some(epoch()),
                member: 1,
                offset: 1,
                length: 1,
                event: 0,
                line: 1,
                column: 1,
            },
        },
    };
    let mut write = JournalWrite::new(
        Actor::Lane(partner),
        "steer:fixture".parse().expect("fixture steer origin"),
        at,
        Producer::AckScanner,
    );
    let retired = write
        .fact(acked.clone(), FactCause::Root)
        .expect("opening retirement builds");
    let next = Ledger::transition_head(current.body(), steer::Steering::Acked(&acked), at)
        .expect("retired opening head builds");
    write.steering_head(retired, move |_| next);
    write
        .fact(
            body::SteerLineTransition {
                line: root,
                input: root,
                action: body::PeerLineAction::Finish,
                body: None,
            },
            FactCause::Pending(retired),
        )
        .expect("line close fact builds");
    write
}

fn free_partner_write(snapshot: &bus::TaskSnapshot) -> JournalWrite {
    let heads = &snapshot.tasks()[0];
    let task = heads.statement().body().born.clone();
    let partner = lane_at(PARTNER);
    let claim = heads
        .claims()
        .iter()
        .find(|held| held.claim().body().lane == partner)
        .expect("partner holds one claim")
        .claim()
        .body()
        .clone();
    let mut write = JournalWrite::new(
        Actor::Lane(partner),
        format!("task:{}", handle(TASK_MOMENT))
            .parse()
            .expect("fixture task origin"),
        instant(7),
        Producer::CommandDoor,
    );
    let freed = write
        .fact(
            body::ClaimFreed {
                task: task.clone(),
                actor: Actor::Lane(partner),
                parked: None,
            },
            FactCause::Root,
        )
        .expect("partner free fact builds");
    write.claim_head(task, freed, move |position| wire::ClaimHead {
        fact: position,
        disposition: wire::ClaimDisposition::Freed,
        ..claim
    });
    write
}

fn target(reminder_only: bool) -> Target<'static> {
    Target {
        lane: lane(),
        last_claim: None,
        pending_inbox: true,
        inbox_reminder_only: reminder_only,
        held_claim: Available::Ready,
        loose_worktree: false,
        drain_queue: Available::None,
        last_resort: false,
        task: None,
        matched: None,
        park: None,
    }
}
