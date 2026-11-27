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
    let opening = lane_at(OPENING);
    committed(
        bus.commit(open_and_arm_write(&claimed, opening))
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
            .invalid
            .is_empty()
    );
    assert!(!reminder[0].pending_inbox, "repeat-only presentation waits");
    assert_eq!(reminder[0].held_claim, Available::None);
    assert_eq!(reminder[0].park, Some(body::LanePark::Line));

    let mut authority = [Target {
        held_claim: Available::None,
        park: Some(body::LanePark::Authority),
        ..target(true)
    }];
    await_armed_lines(&bus, &snapshot, &mut authority).expect("authority park projects");
    assert!(
        !authority[0].pending_inbox,
        "repeat-only presentation still waits"
    );
    assert_eq!(
        authority[0].park,
        Some(body::LanePark::Authority),
        "a standing line never relabels another park"
    );

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
    let mut after_peer_turn = [target(true)];
    await_armed_lines(&bus, &snapshot, &mut after_peer_turn)
        .expect("line park returns after peer-woken turn");
    assert_eq!(
        after_peer_turn[0].park,
        Some(body::LanePark::Line),
        "held-claim continuation parks again after fresh peer input is handled"
    );

    committed(
        bus.commit(close_write(&bus, opening))
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

    let rearmed = empty_arm_lifts_and_rearms(&bus, &reopened, second);

    committed(
        bus.commit(free_claim_write(&rearmed, lane_at(PARTNER), instant(10)))
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
    let invalid = await_armed_lines(&bus, &malformed, &mut no_park)
        .expect("mismatched arm projects")
        .invalid;
    assert_eq!(invalid.len(), 1);
    assert_eq!(invalid[0].0, lane());
    assert_eq!(no_park[0].park, None);

    bus.shutdown().expect("line park fixture stops");
}

#[test]
fn reclaiming_in_the_same_round_lifts_the_old_arm_until_rearmed() {
    let (bus, _) = caught_up_fixture(0);
    committed(
        bus.commit(held_claim_write(&bus, instant(2), |_, _| {}))
            .expect("caller claim commits"),
    );
    committed(
        bus.commit(partner_claim_write(&bus))
            .expect("partner claim commits"),
    );
    let opening = lane_at(OPENING);
    let claimed = bus.task_snapshot().expect("claimed snapshot reads");
    committed(
        bus.commit(open_and_arm_write(&claimed, opening))
            .expect("open line and arm commit"),
    );
    let armed = bus.task_snapshot().expect("armed snapshot reads");
    committed(
        bus.commit(free_claim_write(&armed, lane(), instant(5)))
            .expect("caller claim frees"),
    );
    committed(
        bus.commit(held_claim_write(&bus, instant(6), |_, _| {}))
            .expect("caller replacement claim commits"),
    );
    let reclaimed = bus
        .task_snapshot()
        .expect("replacement claim snapshot reads");
    let mut stale = [target(true)];
    await_armed_lines(&bus, &reclaimed, &mut stale).expect("stale arm projects");
    assert_eq!(stale[0].park, None, "the old arm binds no later claim");
    assert_eq!(stale[0].held_claim, Available::Ready);

    committed(
        bus.commit(arm_write(&reclaimed, opening, instant(7)))
            .expect("replacement claim arm commits"),
    );
    let rearmed = bus.task_snapshot().expect("replacement arm snapshot reads");
    let mut bound = [target(true)];
    await_armed_lines(&bus, &rearmed, &mut bound).expect("replacement arm projects");
    assert_eq!(bound[0].park, Some(body::LanePark::Line));
    bus.shutdown().expect("line park fixture stops");
}

#[test]
fn a_phase_or_round_change_lifts_the_bound_arm() {
    let (bus, _) = caught_up_fixture(0);
    committed(
        bus.commit(held_claim_write(&bus, instant(2), |_, _| {}))
            .expect("caller claim commits"),
    );
    committed(
        bus.commit(partner_claim_write(&bus))
            .expect("partner claim commits"),
    );
    let claimed = bus.task_snapshot().expect("claimed snapshot reads");
    let root = lane_at(OPENING);
    committed(
        bus.commit(open_and_arm_write(&claimed, root))
            .expect("open line and arm commit"),
    );
    let armed = bus.task_snapshot().expect("armed snapshot reads");
    let heads = &armed.tasks()[0];
    let current_phase = heads.workflow().body().phase;
    let other_phase = heads
        .workflow()
        .body()
        .flow
        .iter()
        .copied()
        .find(|phase| *phase != current_phase)
        .expect("fixture workflow has another phase");
    let round = heads.completion().body().round;
    committed(
        bus.commit(arm_context_write(
            &armed,
            root,
            other_phase,
            round,
            instant(5),
        ))
        .expect("later-phase arm commits"),
    );
    let later_phase = bus.task_snapshot().expect("later-phase snapshot reads");
    let mut after_phase = [target(true)];
    await_armed_lines(&bus, &later_phase, &mut after_phase).expect("phase change projects");
    assert_eq!(after_phase[0].park, None);
    assert_eq!(after_phase[0].held_claim, Available::Ready);

    let later_round = wire::Seq::new(round.get() + 1).expect("later round");
    committed(
        bus.commit(arm_context_write(
            &later_phase,
            root,
            current_phase,
            later_round,
            instant(6),
        ))
        .expect("later-round arm commits"),
    );
    let later_round = bus.task_snapshot().expect("later-round snapshot reads");
    let mut after_round = [target(true)];
    await_armed_lines(&bus, &later_round, &mut after_round).expect("round change projects");
    assert_eq!(after_round[0].park, None);
    assert_eq!(after_round[0].held_claim, Available::Ready);
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

fn arm_write(snapshot: &bus::TaskSnapshot, root: CanonicalUuid, at: Instant) -> JournalWrite {
    let heads = &snapshot.tasks()[0];
    let caller = lane();
    let mut write = JournalWrite::new(
        Actor::Lane(caller),
        "steer:fixture".parse().expect("fixture steer origin"),
        at,
        Producer::CommandDoor,
    );
    write
        .fact(
            body::LineArmed {
                lane: caller,
                task: heads.statement().body().born.clone(),
                phase: heads.workflow().body().phase,
                round: heads.completion().body().round,
                roots: vec![root],
            },
            FactCause::Root,
        )
        .expect("replacement arm fact builds");
    write
}

fn empty_arm_lifts_and_rearms(
    bus: &Bus,
    armed: &bus::TaskSnapshot,
    root: CanonicalUuid,
) -> bus::TaskSnapshot {
    committed(
        bus.commit(empty_arm_write(armed))
            .expect("empty arm commits"),
    );
    let cleared = bus.task_snapshot().expect("empty-arm snapshot reads");
    let mut after_empty = [target(true)];
    await_armed_lines(bus, &cleared, &mut after_empty).expect("empty arm projects");
    assert_eq!(after_empty[0].park, None);
    assert_eq!(after_empty[0].held_claim, Available::Ready);
    committed(
        bus.commit(arm_write(&cleared, root, instant(9)))
            .expect("line rearms"),
    );
    bus.task_snapshot().expect("rearmed snapshot reads")
}

fn empty_arm_write(snapshot: &bus::TaskSnapshot) -> JournalWrite {
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
                lane: lane(),
                task: heads.statement().body().born.clone(),
                phase: heads.workflow().body().phase,
                round: heads.completion().body().round,
                roots: Vec::new(),
            },
            FactCause::Root,
        )
        .expect("empty arm fact builds");
    write
}

