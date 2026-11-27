//! Current SYN room publications and actor-addressed steering inputs move
//! under one delivery authority.

use super::*;
use app::RoomEventKind;
use wire::rooms::{
    PrincipalInputRef, RoomConversed, RoomDestination, RoomPublicationKind, RoomRoute,
};

fn instant(text: &str) -> wire::Instant {
    text.parse().expect("fixture instant")
}

fn hunk(lane: &str, offset: u64) -> wire::rooms::TranscriptHunk {
    wire::rooms::TranscriptHunk {
        head: wire::TranscriptSourceHead {
            lane: uuid(lane),
            vendor: "omp".parse().expect("fixture vendor"),
            stream: "session".to_owned(),
            source: uuid(REQUEST),
            next: wire::body::TranscriptPosition {
                member: 1,
                offset: 256,
            },
        },
        locus: wire::MessageLocus {
            member: 1,
            offset,
            length: 32,
            event: 0,
        },
        text: TextRange { start: 0, end: 12 },
    }
}

fn told(position: u64, directive: &str, key: &str, from: &str, to: &str) -> serde_json::Value {
    envelope(
        position,
        "steer::messaged",
        &wire::body::SteerMessaged {
            directive: uuid(directive),
            key: key.to_owned(),
            lane: uuid(to),
            occupant: uuid(to),
            from: uuid(from),
            sender: uuid(from),
            text: "Packet body".to_owned(),
            locus: wire::body::TranscriptLocus {
                source: Some(uuid(REQUEST)),
                member: 1,
                offset: 0,
                length: 1,
                event: 0,
                line: 1,
                column: 1,
            },
            span: wire::body::BodySpan::new(0.."Packet body".len(), "Packet body"),
            room: Some(room()),
            addressed: None,
            line: None,
            peer_body: None,
            covered: Vec::new(),
            admission: None,
            expires: instant("2030-04-13T18:44:09.271Z"),
        },
    )
}

fn acked(position: u64, directive: &str, key: &str, lane: &str) -> serde_json::Value {
    envelope(
        position,
        "steer::acked",
        &wire::body::SteerAcked {
            directive: uuid(directive),
            key: key.to_owned(),
            recipient: wire::Actor::Lane(uuid(lane)),
            evidence: wire::body::SteeringEvidence::Transcript {
                epoch: uuid(EPOCH),
                locus: wire::body::TranscriptLocus {
                    source: Some(uuid(REQUEST)),
                    member: 1,
                    offset: 0,
                    length: 1,
                    event: 0,
                    line: 1,
                    column: 1,
                },
            },
        },
    )
}

fn conversed(
    position: u64,
    packet: &str,
    publication: &str,
    author: &str,
    routes: Vec<RoomRoute>,
    directed: &[&str],
) -> [serde_json::Value; 2] {
    let directed = directed
        .iter()
        .map(|directive| uuid(directive))
        .collect::<Vec<_>>();
    let source = hunk(author, position);
    [
        envelope(
            position,
            "room::messaged",
            &RoomMessaged {
                room: room(),
                publication: authority(packet),
                hunk: source.clone(),
                author: uuid(author),
                directed: directed.clone(),
            },
        ),
        envelope(
            position + 1,
            "room::conversed",
            &RoomConversed {
                publication: authority(publication),
                hunk: source,
                operation: RoomPublicationKind::Message {},
                routes,
                directed,
                expires: instant("2030-04-13T18:44:09.271Z"),
            },
        ),
    ]
}

