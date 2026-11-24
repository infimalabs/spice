use super::*;
use crate::roles::operator_book;
use bus::{Bus, CommitOutcome, JournalCommit};
use journal::{FactCause, Journal, JournalWrite};
use wire::{Actor, Domain, Producer, StreamCursor, StreamFact, TrustHead, TrustState, body};

mod retention;

fn uuid(value: &str) -> wire::CanonicalUuid {
    value.parse().expect("canonical fixture uuid")
}

fn seq(value: u64) -> Seq {
    Seq::new(value).expect("non-zero fixture position")
}

fn position(ordinal: u64) -> Seq {
    seq(Seq::FIRST.get() + ordinal - 1)
}

fn accepted(actor: Actor) -> Accepted {
    Accepted {
        wire: WireVersion::CURRENT,
        actor,
        epoch: epoch(),
        release: "1.2.3".parse().expect("fixture release"),
    }
}

fn epoch() -> wire::CanonicalUuid {
    uuid("018f82d4-6f59-7d06-92e3-bce9131163c3")
}

fn presenter() -> QuotePresenter {
    QuotePresenter::new(|_| Vec::new())
}

fn config_revision() -> ConfigRevision {
    ConfigRevision::new(|| Ok(epoch()))
}

fn message(lane: u8, offset: u64) -> AgentMessage {
    AgentMessage {
        prose_origin: None,
        lane: uuid(&format!("018f82d4-6f59-7d06-92e3-bce9131163{lane:02x}")),
        author: uuid("018f82d4-6f59-7d06-92e3-bce9131163ef"),
        locus: wire::MessageLocus {
            member: 1,
            offset,
            length: 20,
            event: 0,
        },
        authored: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
        authored_synthetic: false,
        observed: None,
        activity: wire::AgentActivityKind::Prose,
        compaction_method: None,
        final_answer: false,
        acknowledgement: None,
        text: format!("lane {lane:02x} message {offset}"),
        provider_error: None,
        quote_target: None,
        directive_contexts: Vec::new(),
        sends: Vec::new(),
        quotes: Vec::new(),
        media_quotes: Vec::new(),
        automatic_speech: None,
        media: Vec::new(),
        markdown_images: Vec::new(),
        context_bytes: 0,
    }
}

fn request(last: u8, subjects: Vec<ViewSubject>, after: Option<Seq>) -> StreamRequest {
    StreamRequest::Subscribe {
        v: WireVersion::CURRENT,
        id: uuid(&format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02x}")),
        epoch: epoch(),
        subjects,
        after: after.map(|seq| StreamCursor {
            epoch: epoch(),
            seq,
        }),
    }
}

fn fixture(notes: usize) -> Bus {
    let bus = Bus::start(Journal::in_memory().expect("authority opens")).expect("bus starts");
    commit_notes(&bus, notes, true);
    bus
}

fn commit_notes(bus: &Bus, notes: usize, trust: bool) -> JournalCommit {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:STREAM-a1".parse().expect("origin"),
        "2030-04-12T18:44:09.271Z".parse().expect("instant"),
        Producer::CommandDoor,
    );
    let task: wire::TaskStamp = "1kKRRsDPx".parse().expect("task stamp");
    let mut cause = FactCause::Root;
    for _ in 0..notes {
        let fact = write
            .fact(body::TaskWoken { task: task.clone() }, cause)
            .expect("fact builds");
        cause = FactCause::Pending(fact);
    }
    if trust {
        let digest = wire::Digest::compute(Domain::ConfigExecutableSurface, b"surface");
        let fact = write
            .fact(
                body::TrustPended {
                    surface: "config".to_owned(),
                    digest: digest.clone(),
                    worktree: None,
                    previous: None,
                    documents: None,
                },
                FactCause::Root,
            )
            .expect("trust fact builds");
        write.trust_head(fact, move |_| TrustHead {
            digest,
            state: TrustState::Pended,
            worktree: None,
        });
    }
    match bus.commit(write).expect("commit answers") {
        CommitOutcome::Committed(commit) => commit,
        other => panic!("fixture commit failed: {other:?}"),
    }
}

