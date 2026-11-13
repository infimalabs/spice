//! Current publication admission survives ordinary round and occupant changes.

use super::*;

const RECIPIENT: u64 = 30;
const RECIPIENT_OCCUPANT: u64 = 31;

struct Round {
    fixture: LaunchFixture,
    bus: Bus,
    task: TaskStamp,
    recipient: wire::OccupantAddress,
}

impl Round {
    /// A seated holder whose shared todo round a partner joined, arrived in,
    /// and then left vacant.
    fn arrived_vacant() -> Self {
        let fixture = LaunchFixture::new();
        let (bus, task) = automatic_claimed_fixture(&fixture);
        target_shared_todo(&bus, &task);
        seed_birth(&bus, fixture.lane, fixture.occupant, fixture.epoch);
        let lane = uuid(RECIPIENT);
        let occupant = uuid(RECIPIENT_OCCUPANT);
        let recipient = add_participant_lane(&bus, &fixture, lane, Some(occupant), "partner")
            .expect("a seated partner has an address");
        join_participant(&bus, &task, lane, occupant, fixture.epoch);
        arrive_participant(&bus, &task, lane);
        vacate(&bus, &fixture, lane);
        Self {
            fixture,
            bus,
            task,
            recipient,
        }
    }

    /// Replace the holder's claim after the partner arrived. The claim index
    /// then retires the partner's freed claim head while the durable arrival
    /// remains authoritative.
    fn replace_sender_claim(&self) {
        let row = self
            .bus
            .task_snapshot()
            .expect("residency snapshot reads")
            .residency()
            .expect("resident remains seated")
            .body()
            .lanes
            .iter()
            .find(|row| row.lane == self.fixture.lane)
            .expect("holder lane remains")
            .clone();
        free_claim(&self.bus, &self.task, self.fixture.lane);
        self.bus
            .task_snapshot()
            .expect("released claim projects before its replacement");
        let at = now();
        seed_task_claim(
            &self.bus,
            &self.task,
            &row,
            self.fixture.epoch,
            at,
            shifted(at, 60),
            shifted(at, 3_600),
        );
    }

    fn finish(self) {
        finish(self.bus);
    }
}

#[test]
fn a_direct_message_uses_the_arrival_after_its_freed_claim_head_retires() {
    let round = Round::arrived_vacant();
    round.replace_sender_claim();
    let task = round.task.clone();
    assert!(
        round
            .bus
            .read(move |read| {
                let heads = read
                    .task_heads_for(&task)?
                    .expect("round remains authoritative");
                let arrival = heads
                    .completion()
                    .body()
                    .arrivals
                    .iter()
                    .find(|arrival| arrival.lane == uuid(RECIPIENT))
                    .expect("partner arrival remains");
                Ok(heads.claim(arrival.claimed).is_none())
            })
            .expect("claim authority reads"),
        "a later claim win retires the completed claim head"
    );

    let source = round.declare_source();
    let view = round.bus.task_snapshot().expect("current view reads");
    assert!(matches!(
        publish(&round, source, &view, false),
        Ok(crate::resident::steering::Published::Sent { .. })
    ));
    let sender = round.fixture.lane;
    assert!(
        round
            .bus
            .read(move |read| {
                crate::resident::steering::peer_wakes_now(
                    read,
                    sender,
                    uuid(RECIPIENT),
                    None,
                    None,
                    None,
                )
            })
            .expect("peer wake authority reads")
    );
    assert!(round.seats());
    round.finish();
}

/// One way the relationship behind a peer wake ends.
#[derive(Clone, Copy, Debug)]
enum Revocation {
    SenderRelease,
    RoundClosure,
    /// The partner took unrelated work and finished it again.
    RecipientWork,
    /// A successor occupied the partner lane without taking work.
    RecipientGeneration,
}

const REVOCATIONS: [Revocation; 4] = [
    Revocation::SenderRelease,
    Revocation::RoundClosure,
    Revocation::RecipientWork,
    Revocation::RecipientGeneration,
];

impl Round {
    fn revoke(&self, how: Revocation) {
        match how {
            Revocation::SenderRelease => free_claim(&self.bus, &self.task, self.fixture.lane),
            Revocation::RoundClosure => close_round(&self.bus, &self.task),
            Revocation::RecipientWork => {
                self.take_other_work();
                free_claim(&self.bus, &other_task(), uuid(RECIPIENT));
            }
            Revocation::RecipientGeneration => self.start_generation(true),
        }
    }

