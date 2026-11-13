use wire::body::SteerSent;
use wire::intent::SteerSend;
use wire::{CanonicalUuid, Instant};

const DIRECTIVE: &str = "018f82d4-6f59-7d06-92e3-bce913116301";
const FANOUT: &str = "018f82d4-6f59-7d06-92e3-bce913116302";
const LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116303";
const EXPIRES: &str = "2030-04-12T19:44:09.271Z";

#[test]
fn current_fanout_roundtrips_and_explicit_null_is_refused() {
    let current = sent(Some(uuid(FANOUT)));
    let bytes = wire::encode(&current).expect("fanout-aware fact encodes");
    assert_eq!(
        wire::decode::<SteerSent>(&bytes).expect("fanout-aware fact decodes"),
        current
    );
    let null = std::str::from_utf8(&bytes)
        .expect("wire JSON is UTF-8")
        .replace(&format!(r#""fanout":"{FANOUT}""#), r#""fanout":null"#);
    assert!(
        wire::decode::<SteerSent>(null.as_bytes()).is_err(),
        "present fanout cannot erase provenance with null"
    );
}

#[test]
fn current_send_intent_requires_fanout_and_preserves_recipient_choice() {
    let missing = format!(r#"{{"lane":"{LANE}","message":"hold the line","attachments":[]}}"#);
    assert!(wire::decode::<SteerSend>(missing.as_bytes()).is_err());
    let explicit = SteerSend {
        fanout: uuid(FANOUT),
        recipients: Vec::new(),
        lane: Some(uuid(LANE)),
        message: "hold the line".to_owned(),
        attachments: Vec::new(),
    };
    assert_eq!(
        wire::decode::<SteerSend>(&wire::encode(&explicit).expect("explicit send encodes"))
            .expect("explicit send decodes"),
        explicit
    );
    let automatic = SteerSend {
        lane: None,
        ..explicit.clone()
    };
    let encoded = wire::encode(&automatic).expect("automatic send encodes");
    assert_eq!(
        wire::decode::<SteerSend>(&encoded).expect("automatic send decodes"),
        automatic
    );
    let explicit_bytes = wire::encode(&explicit).expect("explicit send encodes");
    let null = std::str::from_utf8(&explicit_bytes)
        .expect("wire JSON is UTF-8")
        .replace(&format!(r#""lane":"{LANE}""#), r#""lane":null"#);
    assert!(
        wire::decode::<SteerSend>(null.as_bytes()).is_err(),
        "present lane cannot erase explicit recipient authority with null"
    );
}

fn sent(fanout: Option<CanonicalUuid>) -> SteerSent {
    SteerSent {
        sender: wire::Actor::TEST_PRINCIPAL,
        room: None,
        directive: uuid(DIRECTIVE),
        fanout,
        recipients: Vec::new(),
        key: "1kFbnxxtx".to_owned(),
        recipient: wire::Actor::Lane(uuid(LANE)),
        text: "hold the line".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires: EXPIRES.parse::<Instant>().expect("fixture instant"),
    }
}

fn uuid(value: &str) -> CanonicalUuid {
    value.parse().expect("fixture UUID")
}
