use super::*;
use wire::Instant;

const LANE_A: &str = "11111111-1111-4111-8111-111111111111";
const LANE_B: &str = "22222222-2222-4222-8222-222222222222";
const OCCUPANT: &str = "33333333-3333-4333-8333-333333333333";
const EPOCH: &str = "44444444-4444-4444-8444-444444444444";
const ID_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const ID_B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const ID_C: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const FANOUT: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

fn uuid(value: &str) -> CanonicalUuid {
    value.parse().expect("fixture UUID is canonical")
}

fn numbered(value: usize) -> CanonicalUuid {
    format!("00000000-0000-4000-8000-{value:012x}")
        .parse()
        .expect("numbered fixture UUID is canonical")
}

fn draft(body: &str) -> ComposerDraft {
    ComposerDraft {
        body: body.to_owned(),
        ..ComposerDraft::default()
    }
}

fn submission(id: &str, lane: &str, body: &str) -> Submission {
    Submission::new(
        uuid(id),
        uuid(id),
        uuid(lane),
        (uuid(OCCUPANT), uuid(EPOCH), 1_000),
        body.to_owned(),
        Vec::new(),
        draft(body),
    )
}

fn vacant_submission(id: &str, lane: &str, body: &str) -> Submission {
    Submission::vacant(
        uuid(id),
        uuid(id),
        uuid(lane),
        (uuid(EPOCH), 1_000),
        body.to_owned(),
        Vec::new(),
        draft(body),
    )
}

fn broadcast(index: usize, lane: CanonicalUuid, payload: &Rc<SubmissionPayload>) -> Submission {
    Submission::broadcast(
        numbered(index),
        uuid(FANOUT),
        lane,
        (Some(uuid(OCCUPANT)), uuid(EPOCH)),
        Rc::clone(payload),
        None,
        ComposerSource::Lane(uuid(LANE_A)),
    )
}

fn accept(queue: &mut SubmissionQueue, id: &str, key: &str, disposition: Disposition) {
    queue.sent(uuid(id)).unwrap();
    queue
        .accept(uuid(id), uuid(id), key.to_owned(), disposition)
        .unwrap();
}

fn pending(id: &str, key: &str) -> PendingDirective {
    PendingDirective {
        from: None,
        sender: None,
        principal: None,
        room: None,
        addressed: None,
        wakes: true,
        fanout: None,
        cc: Vec::new(),
        directive: uuid(id),
        key: key.to_owned(),
        text: "durable".to_owned(),
        preview: Some("durable".to_owned()),
        control: None,
        line: None,
        attachments: Vec::new(),
        authored: "2030-01-01T00:00:00.000Z".parse::<Instant>().unwrap(),
        stage: wire::body::SteeringStage::Published,
        escalations: Vec::new(),
        expires: "2030-01-01T00:05:00.000Z".parse::<Instant>().unwrap(),
    }
}

#[test]
fn admission_bounds_batches_atomically_and_counts_one_shared_fanout_payload() {
    let payload = Submission::payload(
        uuid(FANOUT),
        1_000,
        "bounded".to_owned(),
        Vec::new(),
        Vec::new(),
    );
    let batch = (0..SUBMISSION_LIMIT)
        .map(|index| broadcast(1_000 + index, numbered(2_000 + index), &payload))
        .collect();
    let mut queue = SubmissionQueue::default();
    queue.enqueue_all(batch).unwrap();
    assert_eq!(
        (queue.status().len(), queue.identities.len()),
        (SUBMISSION_LIMIT, SUBMISSION_LIMIT)
    );
    assert_eq!(queue.payloads[&uuid(FANOUT)].records, SUBMISSION_LIMIT);
    assert!(Rc::ptr_eq(&queue.payloads[&uuid(FANOUT)].payload, &payload));
    assert_eq!(queue.ready()[0].message(), "bounded");
    assert!(queue.ready()[0].attachments().is_empty());
    assert_eq!(
        queue.source(numbered(1_017)).unwrap(),
        ComposerSource::Lane(uuid(LANE_A))
    );
    let retained = queue.retained_bytes;
    assert!(
        queue
            .enqueue(broadcast(9_000, numbered(9_001), &payload))
            .is_err()
    );
    assert_eq!(
        (queue.status().len(), queue.retained_bytes),
        (SUBMISSION_LIMIT, retained)
    );

    let mut lane = SubmissionQueue::default();
    for index in 0..LANE_SUBMISSION_LIMIT {
        lane.enqueue(broadcast(10_000 + index, uuid(LANE_A), &payload))
            .unwrap();
    }
    assert!(lane.enqueue(submission(ID_A, LANE_A, "overflow")).is_err());

    let mut bytes = SubmissionQueue {
        retained_bytes: RETAINED_BYTES_LIMIT,
        ..SubmissionQueue::default()
    };
    assert!(bytes.enqueue(submission(ID_A, LANE_A, "overflow")).is_err());
}

