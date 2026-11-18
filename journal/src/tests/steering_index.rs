//! Bounded steering authority, atomic publication, and indexed admission.

use super::*;
use crate::{
    CommitError, FactCause, JournalGuard, JournalHead, JournalWrite, MAX_STEERING_PAGE_ROWS,
    RetiredDirection,
};
use wire::body::{
    LaneHeld, SteerAcked, SteerCanceled, SteerDeclined, SteerEscalated, SteerLapsed, SteerSent,
    SteeringStage, SteeringTransition, TranscriptLocus,
};
use wire::{Disposition, Producer, SteeringHead};

const LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116302";
const TARGET: &str = "018f82d4-6f59-7d06-92e3-bce913116399";
const TARGET_KEY: &str = "1kJK6N3Rx";

#[test]
fn steering_fact_and_head_publish_atomically_and_key_collision_rolls_back() {
    let mut journal = Journal::in_memory().expect("steering journal");

    let mut unindexed = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin(),
        instant(),
        Producer::CommandDoor,
    );
    unindexed
        .fact(sent(uuid(TARGET), TARGET_KEY), FactCause::Root)
        .expect("sent fact builds");
    let error = journal
        .apply(unindexed)
        .expect_err("raw steering fact refuses");
    assert!(matches!(error, CommitError::Refused(_)));
    assert!(
        journal
            .fact_snapshot()
            .expect("unchanged journal")
            .facts()
            .is_empty()
    );

    let mut mismatched = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin(),
        instant(),
        Producer::CommandDoor,
    );
    let body = sent(uuid(TARGET), TARGET_KEY);
    let pending = mismatched
        .fact(body.clone(), FactCause::Root)
        .expect("sent fact builds");
    mismatched.steering_head(pending, move |seq| {
        live_head(
            &sent(uuid("018f82d4-6f59-7d06-92e3-bce913116398"), "1kJK6N3Ry"),
            "1kJK6N3Ry".to_owned(),
            seq,
        )
    });
    let error = journal
        .apply(mismatched)
        .expect_err("a head for another directive refuses");
    assert!(matches!(error, CommitError::Refused(_)));
    assert!(
        journal
            .fact_snapshot()
            .expect("mismatch rolls back")
            .facts()
            .is_empty()
    );

    let applied = journal
        .apply(send_write(uuid(TARGET), TARGET_KEY))
        .expect("indexed publication applies");
    let stored = journal
        .read(|read| read.steering_key(TARGET_KEY))
        .expect("indexed key reads")
        .expect("target head exists");
    assert_eq!(stored.position(), applied.last());
    assert_eq!(stored.body().created, applied.first());
    assert!(
        journal
            .steering_index_current()
            .expect("index horizon reads")
    );

    let before = journal.fact_snapshot().expect("pre-collision history");
    let collision = journal
        .apply(send_write(
            uuid("018f82d4-6f59-7d06-92e3-bce913116398"),
            TARGET_KEY,
        ))
        .expect_err("a second directive cannot claim the indexed key");
    assert!(matches!(collision, CommitError::Fault(_)));
    assert_eq!(
        journal.fact_snapshot().expect("post-collision history"),
        before,
        "the fact and unique indexed head must roll back together"
    );
}

#[test]
fn steering_head_mint_moment_must_match_its_current_key() {
    let mut journal = Journal::in_memory().expect("steering journal");
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin(),
        instant(),
        Producer::CommandDoor,
    );
    let body = sent(uuid(TARGET), TARGET_KEY);
    let pending = write
        .fact(body.clone(), FactCause::Root)
        .expect("sent fact builds");
    write.steering_head(pending, move |seq| {
        let mut head = live_head(&body, TARGET_KEY.to_owned(), seq);
        head.minted_micros += 1;
        head
    });

    let error = journal
        .apply(write)
        .expect_err("a mismatched key moment refuses");
    assert!(
        matches!(error, CommitError::Refused(_)),
        "unexpected error: {error}"
    );
    assert!(
        journal
            .fact_snapshot()
            .expect("mismatch rolls back")
            .facts()
            .is_empty()
    );
}

