use wire::{
    Actor, AgentActivityKind, AgentMessage, AutomaticSpeech, AutomaticSpeechKind, CanonicalUuid,
    Envelope, Fact, MessageAcknowledgement, MessageLocus, Packed, StreamAdmissionError,
    StreamCursor, StreamFact, StreamFrame, StreamRequest, ViewSubject, WireVersion,
};

fn uuid(value: &str) -> CanonicalUuid {
    value.parse().expect("canonical fixture uuid")
}

fn seq(value: u64) -> wire::Seq {
    wire::Seq::new(value).expect("non-zero fixture cursor")
}

fn prose_message() -> AgentMessage {
    AgentMessage {
        prose_origin: None,
        lane: uuid("018f82d4-6f59-7d06-92e3-bce9131163d8"),
        author: uuid("018f82d4-6f59-7d06-92e3-bce9131163d9"),
        locus: MessageLocus {
            member: 1,
            offset: 2,
            length: 3,
            event: 4,
        },
        authored: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
        authored_synthetic: false,
        observed: None,
        activity: AgentActivityKind::Prose,
        compaction_method: None,
        final_answer: false,
        acknowledgement: None,
        text: "settled".to_owned(),
        provider_error: None,
        quote_target: None,
        directive_contexts: Vec::new(),
        sends: Vec::new(),
        quotes: Vec::new(),
        automatic_speech: None,
        media: Vec::new(),
        media_quotes: Vec::new(),
        markdown_images: Vec::new(),
        context_bytes: 0,
    }
}

#[test]
fn a_directive_context_names_its_recipient_and_only_operator_direction_is_delivered() {
    let lane = uuid("018f82d4-6f59-7d06-92e3-bce9131163d8");
    let occupant = uuid("018f82d4-6f59-7d06-92e3-bce9131163d9");
    let directive = |author, recipients, packet| wire::MessageDirective {
        start: 0,
        end: 4,
        quote_target: wire::QuoteTarget::Steering {
            key: "amberfern".to_owned(),
            digest: wire::QuoteTarget::text_digest("ship"),
        },
        media_quotes: Vec::new(),
        attribution: wire::QuoteAttribution {
            author,
            recipients,
            packet,
        },
        authored: "2030-04-12T18:44:08.000Z".parse().expect("fixture instant"),
        badge: None,
        review: None,
        line: None,
        delivery: None,
    };
    let operator = || wire::QuoteAuthor::Directive {
        actor: Actor::TEST_PRINCIPAL,
    };
    let lane_only = || {
        vec![wire::QuoteRecipient {
            lane,
            occupant: None,
        }]
    };
    let is_packet = true;

    let peer = wire::QuoteAuthor::Agent { lane, occupant };
    let seated = vec![wire::QuoteRecipient {
        lane,
        occupant: Some(occupant),
    }];
    assert!(directive(peer, seated, is_packet).is_valid_for("> hi"));
    assert!(directive(operator(), lane_only(), is_packet).is_valid_for("> hi"));
    assert!(directive(operator(), lane_only(), false).is_valid_for("> hi"));
    assert!(
        !directive(operator(), Vec::new(), false).is_valid_for("> hi"),
        "a steering directive always reached a lane"
    );

    let review = wire::MessageDirective {
        badge: Some(wire::DirectiveBadge::Review),
        review: Some(wire::MessageReview {
            task: "APP-1kKRRsDRx".parse().expect("fixture review task"),
            finding: "missing boundary coverage".to_owned(),
            guidance: wire::REVIEW_FEEDBACK_GUIDANCE.to_owned(),
        }),
        ..directive(operator(), lane_only(), false)
    };
    assert!(review.is_valid_for("> hi"));
    let mut unguided = review.clone();
    unguided
        .review
        .as_mut()
        .expect("fixture review")
        .guidance
        .clear();
    assert!(
        !unguided.is_valid_for("> hi"),
        "a typed review must carry its handling expectation"
    );
    assert!(
        !wire::MessageDirective {
            review: None,
            ..review.clone()
        }
        .is_valid_for("> hi"),
        "a REVIEW badge without its typed review is not a valid presentation"
    );
    assert!(
        !wire::MessageDirective {
            badge: None,
            ..review.clone()
        }
        .is_valid_for("> hi"),
        "typed review context cannot masquerade as an ordinary direction"
    );

    let delivered = |author, packet| wire::MessageDirective {
        delivery: Some(wire::MessageDelivery {
            fanout: None,
            provisional: false,
        }),
        ..directive(author, lane_only(), packet)
    };
    assert!(delivered(operator(), false).is_valid_for("> hi"));
    assert!(
        !delivered(
            wire::QuoteAuthor::Directive {
                actor: Actor::Spice
            },
            false
        )
        .is_valid_for("> hi"),
        "only the operator's own direction is delivered as its card"
    );
    assert!(
        !delivered(operator(), is_packet).is_valid_for("> hi"),
        "a packet is answered, not delivered as a direction card"
    );
}

