//! What the browser is allowed to believe.
//!
//! Every fixture reaches the app as an encoded document because the socket
//! carries bytes and the app's only honest input is bytes. Valid response
//! documents are first constructed from their wire DTOs, so schema drift is a
//! compile error; deliberately malformed decoder inputs remain raw bytes.

use app::Session;
use serde::{Deserialize, Serialize};
use wire::{CanonicalUuid, FaultClass, Outcome, Reply, ReplyPosition, Seq, SeqRange, WireVersion};

use super::support;

/// Capability this session opens with.
const CAPABILITY: &str = "018f82d4-6f59-7d06-92e3-bce9131163c1";
/// Generation this session addresses.
const EPOCH: &str = "018f82d4-6f59-7d06-92e3-bce9131163c3";
/// A generation that replaced the one above.
const NEXT_EPOCH: &str = "018f82d4-6f59-7d06-92e3-bce9131163c9";

/// Parse a canonical uuid fixture, which is spelled correctly by construction.
fn uuid(text: &str) -> CanonicalUuid {
    text.parse().expect("fixture uuid is canonical")
}

/// A session opened against [`CAPABILITY`] and [`EPOCH`].
fn session() -> Session {
    Session::open(uuid(CAPABILITY), uuid(EPOCH))
}

#[test]
fn an_accepted_frame_cannot_echo_the_session_capability() {
    let bytes = format!(
        concat!(
            r#"{{ "wire": 1, "actor": "lane:018f82d4-6f59-7d06-92e3-bce9131163c2", "#,
            r#""epoch": "{EPOCH}", "release": "1.2.3", "capability": "{CAPABILITY}" }}"#,
        ),
        EPOCH = EPOCH,
        CAPABILITY = CAPABILITY,
    );

    let fault = session()
        .admit_accepted(bytes.as_bytes())
        .expect_err("the resident return frame cannot echo a live credential");

    assert_eq!(fault.class, FaultClass::Protocol);
    assert_eq!(fault.reason, "frame::undecodable");
    assert!(
        !fault.detail.contains(CAPABILITY),
        "the decoder diagnostic echoed the credential: {}",
        fault.detail
    );
}

/// The body of the reply fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Status {
    task: String,
    status: String,
}

/// One committed reply document at `version`, answering `epoch`.
fn response(version: u32, epoch: &str) -> Vec<u8> {
    support::response(
        WireVersion(version),
        uuid("018f82d4-6f59-7d06-92e3-bce9131163c4"),
        uuid(epoch),
        Outcome::Reply(Reply {
            position: ReplyPosition::Committed(SeqRange {
                first: seq(48213),
                last: seq(48214),
            }),
            body: Status {
                task: "SPICE-1kKRRsDPx".to_owned(),
                status: "done".to_owned(),
            },
        }),
    )
}

/// A position, which every fixture spells above zero.
fn seq(value: u64) -> Seq {
    Seq::new(value).expect("fixture position is non-zero")
}

#[test]
fn hello_opens_at_the_version_this_build_speaks() {
    let hello = session().hello();

    assert_eq!(hello.wire, WireVersion::CURRENT);
    assert_eq!(hello.capability, uuid(CAPABILITY));
    assert_eq!(hello.epoch, uuid(EPOCH));
}

#[test]
fn the_opening_a_session_offers_is_the_one_a_resident_admits() {
    let bytes = wire::encode(&session().hello()).expect("hello encodes");
    let crossed: wire::Hello = wire::decode(&bytes).expect("hello decodes");

    assert!(crossed.admit(uuid(EPOCH)).is_ok());
}

#[test]
fn a_requirement_unaware_app_generation_is_stopped_at_discovery() {
    let fault = Session::admit_app_protocol(Some(wire::AppProtocolVersion(3)))
        .expect_err("a requirement-unaware browser does not cross current app DTOs");
    assert_eq!(fault.reason, "bootstrap::app-protocol-mismatch");
}

#[test]
fn the_current_app_protocol_is_admitted_at_discovery() {
    Session::admit_app_protocol(Some(wire::AppProtocolVersion::CURRENT))
        .expect("the current app reaches bootstrap decoding");
}

#[test]
fn an_absent_app_protocol_replaces_the_current_document() {
    let fault = Session::admit_app_protocol(None)
        .expect_err("a response without the pre-socket clock does not open a socket");
    assert_eq!(fault.reason, "bootstrap::app-protocol-mismatch");
}

#[test]
fn an_opening_against_a_replaced_generation_is_rejected_by_the_resident() {
    let hello = session().hello();

    let rejected = hello
        .admit(uuid(NEXT_EPOCH))
        .expect_err("a moved epoch rejects");

    assert!(matches!(rejected, wire::Rejected::Epoch(_)));
}

#[test]
fn a_current_frame_for_this_generation_is_admitted() {
    let admitted = session()
        .admit_frame::<Status>(&response(1, EPOCH))
        .expect("a current frame is admitted");
    let frame = admitted.get();

    assert_eq!(frame.v, WireVersion::CURRENT);
    assert_eq!(frame.epoch, uuid(EPOCH));
}

#[test]
fn a_frame_at_another_wire_version_faults_and_names_both_versions() {
    let fault = session()
        .admit_frame::<Status>(&response(2, EPOCH))
        .expect_err("a foreign version faults");

    assert_eq!(fault.class, FaultClass::Protocol);
    assert_eq!(fault.reason, "frame::wire-mismatch");
    assert!(fault.detail.contains('2'), "{}", fault.detail);
    assert!(
        fault.detail.contains(&WireVersion::CURRENT.to_string()),
        "{}",
        fault.detail
    );
}

#[test]
fn a_frame_answering_another_generation_faults_and_names_both_epochs() {
    let fault = session()
        .admit_frame::<Status>(&response(1, NEXT_EPOCH))
        .expect_err("a foreign epoch faults");

    assert_eq!(fault.class, FaultClass::Protocol);
    assert_eq!(fault.reason, "frame::epoch-mismatch");
    assert!(fault.detail.contains(NEXT_EPOCH), "{}", fault.detail);
    assert!(fault.detail.contains(EPOCH), "{}", fault.detail);
}

#[test]
fn bytes_that_are_not_a_frame_fault_rather_than_being_repaired() {
    let fault = session()
        .admit_frame::<Status>(b"{\"v\": 1, \"id\":")
        .expect_err("truncated bytes fault");

    assert_eq!(fault.class, FaultClass::Protocol);
    assert_eq!(fault.reason, "frame::undecodable");
    assert!(!fault.detail.is_empty());
}

#[test]
fn a_frame_missing_a_required_field_faults_as_undecodable() {
    let missing_epoch = br#"{"v": 1, "id": "018f82d4-6f59-7d06-92e3-bce9131163c4"}"#;

    let fault = session()
        .admit_frame::<Status>(missing_epoch)
        .expect_err("an incomplete frame faults");

    assert_eq!(fault.reason, "frame::undecodable");
}
