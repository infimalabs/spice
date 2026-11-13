//! Room authority comes from each subscription's room snapshot and the live
//! facts after its horizon, never from browser-local foreground state.

use super::*;
use serde::Serialize;
use wire::rooms::{
    DecisionBasis, DecisionOutcome, DecisionRef, ExecutionAuthorization, ExecutionAuthorized,
    PreparationDecided, PreparationDecision, RoomAuthorityStamp, RoomMessaged, RoomSource,
    TextRange,
};

#[path = "rooms/contributions.rs"]
mod contributions;
#[path = "rooms/conversation.rs"]
mod conversation;

const ROOM_EVENT: &str = "dVsYdDj7d";
const LEAVE_EVENT: &str = "dVsYdDj7k";
const STOP_KEY: &str = "dStopKey1";
/// The claim win the fixture stop request names.
const STOPPED_CLAIM: u64 = 3;

fn authority(text: &str) -> RoomAuthorityStamp {
    text.parse().expect("fixture room authority")
}

fn operator_post() -> RoomAuthorityStamp {
    RoomAuthorityStamp::for_micros(authority(ROOM_EVENT).micros() + 1)
        .expect("fixture operator post authority")
}

fn envelope<T: Serialize>(position: u64, kind: &str, body: &T) -> serde_json::Value {
    let mut fact = serde_json::to_value(body)
        .expect("fact body serializes")
        .as_object()
        .expect("fact body is an object")
        .clone();
    fact.insert("kind".to_owned(), serde_json::json!(kind));
    serde_json::json!({
        "v": 1,
        "seq": position,
        "at": "2030-04-12T18:44:09.271Z",
        "actor": OWNER_ACTOR,
        "origin": "thread:operator",
        "fact": fact,
    })
}

fn live_fact(fact: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "stream": "fact",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "fact": fact,
    }))
    .expect("live fact frame serializes")
}

fn room() -> wire::TaskStamp {
    fixture_stamp("PUBLIC-work1")
}

fn room_message(position: u64) -> serde_json::Value {
    let body = RoomMessaged {
        room: room(),
        publication: authority(ROOM_EVENT),
        hunk: wire::rooms::TranscriptHunk {
            head: wire::TranscriptSourceHead {
                lane: uuid(LANE),
                vendor: "omp".parse().expect("fixture vendor"),
                stream: "session".to_owned(),
                source: uuid(REQUEST),
                next: wire::body::TranscriptPosition {
                    member: 1,
                    offset: 64,
                },
            },
            locus: wire::MessageLocus {
                member: 1,
                offset: 8,
                length: 32,
                event: 0,
            },
            text: TextRange { start: 0, end: 12 },
        },
        // An occupant is not a logical lane; the projection must not use this
        // identity as its participant key.
        author: uuid(SECOND_LANE),
        directed: Vec::new(),
    };
    envelope(position, "room::messaged", &body)
}

fn room_post(position: u64) -> serde_json::Value {
    envelope(
        position,
        "room::published",
        &wire::rooms::RoomPublished {
            room: room(),
            publication: operator_post(),
            content: wire::rooms::RoomPublicationContent::Authored {
                text: "Please inspect this room.".to_owned(),
            },
        },
    )
}

/// The stop request asking `lane`, holding the room's claim won at
/// [`STOPPED_CLAIM`], to checkpoint and free.
fn stop_direction(position: u64, lane: &str) -> serde_json::Value {
    let body = serde_json::json!({
        "directive": REQUEST,
        "key": STOP_KEY,
        "recipient": format!("lane:{lane}"),
        "text": "Checkpoint and free this Design",
        "sender": OWNER_ACTOR,
        "control": format!("room-stop:{}:{}", room(), wire::SequenceCode::new(STOPPED_CLAIM)),
        "expires": "2030-04-13T18:44:09.271Z",
    });
    envelope(position, "steer::sent", &body)
}