    /// The partner claims an unrelated task.
    fn take_other_work(&self) {
        let mut row = self
            .bus
            .task_snapshot()
            .expect("residency snapshot reads")
            .residency()
            .expect("resident remains seated")
            .body()
            .lanes
            .iter()
            .find(|row| row.lane == uuid(RECIPIENT))
            .expect("partner lane remains")
            .clone();
        row.occupant = Some(uuid(RECIPIENT_OCCUPANT));
        let at = now();
        seed_task_claim(
            &self.bus,
            &other_task(),
            &row,
            self.fixture.epoch,
            at,
            shifted(at, 60),
            shifted(at, 3_600),
        );
    }

    /// Record another occupant generation for the partner without giving it
    /// work, beginning a fresh vendor session or resuming the retained one.
    fn start_generation(&self, fresh: bool) {
        let snapshot = self.bus.task_snapshot().expect("residency snapshot reads");
        let observed = snapshot.residency().expect("resident remains seated");
        let mut seated = observed.body().clone();
        let row = seated
            .lanes
            .iter_mut()
            .find(|row| row.lane == uuid(RECIPIENT))
            .expect("partner lane remains");
        row.occupant = Some(uuid(RECIPIENT_OCCUPANT + 1));
        row.disposition = "occupied".to_owned();
        let origin: Origin = format!("resident:{}", self.fixture.epoch)
            .parse()
            .expect("resident origin");
        let mut write =
            JournalWrite::new(Actor::Spice, origin, now(), Producer::ResidentRecognition);
        let started = write
            .fact(
                wire::body::LaneStarted {
                    lane: uuid(RECIPIENT),
                    occupant: uuid(RECIPIENT_OCCUPANT + 1),
                    epoch: self.fixture.epoch,
                    group: 7,
                    fresh_session: fresh,
                },
                FactCause::Root,
            )
            .expect("successor birth builds");
        write.residency_head(started, move |_| seated);
        committed(self.bus.commit(write).expect("successor seating commits"));
        vacate(&self.bus, &self.fixture, uuid(RECIPIENT));
    }

    /// Whether a seating commit for the partner's woken successor would pass
    /// its inbox guard under current journal authority.
    fn seats(&self) -> bool {
        self.bus
            .read(|read| crate::resident::lane_surface::pending_wakes(read, uuid(RECIPIENT)))
            .expect("seating authority reads")
    }

    /// Directives still pending for the partner.
    fn pending(&self) -> usize {
        self.bus
            .read(|read| crate::steering_read::pending(read, Some(uuid(RECIPIENT))))
            .expect("pending steering reads")
            .len()
    }
}

fn other_task() -> TaskStamp {
    "1kKRRsDZx".parse().expect("other task stamp")
}