fn commit_trust_heads(bus: &Bus, surfaces: &[&[u8]]) -> JournalCommit {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:STREAM-a1".parse().expect("origin"),
        "2030-04-12T18:44:09.271Z".parse().expect("instant"),
        Producer::CommandDoor,
    );
    for (index, surface) in surfaces.iter().enumerate() {
        let digest = wire::Digest::compute(Domain::ConfigExecutableSurface, surface);
        let fact = write
            .fact(
                body::TrustPended {
                    surface: format!("surface-{index}"),
                    digest: digest.clone(),
                    worktree: None,
                    previous: None,
                    documents: None,
                },
                FactCause::Root,
            )
            .expect("trust fact builds");
        write.trust_head(fact, move |_| TrustHead {
            digest,
            state: TrustState::Pended,
            worktree: None,
        });
    }
    match bus.commit(write).expect("commit answers") {
        CommitOutcome::Committed(commit) => commit,
        other => panic!("fixture commit failed: {other:?}"),
    }
}

fn commit_ingestion(bus: &Bus) -> JournalCommit {
    let lane = uuid("018f82d4-6f59-7d06-92e3-bce9131163e5");
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("lane:{lane}").parse().expect("lane origin"),
        "2030-04-12T18:44:10.271Z".parse().expect("instant"),
        Producer::ResidentRecognition,
    );
    write
        .fact(
            body::JournalIngested {
                lane,
                vendor: "omp".parse().expect("vendor name"),
                stream: "transcript".to_owned(),
                source: uuid("018f82d4-6f59-7d06-92e3-bce9131163e6"),
                supersedes: None,
                from: body::TranscriptPosition {
                    member: 0,
                    offset: 0,
                },
                to: body::TranscriptPosition {
                    member: 0,
                    offset: 4096,
                },
                usage: Vec::new(),
                non_usage_records: 3,
                residue_records: 0,
            },
            FactCause::Root,
        )
        .expect("ingestion fact builds");
    match bus.commit(write).expect("commit answers") {
        CommitOutcome::Committed(commit) => commit,
        other => panic!("fixture commit failed: {other:?}"),
    }
}

fn facts(frame: &StreamFrame) -> &Packed<StreamFact> {
    let StreamFrame::Facts { packed, .. } = frame else {
        panic!("expected facts, found {frame:?}");
    };
    packed
}

fn fact_sequences(delivery: &LiveDelivery) -> Vec<Seq> {
    delivery
        .frames()
        .iter()
        .map(|frame| {
            let StreamFrame::Fact { fact, .. } = frame else {
                panic!("fact subscription emitted {frame:?}")
            };
            fact.seq()
        })
        .collect()
}