#[test]
fn batched_subject_head_targets_its_final_fact_exactly_once() {
    let mut journal = Journal::in_memory().expect("steering journal");
    journal
        .apply(send_write(uuid(TARGET), TARGET_KEY))
        .expect("publication applies");
    let observed = journal
        .read(|read| read.steering_key(TARGET_KEY))
        .expect("published head reads")
        .expect("published head exists");
    let before = journal.fact_snapshot().expect("published history");

    for (label, placement) in [
        ("missing", BatchHead::Missing),
        ("earlier-only", BatchHead::Earlier),
        ("duplicate", BatchHead::Duplicate),
        ("stray", BatchHead::Stray),
    ] {
        let error = journal
            .apply(escalation_batch(&observed, placement))
            .expect_err("invalid batch head placement refuses");
        assert!(matches!(error, CommitError::Refused(_)), "{label}: {error}");
        assert_eq!(
            journal.fact_snapshot().expect("history after refusal"),
            before,
            "{label} head placement must not mutate the journal"
        );
    }

    let applied = journal
        .apply(escalation_batch(&observed, BatchHead::Final))
        .expect("one head on the final subject fact applies");
    let current = journal
        .read(|read| read.steering_key(TARGET_KEY))
        .expect("escalated head reads")
        .expect("escalated head exists");
    assert_eq!(current.position(), applied.last());
    assert_eq!(current.body().stage, SteeringStage::Escalated);
    assert_eq!(
        current
            .body()
            .escalations
            .iter()
            .map(|transition| transition.stage)
            .collect::<Vec<_>>(),
        [SteeringStage::Reminded, SteeringStage::Escalated]
    );
    assert!(
        journal
            .steering_index_current()
            .expect("authority horizon reads")
    );
}

#[test]
fn stale_indexed_admission_loses_the_race_without_mutation() {
    let mut journal = Journal::in_memory().expect("steering journal");
    journal
        .apply(send_write(uuid(TARGET), TARGET_KEY))
        .expect("publication applies");
    let observed = journal
        .read(|read| read.steering_key(TARGET_KEY))
        .expect("head reads")
        .expect("head exists");

    let first = cancel_write(&observed);
    let second = cancel_write(&observed);
    let retired = journal.apply(first).expect("first cancellation wins");
    let before_loser = journal.fact_snapshot().expect("winner history");
    let error = journal.apply(second).expect_err("stale cancellation loses");
    assert!(matches!(error, CommitError::Refused(_)));
    assert_eq!(
        journal.fact_snapshot().expect("loser history"),
        before_loser
    );

    let head = journal
        .read(|read| read.steering_key(TARGET_KEY))
        .expect("retired head reads")
        .expect("retired head exists");
    assert_eq!(head.position(), retired.last());
    assert_eq!(head.body().disposition, Disposition::Canceled);
}

#[test]
fn indexed_read_work_is_constant_as_retired_history_grows() {
    let mut journal = Journal::in_memory().expect("steering journal");
    journal
        .apply(send_write(uuid(TARGET), TARGET_KEY))
        .expect("target publication applies");

    let before_key = measured_key(&journal);
    let before_lane = measured_lane(&journal);
    let before_all = measured_all_lanes(&journal);
    for offset in 0_usize..128 {
        let directive = uuid(&format!("00000000-0000-4000-8000-{offset:012x}"));
        let key = fixture_key(1_000 + offset);
        journal
            .apply(send_write(directive, &key))
            .expect("retired publication applies");
        let head = journal
            .read(|read| read.steering_key(&key))
            .expect("retired head reads")
            .expect("retired head exists");
        journal
            .apply(cancel_write(&head))
            .expect("retirement applies");
    }

    assert_eq!(measured_key(&journal), before_key);
    assert_eq!(measured_lane(&journal), before_lane);
    assert_eq!(measured_all_lanes(&journal), before_all);
    assert_eq!(before_key.queries, 2, "horizon plus one keyed lookup");
    assert_eq!(before_key.rows, 2, "horizon plus one keyed row");
    assert_eq!(before_lane.queries, 2, "horizon plus one live-lane lookup");
    assert_eq!(before_lane.rows, 2, "horizon plus one live-lane row");
    assert!(
        before_lane.decoded_bytes > 0,
        "the one selected steering body was decoded"
    );
    let plan = journal
        .connection
        .prepare(
            "EXPLAIN QUERY PLAN SELECT directive, steer_key, recipient, disposition, created_seq, \
             minted_micros, head_seq, body FROM steering_heads \
             WHERE disposition = 'live' ORDER BY created_seq",
        )
        .expect("all-lanes plan prepares")
        .query_map([], |row| row.get::<_, String>(3))
        .expect("all-lanes plan reads")
        .collect::<Result<Vec<_>, _>>()
        .expect("all-lanes plan decodes")
        .join("\n");
    assert!(
        plan.contains("steering_heads_disposition_created"),
        "all-lanes inbox must use its disposition/creation index: {plan}"
    );
}

