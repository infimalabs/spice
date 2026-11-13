//! The real app runtime retains only admitted, capability-scoped state.

use app::watchband::{ticker_accessible_label, ticker_chunks};
use app::{
    AppRuntime, AppSurface, LaneStatus, Session, Stale, StreamDirective, StreamTerminal,
    stale_reads,
};
use wire::{
    CanonicalUuid, CommandKind, Outcome, Reply, ReplyPosition, Response, Seq, StreamFrame,
    ViewSubject, WireVersion,
};

mod consent;
mod detail;
mod invalidation_and_chronology;
mod lane_authority;
mod member_control;
mod message_history;
mod renewal;
mod rooms;
mod roster;
mod stream_terminal;
mod trust_decision;

use super::support;

const OPERATOR: &str = "018f82d4-6f59-7d06-92e3-bce9131163c1";
const LANE_CAPABILITY: &str = "018f82d4-6f59-7d06-92e3-bce9131163c6";
const EPOCH: &str = "018f82d4-6f59-7d06-92e3-bce9131163c3";
const NEXT_EPOCH: &str = "018f82d4-6f59-7d06-92e3-bce9131163c9";
const NEXT_CAPABILITY: &str = "018f82d4-6f59-7d06-92e3-bce9131163ca";
const LANE: &str = "018f82d4-6f59-7d06-92e3-bce9131163c2";
const PRINCIPAL: &str = "018f82d4-6f59-7d06-92e3-bce9131163b0";
const OWNER_ACTOR: &str = "principal:018f82d4-6f59-7d06-92e3-bce9131163b0";
const REQUEST: &str = "018f82d4-6f59-7d06-92e3-bce9131163c4";
const SECRET_LANE_CAPABILITY: &str = "018f82d4-6f59-7d06-92e3-bce9131163cc";
const SECOND_LANE: &str = "018f82d4-6f59-7d06-92e3-bce9131163cd";
const THIRD_LANE: &str = "018f82d4-6f59-7d06-92e3-bce9131163ce";

fn uuid(text: &str) -> CanonicalUuid {
    text.parse().expect("fixture uuid is canonical")
}

fn instant_ms(text: &str) -> i64 {
    text.parse::<wire::Instant>()
        .expect("fixture instant")
        .get()
        .as_millisecond()
}

fn seq(value: u64) -> Seq {
    Seq::new(value).expect("fixture sequence is non-zero")
}

fn session(capability: &str, epoch: &str) -> Session {
    Session::open(uuid(capability), uuid(epoch))
}

fn acceptance(session: &Session, actor: &str, epoch: &str) -> app::Admitted<wire::Accepted> {
    session
        .admit_accepted(
            format!(r#"{{"wire":1,"actor":"{actor}","epoch":"{epoch}","release":"1.2.3"}}"#)
                .as_bytes(),
        )
        .expect("acceptance is admitted")
}

fn operator_directory(
    session: &Session,
) -> app::Admitted<Response<wire::principal::PrincipalDirectoryAnswer>> {
    let bytes = support::response(
        WireVersion::CURRENT,
        uuid(REQUEST),
        uuid(EPOCH),
        Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(seq(41)),
            body: wire::principal::PrincipalDirectoryAnswer {
                principals: wire::principal::PrincipalDirectory::display(&[
                    wire::principal::PrincipalHead {
                        principal: uuid(PRINCIPAL),
                        born: seq(1),
                        profile: wire::principal::PrincipalProfile {
                            name: Some("Ada Example".to_owned()),
                            email: None,
                        },
                        roles: std::collections::BTreeSet::from([
                            wire::principal::PrincipalRole::Operator,
                        ]),
                        capability: None,
                    },
                ]),
                roles: std::collections::BTreeSet::from([wire::principal::PrincipalRole::Operator]),
            },
        }),
    );
    session
        .admit_frame(&bytes)
        .expect("principal directory answer is admitted")
}

fn operator_runtime(session: &Session) -> AppRuntime {
    let accepted = acceptance(session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    assert!(runtime.apply_principals(&operator_directory(session)));
    runtime
}

fn task_fact(position: u64, cause: Option<u64>) -> serde_json::Value {
    let mut value = serde_json::json!({
        "v": 1,
        "seq": position,
        "at": "2030-04-12T18:44:09.271Z",
        "actor": "principal:00000000-0000-0000-0000-000000000000",
        "origin": "task:1kKRRsDPx",
        "fact": {
            "kind": "task::phased",
            "task": "1kKRRsDPx",
            "phase": "review",
        },
    });
    if let Some(cause) = cause {
        value["cause"] = serde_json::json!(cause);
    }
    value
}

fn packed_frame(
    kind: &str,
    rows: &[serde_json::Value],
    overflow: Option<(u64, u64)>,
    horizon: u64,
) -> Vec<u8> {
    let mut packed = serde_json::json!({
        "rows": rows,
        "horizon": horizon,
    });
    if let Some((omitted, continuation)) = overflow {
        packed["overflow"] = serde_json::json!({
            "omitted": omitted,
            "continuation": continuation,
        });
    }
    serde_json::to_vec(&serde_json::json!({
        "stream": kind,
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "packed": packed,
    }))
    .expect("stream fixture serializes")
}

fn message_frame(
    generation: u64,
    lane: &str,
    author: &str,
    offset: u64,
    event: u64,
    activity: Option<&str>,
    text: &str,
) -> Vec<u8> {
    message_frame_at(
        generation,
        (lane, author, offset, event),
        "2030-04-12T18:44:09.271Z",
        activity,
        text,
    )
}

fn message_frame_at(
    generation: u64,
    (lane, author, offset, event): (&str, &str, u64, u64),
    authored: &str,
    activity: Option<&str>,
    text: &str,
) -> Vec<u8> {
    let mut message = serde_json::json!({
        "lane": lane,
        "author": author,
        "locus": {"member": 1, "offset": offset, "length": 20, "event": event},
        "authored": authored,
        "text": text,
    });
    if let Some(activity) = activity {
        message["activity"] = serde_json::json!(activity);
    }
    message_generation_frame(
        generation,
        vec![message],
        vec![serde_json::json!({"lane": lane, "state": "complete"})],
    )
}

fn transcript_generation(
    generation: u64,
    messages: Vec<serde_json::Value>,
    frontiers: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "generation": generation,
        "horizon": 1,
        "messages": messages,
        "frontiers": frontiers,
    })
}

