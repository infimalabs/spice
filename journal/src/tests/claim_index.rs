//! Bounded current-claim authority and indexed predicate admission.

use super::*;

use crate::{FactCause, JournalWrite};
use wire::body::{ClaimFreed, ClaimWon, SourceBoundary, TaskAdded};
use wire::{ClaimCell, Producer};

#[test]
fn actor_and_active_lane_work_stay_constant_as_inactive_claim_heads_grow() {
    let mut journal = authority_fixture();
    let before = measured_claim_authority(&journal);

    let peers = (1..=64)
        .map(|offset| add_claimed_task(&mut journal, offset, Actor::TEST_PRINCIPAL))
        .collect::<Vec<_>>();
    let mut free = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:claim-index-peer".parse().expect("peer origin"),
        instant(),
        Producer::CommandDoor,
    );
    for task in peers {
        let mut head = journal
            .read(|read| read.task_heads_for(&task))
            .expect("peer claim reads")
            .and_then(|heads| heads.latest_claim().map(|held| held.claim().clone()))
            .expect("peer claim exists")
            .into_body();
        let fact = free
            .fact(
                ClaimFreed {
                    task: task.clone(),
                    actor: Actor::TEST_PRINCIPAL,
                    parked: None,
                },
                FactCause::Existing(head.claimed),
            )
            .expect("peer release fact builds");
        head.disposition = ClaimDisposition::Freed;
        free.claim_head(task, fact, move |position| {
            head.fact = position;
            head
        });
    }
    journal.apply(free).expect("inactive claim heads commit");

    let after = measured_claim_authority(&journal);
    assert_eq!(after, before);
    assert_eq!(
        before.queries, 7,
        "claim, four exact head reads, peer count, and active lanes"
    );
    assert_eq!(before.rows, 7, "only the selected heads and aggregates");
    assert!(before.decoded_bytes > 0, "the selected bundle was decoded");

    let actor_plan = query_plan(
        &journal,
        "SELECT indexed.stamp, indexed.actor, indexed.lane, indexed.disposition, \
                indexed.head_seq, heads.head_seq, heads.body \
         FROM claim_index AS indexed \
         JOIN claim_heads AS heads ON heads.stamp = indexed.stamp \
         WHERE indexed.actor = 'spice' AND indexed.disposition = 'active' \
         ORDER BY indexed.stamp LIMIT 2",
    );
    assert!(
        actor_plan.contains("claim_index_actor_disposition_stamp"),
        "exact actor lookup must use its composite index: {actor_plan}"
    );
    let peer_plan = query_plan(
        &journal,
        "SELECT count(*) FROM claim_index \
         WHERE disposition = 'active' AND actor <> 'spice'",
    );
    assert!(
        peer_plan.contains("claim_index_disposition_actor"),
        "peer count must use the active-disposition index: {peer_plan}"
    );
    let lane_plan = query_plan(
        &journal,
        "SELECT indexed.stamp, indexed.lane, indexed.disposition, \
                indexed.head_seq, heads.head_seq, heads.body \
         FROM claim_index AS indexed \
         JOIN claim_heads AS heads ON heads.stamp = indexed.stamp \
         WHERE indexed.disposition = 'active' \
         ORDER BY indexed.lane, indexed.stamp",
    );
    assert!(
        lane_plan.contains("claim_index_disposition_lane_stamp"),
        "active lane lookup must use its covering order: {lane_plan}"
    );
}

#[test]
fn corruption_outside_the_exact_actor_locus_does_not_poison_the_probe() {
    let mut journal = authority_fixture();
    let peer = add_claimed_task(&mut journal, 900, Actor::TEST_PRINCIPAL);
    journal
        .connection
        .execute(
            "UPDATE claim_heads SET body = ?1 WHERE stamp = ?2",
            params![b"corrupt unrelated claim".to_vec(), peer.as_str()],
        )
        .expect("corrupt only the peer claim body");

    let (own, peers) = journal
        .read(|read| {
            Ok((
                read.active_claim_for_actor(&Actor::Spice)?,
                read.active_claim_count_excluding(&Actor::Spice)?,
            ))
        })
        .expect("out-of-locus corruption is not decoded");
    assert_eq!(
        own.expect("fixture actor claim remains visible")
            .statement()
            .body()
            .title,
        task_head().title
    );
    assert_eq!(peers, 1);
}

#[test]
fn claim_disposition_update_replaces_the_index_predicate_atomically() {
    let mut journal = authority_fixture();
    let peer = add_claimed_task(&mut journal, 901, Actor::TEST_PRINCIPAL);
    assert_eq!(
        journal
            .read(|read| read.active_claim_count_excluding(&Actor::Spice))
            .expect("active peer count reads"),
        1
    );
    let observed = journal
        .read(|read| read.task_heads_for(&peer))
        .expect("peer claim head reads")
        .and_then(|heads| heads.latest_claim().map(|held| held.claim().clone()))
        .expect("peer claim exists")
        .into_body();
    let mut free = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:claim-index-peer".parse().expect("peer origin"),
        instant(),
        Producer::CommandDoor,
    );
    let freed = free
        .fact(
            ClaimFreed {
                task: peer.clone(),
                actor: Actor::TEST_PRINCIPAL,
                parked: None,
            },
            FactCause::Existing(observed.claimed),
        )
        .expect("peer release fact builds");
    let expected_lane = observed.lane;
    let mut inactive = observed;
    inactive.disposition = ClaimDisposition::Freed;
    free.claim_head(peer.clone(), freed, move |position| {
        inactive.fact = position;
        inactive
    });
    journal.apply(free).expect("peer release commits");

    let indexed_lane: String = journal
        .connection
        .query_row(
            "SELECT lane FROM claim_index WHERE stamp = ?1",
            [peer.as_str()],
            |row| row.get(0),
        )
        .expect("indexed claim lane reads");
    assert_eq!(indexed_lane, expected_lane.to_string());
    assert_eq!(
        journal
            .read(|read| read.active_claim_count_excluding(&Actor::Spice))
            .expect("inactive peer count reads"),
        0
    );
}