fn authorization(decision: u64) -> ExecutionAuthorization {
    ExecutionAuthorization {
        decision: DecisionRef {
            controller: room(),
            decision: wire::SequenceCode::new(decision),
        },
        selected_outputs: vec![fixture_stamp("HIDDEN-oops1")],
        output_revisions: Vec::new(),
    }
}

fn authorized(position: u64, decision: u64) -> serde_json::Value {
    let body = ExecutionAuthorized {
        authorization: authorization(decision),
    };
    envelope(position, "proposal::authorized", &body)
}

fn decision_ref(decision: u64) -> DecisionRef {
    DecisionRef {
        controller: room(),
        decision: wire::SequenceCode::new(decision),
    }
}

fn decided(position: u64, decision: u64, outcome: DecisionOutcome) -> serde_json::Value {
    envelope(
        position,
        "proposal::decided",
        &PreparationDecided {
            decision: PreparationDecision {
                decision: decision_ref(decision),
                source: RoomSource::Journal {
                    fact: wire::SequenceCode::new(position - 1),
                },
                basis: DecisionBasis {
                    statement: seq(1),
                    room: None,
                    outputs: Vec::new(),
                },
                outcome,
            },
        },
    )
}

/// The fact page an operator subscription opens with: live, empty, at its horizon.
fn operator_facts(session: &Session, horizon: u64) -> app::Admitted<StreamFrame> {
    admitted_stream(session, &packed_frame("facts", &[], None, horizon))
}

fn room_page(
    session: &Session,
    rows: &[serde_json::Value],
    overflow: Option<(u64, u64)>,
    horizon: u64,
) -> app::Admitted<StreamFrame> {
    admitted_stream(session, &packed_frame("rooms", rows, overflow, horizon))
}

/// The fixture board with the room anchor under `topic`, or absent,
/// optionally held by `LANE` under the claim won at `claimed`, and optionally
/// settled with the collapse reason a collapsed Design records.
fn room_board(
    session: &Session,
    topic: Option<&str>,
    claimed: Option<u64>,
    settlement: Option<(wire::WorkflowStatus, Option<&str>)>,
) -> app::Admitted<Response<wire::intent::BoardAnswer>> {
    let original = board_response(session, "hidden");
    let Outcome::Reply(reply) = &original.get().outcome else {
        panic!("fixture board reply");
    };
    let mut body = reply.body.clone();
    let room = room();
    if let Some(topic_path) = topic {
        let task = body
            .tasks
            .iter_mut()
            .find(|task| task.statement.body.born == room)
            .expect("room anchor is on the fixture board");
        task.statement.body.topic = topic_path.parse().expect("fixture topic");
        task.statement.body.handle =
            wire::TaskHandle::for_topic(&task.statement.body.topic.path, &room);
        task.claims = claimed
            .into_iter()
            .map(|claimed| wire::intent::BoardClaim {
                claim: serde_json::from_value(serde_json::json!({
                    "position": claimed,
                    "body": {
                        "actor": format!("lane:{LANE}"),
                        "fact": claimed,
                        "claimed": claimed,
                        "cell": {"topic": topic_path, "phase": "todo"},
                        "lane": LANE,
                        "occupant": REQUEST,
                        "epoch": EPOCH,
                        "disposition": "active",
                    },
                }))
                .expect("fixture claim head"),
                lease: None,
            })
            .collect();
        if let Some((status, collapse_reason)) = settlement {
            task.workflow.body.status = status;
            task.collapse_reason = collapse_reason.map(str::to_owned);
        }
    } else {
        body.tasks.retain(|task| task.statement.body.born != room);
    }
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
        .expect("room board response is admitted")
}

fn participants(runtime: &AppRuntime) -> Vec<app::RoomParticipant> {
    runtime
        .room_projection()
        .participants(&room(), runtime.board())
}