fn message_generation_frame(
    generation: u64,
    messages: Vec<serde_json::Value>,
    frontiers: Vec<serde_json::Value>,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "stream": "message-generation",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "generation": transcript_generation(generation, messages, frontiers),
    }))
    .expect("message generation serializes")
}

fn message_snapshot_frame(
    generation: u64,
    limit: usize,
    messages: Vec<serde_json::Value>,
    frontiers: Vec<serde_json::Value>,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "stream": "messages",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "limit": limit,
        "generation": transcript_generation(generation, messages, frontiers),
    }))
    .expect("message snapshot serializes")
}

fn initialize_messages(runtime: &mut AppRuntime, session: &Session) {
    runtime.apply_stream_frame(&admitted_stream(
        session,
        &message_snapshot_frame(0, 256, Vec::new(), Vec::new()),
    ));
}

fn admitted_stream(session: &Session, bytes: &[u8]) -> app::Admitted<StreamFrame> {
    session
        .admit_stream(bytes)
        .expect("stream frame is admitted")
}

fn fixture_stamp(label: &str) -> wire::TaskStamp {
    let value = label.bytes().fold(0_i64, |value, byte| {
        (value * 131 + i64::from(byte)) % 10_000_000_000_000
    });
    wire::TaskStamp::for_micros(value).expect("fixture task stamp")
}

fn board_task(
    label: &str,
    _id: &str,
    title: &str,
    topic: &str,
    phase: wire::Phase,
    provenance: Option<&str>,
    requirements: wire::EnvironmentRequirements,
) -> wire::intent::BoardTask {
    let born = fixture_stamp(label);
    wire::intent::BoardTask {
        ownership: None,
        origin_thread: "operator".to_owned(),
        creator_team: None,
        presentation_title: title.to_owned(),
        statement: wire::intent::Positioned {
            position: seq(1),
            body: wire::intent::BoardStatement {
                intent: None,
                preparation: None,
                handle: wire::TaskHandle::for_phase(
                    Some(&topic.parse().expect("fixture topic")),
                    &born,
                    phase,
                ),
                born: born.clone(),
                topic_origin: seq(1),
                pitch: "This work keeps the fixture honest.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements,
                resources: Default::default(),
                topic: topic.parse().expect("fixture topic"),
                priority: None,
                taste: None,
                dependencies: Vec::new(),
                provenance: provenance
                    .map_or_else(|| format!("task:{born}"), str::to_owned)
                    .parse()
                    .expect("fixture origin"),
                creation: wire::body::TaskCreationContext::unavailable(
                    wire::body::TaskCreationUnavailable::PrincipalActor,
                ),
                revision: seq(1),
            },
        },
        workflow: wire::intent::Positioned {
            position: seq(2),
            body: wire::WorkflowHead {
                flow: vec![wire::Phase::Draft, wire::Phase::Todo],
                phase,
                status: wire::WorkflowStatus::pending(),
                deferral: None,
                submitted: false,
                generation: 0,
            },
        },
        completion: wire::intent::Positioned {
            position: seq(2),
            body: wire::CompletionHead::opened(seq(2)),
        },
        claims: Vec::new(),
        original_claimed_at: None,
        invitations: Vec::new(),
        explicit_release: false,
        design_cycle: wire::rooms::DesignCycleFacts::default(),
        design_coverage: None,
        collapse_reason: None,
        controlled: Vec::new(),
        claim_context: None,
        git_context: None,
        parked_commits: None,
    }
}

fn board_response(
    session: &Session,
    hidden_title: &str,
) -> app::Admitted<Response<wire::intent::BoardAnswer>> {
    board_response_with_requirements(
        session,
        hidden_title,
        wire::EnvironmentRequirements::one(
            wire::EnvironmentRequirement::journal_schema(2)
                .expect("future fixture schema is positive"),
        ),
        Vec::new(),
    )
}