fn conversation_runtime_as(
    session: &Session,
    actor: &str,
    snapshot: &[serde_json::Value],
) -> AppRuntime {
    let accepted = acceptance(session, actor, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    assert!(runtime.apply_principals(&operator_directory(session)));
    let horizon = snapshot.len() as u64;
    runtime.apply_stream_frame(&operator_facts(session, horizon));
    runtime.apply_stream_frame(&room_page(session, snapshot, None, horizon));
    assert_eq!(
        runtime.room_projection().status(),
        app::RoomProjectionStatus::Available
    );
    runtime
}

fn conversation_runtime(session: &Session, snapshot: &[serde_json::Value]) -> AppRuntime {
    conversation_runtime_as(session, OWNER_ACTOR, snapshot)
}

fn principal_route(
    context: Option<wire::rooms::RoomAnchor>,
    direction: Option<&str>,
) -> Vec<RoomRoute> {
    vec![RoomRoute {
        destination: RoomDestination::Principal {
            principal: uuid(PRINCIPAL),
        },
        context,
        direction: direction.map(str::to_owned),
    }]
}

fn principal_delivery(
    position: u64,
    publication: &str,
    principal: &str,
    author: &str,
    key: &str,
) -> serde_json::Value {
    let input = PrincipalInputRef::published(&authority(publication), uuid(principal));
    let directive = input.directive;
    envelope(
        position,
        "steer::sent",
        &wire::body::SteerSent {
            directive,
            fanout: None,
            key: key.to_owned(),
            recipient: wire::Actor::Principal(input.principal),
            text: format!("Principal input {publication}"),
            sender: wire::Actor::Lane(uuid(author)),
            room: Some(room()),
            control: Some(wire::peer_line_control(
                wire::body::PeerLineRef {
                    root: directive,
                    parent: None,
                    opening: directive,
                },
                wire::body::PeerBodyKind::Open,
                &[],
            )),
            attachments: Vec::new(),
            recipients: Vec::new(),
            expires: instant("2030-04-13T18:44:09.271Z"),
        },
    )
}

#[test]
fn syn_delivery_receipt_moves_without_rewriting_the_room_event() {
    const PACKET: &str = "dVsYdDj7m";
    const MESSAGE: &str = "dVsYdDj7n";
    const DIRECTIVE: &str = "018f82d4-6f59-7d06-92e3-bce913116301";
    let session = session(OPERATOR, EPOCH);
    let [packet, message] = conversed(
        2,
        PACKET,
        MESSAGE,
        LANE,
        vec![RoomRoute {
            destination: RoomDestination::Lane {
                lane: uuid(SECOND_LANE),
            },
            context: Some(room()),
            direction: None,
        }],
        &[DIRECTIVE],
    );
    let mut runtime = conversation_runtime(
        &session,
        &[
            told(1, DIRECTIVE, MESSAGE, LANE, SECOND_LANE),
            packet,
            message,
        ],
    );
    let event = runtime
        .room_projection()
        .room(&room())
        .and_then(|state| state.events().get(&authority(PACKET)))
        .expect("SYN packet is a room event");
    assert!(matches!(
        &event.kind,
        RoomEventKind::Messaged {
            conversation: Some(conversation),
            ..
        } if *conversation == authority(MESSAGE)
    ));
    assert_eq!(
        runtime
            .room_projection()
            .conversation()
            .delivery(&uuid(DIRECTIVE))
            .expect("delivery")
            .value
            .disposition,
        wire::Disposition::Live
    );
    live(
        &mut runtime,
        &session,
        acked(4, DIRECTIVE, MESSAGE, SECOND_LANE),
    );
    assert_eq!(
        runtime
            .room_projection()
            .conversation()
            .delivery(&uuid(DIRECTIVE))
            .expect("delivery")
            .value
            .disposition,
        wire::Disposition::Acked
    );
}

const PRINCIPAL_PUBLICATION: &str = "dVsYdDj7s";
const PRINCIPAL_PACKET: &str = "dVsYdDj7r";
const PRINCIPAL_KEY: &str = "dPrincipal1";
const PRINCIPAL_CHILD: &str = "018f82d4-6f59-7d06-92e3-bce9131163c1";

fn principal_input_runtime() -> (Session, AppRuntime, PrincipalInputRef) {
    let input = PrincipalInputRef::published(&authority(PRINCIPAL_PUBLICATION), uuid(PRINCIPAL));
    let [packet, publication] = conversed(
        2,
        PRINCIPAL_PACKET,
        PRINCIPAL_PUBLICATION,
        LANE,
        principal_route(Some(room()), None),
        &[],
    );
    let session = session(OPERATOR, EPOCH);
    let runtime = conversation_runtime(
        &session,
        &[
            principal_delivery(1, PRINCIPAL_PUBLICATION, PRINCIPAL, LANE, PRINCIPAL_KEY),
            packet,
            publication,
        ],
    );
    (session, runtime, input)
}

#[test]
fn principal_input_lifecycle_retains_route_identity() {
    let (session, mut runtime, input) = principal_input_runtime();
    let inbox = runtime.principal_inbox();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].input, input);
    assert_eq!(
        inbox[0].delivery.value.recipient,
        wire::Actor::Principal(uuid(PRINCIPAL))
    );
    live(
        &mut runtime,
        &session,
        envelope(
            4,
            "steer::lapsed",
            &wire::body::SteerLapsed {
                key: PRINCIPAL_KEY.to_owned(),
                recipient: wire::Actor::Principal(uuid(PRINCIPAL)),
                expires: instant("2030-04-13T18:44:09.271Z"),
            },
        ),
    );
    live(
        &mut runtime,
        &session,
        envelope(
            5,
            "steer::transitioned",
            &wire::body::SteerLineTransition {
                line: input.directive,
                input: input.directive,
                action: wire::body::PeerLineAction::Unanswered,
                body: None,
            },
        ),
    );
    assert_eq!(
        runtime.principal_inbox()[0].delivery.value.disposition,
        wire::Disposition::Lapsed
    );
    let recovered = instant("2030-04-14T18:44:09.271Z");
    live(
        &mut runtime,
        &session,
        envelope(
            6,
            "steer::recovered",
            &wire::body::SteerRecovered {
                directive: input.directive,
                key: PRINCIPAL_KEY.to_owned(),
                recipient: wire::Actor::Principal(uuid(PRINCIPAL)),
                lapsed_at: instant("2030-04-13T18:44:09.271Z"),
                expires: recovered,
            },
        ),
    );
    let recovered_input = &runtime.principal_inbox()[0].delivery.value;
    assert_eq!(recovered_input.expires, recovered);
    assert_eq!(
        recovered_input.line_action, None,
        "recovery reopens the input instead of retaining its ended line state"
    );
}