#[test]
fn packed_subscription_continues_exactly_then_becomes_live() {
    let bus = fixture(5);
    let source = Source::new(
        bus.reader(),
        epoch(),
        2,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let (mut subscription, frames) = source
        .begin(
            &accepted(Actor::Lane(uuid("018f82d4-6f59-7d06-92e3-bce9131163e5"))),
            &request(0xc4, vec![ViewSubject::Facts], None),
        )
        .expect("subscription begins");

    assert_eq!(
        facts(&frames[0])
            .rows
            .iter()
            .map(StreamFact::seq)
            .collect::<Vec<_>>(),
        [position(1), position(2)]
    );
    assert_eq!(
        facts(&frames[0]).overflow.expect("overflow").continuation,
        position(3)
    );
    assert_eq!(subscription.cursor(), Some(position(2)));
    assert!(!subscription.is_live());

    let moved = StreamRequest::Continue {
        v: WireVersion::CURRENT,
        id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c5"),
        epoch: epoch(),
        subject: ViewSubject::Facts,
        cursor: StreamCursor {
            epoch: epoch(),
            seq: position(4),
        },
    };
    assert!(matches!(
        subscription.continue_page(&source, &moved),
        Err(Failure::Refusal(ref refusal))
            if refusal.reason() == "stream::continuation-moved"
    ));
    assert_eq!(subscription.cursor(), Some(position(2)));

    for (id, cursor) in [(0xc6, 3), (0xc7, 5)] {
        let continuation = StreamRequest::Continue {
            v: WireVersion::CURRENT,
            id: uuid(&format!("018f82d4-6f59-7d06-92e3-bce9131163{id:02x}")),
            epoch: epoch(),
            subject: ViewSubject::Facts,
            cursor: StreamCursor {
                epoch: epoch(),
                seq: position(cursor),
            },
        };
        subscription
            .continue_page(&source, &continuation)
            .expect("exact continuation answers");
    }
    assert!(subscription.is_live());
    assert_eq!(subscription.cursor(), Some(position(6)));
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

fn commit_seats(bus: &Bus, lanes: std::ops::Range<u32>) -> JournalCommit {
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("resident:{}", epoch()).parse().expect("origin"),
        "2030-04-12T18:44:09.272Z".parse().expect("instant"),
        Producer::ResidentRecognition,
    );
    for index in lanes {
        let lane = uuid(&format!("018f82d4-6f59-7d06-92e3-{index:012x}"));
        let capability = uuid(&format!("018f82d4-6f59-7d06-93e3-{index:012x}"));
        write
            .fact(
                body::LaneSeated {
                    lane,
                    team: lane,
                    capability,
                    name: None,
                    worktree: format!("/tmp/serve-stream-lane-{index}"),
                    life: "drive".to_owned(),
                    vendor: "omp".parse().expect("vendor"),
                    model: None,
                    effort: None,
                },
                FactCause::Root,
            )
            .expect("seat fact builds");
    }
    match bus.commit(write).expect("commit answers") {
        CommitOutcome::Committed(commit) => commit,
        other => panic!("seat fixture commit failed: {other:?}"),
    }
}

#[test]
fn fresh_operator_fact_subscription_carries_the_newest_occupancy_then_begins_live() {
    let bus = fixture(2);
    let older = commit_seats(&bus, 0..1);
    commit_notes(&bus, 2, false);
    let newer = commit_seats(&bus, 1..257);
    commit_notes(&bus, 2, false);
    let source = Source::new(
        bus.reader(),
        epoch(),
        2,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let (subscription, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Facts], None),
        )
        .expect("operator subscription begins");

    let page = facts(&frames[0]);
    let horizon = subscription.cursor().expect("live cursor");
    assert_eq!(page.horizon, horizon);
    assert!(page.overflow.is_none());
    assert!(
        page.rows
            .iter()
            .all(|fact| fact.kind() == wire::FactKind::LaneSeated)
    );
    // The read is bounded, and the bound drops the oldest occupancy.
    let carried = page.rows.iter().map(StreamFact::seq).collect::<Vec<_>>();
    let expected = (newer.first()..=newer.last()).map(seq).collect::<Vec<_>>();
    assert_eq!(carried.len(), journal::MAX_RETAINED_FACT_PAGE_ROWS);
    assert_eq!(carried, expected[expected.len() - carried.len()..]);
    assert!(!carried.contains(&seq(older.last())));
    assert!(subscription.is_live());
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn reconnect_is_strictly_after_the_admitted_fact_cursor() {
    let bus = fixture(5);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let (subscription, frames) = source
        .begin(
            &accepted(Actor::Lane(uuid("018f82d4-6f59-7d06-92e3-bce9131163e5"))),
            &request(0xc4, vec![ViewSubject::Facts], Some(position(4))),
        )
        .expect("same-generation resume begins");

    assert_eq!(
        facts(&frames[0])
            .rows
            .iter()
            .map(StreamFact::seq)
            .collect::<Vec<_>>(),
        [position(5), position(6)]
    );
    assert_eq!(subscription.cursor(), Some(position(6)));
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn operator_reconnect_begins_live_instead_of_draining_missed_history() {
    let bus = fixture(5);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let (subscription, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Facts], Some(position(2))),
        )
        .expect("operator resume begins");

    assert!(facts(&frames[0]).rows.is_empty());
    assert!(facts(&frames[0]).overflow.is_none());
    assert_eq!(facts(&frames[0]).horizon, position(6));
    assert_eq!(subscription.cursor(), Some(position(6)));
    assert!(subscription.is_live());
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

/// Commit one Design task without creating room authority.
fn commit_design(bus: &Bus, design: &str) -> JournalCommit {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "thread:operator".parse().expect("origin"),
        "2030-04-12T18:44:09.271Z".parse().expect("instant"),
        Producer::CommandDoor,
    );
    let task: wire::TaskStamp = design.parse().expect("design stamp");
    let flow = vec![
        "design".parse().expect("design phase"),
        "review".parse().expect("review phase"),
    ];
    let creation =
        body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::PrincipalActor);
    let added = write
        .fact(
            body::TaskAdded {
                preparation: None,
                intent: Some(wire::TaskIntent::Design),
                task: task.clone(),
                title: "Room design".to_owned(),
                pitch: "The room needs a design to talk about.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: "/rooms".parse().expect("fixture topic"),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: flow.clone(),
                phase: flow[0],
                creation: creation.clone(),
                revision: 1,
            },
            FactCause::Root,
        )
        .expect("design fact builds");
    let born = task.clone();
    write.task_head(added, move |revision| wire::TaskHead {
        preparation: None,
        intent: Some(wire::TaskIntent::Design),
        born,
        topic_origin: revision,
        title: "Room design".to_owned(),
        pitch: "The room needs a design to talk about.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: "/rooms".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: wire::TaskProvenance::OperatorDraft,
        creation,
        revision,
    });
    let phase = flow[0];
    write.workflow_head(task.clone(), added, move |_| wire::WorkflowHead {
        flow,
        phase,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    match bus.commit(write).expect("commit answers") {
        CommitOutcome::Committed(commit) => commit,
        other => panic!("Design commit failed: {other:?}"),
    }
}

/// Commit one operator post in `design`'s room.
fn commit_post(bus: &Bus, design: &str, publication: &str) -> JournalCommit {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "thread:operator".parse().expect("origin"),
        "2030-04-12T18:44:10.271Z".parse().expect("instant"),
        Producer::CommandDoor,
    );
    write
        .fact(
            wire::rooms::RoomPublished {
                room: design.parse().expect("design stamp"),
                publication: publication.parse().expect("publication stamp"),
                content: wire::rooms::RoomPublicationContent::Authored {
                    text: "ship it".to_owned(),
                },
            },
            FactCause::Root,
        )
        .expect("post fact builds");
    match bus.commit(write).expect("commit answers") {
        CommitOutcome::Committed(commit) => commit,
        other => panic!("post commit failed: {other:?}"),
    }
}