fn board_response_with_requirements(
    session: &Session,
    hidden_title: &str,
    requirements: wire::EnvironmentRequirements,
    trust: Vec<wire::intent::Positioned<wire::TrustHead>>,
) -> app::Admitted<Response<wire::intent::BoardAnswer>> {
    // Principal-authored board fixture.
    let body = wire::intent::BoardAnswer {
        next: None,
        offer: Vec::new(),
        blocked: Vec::new(),
        workflow_waiting: Vec::new(),
        design_cycles: Default::default(),
        oops_generation: None,
        origins: Default::default(),
        acknowledgments: Default::default(),
        closures: Default::default(),
        steps: Default::default(),
        schema: 1,
        cursor: seq(4),
        task_barrier: wire::Digest::compute(wire::Domain::BoardBarrier, b"fixture projection"),
        principal: wire::Actor::TEST_PRINCIPAL,
        actor_route: wire::intent::BoardActorRoute {
            actor: "operator".to_owned(),
            origin_thread: "operator".to_owned(),
        },
        tasks: vec![
            board_task(
                "DRAFT-draft1",
                REQUEST,
                hidden_title,
                "/app/correctness",
                wire::Phase::Draft,
                None,
                wire::EnvironmentRequirements::default(),
            ),
            board_task(
                "PUBLIC-work1",
                NEXT_CAPABILITY,
                "Visible allocator work",
                "/board/visibility",
                wire::Phase::Todo,
                Some("ack:1kQwPZtKx"),
                requirements,
            ),
        ],
        finished: Vec::new(),
        history: None,
        addresses: Default::default(),
        selected: None,
        codes: Default::default(),
        trust,
        resource_pools: Default::default(),
        appliance: wire::manifest::ProductVersion::parse("0.2.0").expect("fixture appliance"),
        residency: wire::intent::Positioned {
            position: seq(3),
            body: wire::ResidencyHead {
                epoch: uuid(EPOCH),
                state: wire::ResidentState::Seated,
                topology_revision: 0,
                lanes: Vec::new(),
                subscriptions: Vec::new(),
                renewals: Vec::new(),
            },
        },
    };
    let bytes = support::response(
        WireVersion::CURRENT,
        uuid(REQUEST),
        uuid(EPOCH),
        Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(seq(4)),
            body,
        }),
    );
    session
        .admit_frame(&bytes)
        .expect("board response is admitted")
}

/// One admitted safe lane roster, one row per `(lane, team, occupant, bound)` tuple.
///
/// The display session identity crosses; capabilities remain outside this DTO.
fn lane_response(rows: &[(&str, &str, Option<&str>, bool)]) -> Vec<u8> {
    lane_response_with_renewals(rows, Vec::new())
}

fn lane_response_with_renewals(
    rows: &[(&str, &str, Option<&str>, bool)],
    renewals: Vec<wire::LaneRenewalRecord>,
) -> Vec<u8> {
    let subscriptions = rows
        .iter()
        .map(|(_, team, _, _)| {
            let formed_at = wire::head::teamed(
                uuid(team),
                rows.iter()
                    .filter(|(_, candidate, _, _)| candidate == team)
                    .map(|(lane, _, _, _)| uuid(lane)),
            )
            .then(|| {
                "2030-04-12T18:44:09.271Z"
                    .parse()
                    .expect("fixture formation instant")
            });
            let team = uuid(team);
            (
                team,
                wire::TeamSubscription {
                    team,
                    formed_at,
                    drive: true,
                    drain: false,
                    name: None,
                    fast_mode: false,
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_values()
        .collect();
    let lanes = rows
        .iter()
        .map(
            |(lane, team, occupant, bound)| wire::intent::LaneAnswerRow {
                lane: uuid(lane),
                team: uuid(team),
                name: None,
                worktree: format!("/tmp/worktree-{}", &lane[..8]),
                speech: wire::SpeechPlayback::Disabled {},
                life: "steer".to_owned(),
                vendor: "omp".parse().expect("fixture vendor"),
                model: bound.then(|| "sonnet".to_owned()),
                seat_model: None,
                effort: bound.then(|| "high".to_owned()),
                seat_effort: None,
                fast_mode: false,
                fast_refusal: None,
                seat_fast_mode: false,
                git: bound.then(|| "current".to_owned()),
                rtk: None,
                occupant: occupant.map(uuid),
                code: None,
                lineage: Vec::new(),
                jobs: Vec::new(),
                session: bound.then(|| "session-fixture".to_owned()),
                bound: *bound,
                disposition: if occupant.is_some() {
                    "occupied".to_owned()
                } else {
                    "vacant".to_owned()
                },
                liveness: if occupant.is_some() {
                    wire::intent::LaneLiveness::Working
                } else {
                    wire::intent::LaneLiveness::Vacant
                },
                hold: None,
                parked: None,
            },
        )
        .collect::<Vec<_>>();
    support::response(
        WireVersion::CURRENT,
        uuid(REQUEST),
        uuid(EPOCH),
        Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(seq(41)),
            body: wire::intent::LaneAnswer {
                epoch: uuid(EPOCH),
                state: wire::ResidentState::Seated,
                topology_revision: 9,
                lanes,
                subscriptions,
                renewals,
                refresh_after_millis: None,
            },
        }),
    )
}

fn fact_frame(fact: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "stream": "fact",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "fact": fact,
    }))
    .expect("fact fixture serializes")
}

