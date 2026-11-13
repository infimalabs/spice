use rusqlite::params;
use wire::{Actor, LaneRenewalRecord, LaneRenewalState, Producer, RenewalIdentity, body};

use crate::{FactCause, JournalWrite};

use super::*;

const TEAM: &str = "018f82d4-6f59-7d06-92e3-bce913116302";
const LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116302";
const PREDECESSOR: &str = "018f82d4-6f59-7d06-92e3-bce913116303";
const SUCCESSOR: &str = "018f82d4-6f59-7d06-92e3-bce913116304";

fn predecessor() -> RenewalIdentity {
    RenewalIdentity {
        occupant: uuid(PREDECESSOR),
        session: "session-predecessor".to_owned(),
        thread: "thread-predecessor".to_owned(),
    }
}

fn successor() -> RenewalIdentity {
    RenewalIdentity {
        occupant: uuid(SUCCESSOR),
        session: "session-successor".to_owned(),
        thread: "thread-successor".to_owned(),
    }
}

fn states() -> Vec<LaneRenewalState> {
    vec![
        LaneRenewalState::Requested {
            predecessor: predecessor(),
            team_slot: 0,
        },
        LaneRenewalState::HandoffPending {
            predecessor: predecessor(),
            team_slot: 0,
        },
        LaneRenewalState::SuccessorStarted {
            predecessor: predecessor(),
            successor: successor(),
            team_slot: 0,
        },
        LaneRenewalState::LaunchRefused {
            predecessor: predecessor(),
            team_slot: 0,
            reason: "selected vendor refused launch".to_owned(),
        },
    ]
}

#[test]
fn unchanged_residency_reuses_one_canonical_decode_and_advancing_heads_invalidate_it() {
    let mut journal = authority_fixture();

    reset_indexed_read_metrics();
    let initial = journal
        .read(|read| read.residency_head())
        .expect("initial residency reads")
        .expect("initial residency exists");
    assert!(indexed_read_metrics().decoded_bytes > 0);

    reset_indexed_read_metrics();
    let repeated = journal
        .read(|read| read.residency_head())
        .expect("unchanged residency reads")
        .expect("unchanged residency exists");
    assert_eq!(repeated, initial);
    assert_eq!(
        indexed_read_metrics(),
        crate::guard::IndexedReadMetrics {
            queries: 1,
            rows: 1,
            decoded_bytes: 0,
        }
    );

    let mut advanced = initial.into_body();
    advanced.topology_revision = 41;
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "resident:up".parse().expect("residency origin"),
        instant(),
        Producer::CommandDoor,
    );
    let transition = write
        .fact(
            body::UpDark {
                epoch: advanced.epoch,
            },
            FactCause::Root,
        )
        .expect("residency transition fact");
    write.residency_head(transition, move |_| advanced);
    journal.apply(write).expect("residency transition commits");

    reset_indexed_read_metrics();
    let changed = journal
        .read(|read| read.residency_head())
        .expect("changed residency reads")
        .expect("changed residency exists");
    assert_eq!(changed.position(), seq(3));
    assert_eq!(changed.body().topology_revision, 41);
    assert!(indexed_read_metrics().decoded_bytes > 0);

    insert_fact(
        &journal,
        fact(
            4,
            Some(3),
            FactKind::TaskPhased,
            json!({"task": "1kKRRsDPx", "phase": "review"}),
        ),
    );
    advance_cursor(&journal, 4);
    let canonical = wire::encode(changed.body()).expect("changed residency encodes");
    let document: serde_json::Value =
        serde_json::from_slice(&canonical).expect("changed residency is JSON");
    let noncanonical = serde_json::to_vec_pretty(&document).expect("pretty residency bytes encode");
    assert_ne!(noncanonical, canonical);
    journal
        .connection
        .execute(
            "UPDATE residency_head SET head_seq = ?1, body = ?2 WHERE singleton = 1",
            params![sql_seq(4), noncanonical],
        )
        .expect("plant changed noncanonical residency");

    journal
        .read(|read| read.residency_head())
        .expect_err("changed noncanonical residency is rejected");
}

#[test]
fn renewal_absence_and_every_durable_state_replay_after_reopen() {
    let scratch = Scratch::new();
    let path = scratch.path().join("renewal-replay.sqlite3");
    let mut journal = authority_file_fixture(&path);

    let initial = journal
        .read(|read| read.residency_head())
        .expect("initial residency reads")
        .expect("initial residency exists");
    assert!(initial.body().renewals.is_empty());

    for state in states() {
        let expected = LaneRenewalRecord {
            team: uuid(TEAM),
            lane: uuid(LANE),
            revision: 41,
            state,
        };
        let mut head = residency_head();
        head.topology_revision = 41;
        head.renewals = vec![expected.clone()];
        journal
            .connection
            .execute(
                "UPDATE residency_head SET body = ?1 WHERE singleton = 1",
                params![wire::encode(&head).expect("renewal head encodes")],
            )
            .expect("renewal head persists");
        drop(journal);

        journal = Journal::open(&path).expect("renewal journal reopens");
        let replayed = journal
            .read(|read| read.residency_head())
            .expect("replayed residency reads")
            .expect("replayed residency exists");
        assert_eq!(replayed.body().renewals, vec![expected]);
        journal
            .verify()
            .expect("replayed renewal authority verifies");
    }
}