#[test]
fn verification_names_claim_index_predicate_disagreement() {
    let journal = authority_fixture();
    journal
        .connection
        .execute(
            "UPDATE claim_index SET lane = '018f82d4-6f59-7d06-92e3-bce913116399' \
             WHERE stamp = '1kKRRsDPx'",
            [],
        )
        .expect("corrupt indexed lane fixture");

    let error = journal
        .verify()
        .expect_err("claim index disagreement must fail verification");
    assert!(error.violations().iter().any(|violation| {
        violation.class == ViolationClass::Head
            && violation.subject == format!("claim_index:1kKRRsDPx:{}", sql_seq(2))
    }));
}

fn measured_claim_authority(journal: &Journal) -> crate::guard::IndexedReadMetrics {
    crate::guard::reset_indexed_read_metrics();
    let (own, peers, lanes) = journal
        .read(|read| {
            Ok((
                read.active_claim_for_actor(&Actor::Spice)?,
                read.active_claim_count_excluding(&Actor::Spice)?,
                read.actively_claimed_lanes()?,
            ))
        })
        .expect("bounded claim authority reads");
    assert_eq!(
        own.expect("fixture claim exists").statement().body().born,
        task_head().born
    );
    assert_eq!(peers, 0);
    assert_eq!(lanes, vec![claim_head().lane]);
    crate::guard::indexed_read_metrics()
}

fn add_unclaimed_task(write: &mut JournalWrite, offset: u64) -> TaskStamp {
    let micros = i64::try_from(10_000 + offset).expect("scale task offset fits i64");
    let task = TaskStamp::for_micros(micros).expect("scale task stamp");
    let title = format!("Unrelated board task {offset}");
    let flow = vec![phase("draft"), phase("todo"), phase("review")];
    let fact = write
        .fact(
            TaskAdded {
                preparation: None,
                intent: None,
                task: task.clone(),
                title: title.clone(),
                pitch: "Claims need a statement to index.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: "/journal".parse().expect("fixture topic"),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: flow.clone(),
                phase: phase("todo"),
                creation: wire::body::TaskCreationContext::unavailable(
                    wire::body::TaskCreationUnavailable::PrincipalActor,
                ),
                revision: 1,
            },
            FactCause::Root,
        )
        .expect("unrelated task fact builds");
    let statement_task = task.clone();
    write.task_head(fact, move |revision| TaskHead {
        preparation: None,
        intent: None,
        born: statement_task,
        topic_origin: revision,
        title,
        pitch: "Claims need a statement to index.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: "/journal".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: wire::TaskProvenance::OperatorDraft,
        creation: wire::body::TaskCreationContext::unavailable(
            wire::body::TaskCreationUnavailable::PrincipalActor,
        ),
        revision,
    });
    let workflow_task = task.clone();
    write.workflow_head(workflow_task, fact, move |_| WorkflowHead {
        flow,
        phase: phase("todo"),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    task
}

pub(super) fn add_claimed_task(journal: &mut Journal, offset: u64, actor: Actor) -> TaskStamp {
    let mut add = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:claim-index-peer".parse().expect("peer origin"),
        instant(),
        Producer::CommandDoor,
    );
    let task = add_unclaimed_task(&mut add, offset);
    journal.apply(add).expect("peer task commits");

    let lane = uuid("018f82d4-6f59-7d06-92e3-bce9131163d1");
    let occupant = uuid("018f82d4-6f59-7d06-92e3-bce9131163d2");
    let epoch = uuid("018f82d4-6f59-7d06-92e3-bce9131163d3");
    let mut claim = JournalWrite::new(
        actor.clone(),
        "task:claim-index-peer".parse().expect("peer origin"),
        instant(),
        Producer::CommandDoor,
    );
    let won = claim
        .fact(
            ClaimWon {
                task: task.clone(),
                actor: actor.clone(),
                lane,
                occupant,
                epoch,
                cell: ClaimCell {
                    topic: Some("/journal".parse().expect("fixture topic")),
                    phase: phase("todo"),
                },
                until: instant(),
                git: None,
                recovery: None,
                source: SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Root,
        )
        .expect("peer claim fact builds");
    let claim_task = task.clone();
    claim.claim_head(claim_task, won, move |position| ClaimHead {
        actor,
        fact: position,
        claimed: position,
        cell: ClaimCell {
            topic: Some("/journal".parse().expect("fixture topic")),
            phase: phase("todo"),
        },
        lane,
        occupant,
        epoch,
        recovery: None,
        disposition: ClaimDisposition::Active,
        acknowledged: 0,
    });
    let lease_task = task.clone();
    claim.lease_head(lease_task, won, move |claimed| LeaseHead {
        claimed,
        start: instant(),
        renewal: instant(),
        maximum: instant(),
        expiry: instant(),
        epoch,
    });
    journal.apply(claim).expect("peer claim commits");
    task
}

pub(super) fn query_plan(journal: &Journal, sql: &str) -> String {
    journal
        .connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("query plan prepares")
        .query_map([], |row| row.get::<_, String>(3))
        .expect("query plan rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("query plan decodes")
        .join("\n")
}