#[test]
fn stream_application_invalidates_only_observable_runtime_changes() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let live = admitted_stream(&session, &fact_frame(task_fact(41, None)));

    assert!(runtime.apply_stream_frame(&live).changed());
    assert!(
        !runtime.apply_stream_frame(&live).changed(),
        "a duplicate live fact does not invalidate the rendered surface"
    );

    let stale = admitted_stream(&session, &fact_frame(task_fact(40, None)));
    assert!(
        !runtime.apply_stream_frame(&stale).changed(),
        "a stale live fact does not invalidate the rendered surface"
    );

    let pong = wire::encode(&StreamFrame::Pong {
        v: WireVersion::CURRENT,
        id: uuid(REQUEST),
        epoch: uuid(EPOCH),
        config_revision: uuid(REQUEST),
    })
    .expect("liveness answer encodes");
    assert!(
        !runtime
            .apply_stream_frame(&admitted_stream(&session, &pong))
            .changed(),
        "a Pong does not invalidate the rendered surface"
    );

    let next = Session::open(uuid(NEXT_CAPABILITY), uuid(NEXT_EPOCH));
    let next_accepted = acceptance(&next, OWNER_ACTOR, NEXT_EPOCH);
    let mut foreign_runtime = AppRuntime::new(&next_accepted);
    assert!(
        !foreign_runtime.apply_stream_frame(&live).changed(),
        "an admitted frame from another scope does not invalidate the rendered surface"
    );
}

#[test]
fn a_restarted_scope_stays_hydrating_until_its_replacement_is_published() {
    let mut surface = AppSurface::default();
    let (_, hydrating) = surface.admit(&acceptance(&session(OPERATOR, EPOCH), OWNER_ACTOR, EPOCH));
    assert!(!hydrating, "the first runtime is shown as it is accepted");
    let shown = surface.publication();

    let restarted = Session::open(uuid(NEXT_CAPABILITY), uuid(NEXT_EPOCH));
    let accepted = acceptance(&restarted, OWNER_ACTOR, NEXT_EPOCH);
    assert!(
        surface.admit(&accepted).1,
        "a restarted resident hydrates first"
    );
    assert!(
        surface.admit(&accepted).1,
        "a connection lost mid-hydration resumes it instead of showing a replacement without snapshots"
    );
    assert!(
        shown
            .read()
            .as_ref()
            .is_some_and(|runtime| !runtime.same_scope(&accepted)),
        "the surface on screen keeps the previous scope throughout"
    );

    let replacement = surface.publication();
    assert!(replacement != shown);
    assert!(
        !surface.admit(&accepted).1,
        "a published replacement reconnects in place"
    );
}

#[test]
fn stream_door_checks_nested_wire_and_terminal_cursor_epoch() {
    let session = session(OPERATOR, EPOCH);
    let mut foreign_envelope = task_fact(41, None);
    foreign_envelope["v"] = serde_json::json!(99);
    let fault = session
        .admit_stream(&packed_frame("facts", &[foreign_envelope], None, 41))
        .expect_err("nested foreign wire faults");
    assert_eq!(fault.reason, "frame::wire-mismatch");

    let stale = serde_json::to_vec(&serde_json::json!({
        "stream": "closing",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "after": {
            "epoch": NEXT_EPOCH,
            "seq": 41,
        },
    }))
    .expect("closing fixture serializes");
    let fault = session
        .admit_stream(&stale)
        .expect_err("foreign terminal cursor faults");
    assert_eq!(fault.reason, "frame::epoch-mismatch");
}

#[test]
fn principal_and_lane_stream_authority_come_from_current_principal_roles() {
    let principal_session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&principal_session, OWNER_ACTOR, EPOCH);
    let mut principal = AppRuntime::new(&accepted);
    assert_eq!(
        principal.subjects(),
        vec![ViewSubject::Facts, ViewSubject::Rooms]
    );
    assert!(principal.offers(CommandKind::PrincipalDirectoryRead));
    assert!(!principal.offers(CommandKind::PrincipalRead));
    assert!(!principal.offers(CommandKind::ConsentDecide));

    assert!(principal.apply_principals(&operator_directory(&principal_session)));
    assert_eq!(
        principal.subjects(),
        vec![
            ViewSubject::Facts,
            ViewSubject::Rooms,
            ViewSubject::Messages,
            ViewSubject::Quota,
        ]
    );
    assert!(principal.offers(CommandKind::ConsentDecide));
    assert_eq!(
        principal
            .principals()
            .and_then(|principals| principals.identity(principal.principal().actor()))
            .and_then(|identity| identity.name.as_deref()),
        Some("Ada Example")
    );

    let lane_session = session(LANE_CAPABILITY, EPOCH);
    let lane = acceptance(&lane_session, &format!("lane:{LANE}"), EPOCH);
    let lane = AppRuntime::new(&lane);
    assert_eq!(
        lane.subjects(),
        vec![ViewSubject::Facts, ViewSubject::Rooms]
    );
    assert!(lane.offers(CommandKind::TaskNote));
    assert!(!lane.offers(CommandKind::ConsentDecide));
}

#[test]
fn quota_generations_replace_monotonically_and_reconnect_hydrates_current() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);

    let initial = quota_frame("quota", 2);
    assert!(
        runtime
            .apply_stream_frame(&admitted_stream(&session, &initial))
            .changed()
    );
    assert_eq!(runtime.quota().map(|quota| quota.generation), Some(2));

    let replay = quota_frame("quota-generation", 1);
    assert!(
        !runtime
            .apply_stream_frame(&admitted_stream(&session, &replay))
            .changed()
    );
    assert_eq!(runtime.quota().map(|quota| quota.generation), Some(2));

    assert!(!runtime.reconnect(&acceptance(&session, OWNER_ACTOR, EPOCH)));
    let hydrated = quota_frame("quota", 3);
    assert!(
        runtime
            .apply_stream_frame(&admitted_stream(&session, &hydrated))
            .changed()
    );
    assert_eq!(runtime.quota().map(|quota| quota.generation), Some(3));
}