#[test]
fn steering_recovery_snapshot_excludes_unrelated_world_history() {
    let mut journal = Journal::in_memory().expect("steering journal");
    journal
        .apply(send_write(uuid(TARGET), TARGET_KEY))
        .expect("steering publication applies");
    let mut ordinary = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin(),
        instant(),
        Producer::ResidentRecognition,
    );
    ordinary
        .fact(
            text_fact("unrelated world history".to_owned()),
            FactCause::Root,
        )
        .expect("ordinary fact builds");
    journal.apply(ordinary).expect("ordinary fact applies");

    journal
        .connection
        .execute(
            "UPDATE fact_envelopes SET envelope = X'00' WHERE kind = 'journal::backed'",
            [],
        )
        .expect("corrupt only unrelated history");

    let snapshot = journal
        .steering_snapshot()
        .expect("steering recovery ignores unrelated bytes");
    assert_eq!(snapshot.horizon(), Seq::new(Seq::FIRST.get() + 1));
    assert_eq!(snapshot.facts().len(), 1);
    assert_eq!(snapshot.facts()[0].fact.kind(), wire::FactKind::SteerSent);
    assert!(
        journal.fact_snapshot().is_err(),
        "a whole-world snapshot would have decoded the unrelated corruption"
    );
}

#[test]
fn a_rebuilt_steering_index_restores_typed_rooms() {
    let mut journal = Journal::in_memory().expect("steering journal");
    let [room, _] = rooms();
    let heads = [
        publish_route(&mut journal, 1, lane(), Some(&room)),
        publish_route(&mut journal, 2, lane(), None),
    ];
    journal
        .connection
        .execute("UPDATE steering_heads SET room = NULL", [])
        .expect("forget typed rooms");

    let cursor = journal.read(|read| read.cursor()).expect("cursor reads");
    journal
        .rebuild_steering_index(cursor, &heads, &[])
        .expect("steering index rebuilds");
    assert_eq!(typed_rooms(&journal), [Some(room.to_string()), None]);
    journal.verify().expect("rebuilt rooms verify");
}

fn rooms() -> [wire::TaskStamp; 2] {
    [
        wire::TaskStamp::for_micros(1_700_000_000_000_000).expect("fixture room"),
        wire::TaskStamp::for_micros(1_700_000_000_000_001).expect("other fixture room"),
    ]
}

fn publish_route(
    journal: &mut Journal,
    index: usize,
    to: CanonicalUuid,
    room: Option<&wire::TaskStamp>,
) -> JournalHead<SteeringHead> {
    let key = fixture_key(2_000 + index);
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin(),
        instant(),
        Producer::CommandDoor,
    );
    let mut body = sent(indexed_uuid(index), &key);
    body.recipient = Actor::Lane(to);
    body.room = room.cloned();
    let pending = write
        .fact(body.clone(), FactCause::Root)
        .expect("routed fact builds");
    let head_key = key.clone();
    write.steering_head(pending, move |seq| live_head(&body, head_key, seq));
    journal.apply(write).expect("room route commits");
    journal
        .read(|read| read.steering_key(&key))
        .expect("room route reads")
        .expect("room route exists")
}

fn typed_rooms(journal: &Journal) -> Vec<Option<String>> {
    journal
        .connection
        .prepare("SELECT room FROM steering_heads ORDER BY created_seq")
        .expect("typed rooms prepare")
        .query_map([], |row| row.get(0))
        .expect("typed rooms read")
        .collect::<Result<_, _>>()
        .expect("typed rooms decode")
}

fn retired_summary(rows: &[RetiredDirection]) -> Vec<(String, Disposition)> {
    rows.iter()
        .map(|row| (row.key.clone(), row.disposition))
        .collect()
}