#[test]
fn stream_requests_are_strict_current_generation_documents() {
    let epoch = uuid("018f82d4-6f59-7d06-92e3-bce9131163c3");
    let request = StreamRequest::Subscribe {
        v: WireVersion::CURRENT,
        id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c4"),
        epoch,
        subjects: vec![ViewSubject::Facts, ViewSubject::Trust],
        after: Some(StreamCursor { epoch, seq: seq(7) }),
    };
    let bytes = wire::encode(&request).expect("request encodes");
    let decoded: StreamRequest = wire::decode(&bytes).expect("request decodes");

    assert_eq!(decoded, request);
    assert!(decoded.admit(epoch).is_ok());
    assert!(
        wire::decode::<StreamRequest>(
            br#"{
            "stream":"subscribe",
            "v":1,
            "id":"018f82d4-6f59-7d06-92e3-bce9131163c4",
            "epoch":"018f82d4-6f59-7d06-92e3-bce9131163c3",
            "subjects":["facts"],
            "extra":true
        }"#
        )
        .is_err()
    );
    assert!(
        wire::decode::<StreamRequest>(
            br#"{
            "stream":"thread",
            "v":1,
            "id":"018f82d4-6f59-7d06-92e3-bce9131163c4",
            "epoch":"018f82d4-6f59-7d06-92e3-bce9131163c3",
            "cursor":{
                "epoch":"018f82d4-6f59-7d06-92e3-bce9131163c3",
                "seq":0
            }
        }"#
        )
        .is_err()
    );
}

#[test]
fn wrong_wire_request_and_stale_cursor_are_distinct_before_application() {
    let epoch = uuid("018f82d4-6f59-7d06-92e3-bce9131163c3");
    let stale = uuid("018f82d4-6f59-7d06-92e3-bce9131163c9");
    let wrong_wire = StreamRequest::Subscribe {
        v: WireVersion(99),
        id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c4"),
        epoch,
        subjects: vec![ViewSubject::Facts],
        after: None,
    };
    let stale_cursor = StreamRequest::Continue {
        v: WireVersion::CURRENT,
        id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c5"),
        epoch,
        subject: ViewSubject::Facts,
        cursor: StreamCursor {
            epoch: stale,
            seq: seq(7),
        },
    };

    assert!(matches!(
        wrong_wire.admit(epoch),
        Err(StreamAdmissionError::Wire(_))
    ));
    assert!(matches!(
        stale_cursor.admit(epoch),
        Err(StreamAdmissionError::Cursor(_))
    ));
}