fn quota_frame(stream: &str, generation: u64) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "stream": stream,
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "generation": {
            "generation": generation,
            "refreshing": false,
        },
    }))
    .expect("quota frame serializes")
}

/// One lane filled to its message floor beside a lane that said one thing.
fn lane_floor_snapshot() -> (Vec<serde_json::Value>, serde_json::Value, Vec<u8>) {
    let floor = u64::try_from(wire::MESSAGE_FLOOR).expect("the floor fits an offset");
    let busy = (0..floor)
        .map(|offset| {
            serde_json::json!({
                "lane": LANE,
                "author": LANE_CAPABILITY,
                "locus": {"member": 1, "offset": 10 + offset, "length": 20, "event": 0},
                "authored": "2030-04-12T18:44:09.271Z",
                "text": format!("first author {offset}"),
            })
        })
        .collect::<Vec<_>>();
    let second = serde_json::json!({
        "lane": SECOND_LANE,
        "author": SECRET_LANE_CAPABILITY,
        "locus": {"member": 1, "offset": 10, "length": 20, "event": 0},
        "authored": "2030-04-12T18:44:10.271Z",
        "text": "second author",
    });
    let mut incoming = busy.clone();
    incoming.push(second.clone());
    let snapshot = message_snapshot_frame(
        1,
        wire::MESSAGE_FLOOR,
        incoming,
        vec![
            serde_json::json!({"lane": LANE, "state": "complete"}),
            serde_json::json!({"lane": SECOND_LANE, "state": "complete"}),
        ],
    );
    (busy, second, snapshot)
}

#[test]
fn message_snapshot_and_live_push_bound_each_lane_without_evicting_its_siblings() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let (busy, second, snapshot) = lane_floor_snapshot();
    assert!(
        runtime
            .apply_stream_frame(&admitted_stream(&session, &snapshot))
            .directives()
            .is_empty()
    );
    assert_eq!(runtime.messages().len(), wire::MESSAGE_FLOOR + 1);
    assert_eq!(
        runtime
            .activities()
            .iter()
            .filter(|activity| activity.lane() == uuid(SECOND_LANE))
            .count(),
        1,
        "the same tape coordinates in two lanes are two activities"
    );
    assert!(
        !runtime
            .apply_stream_frame(&admitted_stream(&session, &snapshot))
            .changed(),
        "an identical retained-message snapshot does not invalidate the surface"
    );
    assert_eq!(
        runtime.activities().len(),
        wire::MESSAGE_FLOOR + 1,
        "snapshot replay is idempotent"
    );
    assert_eq!(
        runtime.messages().len(),
        wire::MESSAGE_FLOOR + 1,
        "snapshot replay restores prose cards"
    );

    let duplicate = message_generation_frame(
        2,
        vec![second],
        vec![
            serde_json::json!({"lane": LANE, "state": "complete"}),
            serde_json::json!({"lane": SECOND_LANE, "state": "complete"}),
        ],
    );
    assert!(
        !runtime
            .apply_stream_frame(&admitted_stream(&session, &duplicate))
            .changed(),
        "a duplicate live message does not invalidate the surface"
    );
    assert_eq!(runtime.messages().len(), wire::MESSAGE_FLOOR + 1);

    let newest = message_generation_frame(
        3,
        vec![serde_json::json!({
            "lane": LANE,
            "author": THIRD_LANE,
            "locus": {"member": 1, "offset": 50, "length": 20, "event": 0},
            "authored": "2030-04-12T18:44:11.271Z",
            "text": "third author",
        })],
        vec![
            serde_json::json!({"lane": LANE, "state": "complete"}),
            serde_json::json!({"lane": SECOND_LANE, "state": "complete"}),
        ],
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &newest));
    let retained = runtime.messages();
    assert_eq!(
        retained
            .iter()
            .filter(|message| message.lane == uuid(LANE))
            .map(|message| message.locus.offset)
            .collect::<Vec<_>>(),
        busy.iter()
            .skip(1)
            .map(|card| card["locus"]["offset"].as_u64().expect("fixture offset"))
            .chain(std::iter::once(50))
            .collect::<Vec<_>>(),
        "the busy lane slides its own floor forward",
    );
    assert_eq!(
        retained
            .iter()
            .filter(|message| message.lane == uuid(SECOND_LANE))
            .map(|message| message.author)
            .collect::<Vec<_>>(),
        [uuid(SECRET_LANE_CAPABILITY)],
        "the quiet sibling keeps everything it said",
    );
    assert_eq!(
        runtime
            .activities()
            .iter()
            .filter(|activity| activity.lane() == uuid(SECOND_LANE))
            .count(),
        1,
        "one lane's new activity replaces only that lane's prior status"
    );
}