#[test]
fn retired_directions_are_lane_scoped_bounded_and_newest_first() {
    const WINDOW: usize = 4;
    let other = indexed_uuid(0xff);
    let mut journal = Journal::in_memory().expect("steering journal");
    let publish = |journal: &mut Journal, write: JournalWrite, key: &str| {
        journal.apply(write).expect("publication applies");
        journal
            .read(|read| read.steering_key(key))
            .expect("published head reads")
            .expect("published head exists")
    };
    let heads = (0..6)
        .map(|index| {
            let key = fixture_key(3_000 + index);
            publish(&mut journal, send_write(indexed_uuid(index), &key), &key)
        })
        .collect::<Vec<_>>();
    let live_key = fixture_key(3_006);
    publish(
        &mut journal,
        send_write(indexed_uuid(6), &live_key),
        &live_key,
    );
    let mut other_write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin(),
        instant(),
        Producer::CommandDoor,
    );
    let other_key = fixture_key(3_007);
    let mut body = sent(indexed_uuid(7), &other_key);
    body.recipient = Actor::Lane(other);
    body.text = "another lane's retired direction".to_owned();
    let pending = other_write
        .fact(body.clone(), FactCause::Root)
        .expect("other-lane fact builds");
    let head_key = other_key.clone();
    other_write.steering_head(pending, move |seq| live_head(&body, head_key, seq));
    let other_head = publish(&mut journal, other_write, &other_key);

    // Retire out of creation order so retirement order is observable.
    let order = [
        (3, Disposition::Acked),
        (0, Disposition::Declined),
        (5, Disposition::Lapsed),
        (1, Disposition::DeadLettered),
        (4, Disposition::Canceled),
        (2, Disposition::Acked),
    ];
    let mut retirements = Vec::new();
    for (index, disposition) in order {
        journal
            .apply(retire_write(&heads[index], disposition))
            .expect("retirement applies");
        retirements.push((heads[index].body().key.clone(), disposition));
    }
    journal
        .apply(retire_write(&other_head, Disposition::Canceled))
        .expect("other-lane retirement applies");

    let scoped = journal
        .read(|read| read.retired_directions(lane(), WINDOW))
        .expect("retired directions read");
    assert_eq!(
        retired_summary(&scoped),
        retirements
            .iter()
            .rev()
            .take(WINDOW)
            .cloned()
            .collect::<Vec<_>>()
    );
    let other_scoped = journal
        .read(|read| read.retired_directions(other, WINDOW))
        .expect("other lane's retired directions read");
    assert_eq!(
        retired_summary(&other_scoped),
        vec![(other_key, Disposition::Canceled)]
    );
    assert_eq!(other_scoped[0].text, other_head.body().text);
    assert_eq!(other_scoped[0].published, other_head.body().published);
    let whole_lane = journal
        .read(|read| read.retired_directions(lane(), usize::from(u16::MAX)))
        .expect("whole-lane retired directions read");
    assert_eq!(
        retired_summary(&whole_lane),
        retirements.iter().rev().cloned().collect::<Vec<_>>(),
        "every retired disposition decodes and the live directive never appears"
    );
    assert!(
        journal
            .read(|read| read.retired_directions(lane(), 0))
            .expect("empty window reads")
            .is_empty()
    );
}