#[test]
fn lane_seated_stream_fact_is_strict_and_credential_free() {
    let capability = uuid("018f82d4-6f59-7d06-92e3-bce9131163d9");
    let lane = uuid("018f82d4-6f59-7d06-92e3-bce9131163d8");
    let typed = Envelope {
        v: WireVersion::CURRENT,
        seq: seq(7),
        at: "2030-04-12T18:44:09.271Z".parse().expect("instant"),
        actor: Actor::Spice,
        origin: "resident:018f82d4-6f59-7d06-92e3-bce9131163c3"
            .parse()
            .expect("origin"),
        cause: None,
        fact: Fact::new(wire::body::LaneSeated {
            lane,
            team: lane,
            capability,
            name: Some("Ava".to_owned()),
            worktree: "/tmp/lane".to_owned(),
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("vendor"),
            model: None,
            effort: None,
        }),
    };
    let durable = wire::encode(&typed).expect("durable fact encodes");
    assert!(String::from_utf8_lossy(&durable).contains(&capability.to_string()));
    let opaque: Envelope<wire::AnyFact> = wire::decode(&durable).expect("durable fact decodes");
    let projected = StreamFact::from(opaque);
    let frame = StreamFrame::Facts {
        v: WireVersion::CURRENT,
        id: uuid("018f82d4-6f59-7d06-92e3-bce9131163d7"),
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
        packed: Packed {
            rows: vec![projected],
            overflow: None,
            horizon: seq(7),
        },
    };

    let rendered = wire::encode(&frame).expect("stream frame encodes");
    assert!(!String::from_utf8_lossy(&rendered).contains(&capability.to_string()));
    let decoded: StreamFrame = wire::decode(&rendered).expect("stream frame decodes");
    assert_eq!(decoded, frame);
    assert!(
        wire::decode::<StreamFact>(&durable).is_err(),
        "the credential-bearing durable shape is not a stream DTO"
    );
    let StreamFrame::Facts { packed, .. } = decoded else {
        panic!("expected facts frame");
    };
    assert_eq!(packed.rows[0].kind(), wire::FactKind::LaneSeated);
    assert!(!packed.rows[0].body().contains_key("capability"));
    assert_eq!(
        packed.rows[0].lane_seated(),
        Some(wire::StreamLaneSeated {
            lane,
            team: lane,
            name: Some("Ava".to_owned()),
            worktree: "/tmp/lane".to_owned(),
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("vendor"),
            model: None,
            effort: None,
        })
    );
}