#[test]
fn recursive_principal_input_shares_its_root_line_state() {
    let (session, mut runtime, input) = principal_input_runtime();
    let expires = instant("2030-04-14T18:44:09.271Z");
    live(
        &mut runtime,
        &session,
        envelope(
            7,
            "steer::sent",
            &wire::body::SteerSent {
                directive: uuid(PRINCIPAL_CHILD),
                fanout: None,
                key: "dPrincipalBody".to_owned(),
                recipient: wire::Actor::Principal(uuid(PRINCIPAL)),
                text: "Raw continuation".to_owned(),
                sender: wire::Actor::Lane(uuid(LANE)),
                room: Some(room()),
                control: Some(wire::peer_line_control(
                    wire::body::PeerLineRef {
                        root: input.directive,
                        parent: None,
                        opening: input.directive,
                    },
                    wire::body::PeerBodyKind::Continue,
                    &[input.directive],
                )),
                attachments: Vec::new(),
                recipients: Vec::new(),
                expires,
            },
        ),
    );
    live(
        &mut runtime,
        &session,
        envelope(
            8,
            "steer::transitioned",
            &wire::body::SteerLineTransition {
                line: input.directive,
                input: input.directive,
                action: wire::body::PeerLineAction::Continue,
                body: Some(uuid(PRINCIPAL_CHILD)),
            },
        ),
    );
    let delivery = runtime
        .room_projection()
        .conversation()
        .delivery(&input.directive)
        .expect("principal input");
    assert_eq!(
        delivery.value.line_action,
        Some(wire::body::PeerLineAction::Continue)
    );
    assert_eq!(delivery.value.line_body, Some(uuid(PRINCIPAL_CHILD)));
    let inbox = runtime.principal_inbox();
    assert_eq!(inbox.len(), 2);
    assert_eq!(inbox[1].input.directive, uuid(PRINCIPAL_CHILD));
    assert_eq!(
        inbox[1]
            .conversation
            .expect("recursive input resolves through its line root")
            .publication
            .publication(),
        &authority(PRINCIPAL_PUBLICATION)
    );
    live(
        &mut runtime,
        &session,
        envelope(
            9,
            "steer::canceled",
            &wire::body::SteerCanceled {
                key: "dPrincipalBody".to_owned(),
                recipient: wire::Actor::Principal(uuid(PRINCIPAL)),
                reason: None,
            },
        ),
    );
    assert_eq!(runtime.principal_inbox().len(), 1);
    assert_eq!(
        runtime
            .room_projection()
            .conversation()
            .delivery(&uuid(PRINCIPAL_CHILD))
            .expect("cancelled recursive input remains presentable")
            .value
            .disposition,
        wire::Disposition::Canceled
    );
}

