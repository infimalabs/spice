//! Stable-node metadata, history pagination, and anonymous-node pruning.

use super::*;
use crate::{FactCause, JournalWrite};
use wire::topic::{TopicDescribed, TopicMoved};

fn publish(journal: &mut Journal, fact: impl wire::Body) {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "topic:metadata".parse().unwrap(),
        instant(),
        wire::Producer::CommandDoor,
    );
    write.fact(fact, FactCause::Root).unwrap();
    journal.apply(write).unwrap();
}

fn describe(journal: &mut Journal, path: &str, description: &str) {
    publish(
        journal,
        TopicDescribed {
            topic: path.parse().unwrap(),
            description: description.to_owned(),
        },
    );
}

fn edit_topic(
    journal: &mut Journal,
    task: &TaskStamp,
    mut head: wire::TaskHead,
    topic: wire::TaskTopic,
    revision: u32,
) -> wire::TaskHead {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "topic:metadata-task".parse().unwrap(),
        instant(),
        wire::Producer::CommandDoor,
    );
    let edited = write
        .fact(
            wire::body::TaskEdited {
                task: task.clone(),
                revision,
                dependencies: None,
                flow: None,
                intent: None,
            },
            FactCause::Root,
        )
        .unwrap();
    head.topic = topic;
    write.task_head(edited, move |position| wire::TaskHead {
        revision: position,
        ..head
    });
    journal.apply(write).unwrap();
    journal
        .read(|read| read.task_head(task))
        .unwrap()
        .unwrap()
        .into_body()
}

fn topic_exists(journal: &Journal, path: &str) -> bool {
    journal
        .read(|read| read.topic_node(&path.parse().unwrap()))
        .unwrap()
        .is_some()
}

fn set_workflow_status(
    journal: &mut Journal,
    task: &TaskStamp,
    status: wire::WorkflowStatus,
) -> wire::TaskHead {
    let workflow = journal
        .read(|read| read.workflow_head(task))
        .unwrap()
        .unwrap()
        .into_body();
    let phase = workflow.phase;
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "topic:metadata-workflow".parse().unwrap(),
        instant(),
        wire::Producer::CommandDoor,
    );
    let phased = write
        .fact(
            wire::body::TaskPhased {
                task: task.clone(),
                phase,
            },
            FactCause::Root,
        )
        .unwrap();
    write.workflow_head(task.clone(), phased, move |_| wire::WorkflowHead {
        status,
        ..workflow
    });
    journal.apply(write).unwrap();
    journal
        .read(|read| read.task_head(task))
        .unwrap()
        .unwrap()
        .into_body()
}

