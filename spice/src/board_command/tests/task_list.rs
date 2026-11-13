use super::*;

#[test]
fn default_task_list_reads_every_filed_row_and_only_its_own_drafts() {
    let (root, bus, epoch, _) = fixture("task-list-actor-route", 0);
    let public = add_task_in_topic(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-route0001",
        "Public route row",
        "/task/cli",
        Some("todo"),
    );
    let mut answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    answer.principal = Actor::Lane(uuid(9_001));
    let (included, excluded) = add_actor_route_rows(&mut answer, &public);
    let listed = task_list(
        &answer,
        &TaskListOptions {
            all: false,
            topic: None,
            limit: None,
            status: None,
        },
    );
    for included in included {
        assert!(
            listed.contains(included.as_str()),
            "missing {included}: {listed}"
        );
    }
    assert!(!listed.contains(excluded.as_str()), "{listed}");
    clean(&root, bus);
}

#[test]
fn bare_task_list_applies_the_interactive_default_and_continuation() {
    let (root, bus, epoch, _) = fixture("task-list-default-bound", 0);
    let task = add_task_in_topic(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-bound0001",
        "Bounded row",
        "/task/cli",
        Some("todo"),
    );
    let mut answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let template = at(&answer, &task).clone();
    for index in 1..206 {
        let mut row = template.clone();
        row.statement.body.born =
            wire::TaskStamp::for_micros(9_000 + index).expect("fixture task stamp");
        row.statement.body.handle =
            TaskHandle::for_topic(&row.statement.body.topic.path, &row.statement.body.born);
        row.presentation_title = format!("Bounded row {index}");
        answer.tasks.push(row);
    }
    let listed = task_list(
        &answer,
        &TaskListOptions {
            all: true,
            topic: None,
            limit: None,
            status: None,
        },
    );
    assert_eq!(listed.matches(" [todo]").count(), 20, "{listed}");
    assert!(listed.contains("withheld 186 of 206 rows"), "{listed}");
    assert!(listed.contains("continue: spice task list"), "{listed}");
    assert!(listed.contains("--after"), "{listed}");
    clean(&root, bus);
}

fn route_row(
    template: &BoardTask,
    micros: i64,
    topic: &str,
    phase: wire::Phase,
    origin_thread: &str,
    title: &str,
) -> BoardTask {
    let mut row = template.clone();
    row.statement.body.born = wire::TaskStamp::for_micros(micros).expect("fixture task stamp");
    row.statement.body.topic = topic.parse().expect("fixture topic");
    row.workflow.body.phase = phase;
    row.statement.body.handle = TaskHandle::for_phase(
        row.statement.body.topic.path.as_ref(),
        &row.statement.body.born,
        phase,
    );
    row.statement.body.provenance = "task:0000002zx".parse().expect("fixture route provenance");
    row.origin_thread = origin_thread.to_owned();
    row.presentation_title = title.to_owned();
    row
}

/// Plant one row per route branch for a lane principal: a filed row from
/// another thread, a Draft on the actor's own thread, and a Draft on another
/// thread that the route must exclude.
fn add_actor_route_rows(
    answer: &mut BoardAnswer,
    public: &TaskHandle,
) -> ([TaskHandle; 3], TaskHandle) {
    let template = at(answer, public).clone();
    answer
        .tasks
        .iter_mut()
        .find(|task| task.statement.body.handle == *public)
        .expect("public route row")
        .origin_thread = "someone_else".to_owned();
    let own = answer.actor_route.origin_thread.clone();
    let authored = route_row(
        &template,
        8_002,
        "/external",
        wire::Phase::Todo,
        "someone_else",
        "Authored filed row",
    );
    let own_draft = route_row(
        &template,
        8_003,
        "/sandbox/route",
        wire::Phase::Draft,
        &own,
        "Own Draft row",
    );
    let outside = route_row(
        &template,
        8_004,
        "/sandbox/secret",
        wire::Phase::Draft,
        "someone_else",
        "Outside Draft row",
    );
    let included = [
        public.clone(),
        authored.statement.body.handle.clone(),
        own_draft.statement.body.handle.clone(),
    ];
    let excluded = outside.statement.body.handle.clone();
    answer.tasks.extend([authored, own_draft, outside]);
    (included, excluded)
}