/// The frames an operator subscription opens with: its live-start fact page,
/// then a two-page room snapshot at the same horizon.
fn opening(session: &Session) -> [app::Admitted<StreamFrame>; 3] {
    [
        operator_facts(session, 8),
        room_page(session, &[room_message(2)], Some((4, 4)), 8),
        room_page(
            session,
            &[authorized(4, 1), stop_direction(7, LANE)],
            None,
            8,
        ),
    ]
}

#[test]
fn unheard_operator_post_tracks_current_listening_and_later_lane_events() {
    let session = session(OPERATOR, EPOCH);
    let mut runtime = operator_runtime(&session);
    runtime.apply_stream_frame(&operator_facts(&session, 1));
    runtime.apply_stream_frame(&room_page(&session, &[room_post(1)], None, 1));
    runtime.apply_board(&room_board(&session, Some("/app/rooms"), None, None));

    let is_unheard = |runtime: &AppRuntime| {
        let projection = runtime.room_projection();
        let has_current_listener = projection.has_current_listener(&room(), runtime.board());
        projection.unheard_post(&room(), &operator_post(), has_current_listener)
    };
    assert!(is_unheard(&runtime), "no lane can have heard the post");
    assert!(
        !runtime
            .room_projection()
            .unheard_post(&room(), &authority(ROOM_EVENT), false),
        "non-post room authority is never marked unheard"
    );

    runtime.apply_board(&room_board(&session, Some("/app/rooms"), Some(3), None));
    assert!(
        !is_unheard(&runtime),
        "a current participant receives the post through catch-up"
    );

    runtime.apply_board(&room_board(&session, Some("/app/rooms"), None, None));
    runtime.apply_stream_frame(&admitted_stream(&session, &live_fact(room_message(2))));
    assert!(
        !runtime
            .room_projection()
            .has_current_listener(&room(), runtime.board()),
        "authoring the message creates participation but no listening authority"
    );
    let left = authority(LEAVE_EVENT);
    runtime.apply_stream_frame(&admitted_stream(
        &session,
        &live_fact(envelope(
            3,
            "room::left",
            &wire::rooms::RoomLeft {
                room: room(),
                publication: left,
                lane: uuid(LANE),
                clause: "read the post".to_owned(),
            },
        )),
    ));
    assert!(
        !is_unheard(&runtime),
        "a later lane event permanently proves that the post was heard"
    );
    assert!(
        !runtime
            .room_projection()
            .unheard_post(&room(), &authority(ROOM_EVENT), false),
        "a lane-authored room event is not an operator post"
    );
}

#[test]
fn operator_subscription_rebuilds_room_authority_from_its_paged_room_snapshot() {
    let session = session(OPERATOR, EPOCH);
    let mut runtime = operator_runtime(&session);
    assert!(runtime.subjects().contains(&ViewSubject::Rooms));
    let [facts, first, last] = opening(&session);

    runtime.apply_stream_frame(&facts);
    assert_eq!(
        runtime.room_projection().status(),
        app::RoomProjectionStatus::Loading,
        "a live-start fact page carries no room authority"
    );
    assert_eq!(
        runtime.apply_stream_frame(&first).directives(),
        vec![StreamDirective::Continue {
            subject: ViewSubject::Rooms,
            cursor: wire::StreamCursor {
                epoch: uuid(EPOCH),
                seq: seq(4),
            },
        }]
    );
    assert_eq!(
        runtime.room_projection().status(),
        app::RoomProjectionStatus::Loading
    );
    assert!(runtime.apply_stream_frame(&last).directives().is_empty());
    let projection = runtime.room_projection();
    assert_eq!(projection.status(), app::RoomProjectionStatus::Available);
    assert_eq!(
        projection.preparation().authorizations[&authorization(1).decision].value,
        authorization(1)
    );
    let positions = projection
        .room(&room())
        .expect("room")
        .events()
        .iter()
        .map(|(id, event)| (id.clone(), event.position))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        positions,
        [(ROOM_EVENT, 2)]
            .map(|(id, position)| (authority(id), seq(position)))
            .into()
    );
    assert_eq!(
        participants(&runtime),
        vec![app::RoomParticipant {
            lane: uuid(LANE),
            reasons: vec![app::ParticipantReason::Publication(authority(ROOM_EVENT))],
        }],
        "a message author occupant is never a participant key, and a stop without a board is no duty"
    );
}

