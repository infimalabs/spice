use super::*;

fn author() -> CanonicalUuid {
    "018f82d4-6f59-7d06-92e3-bce9131163f1"
        .parse()
        .expect("fixture author parses")
}

fn recipient(address: &str, lane: &str, occupant: &str) -> wire::MessageRecipient {
    wire::MessageRecipient {
        address: address.parse().expect("fixture address parses"),
        lane: lane.parse().expect("fixture lane parses"),
        occupant: occupant.parse().expect("fixture occupant parses"),
    }
}

fn reviewers() -> [wire::MessageRecipient; 2] {
    [
        recipient(
            "6K26",
            "018f82d4-6f59-7d06-92e3-bce9131163a1",
            "018f82d4-6f59-7d06-92e3-bce9131163a2",
        ),
        recipient(
            "6Jv5",
            "018f82d4-6f59-7d06-92e3-bce9131163b1",
            "018f82d4-6f59-7d06-92e3-bce9131163b2",
        ),
    ]
}

fn proved(recipients: &[wire::MessageRecipient]) -> PresentationDirectives {
    let mut ledger = PresentationDirectives::default();
    for recipient in recipients {
        ledger.recipients.insert(recipient.address, *recipient);
    }
    ledger
}

fn authored() -> PresentationEvent<'static> {
    PresentationEvent {
        author: Some(author()),
        ..event()
    }
}

fn present(
    text: &str,
    ledger: &PresentationDirectives,
    event: PresentationEvent,
) -> serve::stream::MessagePresentation {
    present_steering_acknowledgements(text, Some(ledger), event).expect("message presents")
}

#[test]
fn an_addressed_packet_presents_its_body_as_one_outgoing_quote_in_header_order() {
    let [first, second] = reviewers();
    let text =
        "Asking both reviewers.\nSYN 6Jv5 6K26: Please check the parser.\nIt rejects empty input.";
    let presented = present(text, &proved(&[first, second]), authored());

    assert_eq!(
        presented.text,
        "Asking both reviewers.\n> Please check the parser.\n> It rejects empty input."
    );
    let [send] = presented.sends.as_slice() else {
        panic!("one packet presents one send: {:?}", presented.sends)
    };
    assert_eq!(
        &presented.text[send.start as usize..send.end as usize],
        "> Please check the parser.\n> It rejects empty input."
    );
    assert_eq!((send.from, send.sender), (lane(), author()));
    assert_eq!(send.recipients, vec![second, first]);
    let source = usize::try_from(send.source.start).expect("source start fits")
        ..usize::try_from(send.source.end).expect("source end fits");
    assert_eq!(&text[source], send.body);
}

#[test]
fn contextual_packets_present_room_only_operator_and_directed_destinations() {
    let [_, addressed] = reviewers();
    let presented = present(
        concat!(
            "SYN #ROOMS-dVrD244Jd: Record this in the room.\n",
            "SYN 6Jv5#ROOMS-dVrD255Kf #dVwg6Zqlf: Send this too.",
        ),
        &proved(&[addressed]),
        authored(),
    );

    assert_eq!(
        presented.text,
        "> Record this in the room.\n\n> Send this too."
    );
    let [room_only, directed] = presented.sends.as_slice() else {
        panic!("both contextual packets present: {:?}", presented.sends)
    };
    assert!(room_only.recipients.is_empty());
    assert!(!room_only.operator);
    assert_eq!(
        room_only.contexts,
        ["ROOMS-dVrD244Jd"
            .parse::<wire::TaskHandle>()
            .expect("fixture task parses")]
    );
    assert_eq!(directed.recipients, [addressed]);
    assert!(directed.operator);
    assert_eq!(
        directed.contexts,
        ["ROOMS-dVrD255Kf"
            .parse::<wire::TaskHandle>()
            .expect("fixture task parses")]
    );
    assert_eq!(room_only.body, "Record this in the room.");
    assert_eq!(directed.body, "Send this too.");
}

#[test]
fn a_packet_and_a_following_acknowledgement_present_as_separate_quotes() {
    let key = "1kQnrzNZx";
    let directive = "Keep one tested path.";
    let mut ledger = proved(&reviewers());
    ledger.directives.insert(
        key.parse().expect("fixture key parses"),
        PresentationDirective {
            actor: wire::Actor::TEST_PRINCIPAL,
            attribution: operator_attribution(),
            fanout: None,
            published: "2030-04-12T18:44:09.001Z"
                .parse()
                .expect("fixture instant parses"),
            lane: lane(),
            text: directive.to_owned(),
            authored: true,
            taste: false,
            review: None,
            disposition: wire::Disposition::Live,
            quote_target: wire::QuoteTarget::Steering {
                key: key.to_owned(),
                digest: wire::QuoteTarget::text_digest(directive),
            },
            media_quotes: Vec::new(),
            retirement: None,
        },
    );

    let presented = present(
        "SYN 6Jv5: Ship it.\nACK 1kQnrzNZx: done.",
        &ledger,
        authored(),
    );

    assert_eq!(
        presented.text,
        "> Ship it.\n\n> Keep one tested path.\n\nDone."
    );
    let ([send], [context]) = (
        presented.sends.as_slice(),
        presented.directive_contexts.as_slice(),
    ) else {
        panic!("one send and one directive context present");
    };
    assert_eq!(
        &presented.text[send.start as usize..send.end as usize],
        "> Ship it."
    );
    assert_eq!(
        &presented.text[context.start as usize..context.end as usize],
        "> Keep one tested path."
    );
    assert_eq!(send.body, "Ship it.");
}

#[test]
fn a_packet_without_proved_recipients_or_an_authoring_occupant_stays_literal() {
    let [first, _] = reviewers();
    let text = "SYN 6Jv5 6K26: Please check the parser.";
    for (ledger, event) in [
        // One unproved address withholds the whole packet's presentation.
        (proved(&[first]), authored()),
        // A tool caption names no authoring occupant and never sends.
        (proved(&reviewers()), event()),
    ] {
        let presented = present(text, &ledger, event);
        assert_eq!(presented.text, text);
        assert!(presented.sends.is_empty());
    }
}