#[test]
fn paired_tool_capture_becomes_one_media_card_while_distinct_tape_events_stay_distinct() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let snapshot = message_snapshot_frame(0, 8, Vec::new(), Vec::new());
    runtime.apply_stream_frame(&admitted_stream(&session, &snapshot));

    let call = message_frame(
        1,
        LANE,
        LANE_CAPABILITY,
        60,
        0,
        Some("tool"),
        "view_image: capture.png",
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &call));
    assert!(
        runtime.messages().is_empty(),
        "the call remains activity, so its answering image is rendered only once"
    );

    let media = serde_json::json!({
        "name": "capture.png",
        "content_type": "image/png",
        "data_url": "data:image/png;base64,iVBORw0KGgo=",
    });
    let result_message = serde_json::json!({
        "lane": LANE,
        "author": LANE_CAPABILITY,
        "locus": {"member": 1, "offset": 80, "length": 20, "event": 1},
        "authored": "2030-04-12T18:44:09.271Z",
        "activity": "tool",
        "text": "view_image: capture.png succeeded",
        "media": [media.clone()],
    });
    let result = message_generation_frame(
        2,
        vec![result_message.clone()],
        vec![serde_json::json!({"lane": LANE, "state": "complete"})],
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &result));
    let replay = message_generation_frame(
        3,
        vec![result_message],
        vec![serde_json::json!({"lane": LANE, "state": "complete"})],
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &replay));
    assert_eq!(runtime.messages().len(), 1, "one tape event is idempotent");
    assert_eq!(runtime.messages()[0].media.len(), 1);

    let distinct = message_generation_frame(
        4,
        vec![serde_json::json!({
            "lane": LANE,
            "author": LANE_CAPABILITY,
            "locus": {"member": 1, "offset": 120, "length": 20, "event": 1},
            "authored": "2030-04-12T18:44:10.271Z",
            "activity": "tool",
            "text": "view_image: capture.png succeeded",
            "media": [media],
        })],
        vec![serde_json::json!({"lane": LANE, "state": "complete"})],
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &distinct));
    assert_eq!(
        runtime.messages().len(),
        2,
        "distinct causal tape events remain separately browsable even for identical bytes"
    );
}

#[test]
fn team_activity_orders_tool_and_prose_without_admitting_snapshot_receipts() {
    let runtime = team_runtime_with_activity_and_receipts();
    let lanes = runtime.lanes().expect("refreshed lane snapshot");
    let team = runtime
        .newest_team_activity(&lanes.lanes[0])
        .expect("team activity survives receipts");
    assert_eq!(team.lane(), uuid(SECOND_LANE));
    assert_eq!(
        runtime
            .newest_lane_activity(uuid(SECOND_LANE))
            .expect("the member lane retains its own newest activity")
            .display_millis(),
        instant_ms("2030-04-12T18:44:10.271Z")
    );
    assert_eq!(team.class(), "prose");
    assert_eq!(team.detail(), "newer team member prose");
    let other = runtime
        .newest_team_activity(&lanes.lanes[1])
        .expect("other team keeps its own activity");
    assert_eq!(other.lane(), uuid(THIRD_LANE));
    assert_eq!(other.class(), "tool");
    assert_eq!(other.detail(), "Read");
}

fn team_runtime_with_activity_and_receipts() -> AppRuntime {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let lanes = lane_response(&[
        (LANE, LANE, Some(LANE_CAPABILITY), true),
        (SECOND_LANE, LANE, Some(SECRET_LANE_CAPABILITY), true),
        (THIRD_LANE, THIRD_LANE, Some(THIRD_LANE), true),
    ]);
    let admitted_lanes: app::Admitted<Response<wire::intent::LaneAnswer>> = session
        .admit_frame(&lanes)
        .expect("lane response is admitted");
    runtime.apply_lanes(&admitted_lanes);
    initialize_messages(&mut runtime, &session);

    let tool = message_frame_at(
        1,
        (LANE, LANE_CAPABILITY, 60, 0),
        "2030-04-12T18:44:09.271Z",
        Some("tool"),
        "Bash",
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &tool));
    let team = &runtime.lanes().expect("lane snapshot").lanes[0];
    let activity = runtime
        .newest_team_activity(team)
        .expect("transcript tool becomes activity");
    assert_eq!(activity.lane(), uuid(LANE));
    assert_eq!(
        activity.display_millis(),
        instant_ms("2030-04-12T18:44:09.271Z")
    );
    assert_eq!(activity.class(), "tool");
    assert_eq!(activity.detail(), "Bash");
    assert!(
        runtime.messages().is_empty(),
        "tool activity does not become a prose card"
    );

    let prose = message_frame_at(
        2,
        (SECOND_LANE, SECRET_LANE_CAPABILITY, 70, 0),
        "2030-04-12T18:44:10.271Z",
        None,
        "newer team member prose",
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &prose));

    let other_tool = message_frame_at(
        3,
        (THIRD_LANE, THIRD_LANE, 80, 0),
        "2030-04-12T18:44:11.271Z",
        Some("tool"),
        "Read",
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &other_tool));

    apply_receipt_noise(&session, &mut runtime, &admitted_lanes);
    runtime
}

fn apply_receipt_noise(
    session: &Session,
    runtime: &mut AppRuntime,
    admitted_lanes: &app::Admitted<Response<wire::intent::LaneAnswer>>,
) {
    // Snapshot receipts still belong to the command-outcome chronology, but
    // none can enter the team activity chronology or displace its prose.
    runtime.apply_lanes(admitted_lanes);
    runtime.apply_board(&board_response(session, "hidden receipt witness"));
    let inbox = support::response(
        WireVersion::CURRENT,
        uuid(REQUEST),
        uuid(EPOCH),
        Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(seq(43)),
            body: wire::intent::InboxAnswer {
                epoch: uuid(EPOCH),
                lanes: Vec::new(),
            },
        }),
    );
    let admitted_inbox: app::Admitted<Response<wire::intent::InboxAnswer>> = session
        .admit_frame(&inbox)
        .expect("inbox response is admitted");
    runtime.apply_inbox(&admitted_inbox);
}