fn arm_context_write(
    snapshot: &bus::TaskSnapshot,
    root: CanonicalUuid,
    phase: wire::Phase,
    round: wire::Seq,
    at: Instant,
) -> JournalWrite {
    let heads = &snapshot.tasks()[0];
    let mut write = JournalWrite::new(
        Actor::Lane(lane()),
        "steer:fixture".parse().expect("fixture steer origin"),
        at,
        Producer::CommandDoor,
    );
    write
        .fact(
            body::LineArmed {
                lane: lane(),
                task: heads.statement().body().born.clone(),
                phase,
                round,
                roots: vec![root],
            },
            FactCause::Root,
        )
        .expect("context arm fact builds");
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

fn free_claim_write(
    snapshot: &bus::TaskSnapshot,
    lane: CanonicalUuid,
    at: Instant,
) -> JournalWrite {
    let heads = &snapshot.tasks()[0];
    let task = heads.statement().body().born.clone();
    let claim = heads
        .claims()
        .iter()
        .find(|held| held.claim().body().lane == lane)
        .expect("lane holds one claim")
        .claim()
        .body()
        .clone();
    let mut write = JournalWrite::new(
        Actor::Lane(lane),
        format!("task:{}", handle(TASK_MOMENT))
            .parse()
            .expect("fixture task origin"),
        at,
        Producer::CommandDoor,
    );
    let freed = write
        .fact(
            body::ClaimFreed {
                task: task.clone(),
                actor: Actor::Lane(lane),
                parked: None,
            },
            FactCause::Root,
        )
        .expect("claim free fact builds");
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
