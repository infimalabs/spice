//! Release-note landing authority derived from task lifecycle facts.

use super::*;
use crate::{FactCause, JournalWrite};

fn arrival(task: &TaskStamp, integrated: &str, local_commits: u32) -> wire::body::TaskArrived {
    wire::body::TaskArrived {
        task: task.clone(),
        operation: None,
        claimed: seq(1),
        lane: uuid("018f82d4-6f59-7d06-92e3-bce913116305"),
        occupant: uuid("018f82d4-6f59-7d06-92e3-bce913116306"),
        phase: "todo".parse().unwrap(),
        round: seq(1),
        validation: "round participant landed".to_owned(),
        git: Some(wire::body::GitBoundary {
            head: integrated.to_owned(),
            integrated: integrated.to_owned(),
            upstream: Some("origin/main".to_owned()),
            upstream_head: None,
            local_commits,
        }),
        source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
        disposition: None,
    }
}

fn publish(journal: &mut Journal, fact: impl wire::Body) {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:1kKRRsDPx".parse().unwrap(),
        instant(),
        wire::Producer::CommandDoor,
    );
    write.fact(fact, FactCause::Root).unwrap();
    journal.apply(write).unwrap();
}

fn contributing(journal: &Journal) -> Vec<(String, String)> {
    journal
        .read(|read| read.release_note_authority())
        .unwrap()
        .landings
        .into_iter()
        .filter(|landing| landing.contributes)
        .map(|landing| (landing.commit, landing.task.as_str().to_owned()))
        .collect()
}

#[test]
fn round_arrivals_land_commits_and_reindex_restores_them() {
    let mut journal = authority_fixture();
    let task: TaskStamp = "1kKRRsDPx".parse().unwrap();
    publish(&mut journal, arrival(&task, "round-first", 1));
    publish(&mut journal, arrival(&task, "round-second", 2));
    publish(&mut journal, arrival(&task, "round-second", 0));
    let landed = vec![
        ("round-first".to_owned(), task.as_str().to_owned()),
        ("round-second".to_owned(), task.as_str().to_owned()),
    ];
    assert_eq!(contributing(&journal), landed);

    let mut duplicate = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:1kKRRsDPx".parse().unwrap(),
        instant(),
        wire::Producer::CommandDoor,
    );
    duplicate
        .fact(arrival(&task, "round-first", 1), FactCause::Root)
        .unwrap();
    assert!(
        journal.apply(duplicate).is_err(),
        "a second contributing landing of one commit is refused"
    );

    journal
        .connection
        .execute("DELETE FROM release_landings", [])
        .unwrap();
    assert!(contributing(&journal).is_empty());
    journal
        .reindex()
        .expect("reindex rebuilds release landings");
    assert_eq!(contributing(&journal), landed);
    journal.verify().expect("rebuilt projection verifies");
}