/// A publication prepared while the round held is admitted by the cause its
/// commit finds, never by the round its view saw. Only a moved residency or
/// held-room claim refuses it as moved; republished from current authority,
/// or committed as prepared, it spends a pair start and keeps that wake.
#[test]
fn a_publication_prepared_before_revocation_is_admitted_by_its_commit_cause() {
    use crate::resident::steering::{Published, Unpublished};

    let fresh = Round::arrived_vacant();
    let source = fresh.declare_source();
    let current = fresh.bus.task_snapshot().expect("current view reads");
    match publish(&fresh, source, &current, true) {
        Ok(Published::Sent {
            resuming,
            held_room,
            ..
        }) => {
            assert_eq!(resuming, vec![fresh.recipient]);
            assert!(held_room);
        }
        other => panic!("an eligible publication sends: {other:?}"),
    }
    assert!(fresh.seats());
    fresh.finish();

    let direct = Round::arrived_vacant();
    let source = direct.declare_source();
    let current = direct.bus.task_snapshot().expect("current view reads");
    match publish(&direct, source, &current, false) {
        Ok(Published::Sent {
            resuming,
            held_room,
            ..
        }) => {
            assert_eq!(resuming, vec![direct.recipient]);
            assert!(!held_room);
        }
        other => panic!("an eligible direct publication sends: {other:?}"),
    }
    assert!(direct.seats());
    direct.finish();

    for how in REVOCATIONS {
        let round = Round::arrived_vacant();
        let source = round.declare_source();
        let stale = round.bus.task_snapshot().expect("prepared view reads");
        round.revoke(how);
        let prepared = publish(&round, source, &stale, true);
        let published = match how {
            Revocation::RoundClosure | Revocation::RecipientWork => prepared,
            Revocation::SenderRelease | Revocation::RecipientGeneration => {
                let Err(Unpublished::Refused { reason, .. }) = prepared else {
                    panic!("{how:?}: a moved view committed: {prepared:?}");
                };
                let expected = match how {
                    Revocation::RecipientGeneration => "steer::residency-moved",
                    _ => "steer::wake-authority-moved",
                };
                assert_eq!(reason, expected, "{how:?}");
                assert_eq!(
                    round.pending(),
                    0,
                    "{how:?}: the refused write left nothing"
                );
                let current = round.bus.task_snapshot().expect("current view reads");
                publish(&round, source, &current, true)
            }
        };
        let expected = match how {
            Revocation::RecipientGeneration => Vec::new(),
            _ => vec![round.recipient],
        };
        match published {
            Ok(Published::Sent {
                queued, resuming, ..
            }) => {
                assert_eq!(queued, expected, "{how:?}");
                assert_eq!(resuming, expected, "{how:?}");
            }
            other => panic!("{how:?}: the message still queues: {other:?}"),
        }
        if !expected.is_empty() {
            assert!(
                matches!(
                    round.admitted(),
                    Some(wire::body::PeerAdmission::PairToken(spend)) if spend.remaining == 4
                ),
                "{how:?}: the ended round admits nothing free"
            );
        }
        assert_eq!(round.seats(), !expected.is_empty(), "{how:?}");
        round.finish();
    }
}

/// Publish the holder's packet to the partner from `view`, naming the round's
/// room when `room` is set.
fn publish(
    round: &Round,
    source: CanonicalUuid,
    view: &bus::TaskSnapshot,
    room: bool,
) -> Result<crate::resident::steering::Published, crate::resident::steering::Unpublished> {
    let handle = format!("DEMO-{}", round.task).parse().expect("room handle");
    let packet = crate::resident::steering::Packet {
        lane: round.fixture.lane,
        author: round.fixture.occupant,
        locus: wire::body::TranscriptLocus {
            source: Some(source),
            member: 1,
            offset: 0,
            length: 10,
            event: 0,
            line: 1,
            column: 1,
        },
        routes: vec![steer::ack::scan::Route {
            audience: Some(steer::ack::scan::Audience::Occupant(round.recipient)),
            context: room.then_some(handle),
            direction: None,
            span: 0..0,
        }],
        body: "please answer",
        text: 0..13,
    };
    crate::resident::steering::publish_packet(
        &round.bus,
        round.fixture.epoch,
        &steer::Config::default(),
        &packet,
        view,
    )
}

/// A body whose admission recorded its same-round cause keeps the start that
/// cause committed: no later sender release, round closure, unrelated work or
/// occupant generation withdraws it.
#[test]
fn a_recorded_same_round_admission_keeps_its_wake_through_every_revocation() {
    for how in REVOCATIONS {
        let round = Round::arrived_vacant();
        let source = round.declare_source();
        let current = round.bus.task_snapshot().expect("current view reads");
        assert!(
            matches!(
                publish(&round, source, &current, true),
                Ok(crate::resident::steering::Published::Sent { ref queued, .. })
                    if *queued == [round.recipient]
            ),
            "{how:?}: the round admits the body"
        );
        assert_eq!(
            round.admitted(),
            Some(wire::body::PeerAdmission::SameRound),
            "{how:?}"
        );
        round.revoke(how);
        assert!(round.seats(), "{how:?}: the committed start survives");
        round.finish();
    }
}

impl Round {
    /// The cause the newest body queued for the partner recorded.
    fn admitted(&self) -> Option<wire::body::PeerAdmission> {
        self.bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read")
            .facts()
            .iter()
            .rev()
            .find_map(|envelope| envelope.fact.read::<wire::body::SteerQueued>())
            .and_then(|queued| queued.admission)
    }