#[test]
fn decision_projection_keeps_each_decision_with_its_author_and_time() {
    let session = session(OPERATOR, EPOCH);
    let mut runtime = operator_runtime(&session);
    runtime.apply_stream_frame(&operator_facts(&session, 5));
    let returned = DecisionOutcome::Returned {
        reason: "the evidence is incomplete".to_owned(),
    };
    runtime.apply_stream_frame(&room_page(
        &session,
        &[
            room_message(1),
            decided(3, 1, returned.clone()),
            decided(5, 2, DecisionOutcome::Concluded {}),
        ],
        None,
        5,
    ));

    let preparation = runtime.room_projection().preparation();
    assert_eq!(
        preparation
            .decisions
            .values()
            .map(|decision| (&decision.value.outcome, decision.position))
            .collect::<Vec<_>>(),
        [
            (&returned, seq(3)),
            (&DecisionOutcome::Concluded {}, seq(5))
        ]
    );
    // Principal author expectation.
    let record = &preparation.decision_records[&decision_ref(1)];
    assert_eq!(record.author, wire::Actor::Principal(uuid(PRINCIPAL)));
    assert_eq!(
        serde_json::to_value(record.at).expect("instant encodes"),
        "2030-04-12T18:44:09.271Z"
    );
}

#[test]
fn snapshot_stops_follow_the_board_and_later_live_facts_retire_authority() {
    let session = session(OPERATOR, EPOCH);
    let mut runtime = operator_runtime(&session);
    for frame in opening(&session) {
        runtime.apply_stream_frame(&frame);
    }
    runtime.apply_board(&room_board(
        &session,
        Some("/app/rooms"),
        Some(STOPPED_CLAIM),
        None,
    ));
    assert_eq!(
        participants(&runtime)[0].reasons,
        vec![
            app::ParticipantReason::Publication(authority(ROOM_EVENT)),
            app::ParticipantReason::Work(room()),
            app::ParticipantReason::Stop(STOP_KEY.to_owned()),
        ]
    );

    runtime.apply_board(&room_board(&session, Some("/app/rooms"), Some(5), None));
    assert_eq!(
        participants(&runtime)[0].reasons,
        vec![
            app::ParticipantReason::Publication(authority(ROOM_EVENT)),
            app::ParticipantReason::Work(room()),
        ]
    );

    runtime.apply_board(&room_board(
        &session,
        Some("/app/rooms"),
        Some(STOPPED_CLAIM),
        None,
    ));
    let canceled = envelope(
        9,
        "steer::canceled",
        &wire::body::SteerCanceled {
            key: STOP_KEY.to_owned(),
            recipient: wire::Actor::Lane(uuid(LANE)),
        },
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &live_fact(canceled)));
    runtime.apply_stream_frame(&admitted_stream(&session, &live_fact(authorized(10, 2))));
    assert_eq!(
        participants(&runtime)[0].reasons,
        vec![
            app::ParticipantReason::Publication(authority(ROOM_EVENT)),
            app::ParticipantReason::Work(room()),
        ]
    );
    assert_eq!(
        runtime.room_projection().preparation().authorizations.len(),
        2
    );
}

#[test]
fn authored_message_records_participation_without_author_attention() {
    let session = session(OPERATOR, EPOCH);
    let mut runtime = operator_runtime(&session);
    for frame in opening(&session) {
        runtime.apply_stream_frame(&frame);
    }
    assert_eq!(
        participants(&runtime),
        vec![app::RoomParticipant {
            lane: uuid(LANE),
            reasons: vec![app::ParticipantReason::Publication(authority(ROOM_EVENT))],
        }],
        "the author's room history is not a reason to deliver later catch-up"
    );
}

