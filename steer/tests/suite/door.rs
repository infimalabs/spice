//! Steering rendering boundaries.

use steer::ack::keys::Key;
use steer::directives::{Ledger, Steering};
use steer::inbox;
use wire::CanonicalUuid;
use wire::body::SteerSent;

/// The lane most of these tests speak to.
const LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116302";

/// A second lane, which must hear none of the first lane's direction.
const OTHER: &str = "018f82d4-6f59-7d06-92e3-bce913116303";

/// The key a rendering is sealed under.
const SEAL: &str = "1kJK6N3Rx";

/// The instant renderings are made at.
fn now() -> wire::Instant {
    "2026-08-01T07:00:00.000Z".parse().expect("fixture now")
}

/// A lane's rendering carries its own pending direction, sealed, and nobody
/// else's.
#[test]
fn a_rendering_carries_one_lane_sealed() {
    let ledger = ledger();
    let lane = lane(LANE);
    let rendered = inbox::render(&seal(), &ledger.pending(lane), now());
    assert_eq!(
        rendered,
        format!(
            "\
<1kJK6N3Rx>
directive={} key=1kJK6M0Vx stage=published expires=2026-08-01T07:07:19.000Z control=drive-drain-queue
DRAIN QUEUE ASAP
directive={} key=1kJK7Sqyx stage=published expires=2026-08-01T08:07:19.000Z
feed the board
</1kJK6N3Rx>
",
            directive("1kJK6M0Vx"),
            directive("1kJK7Sqyx"),
        )
    );
}

#[test]
fn typed_media_reaches_the_lane_once_with_its_directive_identity() {
    let attachment =
        wire::Media::from_base64(r"operator](capture)\雪.png", "image/png", "iVBORw0KGgo=")
            .expect("fixture attachment admits");
    let canonical_markdown = attachment.markdown().expect("fixture Markdown renders");
    let reference = wire::AgentMediaReference::from_media(directive("1kJK6M0Vx"), 0, &attachment)
        .expect("fixture reference derives");
    let encoded_reference =
        String::from_utf8(wire::encode(&reference).expect("fixture reference encodes"))
            .expect("reference JSON is UTF-8");
    let cases = [
        "inspect this capture".to_owned(),
        format!("inspect this capture\n\n{canonical_markdown}"),
    ];

    for text in cases {
        let body = SteerSent {
            sender: wire::Actor::TEST_PRINCIPAL,
            room: None,
            directive: directive("1kJK6M0Vx"),
            fanout: None,
            recipients: Vec::new(),
            key: "1kJK6M0Vx".to_owned(),
            recipient: wire::Actor::Lane(lane(LANE)),
            text,
            control: None,
            attachments: vec![attachment.clone()],
            expires: "2026-08-01T07:07:19.000Z".parse().expect("instant"),
        };
        let mut ledger = Ledger::new();
        ledger
            .absorb(Steering::Sent(&body))
            .expect("typed attachment enters standing");

        let rendered = inbox::render(&seal(), &ledger.pending(lane(LANE)), now());
        assert!(
            rendered.contains(&format!("directive={}", body.directive)),
            "lane rendering lost the durable directive identity: {rendered}",
        );
        assert_eq!(
            rendered.matches(&attachment.data_url).count(),
            0,
            "disposable lane prose must not carry authority bytes: {rendered}",
        );
        assert_eq!(rendered.matches(&encoded_reference).count(), 1);
    }
}

/// The ledger both other tests read their pending direction out of.
fn ledger() -> Ledger {
    let mut ledger = Ledger::new();
    for (key, addressee, text, control, expires) in [
        (
            "1kJK6M0Vx",
            LANE,
            "DRAIN QUEUE ASAP",
            Some("drive-drain-queue"),
            "2026-08-01T07:07:19.000Z",
        ),
        (
            "1kJK7Sqyx",
            LANE,
            "feed the board",
            None,
            "2026-08-01T08:07:19.000Z",
        ),
        (
            "1kJK8KcWx",
            OTHER,
            "another lane's direction",
            None,
            "2026-08-01T09:07:19.000Z",
        ),
    ] {
        ledger
            .absorb(Steering::Sent(&SteerSent {
                sender: wire::Actor::TEST_PRINCIPAL,
                room: None,
                directive: directive(key),
                fanout: None,
                recipients: Vec::new(),
                key: key.to_owned(),
                recipient: wire::Actor::Lane(lane(addressee)),
                text: text.to_owned(),
                control: control.map(str::to_owned),
                attachments: Vec::new(),
                expires: expires.parse().expect("instant"),
            }))
            .expect("directive absorbs");
    }
    ledger
}

fn seal() -> Key {
    SEAL.parse().expect("seal")
}

fn lane(text: &str) -> CanonicalUuid {
    text.parse().expect("lane")
}

fn directive(key: &str) -> CanonicalUuid {
    let suffix = key.bytes().fold(0_u64, |value, byte| {
        value.wrapping_mul(131).wrapping_add(u64::from(byte))
    });
    format!("00000000-0000-4000-8000-{:012x}", suffix & 0xffff_ffff_ffff)
        .parse()
        .expect("fixture directive UUID")
}