    /// Account the holder's transcript source so its packets have bytes.
    fn declare_source(&self) -> CanonicalUuid {
        let source = uuid(70);
        let head = wire::TranscriptSourceHead {
            lane: self.fixture.lane,
            vendor: "omp".parse().expect("vendor"),
            stream: "session".to_owned(),
            source,
            next: wire::body::TranscriptPosition {
                member: 1,
                offset: 100,
            },
        };
        let mut accounting = JournalWrite::new(
            Actor::Spice,
            task_origin(&self.task),
            now(),
            Producer::ResidentRecognition,
        );
        let ingested = accounting
            .fact(
                wire::body::JournalIngested {
                    lane: head.lane,
                    vendor: head.vendor.clone(),
                    stream: head.stream.clone(),
                    source,
                    supersedes: None,
                    from: wire::body::TranscriptPosition {
                        member: 0,
                        offset: 0,
                    },
                    to: head.next,
                    usage: Vec::new(),
                    non_usage_records: 0,
                    residue_records: 0,
                },
                FactCause::Root,
            )
            .expect("accounting fact builds");
        accounting.transcript_source_head(ingested, move |_| head);
        committed(self.bus.commit(accounting).expect("accounting commits"));
        source
    }
}

fn vacate(bus: &Bus, fixture: &LaunchFixture, lane: CanonicalUuid) {
    let snapshot = bus.task_snapshot().expect("pre-vacancy snapshot reads");
    let observed = snapshot.residency().expect("resident remains seated");
    let mut vacant = observed.body().clone();
    let row = vacant
        .lanes
        .iter_mut()
        .find(|row| row.lane == lane)
        .expect("fixture lane exists");
    let occupant = row.occupant.take().expect("the lane is seated");
    row.disposition = "vacant".to_owned();
    let origin: Origin = format!("resident:{}", fixture.epoch)
        .parse()
        .expect("resident origin");
    let mut write = JournalWrite::new(Actor::Spice, origin, now(), Producer::ResidentRecognition);
    let exited = write
        .fact(
            wire::body::LaneExited {
                lane,
                occupant,
                code: 0,
            },
            FactCause::Existing(observed.position()),
        )
        .expect("exit fact builds");
    write.residency_head(exited, move |_| vacant);
    committed(bus.commit(write).expect("vacancy commits"));
}

/// Release `lane`'s active claim on `task`.
fn free_claim(bus: &Bus, task: &TaskStamp, lane: CanonicalUuid) {
    let snapshot = bus.task_snapshot().expect("pre-release snapshot reads");
    let claim = snapshot
        .task_heads(task)
        .expect("task heads read")
        .claims()
        .iter()
        .map(|held| held.claim().body())
        .find(|claim| claim.lane == lane && claim.disposition == ClaimDisposition::Active)
        .expect("active claim")
        .clone();
    let mut write = JournalWrite::new(
        claim.actor.clone(),
        task_origin(task),
        now(),
        Producer::CommandDoor,
    );
    let freed = write
        .fact(
            wire::body::ClaimFreed {
                task: task.clone(),
                actor: claim.actor.clone(),
                parked: None,
            },
            FactCause::Existing(claim.claimed),
        )
        .expect("claim release fact builds");
    write.claim_head(task.clone(), freed, move |position| wire::ClaimHead {
        fact: position,
        disposition: ClaimDisposition::Freed,
        ..claim
    });
    committed(bus.commit(write).expect("claim release commits"));
}

/// Close the round's barrier as a completion does, clearing its arrivals.
fn close_round(bus: &Bus, task: &TaskStamp) {
    let snapshot = bus.task_snapshot().expect("pre-closure snapshot reads");
    let heads = snapshot.task_heads(task).expect("task heads read");
    let mut completion = heads.completion().body().clone();
    let mut write = JournalWrite::new(
        Actor::Spice,
        task_origin(task),
        now(),
        Producer::CommandDoor,
    );
    let completed = write
        .fact(
            wire::body::TaskRoundCompleted {
                task: task.clone(),
                phase: heads.workflow().body().phase,
                round: completion.round,
                participants: Vec::new(),
                status: wire::body::TaskCompletionStatus::Pending,
                next: None,
                disposition: None,
            },
            FactCause::Root,
        )
        .expect("round completion fact builds");
    write.completion_head(task.clone(), completed, move |fact| {
        completion.round = fact;
        completion.arrivals.clear();
        completion
    });
    committed(bus.commit(write).expect("round completion commits"));
}