#[test]
fn transcript_activity_defaults_only_the_documented_prose_shape() {
    let prose = prose_message();
    let encoded = wire::encode(&prose).expect("prose activity encodes");
    let encoded_text = String::from_utf8_lossy(&encoded);
    assert!(
        !encoded_text.contains("activity"),
        "the established prose wire shape stays unchanged"
    );
    assert!(
        !encoded_text.contains("authored_synthetic"),
        "source-authored messages keep the established prose wire shape"
    );
    assert!(
        !encoded_text.contains("observed"),
        "source-authored messages do not grow an observation field"
    );
    assert!(
        !encoded_text.contains("final_answer"),
        "non-final prose keeps the established wire shape"
    );
    assert!(
        !encoded_text.contains("automatic_speech"),
        "silent prose keeps the established wire shape"
    );
    let decoded_prose = wire::decode::<AgentMessage>(&encoded).expect("default prose decodes");
    assert_eq!(decoded_prose.activity, AgentActivityKind::Prose);
    assert!(
        !decoded_prose.authored_synthetic,
        "an omitted authored_synthetic marker decodes as source-authored"
    );
    assert!(!decoded_prose.final_answer);
    assert!(decoded_prose.automatic_speech.is_none());

    let final_answer = AgentMessage {
        final_answer: true,
        ..prose.clone()
    };
    let encoded = wire::encode(&final_answer).expect("final answer marker encodes");
    assert!(String::from_utf8_lossy(&encoded).contains(r#""final_answer":true"#));
    assert!(
        wire::decode::<AgentMessage>(&encoded)
            .expect("final answer marker decodes")
            .final_answer
    );

    let synthetic = AgentMessage {
        authored_synthetic: true,
        ..prose.clone()
    };
    let encoded = wire::encode(&synthetic).expect("synthetic authored marker encodes");
    assert!(String::from_utf8_lossy(&encoded).contains(r#""authored_synthetic":true"#));
    assert!(
        wire::decode::<AgentMessage>(&encoded)
            .expect("synthetic authored marker decodes")
            .authored_synthetic
    );

    let observed = AgentMessage {
        observed: Some(
            "2030-04-12T18:44:10.271Z"
                .parse()
                .expect("fixture observation instant"),
        ),
        ..synthetic
    };
    let encoded = wire::encode(&observed).expect("observed instant encodes");
    assert_eq!(
        wire::decode::<AgentMessage>(&encoded)
            .expect("observed instant decodes")
            .observed,
        observed.observed,
    );

    let tool = AgentMessage {
        activity: AgentActivityKind::Tool,
        text: "Bash".to_owned(),
        ..prose.clone()
    };
    let encoded = wire::encode(&tool).expect("tool activity encodes");
    assert!(String::from_utf8_lossy(&encoded).contains(r#""activity":"tool""#));
    assert_eq!(
        wire::decode::<AgentMessage>(&encoded)
            .expect("explicit tool decodes")
            .activity,
        AgentActivityKind::Tool
    );

    let compaction = AgentMessage {
        activity: AgentActivityKind::Compaction,
        text: "Context compacted".to_owned(),
        compaction_method: Some("snapcompact".to_owned()),
        ..prose
    };
    let encoded = wire::encode(&compaction).expect("compaction activity encodes");
    assert!(String::from_utf8_lossy(&encoded).contains(r#""activity":"compaction""#));
    let decoded = wire::decode::<AgentMessage>(&encoded).expect("explicit compaction decodes");
    assert_eq!(decoded.activity, AgentActivityKind::Compaction);
    assert_eq!(decoded.compaction_method.as_deref(), Some("snapcompact"));

    let unknown = String::from_utf8(encoded)
        .expect("wire is utf-8")
        .replace(r#""activity":"compaction""#, r#""activity":"future""#);
    assert!(
        wire::decode::<AgentMessage>(unknown.as_bytes()).is_err(),
        "an unknown activity kind fails instead of choosing a fallback"
    );
}

#[test]
fn turn_activity_has_one_explicit_wire_shape() {
    let turn = AgentMessage {
        activity: AgentActivityKind::Turn,
        text: "New turn".to_owned(),
        ..prose_message()
    };

    let encoded = wire::encode(&turn).expect("turn activity encodes");

    assert!(String::from_utf8_lossy(&encoded).contains(r#""activity":"turn""#));
    assert_eq!(
        wire::decode::<AgentMessage>(&encoded)
            .expect("explicit turn decodes")
            .activity,
        AgentActivityKind::Turn
    );
}

#[test]
fn message_acknowledgement_has_one_offset_free_wire_shape() {
    let pending = AgentMessage {
        text: "[ACK] remains ordinary prose.".to_owned(),
        acknowledgement: Some(MessageAcknowledgement::Pending),
        ..prose_message()
    };

    let encoded = wire::encode(&pending).expect("pending acknowledgement encodes");
    assert!(String::from_utf8_lossy(&encoded).contains(r#""acknowledgement":"pending""#));
    let decoded = wire::decode::<AgentMessage>(&encoded).expect("pending acknowledgement decodes");
    assert_eq!(
        decoded.acknowledgement,
        Some(MessageAcknowledgement::Pending)
    );
    assert_eq!(decoded.text, "[ACK] remains ordinary prose.");

    let mut explicit_null = serde_json::to_value(prose_message()).expect("prose becomes JSON");
    explicit_null["acknowledgement"] = serde_json::Value::Null;
    assert!(
        wire::decode::<AgentMessage>(
            &serde_json::to_vec(&explicit_null).expect("explicit-null prose encodes")
        )
        .is_err(),
        "absence has one spelling"
    );
}

#[test]
fn automatic_speech_is_typed_and_has_one_absence_spelling() {
    let prose = prose_message();
    let mut explicit_null = serde_json::to_value(&prose).expect("prose becomes JSON");
    explicit_null["automatic_speech"] = serde_json::Value::Null;
    assert!(
        wire::decode::<AgentMessage>(
            &serde_json::to_vec(&explicit_null).expect("explicit-null prose encodes")
        )
        .is_err(),
        "automatic speech has omission as its only absence spelling"
    );

    let spoken = AgentMessage {
        automatic_speech: Some(AutomaticSpeech {
            kind: AutomaticSpeechKind::Final,
            text: "selected final".to_owned(),
        }),
        ..prose
    };
    let encoded = wire::encode(&spoken).expect("automatic speech encodes");
    assert_eq!(
        wire::decode::<AgentMessage>(&encoded)
            .expect("automatic speech decodes")
            .automatic_speech,
        spoken.automatic_speech,
    );
}
