use super::{View, render, review_history};
use crate::task::Entry;
use wire::{
    Actor, AnyFact, Body, CanonicalUuid, Envelope, Fact, Instant, Phase, Seq, TaskHead, TaskStamp,
    WireVersion, WorkflowHead, WorkflowStatus, body,
};

fn stamp(micros: i64) -> TaskStamp {
    TaskStamp::for_micros(micros).expect("fixture microseconds fit a task stamp")
}

fn phase(value: &str) -> Phase {
    value.parse().expect("fixture phase")
}

fn lane(value: &str) -> Actor {
    Actor::Lane(
        value
            .parse::<CanonicalUuid>()
            .expect("fixture lane is canonical"),
    )
}

fn entry(task: TaskStamp) -> Entry {
    let origin = wire::TaskProvenance::Task(task.clone());
    let statement = TaskHead {
        preparation: None,
        intent: None,
        born: task,
        topic_origin: Seq::new(1).expect("nonzero sequence"),
        title: "Review history fixture".to_owned(),
        pitch: "Review history needs a fixture.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: "/board/graph".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: origin,
        creation: body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::SpiceActor),
        revision: Seq::new(1).expect("nonzero sequence"),
    };
    let workflow = WorkflowHead {
        flow: vec![phase("draft"), phase("todo"), phase("review")],
        phase: phase("review"),
        status: WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    };
    Entry::new(
        statement,
        workflow,
        wire::CompletionHead::opened(Seq::new(1).expect("nonzero sequence")),
        Vec::new(),
    )
    .expect("fixture entry is valid")
}

fn envelope<T: Body>(position: u64, actor: Actor, body: T) -> Envelope<AnyFact> {
    let typed = Fact::new(body);
    let fact = wire::decode::<AnyFact>(&wire::encode(&typed).expect("typed fact encodes"))
        .expect("registered fact decodes opaquely");
    Envelope {
        v: WireVersion::CURRENT,
        seq: Seq::new(position).expect("nonzero fixture sequence"),
        at: "2026-08-10T05:00:00.000Z"
            .parse::<Instant>()
            .expect("fixture instant"),
        actor,
        origin: "task:1kLLyCj4x".parse().expect("fixture origin"),
        cause: None,
        fact,
    }
}

fn added(position: u64, task: TaskStamp) -> Envelope<AnyFact> {
    added_with_flow(position, task, &["todo", "review"])
}

fn added_with_flow(position: u64, task: TaskStamp, flow: &[&str]) -> Envelope<AnyFact> {
    let flow = flow.iter().copied().map(phase).collect::<Vec<_>>();
    envelope(
        position,
        Actor::Spice,
        body::TaskAdded {
            preparation: None,
            intent: None,
            task,
            title: "Review history fixture".to_owned(),
            pitch: "Review history needs a fixture.".to_owned(),
            description: None,
            term: Vec::new(),
            requirements: wire::EnvironmentRequirements::default(),
            topic: "/board/graph".parse().expect("fixture topic"),
            submitted: false,
            priority: None,
            taste: None,
            depends: Vec::new(),
            phase: flow[0],
            flow,
            revision: 1,
            creation: body::TaskCreationContext::unavailable(
                body::TaskCreationUnavailable::SpiceActor,
            ),
        },
    )
}

fn completed(position: u64, task: TaskStamp, author: Actor) -> Envelope<AnyFact> {
    envelope(
        position,
        author,
        body::TaskDone {
            task,
            validation: "focused checks pass".to_owned(),
            disposition: None,
            terminal: None,
            git: None,
            source: body::SourceBoundary::Proven { heads: Vec::new() },
        },
    )
}

fn phased(position: u64, task: TaskStamp, author: Actor) -> Envelope<AnyFact> {
    envelope(
        position,
        author,
        body::TaskPhased {
            task,
            phase: phase("review"),
        },
    )
}

fn reviewed(
    position: u64,
    task: TaskStamp,
    reviewer: Actor,
    verdict: body::Verdict,
) -> Envelope<AnyFact> {
    envelope(
        position,
        reviewer.clone(),
        body::TaskReviewed {
            task,
            reviewer,
            verdict,
            status: match verdict {
                body::Verdict::Clean => body::TaskCompletionStatus::Completed,
                body::Verdict::Finding => body::TaskCompletionStatus::Pending,
            },
            finding: (verdict == body::Verdict::Finding).then(|| "finding".to_owned()),
            note: None,
            followups: Vec::new(),
            round: None,
            git: None,
            source: body::SourceBoundary::Proven { heads: Vec::new() },
        },
    )
}