const MICROS_PER_SECOND: i64 = 1_000_000;

/// A room publication identity `seconds` after the fixture room message.
fn later_publication(seconds: i64) -> RoomAuthorityStamp {
    RoomAuthorityStamp::for_micros(authority(ROOM_EVENT).micros() + seconds * MICROS_PER_SECOND)
        .expect("fixture publication")
}

#[test]
fn an_authored_message_does_not_end_a_leave() {
    let session = session(OPERATOR, EPOCH);
    let mut runtime = operator_runtime(&session);
    for frame in opening(&session) {
        runtime.apply_stream_frame(&frame);
    }
    let left = later_publication(1);
    live(
        &mut runtime,
        &session,
        envelope(
            9,
            "room::left",
            &wire::rooms::RoomLeft {
                room: room(),
                publication: left.clone(),
                lane: uuid(LANE),
                clause: "my part is done".to_owned(),
            },
        ),
    );
    let current = participants(&runtime);
    assert_eq!(
        current,
        vec![app::RoomParticipant {
            lane: uuid(LANE),
            reasons: vec![
                app::ParticipantReason::Publication(authority(ROOM_EVENT)),
                app::ParticipantReason::Left {
                    publication: left,
                    clause: "my part is done".to_owned(),
                },
            ],
        }],
        "the leave stands with its clause"
    );

    let returned = later_publication(3);
    let mut contribution = room_message(10);
    contribution["fact"]["publication"] = serde_json::json!(returned.to_string());
    live(&mut runtime, &session, contribution);
    assert_eq!(
        participants(&runtime)[0].reasons,
        vec![
            app::ParticipantReason::Publication(authority(ROOM_EVENT)),
            app::ParticipantReason::Publication(returned),
            app::ParticipantReason::Left {
                publication: later_publication(1),
                clause: "my part is done".to_owned(),
            },
        ],
        "later authorship adds history without restoring listening"
    );
}

#[test]
fn an_asked_lane_stands_asked_until_it_answers_or_leaves() {
    let session = session(OPERATOR, EPOCH);
    let evidence = wire::rooms::DesignNoticeEvidence::Task {
        task: room(),
        statement: wire::Seq::new(1).expect("fixture evidence statement"),
        completed: wire::Seq::new(8).expect("fixture completion"),
        support: wire::rooms::DesignSupport::Implemented,
    };
    let admission =
        wire::rooms::DesignNoticeMatch::new(wire::rooms::DesignReach::Wide, 0.25, 0.1, [7; 32])
            .expect("fixture match");
    let asked = || {
        let mut runtime = operator_runtime(&session);
        for frame in opening(&session) {
            runtime.apply_stream_frame(&frame);
        }
        live(
            &mut runtime,
            &session,
            envelope(
                9,
                "room::noticed",
                &wire::rooms::DesignNoticed {
                    design: room(),
                    statement: wire::Seq::new(1).expect("fixture statement"),
                    lane: uuid(THIRD_LANE),
                    occupant: uuid(REQUEST),
                    session: wire::rooms::DesignSession {
                        vendor: "omp".parse().expect("fixture vendor"),
                        identity: "fixture-session".to_owned(),
                    },
                    compaction: None,
                    opportunity: wire::rooms::DesignNoticeOpportunity::DesignStatement,
                    evidence: evidence.clone(),
                    excerpt: "fixture rooms".to_owned(),
                    terms: vec!["rooms".to_owned()],
                    admission: admission.clone(),
                    asks: Vec::new(),
                },
            ),
        );
        runtime
    };
    let reasons = |runtime: &AppRuntime| {
        participants(runtime)
            .into_iter()
            .find(|participant| participant.lane == uuid(THIRD_LANE))
            .map(|participant| participant.reasons)
    };

    let mut answered = asked();
    assert_eq!(
        reasons(&answered),
        Some(vec![app::ParticipantReason::Asked {
            evidence: evidence.clone(),
            questions: Vec::new(),
            admission: admission.clone(),
        }])
    );
    let contribution = later_publication(1);
    live(
        &mut answered,
        &session,
        envelope(
            10,
            "room::contributed",
            &wire::rooms::RoomContributed {
                room: room(),
                publication: contribution.clone(),
                lane: uuid(THIRD_LANE),
                contribution: wire::rooms::RoomContributionKind::charge(1),
                item: None,
                statement: wire::Seq::new(1).expect("fixture statement"),
                text: "the fold already covers this".to_owned(),
                sources: Vec::new(),
            },
        ),
    );
    assert_eq!(
        reasons(&answered),
        Some(vec![app::ParticipantReason::Publication(contribution)]),
        "a contribution answers the request and subscribes no one"
    );

    let mut declined = asked();
    let left = later_publication(1);
    live(
        &mut declined,
        &session,
        envelope(
            10,
            "room::left",
            &wire::rooms::RoomLeft {
                room: room(),
                publication: left.clone(),
                lane: uuid(THIRD_LANE),
                clause: "not my problem space".to_owned(),
            },
        ),
    );
    assert_eq!(
        reasons(&declined),
        Some(vec![app::ParticipantReason::Left {
            publication: left,
            clause: "not my problem space".to_owned(),
        }]),
        "a leave declines the request"
    );
}