#[test]
fn synthetic_agent_activity_retains_provenance_and_gains_a_display_time() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let lanes = lane_response(&[(LANE, LANE, Some(LANE_CAPABILITY), true)]);
    let admitted_lanes: app::Admitted<Response<wire::intent::LaneAnswer>> = session
        .admit_frame(&lanes)
        .expect("lane response is admitted");
    runtime.apply_lanes(&admitted_lanes);
    initialize_messages(&mut runtime, &session);

    let synthetic = message_generation_frame(
        1,
        vec![serde_json::json!({
            "lane": LANE,
            "author": LANE_CAPABILITY,
            "locus": {"member": 1, "offset": 60, "length": 20, "event": 0},
            "authored": "2000-01-01T00:00:00.087Z",
            "authored_synthetic": true,
            "text": "historical prose without a vendor timestamp",
        })],
        vec![serde_json::json!({"lane": LANE, "state": "complete"})],
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &synthetic));

    let activity = runtime
        .newest_team_activity(&runtime.lanes().expect("lane snapshot").lanes[0])
        .expect("synthetic prose remains visible as activity");
    assert_eq!(
        activity.detail(),
        "historical prose without a vendor timestamp"
    );
    assert_eq!(
        activity.display_millis(),
        instant_ms("2000-01-01T00:00:00.087Z"),
        "a synthetic ordering instant remains available to the display chronology"
    );
    assert_eq!(
        runtime.messages().len(),
        1,
        "synthetic prose still becomes a browsable message card"
    );
    assert!(runtime.messages()[0].authored_synthetic);
    assert_eq!(
        runtime.messages()[0].observed,
        None,
        "display projection does not relabel ordering identity as Spice observation time"
    );
}

#[test]
fn redelivered_older_activity_does_not_displace_newer_lane_activity() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let lanes = lane_response(&[(LANE, LANE, Some(LANE_CAPABILITY), true)]);
    let admitted_lanes: app::Admitted<Response<wire::intent::LaneAnswer>> = session
        .admit_frame(&lanes)
        .expect("lane response is admitted");
    runtime.apply_lanes(&admitted_lanes);
    initialize_messages(&mut runtime, &session);

    let older_tool = message_frame(1, LANE, LANE_CAPABILITY, 60, 0, Some("tool"), "Bash");
    let newer_prose = message_frame(
        2,
        LANE,
        LANE_CAPABILITY,
        90,
        1,
        None,
        "the newest thing this agent did",
    );
    let older_replay = message_frame(3, LANE, LANE_CAPABILITY, 60, 0, Some("tool"), "Bash");
    for frame in [&older_tool, &newer_prose, &older_replay] {
        runtime.apply_stream_frame(&admitted_stream(&session, frame));
    }

    let activity = runtime
        .newest_team_activity(&runtime.lanes().expect("lane snapshot").lanes[0])
        .expect("lane keeps its newest activity");
    assert_eq!(
        activity.detail(),
        "the newest thing this agent did",
        "a redelivered older activity must not displace the newer one"
    );
    assert_eq!(
        runtime.activities().len(),
        2,
        "both unique identities are retained and the replay adds no third row"
    );
}

#[test]
fn team_ticker_prose_leads_with_the_first_authored_line_after_borrowed_quotes() {
    // The composition below is the one `render_team_ticker` performs over
    // `detail()` for a prose activity.
    const TICKER_BUDGET: usize = app::watchband::STATUS_LINE_DOM_LIMIT;
    const STANDING_LABEL: &str = "Jeff";

    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let lanes = lane_response(&[(LANE, LANE, Some(LANE_CAPABILITY), true)]);
    let admitted_lanes: app::Admitted<Response<wire::intent::LaneAnswer>> = session
        .admit_frame(&lanes)
        .expect("lane response is admitted");
    runtime.apply_lanes(&admitted_lanes);
    initialize_messages(&mut runtime, &session);

    let quoted_reply = message_frame(
        1,
        LANE,
        LANE_CAPABILITY,
        60,
        0,
        None,
        "> ship the ticker fix today\n>\n  > and keep the composer rule\n\n\
         On it: the ticker shares the composer header rule.\n\
         A second authored line stays on the card.",
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &quoted_reply));
    let activity = runtime
        .newest_team_activity(&runtime.lanes().expect("lane snapshot").lanes[0])
        .expect("quoted prose still becomes activity");
    assert_eq!(activity.class(), "prose");
    assert_eq!(
        activity.detail(),
        "On it: the ticker shares the composer header rule.",
        "the ticker leads with the first authored line, never the borrowed quote"
    );
    let (prefix, detail) = ticker_chunks(STANDING_LABEL, activity.detail(), TICKER_BUDGET);
    let line = format!("{prefix}: {detail}");
    let accessible =
        ticker_accessible_label(STANDING_LABEL, activity.class(), &line, TICKER_BUDGET);
    assert_eq!(
        line,
        "Jeff: On it: the ticker shares the composer header rule."
    );
    assert_eq!(
        accessible,
        "Jeff: Jeff: On it: the ticker shares the composer header rule."
    );
    for quoted in ["ship the ticker fix", "keep the composer rule", ">"] {
        assert!(
            !line.contains(quoted) && !accessible.contains(quoted),
            "{quoted:?} is the operator's quoted text, not the agent's words"
        );
    }
    assert!(
        runtime
            .messages()
            .iter()
            .any(|message| message.text.starts_with("> ship the ticker fix today\n")),
        "the conversation card still carries the message verbatim, quotes included"
    );

    let entirely_quoted = message_frame(
        2,
        LANE,
        LANE_CAPABILITY,
        90,
        1,
        None,
        "> only the operator's words\n>\n> and nothing authored underneath",
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &entirely_quoted));
    let activity = runtime
        .newest_team_activity(&runtime.lanes().expect("lane snapshot").lanes[0])
        .expect("an entirely quoted reply is still the newest activity");
    assert_eq!(
        activity.detail(),
        "quoted message",
        "with no authored line the ticker wears the composer header's absent-preview label"
    );
    let (prefix, detail) = ticker_chunks(STANDING_LABEL, activity.detail(), TICKER_BUDGET);
    assert_eq!(format!("{prefix}: {detail}"), "Jeff: quoted message");

    let tool = message_frame(
        3,
        LANE,
        LANE_CAPABILITY,
        120,
        2,
        Some("tool"),
        "Bash: grep -n '> quoted' app/src/watchband.rs",
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &tool));
    let activity = runtime
        .newest_team_activity(&runtime.lanes().expect("lane snapshot").lanes[0])
        .expect("tool activity follows");
    assert_eq!(
        activity.detail(),
        "Bash: grep -n '> quoted' app/src/watchband.rs",
        "only prose adopts the quote rule; a tool detail keeps its text verbatim"
    );
}