#[test]
fn indexed_fifo_replays_exact_commands_and_refusal_drops_the_lane_tail() {
    let mut queue = SubmissionQueue::default();
    queue.enqueue(submission(ID_A, LANE_A, "first")).unwrap();
    let mut second = submission(ID_B, LANE_A, "second");
    second.fanout = uuid(ID_A);
    queue.enqueue(second).unwrap();
    queue.enqueue(submission(ID_C, LANE_B, "sibling")).unwrap();
    queue.sent(uuid(ID_A)).unwrap();
    queue.sent(uuid(ID_C)).unwrap();
    assert!(queue.ready().is_empty());
    queue.reconnect();
    assert_eq!(
        queue
            .ready()
            .iter()
            .map(|row| row.id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([uuid(ID_A), uuid(ID_C)])
    );

    accept(&mut queue, ID_A, "1kFirst", Disposition::Live);
    assert_eq!(queue.ready()[0].id, uuid(ID_B));
    queue.sent(uuid(ID_B)).unwrap();
    queue.refuse(uuid(ID_B), "refused").unwrap();
    assert_eq!(queue.rollback(uuid(ID_C)).unwrap().snapshot.body, "sibling");
    assert_eq!(queue.identities.len(), 1);
}

#[test]
fn peer_direction_counts_as_pending_but_only_operator_sends_wake_the_lane() {
    let mut queue = SubmissionQueue::default();
    queue.enqueue(submission(ID_A, LANE_A, "first")).unwrap();
    queue.enqueue(submission(ID_B, LANE_A, "second")).unwrap();
    accept(&mut queue, ID_A, "1kKey", Disposition::Live);
    let status = queue.status();
    let mut peer = pending(ID_C, "1kPeer");
    peer.wakes = false;
    let backend = [peer, pending(ID_A, "1kKey")];

    // The backend row covers the accepted send; the queued one is still
    // the operator's own prediction and wakes the lane like its row will.
    assert_eq!(status.pending(uuid(LANE_A), &backend), 3);
    assert_eq!(status.waking(uuid(LANE_A), &backend), 2);
    // A lane holding only a peer reply is pending but not queued.
    let idle = SubmissionQueue::default().status();
    assert_eq!(idle.pending(uuid(LANE_A), &backend[..1]), 1);
    assert_eq!(idle.waking(uuid(LANE_A), &backend[..1]), 0);
}

#[test]
fn backend_keys_reconcile_accepted_predictions_without_losing_queued_work() {
    let mut queue = SubmissionQueue::default();
    queue.enqueue(submission(ID_A, LANE_A, "first")).unwrap();
    queue.enqueue(submission(ID_B, LANE_A, "second")).unwrap();
    accept(&mut queue, ID_A, "1kKey", Disposition::Live);
    assert_eq!(
        queue.status().awaiting(),
        1,
        "only the queued send awaits acceptance"
    );
    assert_eq!(queue.accepted()[&uuid(ID_A)], "1kKey");
    queue
        .observe_backend([pending(ID_A, "1kKey")].iter(), &BTreeMap::new())
        .unwrap();
    assert_eq!(
        queue.activity_read_requirement(),
        None,
        "a directive still pending does not imply lane activity changed"
    );
    assert_eq!(queue.status().len(), 1);
    assert_eq!(queue.ready()[0].id, uuid(ID_B));

    accept(&mut queue, ID_B, "1kDone", Disposition::Acked);
    assert!(queue.identities.is_empty());
}

#[test]
fn a_consumed_accepted_submission_requires_newer_lane_activity() {
    let mut queue = SubmissionQueue::default();
    queue.enqueue(submission(ID_A, LANE_A, "start")).unwrap();
    accept(&mut queue, ID_A, "1kStarted", Disposition::Live);
    let coalesced = queue.activity_read_requirement();

    queue
        .observe_backend(
            std::iter::empty(),
            &BTreeMap::from([(uuid(ID_A), "1kStarted".to_owned())]),
        )
        .unwrap();

    let causal = queue.activity_read_requirement();
    assert!(causal.is_some());
    queue.activity_read_started(coalesced);
    assert_eq!(
        queue.activity_read_requirement(),
        causal,
        "a lane read started before inbox acceptance cannot satisfy the causal refresh"
    );
    queue.activity_read_started(causal);
    assert_eq!(queue.activity_read_requirement(), None);
    assert!(queue.identities.is_empty());
}

#[test]
fn vacant_submission_sends_directly_replays_and_rebases_its_fifo_tail() {
    let mut queue = SubmissionQueue::default();
    queue
        .enqueue(vacant_submission(ID_A, LANE_A, "first"))
        .unwrap();
    queue
        .enqueue(vacant_submission(ID_B, LANE_A, "second"))
        .unwrap();
    assert_eq!(queue.ready()[0].residency(), Some((None, uuid(EPOCH))));

    queue.sent(uuid(ID_A)).unwrap();
    assert!(queue.ready().is_empty());
    queue.reconnect();
    assert_eq!(queue.ready()[0].id, uuid(ID_A));

    accept(&mut queue, ID_A, "1kFirst", Disposition::Live);
    assert!(queue.ready().is_empty());
    let first_read = queue.awaiting_residency();
    assert_eq!(first_read, [uuid(ID_B)]);
    queue
        .enqueue(vacant_submission(ID_C, LANE_A, "third"))
        .unwrap();
    queue.rebase_awaiting_lane(&first_read, uuid(LANE_A), None, uuid(EPOCH));
    assert_eq!(queue.ready()[0].id, uuid(ID_B));

    accept(&mut queue, ID_B, "dWKbTlwPq", Disposition::Live);
    assert!(queue.ready().is_empty());
    let second_read = queue.awaiting_residency();
    assert_eq!(second_read, [uuid(ID_C)]);
    queue.rebase_awaiting_lane(
        &second_read,
        uuid(LANE_A),
        Some(uuid(OCCUPANT)),
        uuid(EPOCH),
    );
    assert_eq!(
        queue.ready()[0].residency(),
        Some((Some(uuid(OCCUPANT)), uuid(EPOCH)))
    );
    queue.sent(uuid(ID_C)).unwrap();
    queue.refuse(uuid(ID_C), "refused").unwrap();
    assert_eq!(queue.lane_for(uuid(ID_C)), None);
}

fn lane(name: &str) -> ComposerSource {
    ComposerSource::Lane(uuid(name))
}

/// The content a send locks its composer with, checked against both the
/// rendered lock and the submit-time duplicate refusal.
fn held(queue: &SubmissionQueue, id: &str, source: ComposerSource) -> Option<ComposerDraft> {
    let content = queue.held_content(uuid(id)).map(|(composer, sent)| {
        assert_eq!(composer, source);
        (*sent).clone()
    });
    assert_eq!(queue.holds(source), content.is_some());
    assert_eq!(queue.status().holds(source), content.is_some());
    content
}

#[test]
fn a_direct_send_holds_its_exact_composer_until_durable_acceptance() {
    let mut queue = SubmissionQueue::default();
    queue.enqueue(submission(ID_A, LANE_A, "ship it")).unwrap();
    assert_eq!(held(&queue, ID_A, lane(LANE_A)), Some(draft("ship it")));
    queue.sent(uuid(ID_A)).unwrap();
    // A dropped socket replays the same send; it settles nothing.
    queue.reconnect();
    assert_eq!(held(&queue, ID_A, lane(LANE_A)), Some(draft("ship it")));
    assert!(!queue.holds(lane(LANE_B)));
    assert!(!queue.holds(ComposerSource::Creator));
    accept(&mut queue, ID_A, "1kShip", Disposition::Live);
    assert_eq!(held(&queue, ID_A, lane(LANE_A)), None);
}

#[test]
fn every_unaccepted_settlement_unlocks_the_composer_without_clearing_it() {
    let mut refused = SubmissionQueue::default();
    refused
        .enqueue(submission(ID_A, LANE_A, "refused"))
        .unwrap();
    refused.sent(uuid(ID_A)).unwrap();
    refused.refuse(uuid(ID_A), "refused").unwrap();
    assert_eq!(held(&refused, ID_A, lane(LANE_A)), None);

    // The wake never reached a connection.
    let mut disconnected = SubmissionQueue::default();
    disconnected
        .enqueue(submission(ID_A, LANE_A, "offline"))
        .unwrap();
    let rolled_back = disconnected.rollback(uuid(ID_A)).unwrap();
    assert_eq!(*rolled_back.snapshot, draft("offline"));
    assert_eq!(held(&disconnected, ID_A, lane(LANE_A)), None);

    // A vacant seat's tail whose fresh lane read found no target.
    let mut stale = SubmissionQueue::default();
    stale
        .enqueue(vacant_submission(ID_A, LANE_A, "wake"))
        .unwrap();
    stale
        .enqueue(vacant_submission(ID_B, LANE_A, "stale"))
        .unwrap();
    accept(&mut stale, ID_A, "1kWake", Disposition::Live);
    assert_eq!(held(&stale, ID_B, lane(LANE_A)), Some(draft("stale")));
    stale
        .refuse_awaiting_lane(uuid(LANE_A), &[uuid(ID_B)], "unreachable")
        .unwrap();
    assert_eq!(held(&stale, ID_B, lane(LANE_A)), None);
}

#[test]
fn a_broadcast_holds_only_its_source_composer_and_settles_by_the_source_record() {
    let payload = Submission::payload(uuid(FANOUT), 1_000, "go".to_owned(), Vec::new(), Vec::new());
    let residency = (Some(uuid(OCCUPANT)), uuid(EPOCH));
    let source = Submission::broadcast(
        uuid(ID_A),
        uuid(FANOUT),
        uuid(LANE_A),
        residency,
        Rc::clone(&payload),
        Some(draft("go")),
        lane(LANE_A),
    );
    let peer = Submission::broadcast(
        uuid(ID_B),
        uuid(FANOUT),
        uuid(LANE_B),
        residency,
        Rc::clone(&payload),
        None,
        lane(LANE_A),
    );

    let mut accepted = SubmissionQueue::default();
    accepted
        .enqueue_all(vec![source.clone(), peer.clone()])
        .unwrap();
    assert_eq!(held(&accepted, ID_A, lane(LANE_A)), Some(draft("go")));
    assert_eq!(held(&accepted, ID_B, lane(LANE_B)), None);
    accept(&mut accepted, ID_B, "1kPeer", Disposition::Live);
    assert_eq!(
        held(&accepted, ID_A, lane(LANE_A)),
        Some(draft("go")),
        "a peer's acceptance does not settle the source composer"
    );
    accept(&mut accepted, ID_A, "dWKbTlwPr", Disposition::Live);
    assert_eq!(held(&accepted, ID_A, lane(LANE_A)), None);

    let mut refused = SubmissionQueue::default();
    refused.enqueue_all(vec![source, peer]).unwrap();
    refused.sent(uuid(ID_A)).unwrap();
    refused.refuse(uuid(ID_A), "refused").unwrap();
    assert_eq!(held(&refused, ID_A, lane(LANE_A)), None);
}

fn several_agents(lanes: &[(&str, &str)]) -> Vec<Submission> {
    let payload = Submission::payload(
        uuid(FANOUT),
        1_000,
        "all".to_owned(),
        Vec::new(),
        Vec::new(),
    );
    lanes
        .iter()
        .map(|(id, to)| {
            Submission::broadcast(
                uuid(id),
                uuid(FANOUT),
                uuid(to),
                (Some(uuid(OCCUPANT)), uuid(EPOCH)),
                Rc::clone(&payload),
                None,
                ComposerSource::Creator,
            )
        })
        .collect()
}

#[test]
fn a_several_agent_send_holds_its_composer_until_every_branch_settles() {
    let both = [(ID_A, LANE_A), (ID_B, LANE_B)];
    let mut admitted = SubmissionQueue::default();
    admitted
        .enqueue_settled(draft("all"), several_agents(&both))
        .unwrap();
    assert!(admitted.holds(ComposerSource::Creator));
    assert!(admitted.status().holds(ComposerSource::Creator));
    assert!(
        !admitted.holds(lane(LANE_A)) && !admitted.holds(lane(LANE_B)),
        "a creator send leaves every recipient's own composer free"
    );
    assert_eq!(admitted.held_content(uuid(ID_A)), None);
    accept(&mut admitted, ID_A, "1kA", Disposition::Live);
    assert!(
        admitted.status().holds(ComposerSource::Creator),
        "one admitted branch does not settle the send"
    );
    assert_eq!(
        admitted.held_content(uuid(ID_B)),
        Some((ComposerSource::Creator, Rc::new(draft("all")))),
        "the last branch of an all-admitted send clears its content"
    );
    accept(&mut admitted, ID_B, "1kB", Disposition::Live);
    let status = admitted.status();
    assert!(!status.holds(ComposerSource::Creator));
    let outcome = status.settled(ComposerSource::Creator).unwrap();
    assert_eq!(outcome.fanout, uuid(FANOUT));
    assert_eq!(
        outcome.admitted,
        BTreeSet::from([uuid(LANE_A), uuid(LANE_B)])
    );
    assert!(outcome.refused.is_empty());

    let mut partial = SubmissionQueue::default();
    partial
        .enqueue_settled(draft("all"), several_agents(&both))
        .unwrap();
    partial.sent(uuid(ID_A)).unwrap();
    assert!(partial.refuse(uuid(ID_A), "busy").unwrap());
    assert_eq!(
        partial.held_content(uuid(ID_B)),
        None,
        "a send with a refused branch never clears its content"
    );
    accept(&mut partial, ID_B, "1kB", Disposition::Live);
    let status = partial.status();
    assert!(!status.holds(ComposerSource::Creator));
    let outcome = status.settled(ComposerSource::Creator).unwrap();
    assert_eq!(outcome.admitted, BTreeSet::from([uuid(LANE_B)]));
    assert_eq!(
        outcome.refused,
        BTreeMap::from([(uuid(LANE_A), "busy".to_owned())])
    );
}

#[test]
fn a_several_agent_send_names_each_agent_once_and_withdraws_without_an_outcome() {
    let mut queue = SubmissionQueue::default();
    assert!(
        queue
            .enqueue_settled(
                draft("all"),
                several_agents(&[(ID_A, LANE_A), (ID_B, LANE_A)])
            )
            .is_err()
    );
    assert_eq!((queue.identities.len(), queue.retained_bytes), (0, 0));
    assert!(!queue.holds(ComposerSource::Creator));

    queue
        .enqueue_settled(
            draft("all"),
            several_agents(&[(ID_A, LANE_A), (ID_B, LANE_B)]),
        )
        .unwrap();
    queue.rollback(uuid(ID_A)).unwrap();
    queue.rollback(uuid(ID_B)).unwrap();
    let status = queue.status();
    assert!(!status.holds(ComposerSource::Creator));
    assert_eq!(status.settled(ComposerSource::Creator), None);
    assert_eq!(queue.retained_bytes, 0);
}