#[test]
fn same_scope_reconnect_rebuilds_from_the_new_snapshot_with_stable_identities() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = operator_runtime(&session);
    let mut other_browser = operator_runtime(&session);
    for browser in [&mut runtime, &mut other_browser] {
        browser.apply_stream_frame(&operator_facts(&session, 2));
        browser.apply_stream_frame(&room_page(&session, &[room_message(2)], None, 2));
    }
    for browser in [&runtime, &other_browser] {
        assert!(
            browser
                .room_projection()
                .room(&room())
                .expect("room")
                .events()
                .contains_key(&authority(ROOM_EVENT)),
            "both subscribers retain the same durable room publication"
        );
    }

    // Reconnect rebuilds authority from the new snapshot.
    assert!(!runtime.reconnect(&accepted));
    assert_eq!(
        runtime.room_projection().status(),
        app::RoomProjectionStatus::Loading
    );
    assert!(runtime.room_projection().room(&room()).is_none());
    runtime.apply_stream_frame(&operator_facts(&session, 7));
    runtime.apply_stream_frame(&room_page(&session, &[room_message(2)], None, 7));
    let projection = runtime.room_projection();
    assert_eq!(projection.status(), app::RoomProjectionStatus::Available);
    let events = projection.room(&room()).expect("room").events();
    assert_eq!(events[&authority(ROOM_EVENT)].position, seq(2));

    assert_eq!(
        runtime
            .room_projection()
            .anchor_authority(&room(), runtime.board()),
        app::RoomAnchorAuthority::Unavailable
    );
    runtime.apply_board(&room_board(&session, None, None, None));
    assert_eq!(
        runtime
            .room_projection()
            .anchor_authority(&room(), runtime.board()),
        app::RoomAnchorAuthority::Missing
    );
    runtime.apply_board(&room_board(&session, Some("/app/rooms"), None, None));
    assert_eq!(
        runtime
            .room_projection()
            .anchor_authority(&room(), runtime.board()),
        app::RoomAnchorAuthority::Available(wire::TaskHandle::for_topic(
            &"/app/rooms".parse::<wire::Topic>().expect("fixture topic"),
            &room()
        ))
    );
    let at = || "2025-01-01T00:00:00.000Z".parse().expect("fixture instant");
    for (status, collapse_reason, standing) in [
        (
            wire::WorkflowStatus::reviewed(at()),
            None,
            wire::Settlement::Done,
        ),
        (
            wire::WorkflowStatus::closed(at()),
            None,
            wire::Settlement::Closed,
        ),
        (
            wire::WorkflowStatus::closed(at()),
            Some("premise failed"),
            wire::Settlement::Collapsed,
        ),
        (
            wire::WorkflowStatus::deleted(at()),
            None,
            wire::Settlement::Deleted,
        ),
    ] {
        runtime.apply_board(&room_board(
            &session,
            Some("/app/rooms"),
            None,
            Some((status, collapse_reason)),
        ));
        assert_eq!(
            runtime
                .room_projection()
                .anchor_authority(&room(), runtime.board()),
            app::RoomAnchorAuthority::Settled {
                handle: wire::TaskHandle::for_topic(
                    &"/app/rooms".parse::<wire::Topic>().expect("fixture topic"),
                    &room()
                ),
                standing,
            }
        );
    }
    assert!(
        runtime
            .room_projection()
            .room(&room())
            .expect("re-homed room")
            .events()
            .contains_key(&authority(ROOM_EVENT)),
        "a re-homed anchor keeps its durable room cards"
    );
}