#[test]
fn browser_state_retains_drafts_with_their_authored_prose_and_public_topic() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let title = "Allocator failure needing triage";
    let response = board_response(&session, title);
    let wire::Outcome::Reply(reply) = &response.get().outcome else {
        panic!("board fixture is a reply");
    };
    assert_eq!(reply.body.tasks[0].presentation_title(), title);

    runtime.apply_board(&response);

    let board = runtime.board().expect("board response is retained");
    assert_eq!(board.schema, 1);
    let draft = &board.tasks[0];
    assert_eq!(draft.presentation_title(), title);
    assert_eq!(
        draft.statement.body.handle,
        wire::TaskHandle::for_phase(
            Some(&"/app/correctness".parse().expect("fixture topic")),
            &fixture_stamp("DRAFT-draft1"),
            wire::Phase::Draft,
        )
    );
    assert_eq!(
        draft
            .statement
            .body
            .topic
            .path
            .as_ref()
            .map(wire::Topic::as_str),
        Some("/app/correctness")
    );
    assert!(draft.workflow.body.is_draft());
    assert!(draft.workflow.body.status.is_pending());
    let draft_json = serde_json::to_value(draft).expect("browser row serializes");
    assert_eq!(draft_json["presentation_title"], title);
    assert_eq!(
        board.tasks[1].presentation_title(),
        "Visible allocator work"
    );
    assert!(
        !board.environment_satisfies(&board.tasks[1]),
        "the app snapshot evaluates requirements against its observed schema",
    );
}

/// Configuration is outside the journal cursor: observing a policy change
/// must reread ownership without skipping later journal facts.
#[test]
fn configuration_revisions_invalidate_board_without_advancing_journal_cursor() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    runtime.apply_stream_frame(&admitted_stream(
        &session,
        &packed_frame("facts", &[task_fact(41, None)], None, 41),
    ));
    let settled = runtime.resume().expect("resume cursor");
    let terminal = runtime.terminal();

    for (revision, board_stale) in [(REQUEST, true), (REQUEST, false), (NEXT_EPOCH, true)] {
        let pong = wire::encode(&StreamFrame::Pong {
            v: WireVersion::CURRENT,
            id: uuid(REQUEST),
            epoch: uuid(EPOCH),
            config_revision: uuid(revision),
        })
        .expect("configuration probe encodes");
        let application = runtime.apply_stream_frame(&admitted_stream(&session, &pong));
        assert_eq!(application.board_stale(), board_stale);
        assert!(
            !application.changed(),
            "only the fresh board answer redraws ownership"
        );
        assert_eq!(runtime.resume(), Some(settled));
        assert_eq!(runtime.terminal(), terminal);
    }

    // Reconnecting to the same generation still continues from the cursor the
    // fact settled, which is the property the probe could have quietly spent.
    assert!(!runtime.reconnect(&acceptance(&session, OWNER_ACTOR, EPOCH)));
    assert_eq!(runtime.resume(), Some(settled));
}

#[test]
fn same_scope_reconnect_preserves_cursor_and_changed_scope_clears_it() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let frame = admitted_stream(
        &session,
        &packed_frame("facts", &[task_fact(41, None)], None, 41),
    );
    runtime.apply_stream_frame(&frame);
    assert!(!runtime.reconnect(&acceptance(&session, OWNER_ACTOR, EPOCH)));
    assert_eq!(runtime.resume().expect("resume retained").seq, seq(41));

    let next = Session::open(uuid(NEXT_CAPABILITY), uuid(NEXT_EPOCH));
    let next_accepted = acceptance(&next, OWNER_ACTOR, NEXT_EPOCH);
    assert!(runtime.reconnect(&next_accepted));
    assert_eq!(runtime.resume(), None);
    assert_eq!(runtime.terminal(), None);
}
