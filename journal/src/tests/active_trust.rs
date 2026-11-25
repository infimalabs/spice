//! Live-task trust requirements project one owner per task and one counted
//! frontier row per distinct digest.

use super::*;

use crate::{FactCause, JournalWrite};
use wire::Producer;
use wire::body::{TaskAdded, TaskDeleted, TaskEdited};

#[test]
fn live_task_owners_count_one_frontier_row_per_digest() {
    let mut journal = authority_fixture();
    let (first, second) = (digest("a"), digest("b"));
    let left = add_trusting_task(&mut journal, 1, &first);
    let right = add_trusting_task(&mut journal, 2, &first);
    assert_eq!(frontier(&journal), vec![(first.to_string(), 2)]);

    require(&mut journal, &left, &second);
    assert_eq!(
        frontier(&journal),
        vec![(first.to_string(), 1), (second.to_string(), 1)]
    );
    assert_eq!(owner(&journal, &left), Some(second.to_string()));

    settle(&mut journal, &right);
    assert_eq!(frontier(&journal), vec![(second.to_string(), 1)]);
    assert_eq!(owner(&journal, &right), None);

    require(&mut journal, &left, &first);
    assert_eq!(frontier(&journal), vec![(first.to_string(), 1)]);
    settle(&mut journal, &left);
    assert_eq!(frontier(&journal), Vec::new());

    let retained: i64 = journal
        .connection
        .query_row(
            "SELECT count(*) FROM trust_heads WHERE digest = ?1",
            [first.as_str()],
            |row| row.get(0),
        )
        .expect("trust head count reads");
    assert_eq!(retained, 1, "the exact trust decision outlives its owners");
    journal
        .verify()
        .expect("projection agrees with current heads");
}

#[test]
fn verification_names_owner_and_frontier_drift() {
    let trust = digest("a");
    let cases = [
        (
            "UPDATE active_trust_digests SET requirements = 2",
            format!("active_trust_digests:{trust}"),
        ),
        (
            "INSERT INTO active_trust_digests(digest, requirements) VALUES ('stale', 1)",
            "active_trust_digests:stale".to_owned(),
        ),
    ];
    for (corruption, subject) in cases {
        let mut journal = authority_fixture();
        add_trusting_task(&mut journal, 1, &trust);
        journal
            .connection
            .execute(corruption, [])
            .expect("corrupt frontier");
        let error = journal
            .verify()
            .expect_err("frontier drift fails verification");
        assert!(
            error
                .violations()
                .iter()
                .any(|violation| violation.subject == subject),
            "{subject} not named: {:?}",
            error.violations()
        );
    }

    let mut journal = authority_fixture();
    let task = add_trusting_task(&mut journal, 1, &trust);
    journal
        .connection
        .execute(
            "UPDATE task_heads SET trust_digest = NULL WHERE stamp = ?1",
            [task.as_str()],
        )
        .expect("corrupt owner");
    let error = journal
        .verify()
        .expect_err("owner drift fails verification");
    let subjects = error
        .violations()
        .iter()
        .map(|violation| violation.subject.as_str())
        .collect::<Vec<_>>();
    assert!(
        subjects.contains(&format!("task_heads:{task}").as_str()),
        "{subjects:?}"
    );
    assert!(
        subjects.contains(&format!("active_trust_digests:{trust}").as_str()),
        "{subjects:?}"
    );
}

#[test]
fn crossing_projects_only_live_task_trust_owners() {
    let mut connection = Connection::open_in_memory().expect("open schema fixture");
    let transaction = connection.transaction().expect("crossing transaction");
    let (prior, crossing) = crate::migrate::CROSSING_STEPS.split_at(2);
    for (_, step) in prior {
        step(&transaction).expect("cross to schema two");
    }
    insert_fact_into(
        &transaction,
        fact(
            1,
            None,
            FactKind::TaskPhased,
            serde_json::json!({"task": "1kKRRsDPx", "phase": "review"}),
        ),
    );
    let first = digest("a");
    let pending = [(1, &first), (2, &first), (3, &digest("b"))];
    for (offset, trust) in pending {
        stored_task(&transaction, offset, trust, wire::WorkflowStatus::pending());
    }
    stored_task(
        &transaction,
        4,
        &digest("c"),
        wire::WorkflowStatus::deleted(instant()),
    );
    let [(_, cross), ..] = crossing else {
        panic!("schema two crosses in one step");
    };
    cross(&transaction).expect("cross to schema three");

    assert_eq!(
        frontier_of(&transaction),
        vec![(first.to_string(), 2), (digest("b").to_string(), 1)]
    );
    let mut violations = Vec::new();
    crate::active_trust::verify(&transaction, &mut violations).expect("projection verifies");
    assert!(violations.is_empty(), "{violations:?}");
}