#[test]
fn assignment_without_a_completed_review_never_becomes_review_history() {
    let task = stamp(1_800_000_000_000);
    let reviewer = lane("019fe57c-a453-7ec1-a400-26e6901aaebf");
    let entries = vec![entry(task.clone())];
    let facts = vec![
        added(1, task.clone()),
        envelope(
            2,
            Actor::Spice,
            body::TaskAssigned {
                task,
                reviewer,
                phase: phase("todo"),
            },
        ),
    ];

    let reviews = review_history(&facts, &entries).expect("assignment-only history is valid");
    assert!(reviews.is_empty());
    assert_eq!(
        render(View::Review, &entries, &reviews).text(),
        "graph review\n"
    );
}

#[test]
fn completed_reviews_join_phase_authors_and_render_stably_from_reversed_facts() {
    let first = stamp(1_800_000_000_001);
    let second = stamp(1_800_000_000_002);
    let first_author = lane("019fe57c-a453-7ec1-a400-26e6901aaeb1");
    let second_author = lane("019fe57c-a453-7ec1-a400-26e6901aaeb2");
    let first_reviewer = lane("019fe57c-a453-7ec1-a400-26e6901aaeb3");
    let second_reviewer = lane("019fe57c-a453-7ec1-a400-26e6901aaeb4");
    let entries = vec![entry(second.clone()), entry(first.clone())];
    let facts = vec![
        added(1, first.clone()),
        added(2, second.clone()),
        completed(3, first.clone(), first_author.clone()),
        phased(4, first.clone(), first_author.clone()),
        reviewed(5, first, first_reviewer.clone(), body::Verdict::Clean),
        completed(6, second.clone(), second_author.clone()),
        phased(7, second.clone(), second_author.clone()),
        reviewed(8, second, second_reviewer.clone(), body::Verdict::Finding),
    ];
    let forward = review_history(&facts, &entries).expect("durable history joins");
    let mut reversed_facts = facts;
    reversed_facts.reverse();
    let reversed = review_history(&reversed_facts, &entries).expect("input order is irrelevant");

    assert_eq!(forward, reversed);
    assert_eq!(forward.len(), 2);
    let rendered = render(View::Review, &entries, &forward).text().to_owned();
    assert!(rendered.contains(&format!("{first_reviewer} -> {first_author}")));
    assert!(rendered.contains(&format!("{second_reviewer} -> {second_author}")));
    assert!(rendered.contains("todo clean"));
    assert!(rendered.contains("todo finding"));
    assert!(!rendered.contains("assigned"));

    let mut reversed_edges = forward;
    reversed_edges.reverse();
    assert_eq!(
        rendered,
        render(View::Review, &entries, &reversed_edges).text()
    );
}

#[test]
fn a_review_no_completed_phase_precedes_names_no_author_beside_joined_reviews() {
    let review_only = stamp(1_800_000_000_003);
    let ordinary = stamp(1_800_000_000_004);
    let author = lane("019fe57c-a453-7ec1-a400-26e6901aaeb5");
    let first_reviewer = lane("019fe57c-a453-7ec1-a400-26e6901aaeb6");
    let second_reviewer = lane("019fe57c-a453-7ec1-a400-26e6901aaeb7");
    let entries = vec![entry(review_only.clone()), entry(ordinary.clone())];
    let handles = entries
        .iter()
        .map(|entry| entry.handle().to_string())
        .collect::<Vec<_>>();
    let facts = vec![
        added_with_flow(1, review_only.clone(), &["review"]),
        added(2, ordinary.clone()),
        reviewed(3, review_only, first_reviewer.clone(), body::Verdict::Clean),
        completed(4, ordinary.clone(), author.clone()),
        phased(5, ordinary.clone(), author.clone()),
        reviewed(6, ordinary, second_reviewer.clone(), body::Verdict::Finding),
    ];
    let forward = review_history(&facts, &entries).expect("a review-first flow is valid history");
    let mut reversed_facts = facts;
    reversed_facts.reverse();

    assert_eq!(
        forward,
        review_history(&reversed_facts, &entries).expect("input order is irrelevant")
    );
    assert_eq!(
        render(View::Review, &entries, &forward).text(),
        format!(
            "graph review\n\
             {first_reviewer} -> {} [no completed phase clean]\n\
             {second_reviewer} -> {author} [{} todo finding]\n",
            handles[0], handles[1],
        )
    );
}