#[test]
fn head_reads_take_message_content_from_the_creating_fact() {
    let mut journal = Journal::in_memory().expect("steering journal");
    let imaged_key = fixture_key(4_000);
    let mut imaged = sent(indexed_uuid(0), &imaged_key);
    imaged.text = "look at this diagram ".repeat(64);
    imaged.attachments = vec![image_attachment()];
    journal
        .apply(publication(imaged.clone()))
        .expect("operator send with an image applies");
    let peer_key = fixture_key(4_001);
    let peer = peer_message(indexed_uuid(1), &peer_key);
    journal
        .apply(peer_write(peer.clone()))
        .expect("peer message applies");
    let expected = [
        (
            imaged_key.as_str(),
            imaged.text.clone(),
            imaged.attachments.clone(),
        ),
        (peer_key.as_str(), peer.text.clone(), Vec::new()),
    ];

    let assert_content = |journal: &Journal, label: &str| {
        for (key, text, attachments) in &expected {
            let head = journal
                .read(|read| read.steering_key(key))
                .expect("keyed head reads")
                .expect("keyed head exists");
            assert_eq!(&head.body().text, text, "{label}: {key} keyed text");
            assert_eq!(
                &head.body().attachments,
                attachments,
                "{label}: {key} keyed attachments"
            );
            let stored = stored_body(journal, key);
            assert!(
                stored.text.is_empty() && stored.attachments.is_empty(),
                "{label}: {key} row holds message content"
            );
        }
    };
    assert_content(&journal, "live");
    let page = journal
        .read(|read| read.pending_steering(None, FactPageStart::After(None)))
        .expect("live page reads");
    let paged = page
        .rows()
        .iter()
        .map(|head| {
            (
                head.body().key.as_str(),
                head.body().text.clone(),
                head.body().attachments.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        paged,
        expected
            .iter()
            .map(|(key, text, attachments)| (*key, text.clone(), attachments.clone()))
            .collect::<Vec<_>>()
    );

    for (key, disposition) in [
        (imaged_key.as_str(), Disposition::Acked),
        (peer_key.as_str(), Disposition::Canceled),
    ] {
        let head = journal
            .read(|read| read.steering_key(key))
            .expect("live head reads")
            .expect("live head exists");
        journal
            .apply(retire_write(&head, disposition))
            .expect("retirement applies");
    }
    assert_content(&journal, "retired");
    let retired = journal
        .read(|read| read.retired_directions(lane(), usize::from(u16::MAX)))
        .expect("retired directions read");
    for (key, text, _) in &expected {
        let row = retired
            .iter()
            .find(|row| row.key == *key)
            .expect("retired row present");
        assert_eq!(&row.text, text, "{key} retired text");
    }
    journal.verify().expect("content-free rows verify");
}

#[test]
fn standing_changes_write_rows_independent_of_message_size() {
    type Transition = fn(&JournalHead<SteeringHead>) -> JournalWrite;
    let mut journal = Journal::in_memory().expect("steering journal");
    let transitions: [(&str, Transition); 4] = [
        ("ack", |head| retire_write(head, Disposition::Acked)),
        ("remind", |head| escalation_batch(head, BatchHead::Final)),
        ("cancel", |head| retire_write(head, Disposition::Canceled)),
        ("lapse", |head| retire_write(head, Disposition::Lapsed)),
    ];
    for (index, (label, transition)) in transitions.into_iter().enumerate() {
        let mut sizes = Vec::new();
        for (offset, repeat) in [(0, 1), (1, 512)] {
            let key = fixture_key(5_000 + index * 2 + offset);
            let mut body = sent(indexed_uuid(index * 2 + offset), &key);
            body.text = "words ".repeat(repeat);
            if repeat > 1 {
                body.attachments = vec![image_attachment()];
            }
            journal.apply(publication(body)).expect("send applies");
            let head = journal
                .read(|read| read.steering_key(&key))
                .expect("live head reads")
                .expect("live head exists");
            journal
                .apply(transition(&head))
                .expect("transition applies");
            sizes.push(stored_bytes(&journal, &key).len());
        }
        assert_eq!(sizes[0], sizes[1], "{label} row size follows message size");
    }
    journal.verify().expect("transitioned rows verify");
}

fn publication(body: SteerSent) -> JournalWrite {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin(),
        instant(),
        Producer::CommandDoor,
    );
    let pending = write
        .fact(body.clone(), FactCause::Root)
        .expect("sent fact builds");
    write.steering_head(pending, move |seq| {
        let key = body.key.clone();
        live_head(&body, key, seq)
    });
    write
}

fn peer_message(directive: CanonicalUuid, key: &str) -> wire::body::SteerMessaged {
    wire::body::SteerMessaged {
        directive,
        key: key.to_owned(),
        lane: lane(),
        occupant: uuid("018f82d4-6f59-7d06-92e3-bce913116307"),
        from: uuid("018f82d4-6f59-7d06-92e3-bce913116308"),
        sender: uuid("018f82d4-6f59-7d06-92e3-bce913116309"),
        text: "a peer's own words".to_owned(),
        locus: TranscriptLocus {
            source: Some(uuid("018f82d4-6f59-7d06-92e3-bce913116306")),
            member: 1,
            offset: 0,
            length: 18,
            event: 0,
            line: 1,
            column: 1,
        },
        span: wire::body::BodySpan::new(0.."a peer's own words".len(), "a peer's own words"),
        room: None,
        addressed: None,
        line: None,
        peer_body: None,
        covered: Vec::new(),
        admission: None,
        expires: "2030-04-12T19:44:09.271Z".parse().expect("expiry"),
    }
}

fn peer_write(message: wire::body::SteerMessaged) -> JournalWrite {
    let mut write = JournalWrite::new(Actor::Spice, origin(), instant(), Producer::AckScanner);
    let pending = write
        .fact(message.clone(), FactCause::Root)
        .expect("peer message builds");
    write.steering_head(pending, move |seq| {
        let sent = message.as_sent();
        live_head(&sent, sent.key.clone(), seq)
    });
    write
}

fn stored_bytes(journal: &Journal, key: &str) -> Vec<u8> {
    journal
        .connection
        .query_row(
            "SELECT body FROM steering_heads WHERE steer_key = ?1",
            [key],
            |row| row.get(0),
        )
        .expect("stored row reads")
}

fn stored_body(journal: &Journal, key: &str) -> SteeringHead {
    wire::decode(&stored_bytes(journal, key)).expect("stored row decodes")
}

#[test]
fn live_steering_pages_bound_rows_decode_work_and_preserve_continuation_order() {
    let mut journal = Journal::in_memory().expect("steering journal");
    let principal = Actor::TEST_PRINCIPAL
        .principal()
        .expect("test actor is a principal");
    let roles = std::collections::BTreeSet::from([wire::principal::PrincipalRole::Operator]);
    let mut admission = JournalWrite::new(Actor::Spice, origin(), instant(), Producer::CommandDoor);
    let born = admission
        .fact(
            wire::principal::PrincipalAdmitted {
                principal,
                profile: wire::principal::PrincipalProfile::default(),
                roles: roles.clone(),
                capability: None,
            },
            FactCause::Root,
        )
        .expect("principal admission builds");
    admission.principal_head(born, move |born| wire::principal::PrincipalHead {
        principal,
        born,
        profile: wire::principal::PrincipalProfile::default(),
        roles,
        capability: None,
    });
    journal
        .apply(admission)
        .expect("operator principal admission applies");
    let total = MAX_STEERING_PAGE_ROWS + 3;
    for index in 0..total {
        let key = fixture_key(6_000 + index);
        journal
            .apply(send_write(indexed_uuid(index), &key))
            .expect("live steering publication applies");
    }
    for index in 0..total {
        let key = fixture_key(7_000 + index);
        journal
            .apply(send_write_for(
                indexed_uuid(total + index),
                &key,
                indexed_uuid(total * 2 + index),
            ))
            .expect("another lane's live steering publication applies");
    }

    crate::guard::reset_indexed_read_metrics();
    let first = journal
        .read(|read| read.pending_steering(Some(&Actor::Lane(lane())), FactPageStart::After(None)))
        .expect("first bounded page reads");
    let first_metrics = crate::guard::indexed_read_metrics();
    let overflow = first
        .overflow()
        .expect("large live set exposes continuation");
    assert_eq!(first.rows().len(), MAX_STEERING_PAGE_ROWS);
    assert_eq!(overflow.omitted, 3);
    assert_eq!(
        first.horizon().map(Seq::get),
        Some(Seq::FIRST.get() + (total * 2) as u64)
    );
    assert_eq!(
        first_metrics.queries, 2,
        "index horizon plus one page query"
    );
    assert_eq!(
        first_metrics.rows,
        u64::try_from(MAX_STEERING_PAGE_ROWS + 2).expect("fixed page metrics fit"),
        "index horizon, carried rows, and one continuation row"
    );

    crate::guard::reset_indexed_read_metrics();
    let operator = journal
        .read(|read| read.pending_operator_steering(FactPageStart::After(None)))
        .expect("operator page reads");
    assert_eq!(operator.rows(), first.rows());
    assert_eq!(
        operator.overflow().expect("all lanes overflow").omitted,
        u64::try_from(total * 2 - MAX_STEERING_PAGE_ROWS).expect("omitted count fits")
    );
    let operator_metrics = crate::guard::indexed_read_metrics();
    assert_eq!(
        operator_metrics.queries,
        first_metrics.queries + 1,
        "the operator page first reads the principal role heads"
    );
    assert_eq!(
        operator_metrics.rows,
        first_metrics.rows + 1,
        "the admitted operator adds one decoded role head"
    );
    assert!(
        operator_metrics.decoded_bytes > first_metrics.decoded_bytes,
        "the admitted operator's role head is decoded before the page"
    );

    crate::guard::reset_indexed_read_metrics();
    let all_lanes = journal
        .read(|read| read.pending_steering(None, FactPageStart::After(None)))
        .expect("all-lane page reads");
    assert_eq!(all_lanes.rows(), first.rows());
    assert_eq!(all_lanes.overflow(), operator.overflow());
    assert_eq!(crate::guard::indexed_read_metrics(), first_metrics);

    let next = FactPageStart::At(overflow.continuation);
    let second = journal
        .read(|read| read.pending_steering(Some(&Actor::Lane(lane())), next))
        .expect("continuation page reads");
    assert_eq!(second.rows().len(), 3);
    assert!(second.overflow().is_none());
    let positions = first
        .rows()
        .iter()
        .chain(second.rows())
        .map(JournalHead::position)
        .collect::<Vec<_>>();
    assert_eq!(positions.len(), total);
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(second.rows()[0].body().created, overflow.continuation);
}

fn measured_key(journal: &Journal) -> crate::guard::IndexedReadMetrics {
    crate::guard::reset_indexed_read_metrics();
    let head = journal
        .read(|read| read.steering_key(TARGET_KEY))
        .expect("target key reads")
        .expect("target head exists");
    assert_eq!(head.body().directive, uuid(TARGET));
    crate::guard::indexed_read_metrics()
}

fn measured_lane(journal: &Journal) -> crate::guard::IndexedReadMetrics {
    crate::guard::reset_indexed_read_metrics();
    let pending = journal
        .read(|read| read.pending_steering(Some(&Actor::Lane(lane())), FactPageStart::After(None)))
        .expect("live lane reads");
    assert_eq!(pending.rows().len(), 1);
    assert_eq!(pending.rows()[0].body().directive, uuid(TARGET));
    crate::guard::indexed_read_metrics()
}

fn measured_all_lanes(journal: &Journal) -> crate::guard::IndexedReadMetrics {
    crate::guard::reset_indexed_read_metrics();
    let pending = journal
        .read(|read| read.pending_steering(None, FactPageStart::After(None)))
        .expect("all live lanes read");
    assert_eq!(pending.rows().len(), 1);
    assert_eq!(pending.rows()[0].body().directive, uuid(TARGET));
    crate::guard::indexed_read_metrics()
}

#[derive(Clone, Copy)]
enum BatchHead {
    Missing,
    Earlier,
    Final,
    Duplicate,
    Stray,
}

fn escalation_batch(observed: &JournalHead<SteeringHead>, placement: BatchHead) -> JournalWrite {
    let at = instant();
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        observed.body().origin.clone(),
        at,
        Producer::BusTimer,
    );
    let mut next = observed.body().clone();
    let mut facts = Vec::new();
    for stage in [SteeringStage::Reminded, SteeringStage::Escalated] {
        let pending = write
            .fact(
                SteerEscalated {
                    directive: observed.body().directive,
                    key: observed.body().key.clone(),
                    recipient: observed.body().recipient.clone(),
                    stage,
                },
                FactCause::Existing(observed.body().created),
            )
            .expect("escalation fact builds");
        facts.push(pending);
        next.stage = stage;
        next.escalations.push(SteeringTransition { stage, at });
    }
    let first = facts[0];
    let final_fact = facts[1];
    match placement {
        BatchHead::Missing => {}
        BatchHead::Earlier => write.steering_head(first, move |_| next),
        BatchHead::Final => write.steering_head(final_fact, move |_| next),
        BatchHead::Duplicate => {
            let duplicate = next.clone();
            write.steering_head(final_fact, move |_| next);
            write.steering_head(final_fact, move |_| duplicate);
        }
        BatchHead::Stray => {
            let final_head = next.clone();
            write.steering_head(final_fact, move |_| final_head);
            let ordinary = write
                .fact(
                    LaneHeld {
                        lane: lane(),
                        evidence: "stray steering head target".to_owned(),
                        until: at,
                        vendor_failure: None,
                    },
                    FactCause::Root,
                )
                .expect("ordinary fact builds");
            write.steering_head(ordinary, move |_| next);
        }
    }
    write
}