/// An operator subscription starts live, so its room authority arrives as a
/// room snapshot at that same horizon, and only later facts arrive live.
#[test]
fn operator_room_snapshot_meets_live_facts_at_one_horizon() {
    let bus = fixture(2);
    commit_design(&bus, "dVsYdDj70");
    let first = commit_post(&bus, "dVsYdDj70", "dVsYdDj7d");
    let second = commit_post(&bus, "dVsYdDj70", "dVsYdDj7f");
    let source = Source::new(
        bus.reader(),
        epoch(),
        1,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let (mut subscription, frames) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Facts, ViewSubject::Rooms], None),
        )
        .expect("operator subscription begins");
    assert!(facts(&frames[0]).rows.is_empty());
    assert_eq!(facts(&frames[0]).horizon, seq(second.last()));
    let StreamFrame::Rooms { packed, .. } = &frames[1] else {
        panic!("expected a room snapshot: {frames:?}");
    };
    assert_eq!(packed.horizon, seq(second.last()));
    assert_eq!(
        packed.rows.iter().map(StreamFact::seq).collect::<Vec<_>>(),
        [seq(first.last())]
    );
    let continuation = packed
        .overflow
        .expect("one-row pages overflow")
        .continuation;
    assert!(!subscription.is_live());

    let request = StreamRequest::Continue {
        v: WireVersion::CURRENT,
        id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c5"),
        epoch: epoch(),
        subject: ViewSubject::Rooms,
        cursor: StreamCursor {
            epoch: epoch(),
            seq: continuation,
        },
    };
    let StreamFrame::Rooms { packed, .. } = subscription
        .continue_page(&source, &request)
        .expect("exact room continuation answers")
    else {
        panic!("expected room continuation");
    };
    assert_eq!(
        packed.rows.iter().map(StreamFact::seq).collect::<Vec<_>>(),
        [seq(second.last())]
    );
    assert!(packed.overflow.is_none());
    assert!(subscription.is_live());
    assert_eq!(subscription.cursor(), Some(seq(second.last())));

    let committed = commit_post(&bus, "dVsYdDj70", "dVsYdDj7g");
    let written = seq(committed.first());
    let delivery = subscription
        .advance(&source, committed)
        .expect("commit advances stream")
        .expect("new authority creates a delivery");
    let [StreamFrame::Fact { fact, .. }] = delivery.frames() else {
        panic!("expected one live fact under the one-row page: {delivery:?}");
    };
    assert_eq!(fact.seq(), written);
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[tokio::test(flavor = "current_thread")]
async fn durable_broadcast_drives_later_updates_without_refresh() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let mut broadcasts = source.subscribe();
    let (mut subscription, _) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Facts], None),
        )
        .expect("subscription begins");

    let committed = commit_notes(&bus, 1, false);
    let noticed = broadcasts.recv().await.expect("commit broadcasts");
    assert_eq!(noticed, committed);
    let delivery = subscription
        .advance(&source, noticed)
        .expect("broadcast advances stream")
        .expect("new fact creates a delivery");
    assert_eq!(delivery.frames().len(), 1);
    let StreamFrame::Fact { fact, .. } = &delivery.frames()[0] else {
        panic!("expected live fact: {delivery:?}");
    };
    assert_eq!(fact.seq(), position(3));
    assert_eq!(
        subscription.cursor(),
        Some(position(2)),
        "materialization cannot move the resumable cursor before send"
    );
    subscription.admit(delivery);
    assert_eq!(subscription.cursor(), Some(position(3)));
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[tokio::test(flavor = "current_thread")]
async fn transcript_ingestion_advances_live_cursor_without_crossing_app() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let mut broadcasts = source.subscribe();
    let (mut subscription, _) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Facts], None),
        )
        .expect("subscription begins");
    let before = subscription.cursor().expect("operator begins live");

    let committed = commit_ingestion(&bus);
    let noticed = broadcasts.recv().await.expect("commit broadcasts");
    assert_eq!(noticed, committed);
    let delivery = subscription
        .advance(&source, noticed)
        .expect("broadcast advances stream")
        .expect("accounting receipt advances the cursor");
    assert!(
        delivery.frames().is_empty(),
        "resident-only accounting does not cross the app socket"
    );
    assert_eq!(subscription.cursor(), Some(before));
    subscription.admit(delivery);
    assert_eq!(subscription.cursor(), Some(seq(committed.last())));
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[tokio::test(flavor = "current_thread")]
async fn one_large_commit_delivers_one_indexed_fact_quantum_per_turn() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        3,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let mut broadcasts = source.subscribe();
    let (mut subscription, _) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Facts], None),
        )
        .expect("subscription begins");
    let initial = subscription.cursor().expect("operator begins live");

    let committed = commit_notes(&bus, 11, false);
    let noticed = broadcasts.recv().await.expect("commit broadcasts");
    assert_eq!(noticed, committed);
    let mut delivered = Vec::new();
    let mut first = true;
    loop {
        let before = source.read_counts();
        let delivery = if first {
            first = false;
            subscription
                .advance(&source, noticed)
                .expect("notice advances")
                .expect("notice creates delivery")
        } else {
            subscription
                .continue_live(&source)
                .expect("continuation advances")
                .expect("catch-up retains a delivery")
        };
        let after = source.read_counts();
        assert_eq!(after.1 - before.1, 1, "one fact-page query per turn");
        assert_eq!(
            after.0 - before.0,
            usize::from(delivered.is_empty()),
            "only the notification captures one head barrier"
        );
        assert!(delivery.frames().len() <= 3);
        delivered.extend(fact_sequences(&delivery));
        if delivered.len() == 3 {
            assert_eq!(
                subscription.cursor(),
                Some(initial),
                "materialized frames are not yet resumable"
            );
        }
        subscription.admit(delivery);
        if !subscription.has_continuation() {
            break;
        }
    }

    assert_eq!(
        delivered,
        (initial.get() + 1..=committed.last())
            .map(seq)
            .collect::<Vec<_>>()
    );
    assert_eq!(subscription.cursor(), Some(seq(committed.last())));
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[tokio::test(flavor = "current_thread")]
async fn queued_commits_coalesce_to_the_newest_barrier_without_duplicates() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        2,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let mut broadcasts = source.subscribe();
    let (mut subscription, _) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Facts], None),
        )
        .expect("subscription begins");
    let initial = subscription.cursor().expect("operator begins live");

    let first = commit_notes(&bus, 5, false);
    let second = commit_notes(&bus, 4, false);
    let first_notice = broadcasts.recv().await.expect("first commit broadcasts");
    let second_notice = broadcasts.recv().await.expect("second commit broadcasts");
    assert_eq!((first_notice, second_notice), (first, second));

    let before = source.read_counts();
    let first_delivery = subscription
        .advance(&source, first_notice)
        .expect("first notice advances")
        .expect("first notice creates delivery");
    let after = source.read_counts();
    assert_eq!((after.0 - before.0, after.1 - before.1), (1, 1));
    let mut delivered = fact_sequences(&first_delivery);
    subscription.admit(first_delivery);

    let before = source.read_counts();
    let second_delivery = subscription
        .advance(&source, second_notice)
        .expect("queued notice advances")
        .expect("queued notice creates one delivery");
    let after = source.read_counts();
    assert_eq!(
        (after.0 - before.0, after.1 - before.1),
        (0, 1),
        "the queued notice reuses the already coalesced newest head"
    );
    delivered.extend(fact_sequences(&second_delivery));
    subscription.admit(second_delivery);

    while subscription.has_continuation() {
        let delivery = subscription
            .continue_live(&source)
            .expect("continuation advances")
            .expect("catch-up retains a delivery");
        delivered.extend(fact_sequences(&delivery));
        subscription.admit(delivery);
    }
    assert_eq!(
        delivered,
        (initial.get() + 1..=second.last())
            .map(seq)
            .collect::<Vec<_>>()
    );
    assert_eq!(subscription.cursor(), Some(seq(second.last())));
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[tokio::test(flavor = "current_thread")]
async fn broadcast_lag_during_catch_up_resumes_after_the_last_admitted_quantum() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        3,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let mut broadcasts = source.subscribe();
    let (mut subscription, _) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Facts], None),
        )
        .expect("subscription begins");
    let initial = subscription.cursor().expect("operator begins live");

    let large = commit_notes(&bus, 11, false);
    let notice = broadcasts.recv().await.expect("large commit broadcasts");
    assert_eq!(notice, large);
    let delivery = subscription
        .advance(&source, notice)
        .expect("notice advances")
        .expect("large commit creates delivery");
    subscription.admit(delivery);
    let admitted = seq(initial.get() + 3);
    assert_eq!(subscription.cursor(), Some(admitted));
    assert!(subscription.has_continuation());

    for _ in 0..70 {
        commit_notes(&bus, 1, false);
    }
    let tokio::sync::broadcast::error::RecvError::Lagged(dropped) = broadcasts
        .recv()
        .await
        .expect_err("the retained notice channel overflows")
    else {
        panic!("the retained broadcast closed instead of lagging")
    };
    let overflow = subscription.overflow(dropped).expect("lag is resumable");
    assert!(matches!(
        overflow,
        StreamFrame::Overflow { after, dropped: observed, .. }
            if after.seq == admitted && observed == dropped
    ));
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[tokio::test(flavor = "current_thread")]
async fn live_trust_overflow_retains_its_snapshot_for_exact_continuation() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        2,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let mut broadcasts = source.subscribe();
    let (mut subscription, _) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Trust], None),
        )
        .expect("subscription begins");
    assert!(subscription.is_live());

    let committed = commit_trust_heads(&bus, &[b"second", b"third"]);
    let noticed = broadcasts.recv().await.expect("commit broadcasts");
    assert_eq!(noticed, committed);
    let delivery = subscription
        .advance(&source, noticed)
        .expect("broadcast advances stream")
        .expect("new trust heads create a delivery");
    let StreamFrame::Trust { packed, .. } = &delivery.frames()[0] else {
        panic!("expected trust replacement: {delivery:?}");
    };
    let continuation = packed.overflow.expect("trust page overflows").continuation;
    let first_positions = packed
        .rows
        .iter()
        .map(|row| row.position)
        .collect::<Vec<_>>();
    assert_eq!(packed.rows.len(), 2);
    subscription.admit(delivery);
    assert!(!subscription.is_live());

    let request = StreamRequest::Continue {
        v: WireVersion::CURRENT,
        id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c5"),
        epoch: epoch(),
        subject: ViewSubject::Trust,
        cursor: StreamCursor {
            epoch: epoch(),
            seq: continuation,
        },
    };
    let frame = subscription
        .continue_page(&source, &request)
        .expect("exact trust continuation answers");
    let StreamFrame::Trust { packed, .. } = frame else {
        panic!("expected trust continuation");
    };
    assert_eq!(packed.rows.len(), 1);
    assert!(packed.overflow.is_none());
    let positions = first_positions
        .into_iter()
        .chain(packed.rows.iter().map(|row| row.position))
        .collect::<Vec<_>>();
    assert_eq!(positions.len(), 3);
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(subscription.is_live());
    assert_eq!(subscription.cursor(), Some(seq(committed.last())));
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[test]
fn lane_principal_never_admits_operator_only_subjects() {
    let bus = fixture(1);
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let lane = uuid("018f82d4-6f59-7d06-92e3-bce9131163c2");
    for subject in [
        ViewSubject::Trust,
        ViewSubject::Messages,
        ViewSubject::Quota,
    ] {
        let failure = source
            .begin(
                &accepted(Actor::Lane(lane)),
                &request(0xc4, vec![subject], None),
            )
            .expect_err("lane operator-only view refuses");

        assert!(matches!(
            failure,
            Failure::Refusal(ref refusal)
                if refusal.reason() == "stream::subject-authority"
        ));
    }
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

#[tokio::test]
async fn quota_subscription_hydrates_replaces_reconnects_and_exposes_lag() {
    let bus = fixture(1);
    let feed = QuotaFeed::new(quota_generation(3));
    let source = Source::new(
        bus.reader(),
        epoch(),
        10,
        presenter(),
        operator_book(),
        config_revision(),
    )
    .with_quota(feed.clone());
    let mut updates = source.subscribe_quota().expect("quota source subscribes");
    let (mut subscription, initial) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc4, vec![ViewSubject::Quota], None),
        )
        .expect("operator quota subscription begins");
    assert!(matches!(
        initial.as_slice(),
        [StreamFrame::Quota { generation, .. }] if generation.generation == 3
    ));

    feed.publish(quota_generation(4));
    let generation = updates.recv().await.expect("live quota generation arrives");
    let frame = subscription
        .quota(generation)
        .expect("newer generation projects");
    assert!(matches!(
        frame,
        StreamFrame::QuotaGeneration { ref generation, .. }
            if generation.generation == 4
    ));
    subscription.admit_quota(4);
    feed.publish(quota_generation(3));
    assert!(
        updates.try_recv().is_err(),
        "older generation is not announced"
    );

    let (_, reconnected) = source
        .begin(
            &accepted(Actor::TEST_PRINCIPAL),
            &request(0xc5, vec![ViewSubject::Quota], None),
        )
        .expect("reconnected quota subscription begins");
    assert!(matches!(
        reconnected.as_slice(),
        [StreamFrame::Quota { generation, .. }] if generation.generation == 4
    ));

    let mut lagged = source.subscribe_quota().expect("lag probe subscribes");
    for generation in 5..=22 {
        feed.publish(quota_generation(generation));
    }
    assert!(matches!(
        lagged.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Lagged(2))
    ));
    drop(source);
    bus.shutdown().expect("bus shuts down");
}

fn quota_generation(generation: u64) -> wire::QuotaGeneration {
    wire::QuotaGeneration {
        generation,
        ..wire::QuotaGeneration::default()
    }
}

#[test]
fn thread_request_walks_one_indexed_barrier_and_names_continuation() {
    let bus = fixture(5);
    let source = Source::new(
        bus.reader(),
        epoch(),
        2,
        presenter(),
        operator_book(),
        config_revision(),
    );
    let request = StreamRequest::Thread {
        v: WireVersion::CURRENT,
        id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c4"),
        epoch: epoch(),
        cursor: StreamCursor {
            epoch: epoch(),
            seq: position(5),
        },
    };
    let frame = source
        .thread(&accepted(Actor::TEST_PRINCIPAL), &request)
        .expect("thread answers");
    let StreamFrame::Thread { packed, .. } = frame else {
        panic!("expected thread frame");
    };

    assert_eq!(
        packed.rows.iter().map(StreamFact::seq).collect::<Vec<_>>(),
        [position(5), position(4)]
    );
    assert_eq!(packed.overflow.expect("overflow").continuation, position(3));
    drop(source);
    bus.shutdown().expect("bus shuts down");
}