fn stamp(offset: u64) -> TaskStamp {
    TaskStamp::for_micros(i64::try_from(20_000 + offset).expect("offset fits"))
        .expect("fixture task stamp")
}

fn trusting_requirements(trust: &Digest) -> wire::EnvironmentRequirements {
    wire::EnvironmentRequirements::one(wire::EnvironmentRequirement::trust_grant(trust.clone()))
}

fn stored_task(connection: &Connection, offset: u64, trust: &Digest, status: wire::WorkflowStatus) {
    let task = stamp(offset);
    let mut head = task_head();
    head.born = task.clone();
    head.requirements = trusting_requirements(trust);
    let mut workflow = workflow_head();
    workflow.status = status;
    insert_task_head(connection, task.as_str(), seq(1), &head);
    insert_head(
        connection,
        "workflow_heads",
        "stamp",
        task.as_str(),
        1,
        &workflow,
    );
}

fn write(journal: &mut Journal, build: impl FnOnce(&mut JournalWrite)) {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "task:active-trust".parse().expect("fixture origin"),
        instant(),
        Producer::CommandDoor,
    );
    build(&mut write);
    journal.apply(write).expect("fixture write commits");
}

fn add_trusting_task(journal: &mut Journal, offset: u64, trust: &Digest) -> TaskStamp {
    let task = stamp(offset);
    let requirements = trusting_requirements(trust);
    let flow = vec![phase("draft"), phase("todo"), phase("review")];
    write(journal, |write| {
        let fact = write
            .fact(
                TaskAdded {
                    preparation: None,
                    intent: None,
                    task: task.clone(),
                    title: format!("Trusting task {offset}"),
                    pitch: "Trusted work names its digest.".to_owned(),
                    description: None,
                    term: Vec::new(),
                    requirements: requirements.clone(),
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
            .expect("task fact builds");
        let born = task.clone();
        write.task_head(fact, move |revision| TaskHead {
            preparation: None,
            intent: None,
            born,
            topic_origin: revision,
            title: format!("Trusting task {offset}"),
            pitch: "Trusted work names its digest.".to_owned(),
            description: None,
            term: Vec::new(),
            requirements,
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
        write.workflow_head(task.clone(), fact, move |_| WorkflowHead {
            flow,
            phase: phase("todo"),
            status: wire::WorkflowStatus::pending(),
            deferral: None,
            submitted: false,
            generation: 0,
        });
    });
    task
}

fn current(journal: &Journal, task: &TaskStamp) -> (TaskHead, WorkflowHead) {
    let heads = journal
        .read(|read| read.task_heads_for(task))
        .expect("task heads read")
        .expect("task exists");
    (
        heads.statement().body().clone(),
        heads.workflow().body().clone(),
    )
}

fn require(journal: &mut Journal, task: &TaskStamp, trust: &Digest) {
    let (mut head, _) = current(journal, task);
    head.requirements = trusting_requirements(trust);
    write(journal, |write| {
        let fact = write
            .fact(
                TaskEdited {
                    task: task.clone(),
                    revision: 2,
                    dependencies: None,
                    flow: None,
                    intent: None,
                },
                FactCause::Root,
            )
            .expect("edit fact builds");
        write.task_head(fact, move |revision| {
            head.revision = revision;
            head
        });
    });
}

fn settle(journal: &mut Journal, task: &TaskStamp) {
    let (_, mut workflow) = current(journal, task);
    workflow.status = wire::WorkflowStatus::deleted(instant());
    write(journal, |write| {
        let fact = write
            .fact(
                TaskDeleted {
                    task: task.clone(),
                    reason: "No longer needed.".to_owned(),
                    claimed: None,
                },
                FactCause::Root,
            )
            .expect("delete fact builds");
        write.workflow_head(task.clone(), fact, move |_| workflow);
    });
}

fn frontier(journal: &Journal) -> Vec<(String, i64)> {
    frontier_of(&journal.connection)
}

fn frontier_of(connection: &Connection) -> Vec<(String, i64)> {
    let mut statement = connection
        .prepare("SELECT digest, requirements FROM active_trust_digests ORDER BY digest")
        .expect("frontier query prepares");
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("frontier reads")
        .collect::<Result<_, _>>()
        .expect("frontier rows decode")
}

fn owner(journal: &Journal, task: &TaskStamp) -> Option<String> {
    journal
        .connection
        .query_row(
            "SELECT trust_digest FROM task_heads WHERE stamp = ?1",
            [task.as_str()],
            |row| row.get(0),
        )
        .expect("owner reads")
}