fn add_pending_task(journal: &mut Journal, micros: i64, path: &str) -> TaskStamp {
    let task = TaskStamp::for_micros(micros).unwrap();
    let phase = phase("todo");
    let topic: wire::TaskTopic = path.parse().unwrap();
    let creation = wire::body::TaskCreationContext::unavailable(
        wire::body::TaskCreationUnavailable::PrincipalActor,
    );
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "topic:metadata-pending".parse().unwrap(),
        instant(),
        wire::Producer::CommandDoor,
    );
    let added = write
        .fact(
            wire::body::TaskAdded {
                preparation: None,
                intent: None,
                task: task.clone(),
                title: "Retained pending neighbor".to_owned(),
                pitch: "Pending work keeps its live topic identity.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: topic.clone(),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: vec![wire::Phase::Draft, phase],
                phase,
                revision: 1,
                creation: creation.clone(),
            },
            FactCause::Root,
        )
        .unwrap();
    let statement_task = task.clone();
    write.task_head(added, move |revision| wire::TaskHead {
        preparation: None,
        intent: None,
        born: statement_task,
        topic_origin: revision,
        title: "Retained pending neighbor".to_owned(),
        pitch: "Pending work keeps its live topic identity.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic,
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: wire::TaskProvenance::OperatorDraft,
        creation,
        revision,
    });
    let workflow_task = task.clone();
    write.workflow_head(workflow_task, added, move |_| wire::WorkflowHead {
        flow: vec![wire::Phase::Draft, phase],
        phase,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    journal.apply(write).unwrap();
    task
}

#[test]
fn metadata_and_paged_history_follow_node_identity_across_moves() {
    let mut journal = authority_fixture();
    describe(&mut journal, "/metadata/child", "owned description");
    let original = journal
        .read(|read| read.topic_read(&"/metadata/child".parse().unwrap(), None))
        .unwrap();
    let original_node = original.node.unwrap();
    for index in 0..65 {
        describe(
            &mut journal,
            "/metadata/child",
            &format!("description {index}"),
        );
    }
    let first = journal
        .read(|read| read.topic_read(&"/metadata/child".parse().unwrap(), None))
        .unwrap();
    let cursor = first
        .next
        .expect("history continues after its bounded page");
    publish(
        &mut journal,
        TopicMoved {
            from: "/metadata".parse().unwrap(),
            to: "/archive/metadata".parse().unwrap(),
        },
    );
    let second = journal
        .read(|read| read.topic_read(&"/archive/metadata/child".parse().unwrap(), Some(cursor)))
        .unwrap();
    let node = second.node.unwrap();
    assert_eq!(node.id, original_node.id);
    assert_eq!(node.parent, original_node.parent);
    assert_eq!(node.description, "description 64");
    assert!(second.next.is_none());
    let history = first
        .history
        .into_iter()
        .chain(second.history)
        .collect::<Vec<_>>();
    assert!(history.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    let descriptions = history
        .iter()
        .filter_map(|row| row.fact.read::<TopicDescribed>())
        .map(|fact| fact.description)
        .collect::<Vec<_>>();
    assert_eq!(
        descriptions,
        std::iter::once("owned description".to_owned())
            .chain((0..65).map(|index| format!("description {index}")))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        history
            .last()
            .unwrap()
            .fact
            .read::<TopicMoved>()
            .unwrap()
            .to
            .as_str(),
        "/archive/metadata"
    );
    assert!(
        journal
            .read(|read| read.topic_node(&"/metadata/child".parse().unwrap()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn anonymous_leaves_and_ancestors_prune_without_disturbing_shared_or_moved_nodes() {
    let mut journal = authority_fixture();
    let before = crate::topic::metadata::revision(&journal.connection).unwrap();
    let missing = journal
        .read(|read| read.topic_read(&"/anonymous/missing".parse().unwrap(), None))
        .unwrap();
    assert!(missing.node.is_none());
    assert_eq!(
        crate::topic::metadata::revision(&journal.connection).unwrap(),
        before
    );
    describe(&mut journal, "/anonymous/missing", "");
    let after = crate::topic::metadata::revision(&journal.connection).unwrap();
    assert!(after > before);
    assert!(!topic_exists(&journal, "/anonymous/missing"));
    assert!(!topic_exists(&journal, "/anonymous"));

    describe(&mut journal, "/shared/keep", "retained");
    describe(&mut journal, "/shared/drop", "temporary");
    describe(&mut journal, "/shared/drop", "");
    assert!(topic_exists(&journal, "/shared"));
    assert!(topic_exists(&journal, "/shared/keep"));
    assert!(!topic_exists(&journal, "/shared/drop"));
    describe(&mut journal, "/shared/keep", "");
    assert!(!topic_exists(&journal, "/shared"));

    describe(&mut journal, "/vacated/branch/leaf", "moves with its node");
    let branch = journal
        .read(|read| read.topic_node(&"/vacated/branch".parse().unwrap()))
        .unwrap()
        .unwrap();
    publish(
        &mut journal,
        TopicMoved {
            from: "/vacated/branch".parse().unwrap(),
            to: "/destination/branch".parse().unwrap(),
        },
    );
    assert!(!topic_exists(&journal, "/vacated"));
    assert_eq!(
        journal
            .read(|read| read.topic_node(&"/destination/branch".parse().unwrap()))
            .unwrap()
            .unwrap()
            .id,
        branch.id
    );
    assert!(topic_exists(&journal, "/destination/branch/leaf"));
    journal.verify().expect("pruned topic tree verifies");
}

#[test]
fn settlement_snapshots_paths_prunes_live_nodes_and_reopen_resolves_fresh_identity() {
    let mut journal = authority_fixture();
    let owner = "018f82d4-6f59-7d06-92e3-bce913116411";
    let task: TaskStamp = "1kKRRsDPx".parse().unwrap();
    let original = journal
        .read(|read| read.task_head(&task))
        .unwrap()
        .unwrap()
        .into_body();
    let pending = edit_topic(
        &mut journal,
        &task,
        original,
        wire::TaskTopic {
            path: Some(format!("{owner}/archive/ephemeral/final").parse().unwrap()),
            placed: Some(format!("{owner}/router/ephemeral").parse().unwrap()),
            named: None,
            unmatched: false,
            nodes: None,
        },
        2,
    );
    let old = pending.topic.nodes.expect("pending task binds live nodes");
    let old_final = old.node.unwrap();
    let old_placed = old.placed.unwrap();
    let neighbor = add_pending_task(&mut journal, 40_000, "/archive/live");
    let before = crate::topic::metadata::revision(&journal.connection)
        .unwrap()
        .unwrap();

    let settled = set_workflow_status(
        &mut journal,
        &task,
        wire::WorkflowStatus::completed(instant()),
    );
    assert_eq!(
        settled.topic.path.as_ref().unwrap().as_str(),
        format!("{owner}/archive/ephemeral/final")
    );
    assert_eq!(
        settled.topic.placed.as_ref().unwrap().as_str(),
        format!("{owner}/router/ephemeral")
    );
    assert!(settled.topic.nodes.is_none());
    assert!(!topic_exists(&journal, owner));
    assert!(topic_exists(&journal, "/archive/live"));
    assert!(
        journal
            .read(|read| read.task_head(&neighbor))
            .unwrap()
            .unwrap()
            .body()
            .topic
            .nodes
            .is_some()
    );
    assert!(
        crate::topic::metadata::revision(&journal.connection)
            .unwrap()
            .unwrap()
            > before
    );

    let archived = edit_topic(
        &mut journal,
        &task,
        settled,
        wire::TaskTopic {
            path: Some(format!("{owner}/archive/edited/final").parse().unwrap()),
            placed: Some(format!("{owner}/router/edited").parse().unwrap()),
            named: None,
            unmatched: false,
            nodes: None,
        },
        3,
    );
    assert!(archived.topic.nodes.is_none());
    assert!(!topic_exists(&journal, &format!("{owner}/archive/edited")));
    assert!(!topic_exists(&journal, owner));

    let reopened = set_workflow_status(&mut journal, &task, wire::WorkflowStatus::pending());
    let rebound = reopened
        .topic
        .nodes
        .expect("reopened task binds current nodes");
    assert_ne!(rebound.node.unwrap(), old_final);
    assert_ne!(rebound.placed.unwrap(), old_placed);
    assert_eq!(reopened.topic.path, archived.topic.path);
    assert_eq!(reopened.topic.placed, archived.topic.placed);
    journal
        .verify()
        .expect("settled and reopened task storage verifies");
}

#[test]
fn live_references_pin_nodes_while_historical_records_keep_only_authored_paths() {
    let mut journal = authority_fixture();
    let task: TaskStamp = "1kKRRsDPx".parse().unwrap();
    let original = journal
        .read(|read| read.task_head(&task))
        .unwrap()
        .unwrap()
        .into_body();
    let birth = original.topic.path.clone().unwrap();

    let transient = edit_topic(
        &mut journal,
        &task,
        original,
        wire::TaskTopic::from("/transient/final".parse::<wire::Topic>().unwrap()),
        2,
    );
    let placed = edit_topic(
        &mut journal,
        &task,
        transient,
        wire::TaskTopic {
            path: Some("/current".parse().unwrap()),
            placed: Some("/placed/only".parse().unwrap()),
            named: None,
            unmatched: false,
            nodes: None,
        },
        3,
    );
    assert!(!topic_exists(&journal, "/transient"));
    assert!(topic_exists(&journal, "/placed/only"));
    assert!(!topic_exists(&journal, birth.as_str()));

    let current = edit_topic(
        &mut journal,
        &task,
        placed,
        wire::TaskTopic::from("/current".parse::<wire::Topic>().unwrap()),
        4,
    );
    assert!(!topic_exists(&journal, "/placed"));
    assert_eq!(current.topic.path.as_ref().unwrap().as_str(), "/current");

    describe(&mut journal, "/release/only", "temporary");
    let release_node = crate::topic::lookup(&journal.connection, &"/release/only".parse().unwrap())
        .unwrap()
        .unwrap();
    journal
        .connection
        .execute(
            "INSERT INTO release_landings(
                seq, commit_id, task, topic, title, contributes, verdict
             ) VALUES (?1, 'retained-release', ?2, ?3, 'retained title', 0, NULL)",
            rusqlite::params![
                i64::try_from(current.revision.get()).unwrap(),
                task.as_str(),
                "/release/only"
            ],
        )
        .unwrap();
    describe(&mut journal, "/release/only", "");
    assert!(!topic_exists(&journal, "/release"));
    assert_eq!(
        journal
            .read(|read| read.release_note_authority())
            .unwrap()
            .landings[0]
            .topic
            .as_str(),
        "/release/only"
    );
    describe(&mut journal, "/release/only", "reused");
    let reused = crate::topic::lookup(&journal.connection, &"/release/only".parse().unwrap())
        .unwrap()
        .unwrap();
    assert_ne!(reused, release_node);
    describe(&mut journal, "/release/only", "");
    assert!(!topic_exists(&journal, "/release"));

    let added = journal
        .fact_snapshot()
        .unwrap()
        .facts()
        .iter()
        .find_map(|row| row.fact.read::<wire::body::TaskAdded>())
        .unwrap();
    assert_eq!(added.topic.path, Some(birth));
    journal
        .verify()
        .expect("retained topic references and foreign keys verify");
}

#[test]
fn routing_topology_reserves_empty_paths_and_tracks_node_identity_across_moves() {
    let mut journal = authority_fixture();
    describe(
        &mut journal,
        "/structure/empty/leaf",
        "retains its otherwise empty parent",
    );
    let nodes = journal.read(|read| read.topic_routing_nodes()).unwrap();
    let id = nodes
        .iter()
        .find(|node| node.path.as_str() == "/structure/empty")
        .unwrap()
        .id;
    publish(
        &mut journal,
        TopicMoved {
            from: "/structure".parse().unwrap(),
            to: "/renamed".parse().unwrap(),
        },
    );
    let moved = journal.read(|read| read.topic_routing_nodes()).unwrap();
    assert_eq!(
        moved
            .iter()
            .find(|node| node.id == id)
            .unwrap()
            .path
            .as_str(),
        "/renamed/empty"
    );
}