fn send_write(directive: CanonicalUuid, key: &str) -> JournalWrite {
    send_write_for(directive, key, lane())
}

pub(super) fn send_write_for(
    directive: CanonicalUuid,
    key: &str,
    lane: CanonicalUuid,
) -> JournalWrite {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin(),
        instant(),
        Producer::CommandDoor,
    );
    let mut body = sent(directive, key);
    body.recipient = Actor::Lane(lane);
    let pending = write
        .fact(body.clone(), FactCause::Root)
        .expect("sent fact builds");
    let key = key.to_owned();
    write.steering_head(pending, move |seq| live_head(&body, key, seq));
    write
}

fn indexed_uuid(index: usize) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{:012x}", index + 0x100)
        .parse()
        .expect("indexed fixture UUID")
}
fn fixture_key(index: usize) -> String {
    let offset = i64::try_from(index).expect("fixture index fits");
    wire::TaskStamp::for_micros(1_700_000_000_000_000 + offset)
        .expect("fixture steering key")
        .to_string()
}

fn cancel_write(observed: &JournalHead<SteeringHead>) -> JournalWrite {
    retire_write(observed, Disposition::Canceled)
}

pub(super) fn retire_write(
    observed: &JournalHead<SteeringHead>,
    disposition: Disposition,
) -> JournalWrite {
    let expected = observed.clone();
    let (actor, producer) = match disposition {
        Disposition::Acked | Disposition::Declined => {
            (observed.body().recipient.clone(), Producer::AckScanner)
        }
        Disposition::Lapsed => (Actor::Spice, Producer::BusTimer),
        Disposition::DeadLettered => (Actor::Spice, Producer::ResidentRecognition),
        Disposition::Canceled | Disposition::Live => (Actor::TEST_PRINCIPAL, Producer::CommandDoor),
    };
    let mut write = JournalWrite::new(actor, origin(), instant(), producer);
    write.guard(JournalGuard::new(move |read| {
        let current = read
            .steering_head(expected.body().directive)?
            .ok_or_else(|| crate::GuardError::refused("directive disappeared"))?;
        if current.position() != expected.position()
            || current.body().disposition != Disposition::Live
        {
            return Err(crate::GuardError::refused("directive standing moved"));
        }
        Ok(())
    }));
    let head = observed.body();
    let (directive, key, lane) = (head.directive, head.key.clone(), head.recipient.clone());
    let cause = FactCause::Existing(observed.position());
    let locus = TranscriptLocus {
        source: Some(uuid("018f82d4-6f59-7d06-92e3-bce913116306")),
        member: 1,
        offset: 0,
        length: 1,
        event: 0,
        line: 1,
        column: 1,
    };
    let epoch = uuid("018f82d4-6f59-7d06-92e3-bce913116305");
    let pending = match disposition {
        Disposition::Acked => write.fact(
            SteerAcked {
                directive,
                key,
                recipient: lane,
                evidence: wire::body::SteeringEvidence::Transcript { epoch, locus },
            },
            cause,
        ),
        Disposition::Declined => write.fact(
            SteerDeclined {
                directive,
                key,
                recipient: lane,
                evidence: wire::body::SteeringEvidence::Transcript { epoch, locus },
                reason: "cannot".to_owned(),
            },
            cause,
        ),
        Disposition::Lapsed => write.fact(
            SteerLapsed {
                key,
                recipient: lane,
                expires: head.expires,
            },
            cause,
        ),
        Disposition::DeadLettered => write.fact(
            wire::steering::SteerDeadLettered {
                directive,
                key,
                lane: lane.lane().expect("dead-letter fixture is lane-addressed"),
                reason: "vacant".to_owned(),
                requeue: "spice steer recover".to_owned(),
            },
            cause,
        ),
        Disposition::Canceled => write.fact(
            SteerCanceled {
                key,
                recipient: lane,
                reason: None,
            },
            cause,
        ),
        Disposition::Live => panic!("retirement needs a terminal disposition"),
    }
    .expect("retirement fact builds");
    let mut next = head.clone();
    next.disposition = disposition;
    write.steering_head(pending, move |_| next);
    write
}