#[test]
fn room_snapshots_refuse_moved_horizons_regressions_and_live_gaps() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = operator_runtime(&session);
    let requires_snapshot = |runtime: &AppRuntime| {
        runtime.room_projection().status() == app::RoomProjectionStatus::RequiresSnapshot
    };

    runtime.apply_stream_frame(&room_page(&session, &[room_message(1)], Some((1, 3)), 4));
    assert_eq!(
        runtime
            .apply_stream_frame(&room_page(&session, &[room_message(3)], None, 5))
            .directives(),
        vec![StreamDirective::Reconnect],
        "every page of one snapshot declares the same horizon"
    );
    assert!(requires_snapshot(&runtime));

    assert!(!runtime.reconnect(&accepted));
    runtime.apply_stream_frame(&room_page(&session, &[room_message(1)], Some((1, 3)), 4));
    assert_eq!(
        runtime
            .apply_stream_frame(&room_page(&session, &[room_message(2)], None, 4))
            .directives(),
        vec![StreamDirective::Reconnect],
        "a continuation never moves backward"
    );
    assert!(requires_snapshot(&runtime));

    assert!(!runtime.reconnect(&accepted));
    runtime.apply_stream_frame(&admitted_stream(&session, &live_fact(room_message(6))));
    assert_eq!(
        runtime
            .apply_stream_frame(&room_page(&session, &[room_message(1)], None, 5))
            .directives(),
        vec![StreamDirective::Reconnect],
        "a live fact past the snapshot horizon before completion is a gap"
    );
    assert!(requires_snapshot(&runtime));

    // The runtime already admitted live position 6; overlap is judged above it.
    assert!(!runtime.reconnect(&accepted));
    runtime.apply_stream_frame(&room_page(&session, &[room_message(1)], None, 8));
    assert_eq!(
        runtime.room_projection().status(),
        app::RoomProjectionStatus::Available
    );
    assert_eq!(
        runtime
            .apply_stream_frame(&admitted_stream(&session, &live_fact(room_message(7))))
            .directives(),
        vec![StreamDirective::Reconnect],
        "a live fact at or before the horizon overlaps the snapshot"
    );
    assert!(requires_snapshot(&runtime));

    assert!(!runtime.reconnect(&accepted));
    runtime.apply_stream_frame(&room_page(&session, &[room_message(1)], None, 8));
    runtime.apply_stream_frame(&admitted_stream(&session, &live_fact(room_post(9))));
    assert_eq!(
        runtime
            .room_projection()
            .room(&room())
            .expect("room")
            .events()[&operator_post()]
            .position,
        seq(9)
    );
    assert_eq!(
        runtime.room_projection().status(),
        app::RoomProjectionStatus::Available
    );
}

fn live(runtime: &mut AppRuntime, session: &Session, fact: serde_json::Value) {
    runtime.apply_stream_frame(&admitted_stream(session, &live_fact(fact)));
}