#[test]
fn principal_inbox_uses_exact_route_identity_before_source_text_arrives() {
    const MINE_PACKET: &str = "dVsYdDj7v";
    const MINE: &str = "dVsYdDj7w";
    const THEIRS_PACKET: &str = "dVsYdDj7x";
    const THEIRS: &str = "dVsYdDj7y";
    const THEM: &str = "018f82d4-6f59-7d06-92e3-bce9131163b1";
    let session = session(OPERATOR, EPOCH);
    let [mine_packet, mine] = conversed(
        2,
        MINE_PACKET,
        MINE,
        LANE,
        principal_route(None, Some("dDirection1")),
        &[],
    );
    let [their_packet, theirs] = conversed(
        5,
        THEIRS_PACKET,
        THEIRS,
        LANE,
        vec![RoomRoute {
            destination: RoomDestination::Principal {
                principal: uuid(THEM),
            },
            context: None,
            direction: Some("dDirection2".to_owned()),
        }],
        &[],
    );
    let snapshot = [
        principal_delivery(1, MINE, PRINCIPAL, LANE, "dMine"),
        mine_packet,
        mine,
        principal_delivery(4, THEIRS, THEM, LANE, "dTheirs"),
        their_packet,
        theirs,
    ];
    let mut runtime = conversation_runtime(&session, &snapshot);
    let inbox = runtime.principal_inbox();
    assert_eq!(inbox.len(), 1);
    assert_eq!(
        inbox[0]
            .conversation
            .expect("root input publication")
            .publication
            .publication(),
        &authority(MINE)
    );
    assert_eq!(runtime.begin_principal_input_texts(), vec![authority(MINE)]);
    assert!(runtime.begin_principal_input_texts().is_empty());
}

const DELIVERED_KEY: &str = "dW4Wl4fRP";

fn operator_sent(position: u64) -> serde_json::Value {
    envelope(
        position,
        "steer::sent",
        &wire::body::SteerSent {
            directive: uuid(REQUEST),
            fanout: Some(uuid(SECOND_LANE)),
            recipients: Vec::new(),
            key: DELIVERED_KEY.to_owned(),
            recipient: wire::Actor::Lane(uuid(LANE)),
            text: "Ship it".to_owned(),
            sender: wire::Actor::Principal(uuid(PRINCIPAL)),
            room: None,
            control: None,
            attachments: Vec::new(),
            expires: instant("2030-04-13T18:44:09.271Z"),
        },
    )
}

// Principal direction fixture.
/// The tape card the lane's transcript carries once omp delivers the send.
fn delivered_direction_card(offset: u64) -> serde_json::Value {
    let text = "> Ship it";
    serde_json::json!({
        "lane": LANE,
        "author": LANE_CAPABILITY,
        "locus": {"member": 1, "offset": offset, "length": 20, "event": 0},
        "authored": "2030-04-12T18:44:09.271Z",
        "activity": "direction",
        "text": text,
        "directive_contexts": [serde_json::to_value(wire::MessageDirective {
            start: 0,
            end: u32::try_from(text.len()).expect("fixture length"),
            quote_target: wire::QuoteTarget::Steering {
                key: DELIVERED_KEY.to_owned(),
                digest: wire::QuoteTarget::text_digest("Ship it"),
            },
            media_quotes: Vec::new(),
            attribution: wire::QuoteAttribution {
                author: wire::QuoteAuthor::Directive {
                    actor: wire::Actor::Principal(uuid(PRINCIPAL)),
                },
                recipients: Vec::new(),
                packet: false,
            },
            authored: instant("2030-04-12T18:44:09.271Z"),
            badge: None,
            review: None,
            line: None,
            delivery: Some(wire::MessageDelivery {
                fanout: Some(uuid(SECOND_LANE)),
                provisional: false,
            }),
        })
        .expect("directive serializes")]
    })
}

fn seated_member() -> app::LaneMember {
    app::LaneMember {
        lane: uuid(LANE),
        name: None,
        worktree: "worktree".to_owned(),
        speech: wire::SpeechPlayback::Disabled {},
        life: "steer".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        seat_model: None,
        effort: None,
        seat_effort: None,
        fast_mode: false,
        fast_refusal: None,
        seat_fast_mode: false,
        occupant: Some(uuid(LANE_CAPABILITY)),
        session: None,
        disposition: "seated".to_owned(),
        liveness: wire::intent::LaneLiveness::Working,
        blocked_on: None,
        bound: true,
        wakeable: true,
        jobs: Vec::new(),
        parked: None,
    }
}