fn image_attachment() -> wire::Media {
    wire::Media {
        name: "diagram.png".to_owned(),
        content_type: "image/png".to_owned(),
        data_url: "data:image/png;base64,aW1hZ2U=".to_owned(),
    }
}

pub(super) fn sent(directive: CanonicalUuid, key: &str) -> SteerSent {
    SteerSent {
        sender: Actor::TEST_PRINCIPAL,
        room: None,
        directive,
        fanout: None,
        recipients: Vec::new(),
        key: key.to_owned(),
        recipient: Actor::Lane(lane()),
        text: "hold the indexed line".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires: "2030-04-12T19:44:09.271Z".parse().expect("expiry"),
    }
}

pub(super) fn live_head(body: &SteerSent, key: String, created: Seq) -> SteeringHead {
    let minted_micros = key
        .parse::<wire::TaskStamp>()
        .expect("current steering key")
        .micros();
    SteeringHead {
        sender: body.sender.clone(),
        room: body.room.clone(),
        directive: body.directive,
        fanout: body.fanout,
        recipients: Vec::new(),
        key,
        recipient: body.recipient.clone(),
        text: body.text.clone(),
        control: body.control.clone(),
        attachments: body.attachments.clone(),
        published: instant(),
        expires: body.expires,
        stage: SteeringStage::Published,
        escalations: Vec::new(),
        disposition: Disposition::Live,
        origin: origin(),
        created,
        minted_micros,
    }
}

fn lane() -> CanonicalUuid {
    uuid(LANE)
}

pub(super) fn origin() -> Origin {
    "resident:018f82d4-6f59-7d06-92e3-bce913116304"
        .parse()
        .expect("fixture origin")
}
