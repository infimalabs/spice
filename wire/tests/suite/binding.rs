//! The body-and-answer binding carried by a command kind.
//!
//! WIRE states that a command's kind selects the body DTO, witness schema,
//! transition, and reply DTO together. The witness half is proven next door in
//! `registry.rs`; this file proves the other two. Exhaustiveness itself is
//! enforced by a match and cannot be asserted from a test, so what is asserted
//! here is that every kind resolves to exactly one body and one answer, that
//! every declared shape is actually reachable and decodable, that a body
//! spelled for the wrong kind is refused rather than truncated, and that each
//! WIRE spelling re-encodes unchanged.

use wire::intent::{
    AnswerShape, BoardTask, CommandAnswer, OriginRequest, TaskDetailAnswer, TaskDetailClosure,
    TaskRelationEdge, TaskRelationFrontier, TaskRelationKind, TaskRelationNode,
    TaskRelationNodeRef, TaskRelationTruncation, TaskRelationsAnswer, TaskTimelineAnswer,
    TaskTimelineEntry, TaskTimelineEvent,
};
use wire::{
    Actor, CanonicalUuid, CommandKind, Instant, LaneResidency, ResidencyHead, ResidentState, Seq,
    TeamSubscription, WorkflowHead,
};

/// Every command body, spelled as WIRE spells it, paired with the kind whose
/// binding must decode it.
const BODIES: &[(CommandKind, &str)] = &[
    (CommandKind::BoardRead, r#"{ "scope": "all" }"#),
    (CommandKind::TaskDetail, r#"{ "task": "1kKRRsDPx" }"#),
    (
        CommandKind::TaskRelations,
        r#"{
  "roots": ["1kKRRsDPx"],
  "relations": ["dependency", "controller", "review"],
  "depth": 2,
  "node_limit": 32
}"#,
    ),
    (
        CommandKind::TaskTimeline,
        r#"{
  "lanes": ["6f5ae476-efa6-471b-9bed-0f6994c3dcfa"],
  "before": 42,
  "limit": 50
}"#,
    ),
    (CommandKind::BoardGraph, r#"{ "view": "dependencies" }"#),
    (CommandKind::BoardHandout, r#"{ "output": "-" }"#),
    (CommandKind::DocLedger, r#"{ "task": "SPICE-1kKRRsDPx" }"#),
    (CommandKind::GlobalSettingsRead, r#"{}"#),
    (CommandKind::GlobalSettingsSet, r#"{ "fast_mode": true }"#),
    (CommandKind::VendorCatalog, r#"{}"#),
    (
        CommandKind::TaskAdd,
        r#"{
  "title": "Bind every command kind to its body and reply DTO",
  "topic": "/wire",
  "term": ["every kind names one body and one reply"],
  "origin": "task:WIRE-parent",
  "depends": ["SPICE-1kKRRsDPx"]
}"#,
    ),
    (
        CommandKind::TaskOopsAdd,
        r#"{
  "design": "1kKRRsDPx",
  "title": "Create one system-owned hidden task"
}"#,
    ),
    (
        CommandKind::TaskSweep,
        r#"{
  "preview": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "plan": "1kKRRsDPx",
  "children": ["1kKRRsDQx"],
  "content": {
    "topic": "/gate",
    "until": "2026-07-31T05:49:09.000Z",
    "plan": { "title": "Sweep", "pitch": "Why.", "description": "Debt.", "term": [] },
    "children": [
      { "title": "Heal a.rs", "pitch": "Why.", "description": "Debt.", "term": ["healed"] }
    ]
  }
}"#,
    ),
    (
        CommandKind::TaskEdit,
        r#"{ "task": "SPICE-1kKRRsDPx", "title": "a replacement statement" }"#,
    ),
    (
        CommandKind::TaskDelete,
        r#"{ "task": "SPICE-1kKRRsDPx", "reason": "filed against the wrong surface" }"#,
    ),
    (
        CommandKind::TaskClose,
        r#"{ "task": "SPICE-1kKRRsDPx", "validation": "verified in the browser", "force_claimed": true }"#,
    ),
    (
        CommandKind::TaskNote,
        r#"{ "task": "SPICE-1kKRRsDPx", "note": "the registry row is exhaustive" }"#,
    ),
    (
        CommandKind::TaskDefer,
        r#"{ "task": "SPICE-1kKRRsDPx", "wait": "2026-07-31T05:49:09.000Z" }"#,
    ),
    (CommandKind::TaskWake, r#"{ "task": "SPICE-1kKRRsDPx" }"#),
    (
        CommandKind::TaskDepends,
        r#"{ "task": "SPICE-1kKRRsDPx", "after": ["SPICE-1kKRRsDQx"], "remove": true }"#,
    ),
    (
        CommandKind::TaskDone,
        r#"{ "task": "SPICE-1kKRRsDPx", "validation": "cargo test --workspace" }"#,
    ),
    (
        CommandKind::TaskReview,
        r#"{ "task": "SPICE-1kKRRsDPx", "finding": "accept" }"#,
    ),
    (CommandKind::ClaimWin, r#"{ "rule": "next" }"#),
    (
        CommandKind::ClaimRefresh,
        r#"{ "task": "SPICE-1kKRRsDPx" }"#,
    ),
    (CommandKind::ClaimFree, r#"{ "task": "SPICE-1kKRRsDPx" }"#),
    (CommandKind::ClaimLapse, r#"{ "task": "SPICE-1kKRRsDPx" }"#),
    (
        CommandKind::SteerSend,
        r##"{
  "fanout": "018f82d4-6f59-7d06-92e3-bce9131163c1",
  "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "message": "drain the queue"
}"##,
    ),
    (CommandKind::SteerInbox, r#"{}"#),
    (
        CommandKind::SteerLineControl,
        r#"{
  "input": {
    "directive": "018f82d4-6f59-7d06-92e3-bce9131163c4",
    "principal": "018f82d4-6f59-7d06-92e3-bce9131163c7"
  },
  "action": "ACK",
  "body": "received"
}"#,
    ),
    (
        CommandKind::QuoteRead,
        r#"{ "target": "spice://quote/v1/steer/1kQnrzNZx/6ac2a419aad5b1cb8a9b632aed2429f5f7bd479de18dd1bb1bb031643450b335" }"#,
    ),
    (
        CommandKind::SteerResend,
        r#"{ "directive": "018f82d4-6f59-7d06-92e3-bce9131163c4" }"#,
    ),
    (
        CommandKind::SteerRecover,
        r#"{ "directive": "018f82d4-6f59-7d06-92e3-bce9131163c4" }"#,
    ),
    (
        CommandKind::SteerCancel,
        r#"{ "directive": "018f82d4-6f59-7d06-92e3-bce9131163c4" }"#,
    ),
    (
        CommandKind::ConsentDecide,
        r#"{ "request": "018f82d4-6f59-7d06-92e3-bce9131163c4", "answer": "asked" }"#,
    ),
    (CommandKind::ConsentRead, r#"{}"#),
    (CommandKind::ConsentPolicy, r#"{}"#),
    (
        CommandKind::TrustPend,
        r#"{ "digest": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }"#,
    ),
    (
        CommandKind::TrustGrant,
        r#"{ "digest": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }"#,
    ),
    (
        CommandKind::TrustRevoke,
        r#"{ "digest": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }"#,
    ),
    (CommandKind::LaneRead, r#"{}"#),
    (
        CommandKind::LaneSeat,
        r#"{
  "topology_revision": 9,
  "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "worktree": "worktrees/h",
  "life": "drive",
  "vendor": "omp"
}"#,
    ),
    (
        CommandKind::LaneState,
        r#"{
  "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "action": "wake",
  "worktree": "/worktrees/successor"
}"#,
    ),
    (
        CommandKind::LaneRenewal,
        r#"{
  "topology_revision": 9,
  "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "lane": "018f82d4-6f59-7d06-92e3-bce9131163c3",
  "requested": true
}"#,
    ),
    (
        CommandKind::LaneRetune,
        r#"{
  "topology_revision": 9,
  "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "model": "claude-opus-5-5"
}"#,
    ),
    (
        CommandKind::LaneEffortRetune,
        r#"{
  "topology_revision": 9,
  "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "effort": "high"
}"#,
    ),
    (
        CommandKind::LaneFastRetune,
        r#"{
  "topology_revision": 9,
  "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "fast_mode": true
}"#,
    ),
    (
        CommandKind::TeamConfigure,
        r#"{
  "topology_revision": 9,
  "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "drain": true
}"#,
    ),
    (
        CommandKind::TeamArrange,
        r#"{
  "topology_revision": 9,
  "teams": [
    {
      "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
      "lanes": [
        "018f82d4-6f59-7d06-92e3-bce9131163c3",
        "018f82d4-6f59-7d06-92e3-bce9131163c4"
      ]
    },
    {
      "team": "018f82d4-6f59-7d06-92e3-bce9131163c5",
      "lanes": ["018f82d4-6f59-7d06-92e3-bce9131163c6"]
    }
  ]
}"#,
    ),
    (
        CommandKind::LaneRecall,
        r#"{ "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2" }"#,
    ),
    (
        CommandKind::ProofRecognize,
        r#"{ "key": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd" }"#,
    ),
    (CommandKind::UpChange, r#"{ "state": "quiescing" }"#),
];

mod answers;
use answers::ANSWERS;

#[test]
fn every_body_decodes_as_the_shape_its_kind_binds() {
    for (kind, body) in BODIES {
        let shape = kind.binding().body;
        let intent = shape
            .decode(body.as_bytes())
            .unwrap_or_else(|error| panic!("{kind} body does not decode as {shape:?}: {error}"));
        assert_eq!(
            intent.shape(),
            shape,
            "{kind} decoded into a shape its binding does not name",
        );
    }
}

#[test]
fn every_body_re_encodes_unchanged() {
    for (kind, body) in BODIES {
        let intent = kind
            .binding()
            .body
            .decode(body.as_bytes())
            .expect("the fixture decodes");
        let spelled: serde_json::Value =
            serde_json::from_str(body).expect("the fixture is well-formed JSON");
        let round_tripped = serde_json::to_value(&intent).expect("an intent encodes");
        assert_eq!(
            round_tripped, spelled,
            "{kind} did not re-encode to the spelling it was decoded from",
        );
    }
}

#[test]
fn every_answer_decodes_as_the_shape_that_names_its_dto() {
    for (shape, body) in ANSWERS {
        let answer = shape
            .decode(body.as_bytes())
            .unwrap_or_else(|error| panic!("{shape:?} answer does not decode: {error}"));
        assert_eq!(answer.shape(), *shape);
    }
}

#[test]
fn every_answer_re_encodes_unchanged() {
    for (shape, body) in ANSWERS {
        let answer = shape.decode(body.as_bytes()).expect("the fixture decodes");
        let spelled: serde_json::Value =
            serde_json::from_str(body).expect("the fixture is well-formed JSON");
        let round_tripped = serde_json::to_value(&answer).expect("an answer encodes");
        assert_eq!(
            round_tripped, spelled,
            "{shape:?} did not re-encode to the spelling it was decoded from",
        );
    }
}

#[test]
fn every_answer_decodes_alike_from_its_bytes_and_its_parsed_value() {
    for (shape, body) in ANSWERS {
        let from_bytes = shape.decode(body.as_bytes()).expect("the fixture decodes");
        let value: serde_json::Value =
            serde_json::from_str(body).expect("the fixture is well-formed JSON");
        let from_value = shape
            .decode_value(value)
            .unwrap_or_else(|error| panic!("{shape:?} parsed answer does not decode: {error}"));
        assert_eq!(
            from_value, from_bytes,
            "{shape:?} decodes differently from a parsed value"
        );
    }
}

/// The single board task row spelled in [`ANSWERS`], as parsed JSON.
fn spelled_board_task() -> serde_json::Value {
    let (_, body) = ANSWERS
        .iter()
        .find(|(shape, _)| *shape == AnswerShape::Board)
        .expect("the answer table spells a board body");
    let spelled: serde_json::Value =
        serde_json::from_str(body).expect("the board fixture is well-formed JSON");
    let mut tasks = spelled["tasks"]
        .as_array()
        .expect("the board body carries a task array")
        .clone();
    assert_eq!(tasks.len(), 1, "the board fixture spells exactly one task");
    tasks.remove(0)
}

#[test]
fn task_detail_answer_round_trips_through_its_registered_shape() {
    let task: BoardTask =
        serde_json::from_value(spelled_board_task()).expect("board task fixture decodes");
    let expected = TaskDetailAnswer {
        task,
        media: vec![
            wire::Media::from_base64("capture.png", "image/png", "iVBORw0KGgo=")
                .expect("fixture media admits"),
        ],
        closure: Some(TaskDetailClosure {
            position: seq(9),
            at: instant("2026-08-01T06:52:03.000Z"),
            author: Actor::TEST_PRINCIPAL,
            validation: "Verified in the browser.".to_owned(),
            phase: "todo".parse().expect("fixture phase"),
            claimed: Some(Actor::Lane(
                "018f82d4-6f59-7d06-92e3-bce913116411"
                    .parse()
                    .expect("fixture lane"),
            )),
        }),
        deletion: None,
        review: None,
        disposition: Some(wire::intent::TaskDetailDisposition {
            position: seq(8),
            at: instant("2026-08-01T06:51:03.000Z"),
            author: Actor::TEST_PRINCIPAL,
            disposition: wire::body::PlanDisposition::Covered {
                cover: wire::body::DispositionCover::Task(
                    "1kKRRsDPx".parse().expect("fixture stamp"),
                ),
            },
            validation: "The landed contract already answers this.".to_owned(),
        }),
        origin: Some(wire::intent::TaskDetailOrigin {
            published: instant("2026-08-01T06:50:03.000Z"),
            directive: wire::QuotePresentation::Resolved {
                target: "spice://quote/v1/steer/dXXabcDEG/sha256:00".to_owned(),
                text: "Show the request behind this origin".to_owned(),
                attribution: wire::QuoteAttribution {
                    author: wire::QuoteAuthor::Directive {
                        actor: Actor::TEST_PRINCIPAL,
                    },
                    recipients: Vec::new(),
                    packet: false,
                },
                disposition: Some(wire::Disposition::Acked),
                quotes: Vec::new(),
                media_quotes: Vec::new(),
            },
        }),
        description_quotes: vec![wire::QuotePresentation::Unavailable {
            target: "spice://quote/v1/steer/dXXabcDEF/sha256:00".to_owned(),
            reason: wire::QuoteUnavailable::Unknown,
        }],
    };
    let bytes = wire::encode(&expected).expect("task detail answer encodes");
    let CommandAnswer::TaskDetail(actual) = AnswerShape::TaskDetail
        .decode(&bytes)
        .expect("task detail answer decodes through its registered shape")
    else {
        panic!("task detail shape decoded another answer")
    };
    assert_eq!(*actual, expected);
}

#[test]
fn origin_request_preview_keeps_short_requests_whole_and_bounds_long_ones() {
    let limit = wire::intent::ORIGIN_REQUEST_PREVIEW_LIMIT;
    let whole = "é".repeat(limit);
    assert_eq!(
        OriginRequest::preview(&whole),
        OriginRequest {
            text: whole.clone(),
            truncated: false,
        },
    );
    let long = format!("{whole}ü and more");
    assert_eq!(
        OriginRequest::preview(&long),
        OriginRequest {
            text: whole,
            truncated: true,
        },
        "a long request keeps exactly the bound's characters, cut on a character boundary",
    );
}

#[test]
fn task_relations_answer_round_trips_discriminated_nodes_and_frontier() {
    let task: wire::TaskStamp = "1kKRRsDPx".parse().expect("fixture task stamp");
    let dependency: wire::TaskStamp = "1kKRRsDQx".parse().expect("fixture dependency stamp");
    let edge = TaskRelationEdge {
        relation: TaskRelationKind::Dependency,
        from: TaskRelationNodeRef::Task { task: task.clone() },
        to: TaskRelationNodeRef::Task {
            task: dependency.clone(),
        },
    };
    let expected = TaskRelationsAnswer {
        snapshot: seq(7),
        roots: vec![task.clone()],
        nodes: vec![
            TaskRelationNode::Task {
                task,
                handle: "SPICE-1kKRRsDPx".parse().expect("fixture task handle"),
                title: "Inspect bounded relationships".to_owned(),
                phase: "todo".parse().expect("fixture phase"),
                status: wire::WorkflowStatusKind::Pending,
            },
            TaskRelationNode::Origin {
                origin: "ack:dWDbyhgRP".parse().expect("fixture origin"),
                request: Some(OriginRequest {
                    text: "Show the request".to_owned(),
                    truncated: true,
                }),
            },
        ],
        edges: Vec::new(),
        truncation: Some(TaskRelationTruncation {
            frontier: vec![TaskRelationFrontier { depth: 1, edge }],
            omitted: 2,
        }),
    };
    let bytes = wire::encode(&expected).expect("task relations answer encodes");
    let CommandAnswer::TaskRelations(actual) = AnswerShape::TaskRelations
        .decode(&bytes)
        .expect("task relations answer decodes through its registered shape")
    else {
        panic!("task relations shape decoded another answer")
    };
    assert_eq!(*actual, expected);
}

#[test]
fn task_timeline_answer_round_trips_payload_and_bare_events() {
    let lane: CanonicalUuid = "6f5ae476-efa6-471b-9bed-0f6994c3dcfa"
        .parse()
        .expect("fixture lane");
    let entry = |position, event| TaskTimelineEntry {
        position: seq(position),
        at: instant("2026-08-01T06:47:19.000Z"),
        lane,
        task: "1kKRRsDPx".parse().expect("fixture task stamp"),
        handle: "SPICE-1kKRRsDPx".parse().expect("fixture task handle"),
        title: "Page team transitions".to_owned(),
        event,
    };
    let expected = TaskTimelineAnswer {
        snapshot: seq(9),
        lanes: vec![lane],
        entries: vec![
            entry(
                9,
                TaskTimelineEvent::Phased {
                    phase: "review".parse().expect("fixture phase"),
                },
            ),
            entry(8, TaskTimelineEvent::Claimed {}),
        ],
        next: Some(seq(8)),
    };
    let bytes = wire::encode(&expected).expect("task timeline answer encodes");
    let CommandAnswer::TaskTimeline(actual) = AnswerShape::TaskTimeline
        .decode(&bytes)
        .expect("task timeline answer decodes through its registered shape")
    else {
        panic!("task timeline shape decoded another answer")
    };
    assert_eq!(*actual, expected);
}

fn seq(position: u64) -> Seq {
    Seq::new(position).expect("a fact position")
}

fn uuid(spelled: &str) -> CanonicalUuid {
    spelled.parse().expect("a canonical uuid")
}

fn instant(spelled: &str) -> Instant {
    spelled.parse().expect("an instant")
}

#[test]
fn workflow_documents_round_trip_every_admitted_state_in_the_canonical_shape() {
    const DOCUMENTS: &[&str] = &[
        r#"{"flow":["todo"],"phase":"todo","status":"pending","review":"unassigned"}"#,
        r#"{"flow":["todo"],"phase":"todo","status":"pending","review":"clean"}"#,
        r#"{"flow":["todo"],"phase":"todo","status":"completed","review":"unassigned","completion":"2026-08-01T06:07:19.000Z"}"#,
        r#"{"flow":["todo"],"phase":"todo","status":"completed","review":"clean","completion":"2026-08-01T06:07:19.000Z"}"#,
        r#"{"flow":["todo"],"phase":"todo","status":"completed","review":"closed","completion":"2026-08-01T06:07:19.000Z"}"#,
        r#"{"flow":["todo"],"phase":"todo","status":"deleted","review":"closed","completion":"2026-08-01T06:07:19.000Z"}"#,
    ];

    for document in DOCUMENTS {
        let expected: serde_json::Value =
            serde_json::from_str(document).expect("canonical workflow JSON");
        let workflow: WorkflowHead =
            serde_json::from_str(document).expect("canonical workflow decodes");
        assert_eq!(
            serde_json::to_value(workflow).expect("workflow encodes"),
            expected
        );
    }
}

#[test]
fn workflow_documents_refuse_contradictory_state_fields() {
    const DOCUMENTS: &[&str] = &[
        r#"{"flow":["todo"],"phase":"todo","status":"pending","review":"unassigned","completion":"2026-08-01T06:07:19.000Z"}"#,
        r#"{"flow":["todo"],"phase":"todo","status":"completed","review":"unassigned"}"#,
        r#"{"flow":["todo"],"phase":"todo","status":"deleted","review":"unassigned","completion":"2026-08-01T06:07:19.000Z"}"#,
    ];

    for document in DOCUMENTS {
        assert!(
            serde_json::from_str::<WorkflowHead>(document).is_err(),
            "contradictory workflow decoded: {document}"
        );
    }
}

#[test]
fn the_answer_union_reports_the_shape_it_carries() {
    let answer = CommandAnswer::Taste(wire::intent::TasteAnswer {
        name: "polling".parse().expect("a name"),
        confirmed: true,
    });
    assert_eq!(answer.shape(), AnswerShape::Taste);
}

#[test]
fn a_body_spelled_for_another_kind_is_refused() {
    // `task::done` names a validation; the shape bound to `task::wake` names a
    // task alone. Bodies deny unknown fields, so the extra key is a refusal
    // rather than a silent truncation into a valid-looking wake.
    let done = r#"{ "task": "SPICE-1kKRRsDPx", "validation": "cargo test --workspace" }"#;
    let shape = CommandKind::TaskWake.binding().body;
    assert!(
        shape.decode(done.as_bytes()).is_err(),
        "a done body decoded as the shape bound to task::wake",
    );

    // The reverse direction fails on the missing field rather than an extra.
    let bare = r#"{ "task": "SPICE-1kKRRsDPx" }"#;
    let shape = CommandKind::TaskDone.binding().body;
    assert!(
        shape.decode(bare.as_bytes()).is_err(),
        "a body missing its required validation decoded as task::done",
    );
}

#[test]
fn team_switches_preserve_drain_requires_drive() {
    let mut team = TeamSubscription {
        team: uuid("018f82d4-6f59-7d06-92e3-bce913116301"),
        formed_at: None,
        drive: false,
        drain: false,
        name: None,
        fast_mode: false,
    };
    team.set_drain(true);
    assert!(team.drive && team.drain);
    team.set_drain(false);
    assert!(team.drive && !team.drain);
    team.set_drain(true);
    team.set_drive(false);
    assert!(!team.drive && !team.drain);
    team.set_drive(true);
    assert!(team.drive && !team.drain);
}

/// One definition of teamed governs rendering and placement.
///
/// A lane seated alone under its own identity is a defined agent, not a team;
/// allocation never asks, since a lane follows its team's policy either way.
#[test]
fn a_lane_seated_alone_under_its_own_identity_is_not_on_a_team() {
    let lane = uuid("018f82d4-6f59-7d06-92e3-bce913116301");
    let peer = uuid("018f82d4-6f59-7d06-92e3-bce913116302");
    let team = uuid("018f82d4-6f59-7d06-92e3-bce913116303");
    assert!(
        !wire::head::teamed(lane, [lane]),
        "a default seat is not a team"
    );
    assert!(
        !wire::head::teamed(lane, []),
        "a team without members is not one"
    );
    assert!(
        wire::head::teamed(team, [lane]),
        "one agent placed on a team is on it"
    );
    assert!(
        wire::head::teamed(lane, [lane, peer]),
        "two agents under either identity are a team"
    );
    assert!(wire::head::teamed(lane, [peer, lane]));

    let mut residency = ResidencyHead {
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce913116310"),
        state: ResidentState::Seated,
        topology_revision: 0,
        lanes: vec![residency_row(lane, lane), residency_row(peer, peer)],
        subscriptions: Vec::new(),
        renewals: Vec::new(),
    };
    assert_eq!(residency.team_members(lane).collect::<Vec<_>>(), [lane]);
    residency.lanes[1].team = lane;
    assert_eq!(
        residency.team_members(lane).collect::<Vec<_>>(),
        [lane, peer]
    );
    assert_eq!(residency.team_members(peer).count(), 0);
}

fn residency_row(lane: CanonicalUuid, team: CanonicalUuid) -> LaneResidency {
    LaneResidency {
        lane,
        team,
        name: None,
        worktree: "/fixture".to_owned(),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: None,
        lineage: Vec::new(),
        capabilities: Vec::new(),
        session: None,
        disposition: "vacant".to_owned(),
    }
}

#[test]
fn fast_mode_is_on_when_any_of_lane_team_or_global_enables_it() {
    let lane = uuid("018f82d4-6f59-7d06-92e3-bce913116301");
    let peer = uuid("018f82d4-6f59-7d06-92e3-bce913116302");
    let team = uuid("018f82d4-6f59-7d06-92e3-bce913116303");
    let other = uuid("018f82d4-6f59-7d06-92e3-bce913116304");
    let subscription = |team, fast_mode| TeamSubscription {
        team,
        formed_at: None,
        drive: true,
        drain: false,
        name: None,
        fast_mode,
    };
    for lane_flag in [false, true] {
        for team_flag in [false, true] {
            for global in [false, true] {
                let mut row = residency_row(lane, team);
                row.fast_mode = lane_flag;
                let residency = ResidencyHead {
                    epoch: uuid("018f82d4-6f59-7d06-92e3-bce913116310"),
                    state: ResidentState::Seated,
                    topology_revision: 0,
                    lanes: vec![row.clone(), residency_row(peer, other)],
                    subscriptions: vec![subscription(team, team_flag), subscription(other, true)],
                    renewals: Vec::new(),
                };
                assert_eq!(
                    residency.fast_mode(&row, global),
                    lane_flag || team_flag || global,
                    "lane={lane_flag} team={team_flag} global={global}"
                );
            }
        }
    }
}