fn complete_frontier() -> Vec<serde_json::Value> {
    vec![serde_json::json!({"lane": LANE, "state": "complete"})]
}

fn retains_direction_card(runtime: &AppRuntime) -> bool {
    runtime
        .messages()
        .iter()
        .any(|card| card.activity == wire::AgentActivityKind::Direction)
}

#[test]
fn a_delivered_direction_never_returns_as_provisional_after_its_card_is_cut() {
    let session = session(OPERATOR, EPOCH);
    let members = vec![seated_member()];
    let mut runtime = conversation_runtime(&session, &[operator_sent(1)]);
    initialize_messages(&mut runtime, &session);
    assert_eq!(
        runtime.provisional_directions(&members).len(),
        1,
        "before delivery the send projects provisionally"
    );

    runtime.apply_stream_frame(&admitted_stream(
        &session,
        &message_generation_frame(1, vec![delivered_direction_card(50)], complete_frontier()),
    ));
    assert!(retains_direction_card(&runtime));
    assert!(runtime.provisional_directions(&members).is_empty());

    // A compaction above the delivered card cuts it from the retained store
    // while the directive stays live.
    let mut later = vec![serde_json::json!({
        "lane": LANE,
        "author": LANE_CAPABILITY,
        "locus": {"member": 1, "offset": 90, "length": 20, "event": 0},
        "authored": "2030-04-12T18:45:09.271Z",
        "activity": "compaction",
        "text": "compacted",
    })];
    later.extend((0..wire::MESSAGE_FLOOR as u64 + 300).map(|n| {
        serde_json::json!({
            "lane": LANE,
            "author": LANE_CAPABILITY,
            "locus": {"member": 1, "offset": 100 + n, "length": 1, "event": 0},
            "authored": "2030-04-12T18:46:09.271Z",
            "text": format!("prose {n}"),
        })
    }));
    runtime.apply_stream_frame(&admitted_stream(
        &session,
        &message_generation_frame(2, later, complete_frontier()),
    ));
    assert!(
        !retains_direction_card(&runtime),
        "the horizon cut the card"
    );
    assert!(
        runtime.provisional_directions(&members).is_empty(),
        "the send does not come back as queued"
    );
}

#[test]
fn a_restored_delivered_direction_is_not_provisional_when_its_send_arrives_later() {
    let session = session(OPERATOR, EPOCH);
    let members = vec![seated_member()];
    let mut runtime = operator_runtime(&session);
    runtime.apply_stream_frame(&admitted_stream(
        &session,
        &message_snapshot_frame(
            0,
            256,
            vec![delivered_direction_card(50)],
            complete_frontier(),
        ),
    ));
    runtime.apply_stream_frame(&operator_facts(&session, 1));
    runtime.apply_stream_frame(&room_page(&session, &[operator_sent(1)], None, 1));
    assert_eq!(
        runtime.room_projection().status(),
        app::RoomProjectionStatus::Available
    );
    assert!(runtime.provisional_directions(&members).is_empty());
}

const QUOTING_KEY: &str = "dW4Wl4fRQ";

fn prose_target(offset: u64, text: &str) -> wire::QuoteTarget {
    wire::QuoteTarget::Tape {
        lane: uuid(LANE),
        source: uuid(REQUEST),
        locus: wire::MessageLocus {
            member: 1,
            offset,
            length: 20,
            event: 0,
        },
        digest: wire::QuoteTarget::text_digest(text),
    }
}

/// A retained prose card whose own quote target names `text`.
fn quoteable_prose_card(offset: u64, text: &str) -> serde_json::Value {
    serde_json::json!({
        "lane": LANE,
        "author": LANE_CAPABILITY,
        "locus": {"member": 1, "offset": offset, "length": 20, "event": 0},
        "authored": "2030-04-12T18:43:09.271Z",
        "text": text,
        "quote_target": prose_target(offset, text),
    })
}

fn quoted_media() -> wire::Media {
    wire::Media {
        name: "capture.png".to_owned(),
        content_type: "image/png".to_owned(),
        data_url: "data:image/png;base64,iVBORw0KGgo=".to_owned(),
    }
}

