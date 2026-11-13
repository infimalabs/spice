use super::*;

// Board row fixtures the sibling modules share: Draft rows, edit seeds,
// dependency edges, and the document readers that assert their renderings.

pub(super) fn add_draft_row(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    task: &str,
    title: &str,
) -> TaskHandle {
    let value = task.bytes().fold(0_i64, |value, byte| {
        (value * 131 + i64::from(byte)) % 10_000_000_000_000
    });
    let born = wire::TaskStamp::for_micros(value).expect("Draft fixture stamp");
    let task = TaskHandle::for_phase(
        Some(&"/board/test".parse().expect("fixture topic")),
        &born,
        wire::Phase::Draft,
    );
    assert_committed(issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born }],
        &TaskAdd {
            topic: Some("/board/test".parse().expect("fixture topic")),
            description: Some(title.to_owned()),
            origin: Some(fixture_origin()),
            flow: Some("plan,todo,review".to_owned()),
            draft: true,
            ..adding(title.to_owned(), "This work keeps the fixture honest.")
        },
    ));
    task
}

pub(super) fn seed_edit_tasks(
    bus: &Bus,
    epoch: CanonicalUuid,
) -> (TaskHandle, TaskHandle, TaskHandle) {
    let alpha = add(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "BOARD-alpha001",
        "Alpha work",
        Some("todo"),
    );
    let beta = add(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "BOARD-beta0001",
        "Beta prerequisite",
        Some("todo"),
    );
    let oops = add_draft_row(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "OOPS-oops0001",
        "A command was unexpectedly awkward",
    );
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskNote,
        vec![Witness::Existence {
            task: at(&board(bus, epoch, Actor::TEST_PRINCIPAL), &alpha)
                .statement
                .body
                .born
                .clone(),
        }],
        &TaskNote {
            task: alpha.clone(),
            note: "one current observation".to_owned(),
        },
    ));
    (alpha, beta, oops)
}

pub(super) fn exercise_deferral_staleness(bus: &Bus, epoch: CanonicalUuid, alpha: &TaskHandle) {
    let before_defer = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let alpha_before = at(&before_defer, alpha).clone();
    let until = instant_after(current_instant().expect("clock"), 300).expect("future instant");
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDefer,
        vec![
            workflow_witness(&alpha_before),
            deferral_witness(&alpha_before),
        ],
        &TaskDefer {
            task: alpha.clone(),
            wait: until,
        },
    ));
    let stale = issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskWake,
        vec![
            workflow_witness(&alpha_before),
            deferral_witness(&alpha_before),
        ],
        &TaskWake {
            task: alpha.clone(),
            claim: false,
        },
    );
    assert!(matches!(
        stale,
        Outcome::Refusal(ref refusal) if refusal.reason() == "board::witness-moved"
    ));
    let deferred = board(bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&deferred, alpha).workflow.body.deferral, Some(until));
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskWake,
        vec![
            workflow_witness(at(&deferred, alpha)),
            deferral_witness(at(&deferred, alpha)),
        ],
        &TaskWake {
            task: alpha.clone(),
            claim: false,
        },
    ));
}

pub(super) fn add_dependency(
    bus: &Bus,
    epoch: CanonicalUuid,
    alpha: &TaskHandle,
    beta: &TaskHandle,
) {
    let before_edge = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let alpha_heads = at(&before_edge, alpha);
    let beta_heads = at(&before_edge, beta);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDepends,
        vec![
            statement_witness(alpha_heads),
            workflow_witness(alpha_heads),
            claim_witness(alpha_heads),
            statement_witness(beta_heads),
            workflow_witness(beta_heads),
            Witness::Edge {
                task: alpha_heads.statement.body.born.clone(),
                after: beta_heads.statement.body.born.clone(),
            },
        ],
        &TaskDepends {
            task: alpha.clone(),
            after: vec![beta.clone()],
            remove: false,
        },
    ));
}

pub(super) fn assert_graph_handout_and_ledger(
    bus: &Bus,
    epoch: CanonicalUuid,
    alpha: &TaskHandle,
) -> Outcome<CommandAnswer> {
    let graph_one = graph(bus, epoch);
    let graph_two = graph(bus, epoch);
    assert_eq!(graph_one, graph_two);
    let Outcome::Reply(Reply {
        position: graph_position @ ReplyPosition::Snapshot(_),
        body: CommandAnswer::Text(ref graph),
    }) = graph_one
    else {
        panic!("graph was not a text snapshot: {graph_one:?}");
    };
    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let alpha_heads = at(&visible, alpha);
    let beta_stamp = alpha_heads
        .statement
        .body
        .dependencies
        .first()
        .expect("alpha depends on beta");
    let beta = visible
        .tasks
        .iter()
        .find(|task| &task.statement.body.born == beta_stamp)
        .expect("dependency task is visible");
    assert!(
        graph
            .text
            .contains(&format!("{} -> {}", alpha, beta.statement.body.handle))
    );
    let handout = issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardHandout,
        Vec::new(),
        &BoardHandout {
            output: "-".to_owned(),
            at: None,
        },
    );
    let Outcome::Reply(Reply {
        position: handout_position,
        body: CommandAnswer::Text(ref document),
    }) = handout
    else {
        panic!("handout was not a text snapshot: {handout:?}");
    };
    assert_eq!(handout_position, graph_position);
    assert!(document.text.contains(&graph.text));
    assert_ledger_contains(bus, epoch, alpha, "# Alpha work");
    graph_one
}

fn graph(bus: &Bus, epoch: CanonicalUuid) -> Outcome<CommandAnswer> {
    issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardGraph,
        Vec::new(),
        &BoardGraph {
            view: "dependencies".to_owned(),
            all: false,
            at: None,
        },
    )
}

pub(super) fn assert_ledger_contains(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &TaskHandle,
    expected: &str,
) {
    let ledger = issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::DocLedger,
        Vec::new(),
        &DocLedger { task: task.clone() },
    );
    assert!(matches!(
        ledger,
        Outcome::Reply(Reply {
            body: CommandAnswer::Text(TextAnswer { ref text }),
            ..
        }) if text.contains(expected)
    ));
}

pub(super) fn assert_render_form_parity(graph: &Outcome<CommandAnswer>) {
    let text = crate::render::render(graph, crate::render::Form::Text).expect("graph text renders");
    let json = crate::render::render(graph, crate::render::Form::Json).expect("graph JSON renders");
    let text_body = wire::intent::AnswerShape::Text
        .decode(text.document())
        .expect("text body is the selected command answer");
    let json_outcome: Outcome<serde_json::Value> =
        wire::decode(json.document()).expect("JSON is the typed outcome");
    let Outcome::Reply(Reply { body, .. }) = graph else {
        unreachable!("graph reply matched above")
    };
    assert_eq!(&text_body, body);
    assert_eq!(
        serde_json::to_value(json_outcome).expect("decoded JSON outcome re-encodes"),
        serde_json::to_value(graph).expect("source outcome encodes"),
    );
}