fn prose_media_quote(offset: u64, media: &wire::Media) -> wire::MediaQuote {
    wire::MediaQuote {
        target: wire::QuoteTarget::TapeMedia {
            lane: uuid(LANE),
            source: uuid(REQUEST),
            locus: wire::MessageLocus {
                member: 1,
                offset,
                length: 20,
                event: 0,
            },
            ordinal: 0,
            digest: media.identity().expect("fixture media identity"),
        },
        name: media.name.clone(),
    }
}

fn quoteable_media_prose_card(offset: u64, text: &str, media: &wire::Media) -> serde_json::Value {
    serde_json::json!({
        "lane": LANE,
        "author": LANE_CAPABILITY,
        "locus": {"member": 1, "offset": offset, "length": 20, "event": 0},
        "authored": "2030-04-12T18:43:09.271Z",
        "text": text,
        "quote_target": prose_target(offset, text),
        "media": [media],
        "media_quotes": [prose_media_quote(offset, media)],
    })
}

/// A later operator send whose text quotes `targets` in order.
fn quoting_send(position: u64, targets: &[wire::QuoteTarget]) -> serde_json::Value {
    let text = targets
        .iter()
        .map(wire::QuoteTarget::markdown)
        .collect::<Vec<_>>()
        .join("\n\nand\n\n");
    envelope(
        position,
        "steer::sent",
        &wire::body::SteerSent {
            directive: uuid(THIRD_LANE),
            fanout: Some(uuid(SECOND_LANE)),
            recipients: Vec::new(),
            key: QUOTING_KEY.to_owned(),
            recipient: wire::Actor::Lane(uuid(LANE)),
            text,
            sender: wire::Actor::Principal(uuid(PRINCIPAL)),
            room: None,
            control: None,
            attachments: Vec::new(),
            expires: instant("2030-04-13T18:44:09.271Z"),
        },
    )
}

#[test]
fn a_provisional_direction_fills_its_quotes_from_retained_cards_in_message_order() {
    let session = session(OPERATOR, EPOCH);
    let members = vec![seated_member()];
    let media = quoted_media();
    let quoted_text = format!(
        "agent said so\n\n{}",
        media.reference_markdown().expect("fixture media reference")
    );
    let quoted_prose = prose_target(30, &quoted_text);
    let directive = wire::QuoteTarget::Steering {
        key: DELIVERED_KEY.to_owned(),
        digest: wire::QuoteTarget::text_digest("Ship it"),
    };
    let stale = prose_target(40, "what the card no longer says");
    let unretained = prose_target(70, "never shown");
    let mut runtime = conversation_runtime(
        &session,
        &[
            operator_sent(1),
            quoting_send(
                2,
                &[quoted_prose.clone(), directive.clone(), stale, unretained],
            ),
        ],
    );
    runtime.apply_stream_frame(&admitted_stream(
        &session,
        &message_snapshot_frame(
            0,
            256,
            vec![
                quoteable_media_prose_card(30, &quoted_text, &media),
                quoteable_prose_card(40, "what the card says"),
                delivered_direction_card(50),
            ],
            complete_frontier(),
        ),
    ));

    let directions = runtime.provisional_directions(&members);
    let [card] = directions.as_slice() else {
        panic!("only the undelivered send projects: {directions:?}");
    };
    assert_eq!(
        card.quotes,
        vec![
            wire::QuotePresentation::Resolved {
                target: quoted_prose.to_string(),
                text: wire::media::with_media_payloads(&quoted_text, std::slice::from_ref(&media),)
                    .expect("fixture media expands")
                    .into_owned(),
                attribution: wire::QuoteAttribution {
                    author: wire::QuoteAuthor::Agent {
                        lane: uuid(LANE),
                        occupant: uuid(LANE_CAPABILITY),
                    },
                    recipients: Vec::new(),
                    packet: false,
                },
                disposition: None,
                quotes: Vec::new(),
                media_quotes: vec![prose_media_quote(30, &media)],
            },
            // Principal direction expectation.
            wire::QuotePresentation::Resolved {
                target: directive.to_string(),
                text: "Ship it".to_owned(),
                attribution: wire::QuoteAttribution {
                    author: wire::QuoteAuthor::Directive {
                        actor: wire::Actor::Principal(uuid(PRINCIPAL)),
                    },
                    recipients: Vec::new(),
                    packet: false,
                },
                disposition: Some(wire::Disposition::Live),
                quotes: Vec::new(),
                media_quotes: Vec::new(),
            },
        ],
        "a stale digest of a retained locus and a target never shown stay bare"
    );
}
