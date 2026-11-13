//! Reading authored SYN packets out of assistant prose.
//!
//! Publication packets share the acknowledgment header grammar's
//! authored-context rules but publish rather than retire: each placed packet
//! names its routes, each an audience, a task context, or an audience in a
//! context, and owns exactly the body hunk up to the next placed packet.

use steer::ack::keys::Key;
use steer::ack::scan::{
    Audience, Message, MessageRefusal, Outcome, Standing, ack_reaction, answer,
    has_acknowledgement, has_message, placed_headers, scan, scan_acknowledgements, scan_messages,
};
use wire::{Disposition, OccupantAddress, OccupantAddressError, TaskHandle};

/// Every key any fixture names is live.
struct Live;

impl Standing for Live {
    fn disposition(&self, _: &Key) -> Option<Disposition> {
        Some(Disposition::Live)
    }
}

/// The reply clause `answer` locates for `token`.
fn clause<'prose>(prose: &'prose str, token: &str) -> Option<&'prose str> {
    answer(prose, token, &Live).map(|range| &prose[range])
}

fn address(text: &str) -> OccupantAddress {
    text.parse().expect("fixture address")
}

/// Selectors as written and the exact body bytes one packet publishes.
fn sent<'prose>(prose: &'prose str, message: &Message) -> (Vec<&'prose str>, &'prose str) {
    let publication = message
        .publication
        .as_ref()
        .unwrap_or_else(|refusal| panic!("refused {refusal:?} in {prose:?}"));
    let selectors = publication
        .routes
        .iter()
        .map(|route| {
            let written = &prose[route.span.clone()];
            let (audience, context) = written.split_once('#').unwrap_or((written, ""));
            assert_eq!(
                route.audience,
                (!audience.is_empty()).then(|| Audience::Occupant(address(audience)))
            );
            assert_eq!(
                route.context,
                (!context.is_empty()).then(|| handle(context))
            );
            written
        })
        .collect();
    (selectors, &prose[publication.body.clone()])
}

fn handle(text: &str) -> TaskHandle {
    text.parse().expect("fixture handle")
}

fn refusal(prose: &str) -> MessageRefusal {
    let messages = scan_messages(prose);
    assert_eq!(messages.len(), 1, "expected one packet in {prose:?}");
    messages[0]
        .publication
        .clone()
        .expect_err("packet is refused")
}

/// Each selector owns its own qualifier: room records, directed-in-room, and
/// direct occupant routes share one body without lending each other context.
#[test]
fn selectors_route_to_occupants_rooms_or_both() {
    let prose = "SYN #ROOMS-dVrD244Jd: Background note.\n\
                 SYN 6HmS#ROOMS-dVrD244Jd 7Bc, #DESIGN-dVrD1Kz1l: Shared.";
    let messages = scan_messages(prose);
    assert_eq!(messages.len(), 2);
    assert_eq!(
        sent(prose, &messages[0]),
        (vec!["#ROOMS-dVrD244Jd"], "Background note.")
    );
    assert_eq!(
        sent(prose, &messages[1]),
        (
            vec!["6HmS#ROOMS-dVrD244Jd", "7Bc", "#DESIGN-dVrD1Kz1l"],
            "Shared."
        )
    );
    let publication = messages[1].publication.as_ref().expect("published");
    assert_eq!(
        publication.addresses().collect::<Vec<_>>(),
        [address("6HmS"), address("7Bc")]
    );
}

/// A task pointer records its room and reaches nobody.
#[test]
fn a_task_pointer_records_the_room_without_reaching_an_occupant() {
    let prose = "SYN #ROOMS-dVrD244Jd: Background note.";
    let [message] = scan_messages(prose).try_into().expect("one packet");
    let publication = message.publication.expect("the task pointer records");
    let [route] = publication.routes.as_slice() else {
        panic!("one route");
    };
    assert_eq!(route.audience, None);
    assert_eq!(route.context, Some(handle("ROOMS-dVrD244Jd")));

    let selectorless = "SYN: current task";
    let [message] = scan_messages(selectorless)
        .try_into()
        .expect("one selectorless packet");
    let publication = message.publication.expect("resident resolves current task");
    assert!(publication.routes.is_empty());
    assert_eq!(&selectorless[publication.body], "current task");
}

/// A malformed qualifier, a repeated destination, or a room named twice
/// refuses the whole packet.
#[test]
fn malformed_or_repeated_destinations_refuse_the_packet() {
    assert!(matches!(
        refusal("SYN 6HmS#rooms-x: lower stem."),
        MessageRefusal::Context { span, .. } if span == (9..16)
    ));
    assert!(matches!(
        refusal("SYN #ROOMS-not-a-stamp: malformed room."),
        MessageRefusal::Context { span, .. } if span == (5..22)
    ));
    assert!(matches!(
        refusal("SYN 6HmS#: empty."),
        MessageRefusal::Context { span, .. } if span == (9..9)
    ));
    assert_eq!(
        refusal("SYN #ROOMS-dVrD244Jd #ROOMS-dVrD244Jd: twice."),
        MessageRefusal::RepeatedContext {
            context: handle("ROOMS-dVrD244Jd"),
            span: 21..37,
        }
    );
    assert_eq!(
        refusal("SYN 6HmS#ROOMS-dVrD244Jd 6HmS#DESIGN-dVrD1Kz1l: one audience."),
        MessageRefusal::Repeated {
            audience: Audience::Occupant(address("6HmS")),
            span: 25..29,
        }
    );
    let prose = "SYN #ROOMS-dVrD244Jd #PLAN-dVrD245Kc 6HmS#ROOMS-dVrD244Jd: two rooms.";
    assert_eq!(
        sent(prose, &scan_messages(prose)[0]).0,
        [
            "#ROOMS-dVrD244Jd",
            "#PLAN-dVrD245Kc",
            "6HmS#ROOMS-dVrD244Jd"
        ]
    );
}

/// A hyphenless qualifier names a steering key: `#KEY` identifies the exact
/// input whose authorized counterpart receives a linked line. An occupant is
/// never qualified by an input key, and a hyphenless qualifier that spells no
/// key stays a malformed task context.
#[test]
fn a_direction_key_routes_to_the_operator_about_that_direction() {
    let key = || Some("dVwg6Zqlf".parse::<Key>().expect("fixture key"));
    let routes = |prose: &str| {
        let messages = scan_messages(prose);
        assert_eq!(messages.len(), 1, "expected one packet in {prose:?}");
        messages[0]
            .publication
            .as_ref()
            .unwrap_or_else(|refusal| panic!("refused {refusal:?} in {prose:?}"))
            .routes
            .iter()
            .map(|route| {
                (
                    route.audience,
                    route.context.clone(),
                    route.direction.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        routes("SYN #dVwg6Zqlf: Which first?"),
        [(Some(Audience::Input), None, key())]
    );
    assert_eq!(
        routes("SYN #dVwg6Zqlf 6HmS#ROOMS-dVrD244Jd: Noted."),
        [
            (Some(Audience::Input), None, key()),
            (
                Some(Audience::Occupant(address("6HmS"))),
                Some(handle("ROOMS-dVrD244Jd")),
                None
            ),
        ]
    );
    assert_eq!(
        refusal("SYN 6HmS#dVwg6Zqlf: wrong audience."),
        MessageRefusal::DirectionAudience { span: 4..18 }
    );
    assert_eq!(
        refusal("SYN #dVwg6Zqlf #dVwg6Zqlf: twice."),
        MessageRefusal::Repeated {
            audience: Audience::Input,
            span: 15..25,
        }
    );
    assert!(matches!(
        refusal("SYN 6HmS#rooms: no key."),
        MessageRefusal::Context { span, .. } if span == (9..14)
    ));
    assert!(scan_messages("We tracked it in #dVwg6Zqlf: done.").is_empty());
}

/// A body keeps spaces, wrapped lines, blank lines, and packet-like text that
/// places no header, ending only at the next placed SYN, ACK, FIN, or NAK header
/// or the end of the prose. Each placed neighbor still stands at its own locus.
#[test]
fn a_body_runs_to_the_next_placed_packet_or_the_end() {
    let prose = concat!(
        "Context the peers never see.\n\n",
        "SYN 6HmS, 7Bc 9x: First line.\n",
        "Wrapped   line.\n",
        " \t\r\n",
        "Second paragraph; the example SYN 6HmS: narrated.\n",
        "ACK 1kJK6N3Rx: applied\n\nacross a blank line\n",
        "NAK 1kJK6M0Vx: declined SYN 6HmS: Only for you.\n\n",
        "Closing report.",
    );
    let messages = scan_messages(prose);

    assert_eq!(messages.len(), 2);
    assert_eq!(&prose[messages[0].header.clone()], "SYN 6HmS, 7Bc 9x:");
    assert_eq!(
        sent(prose, &messages[0]),
        (
            vec!["6HmS", "7Bc", "9x"],
            "First line.\nWrapped   line.\n \t\r\nSecond paragraph; the example SYN 6HmS: narrated."
        )
    );
    let private = prose.find("SYN 6HmS: Only").expect("fixture packet");
    assert_eq!(
        messages[1].header,
        private..private + "SYN 6HmS:".len(),
        "a packet after a clause starts at its own header"
    );
    assert_eq!(
        sent(prose, &messages[1]),
        (vec!["6HmS"], "Only for you.\n\nClosing report.")
    );
    assert_eq!(
        clause(prose, "1kJK6N3Rx"),
        Some("applied\n\nacross a blank line")
    );
    assert_eq!(clause(prose, "1kJK6M0Vx"), Some("declined"));

    let retired: Vec<_> = scan(prose, &Live)
        .into_iter()
        .map(|recognition| recognition.outcome)
        .collect();
    assert!(matches!(
        retired.as_slice(),
        [
            Outcome::Retired {
                key: acked,
                disposition: Disposition::Acked,
                ..
            },
            Outcome::Retired {
                key: declined,
                disposition: Disposition::Declined,
                ..
            },
        ] if acked.as_str() == "1kJK6N3Rx" && declined.as_str() == "1kJK6M0Vx"
    ));
}

/// A control that carries no peer payload gives the following paragraph back
/// to ordinary prose. The boundary is authored text alone: ACK reactions and a
/// bodyless FIN cut, while each header still retires its key.
#[test]
fn payload_free_controls_end_at_the_first_blank_line() {
    for (prose, token, expected, ordinary) in [
        (
            "ACK 1kJK6N3Rx\n\nOperator report.",
            "1kJK6N3Rx",
            None,
            "Operator report.",
        ),
        (
            "ACK 1kJK6M0Vx: noted.!;,\n\nOperator report.",
            "1kJK6M0Vx",
            Some("noted.!;,"),
            "Operator report.",
        ),
        (
            "ACK 1kJK7Sqyx: ThAnKs\n \t\r\nOperator report.",
            "1kJK7Sqyx",
            Some("ThAnKs"),
            "Operator report.",
        ),
        (
            "FIN 1kJKB307x\n\nOperator report.",
            "1kJKB307x",
            None,
            "Operator report.",
        ),
    ] {
        assert_eq!(clause(prose, token), expected, "wrong body in {prose:?}");
        assert_eq!(
            prose.rsplit_once('\n').map(|(_, tail)| tail),
            Some(ordinary),
            "the later paragraph remains ordinary prose"
        );
        assert!(matches!(
            scan(prose, &Live).as_slice(),
            [recognition] if matches!(&recognition.outcome, Outcome::Retired { .. })
        ));
    }
}

#[test]
fn ack_reactions_accept_only_nods_with_their_trailing_punctuation() {
    for clause in ["ok.", "NOTED,!;", "thanks:", "+1!!!", "  Thanks.,:;! \n"] {
        assert!(ack_reaction(Some(clause)), "{clause:?}");
    }
    for clause in [".", "thanks?", "thanks. friend", "noted …", "+1-"] {
        assert!(!ack_reaction(Some(clause)), "{clause:?}");
    }
}

/// Quoted, coded, rendered, negated, hypothetical, and narrated markers send
/// nothing, and inside a body they are content rather than boundaries.
#[test]
fn unauthored_markers_neither_publish_nor_bound_a_body() {
    for prose in [
        "> SYN 6HmS: quoted.",
        "    SYN 6HmS: indented code.",
        "```\nSYN 6HmS: fenced.\n```",
        "Use `SYN 6HmS: hi` to send.",
        "src/peer.rs:3: SYN 6HmS: rendered source.",
        "I will not SYN 6HmS: negated.",
        "Don't SYN 6HmS: negated.",
        "Never SYN 6HmS: negated.",
        "I shouldn\u{2019}t SYN 6HmS: negated typographically.",
        "You mustn't SYN 6HmS: negated.",
        "If I SYN 6HmS: hypothetical.",
        "The example SYN 6HmS: narrated.",
        "The \u{2018}example\u{2019} SYN 6HmS: narrated.",
        "The example\u{2019}s SYN 6HmS: narrated.",
        "I will \u{2018}not\u{2019} SYN 6HmS: negated.",
        "If\u{2019} I SYN 6HmS: hypothetical.",
    ] {
        assert_eq!(scan_messages(prose), [], "published {prose:?}");
    }

    let prose = concat!(
        "SYN 6HmS: Paste this:\n",
        "```\nACK 1kJK6N3Rx: example\nSYN 7Bc: example\n```\n",
        "> NAK 1kJK6M0Vx: quoted\n",
        "Then reply.",
    );
    let messages = scan_messages(prose);
    assert_eq!(messages.len(), 1);
    assert_eq!(sent(prose, &messages[0]).1, &prose["SYN 6HmS: ".len()..]);

    for prose in [
        "SYN 6HmS: Keep going. Don't SYN 7Bc: that one is still ours.",
        "SYN 6HmS: Keep going. Don\u{2019}t SYN 7Bc: that one is still ours.",
    ] {
        let messages = scan_messages(prose);
        assert_eq!(messages.len(), 1, "{prose:?}");
        assert_eq!(sent(prose, &messages[0]).1, &prose["SYN 6HmS: ".len()..]);
    }
    assert!(
        scan(prose, &Live)
            .iter()
            .all(|recognition| !matches!(recognition.outcome, Outcome::Retired { .. }))
    );
}

/// Markdown list examples describe packets; they never send or retire one.
#[test]
fn list_item_headers_open_no_packet() {
    for prose in [
        "- SYN 6HmS: unordered example.",
        "+ ACK 1kJK6N3Rx: unordered example.",
        "* NAK 1kJK6N3Rx: unordered example.",
        "1. SYN 6HmS: ordered example.",
        "  12) **ACK 1kJK6N3Rx:** ordered example.",
    ] {
        assert!(!has_message(prose), "published {prose:?}");
        assert!(!has_acknowledgement(prose), "retired {prose:?}");
        assert!(placed_headers(prose).is_empty(), "placed {prose:?}");
    }
}

/// A protocol name inside an ordinary clause does not become a header merely
/// because later prose contains route-shaped text.
#[test]
fn mid_clause_message_narration_is_not_a_packet_or_body_boundary() {
    let narration = "A FIN and a SYN #dYNbhjfRj both refused";
    assert!(scan_messages(narration).is_empty());
    assert!(placed_headers(narration).is_empty());

    let prose = format!("SYN 6HmS: Keep all of this: {narration}");
    let [message] = scan_messages(&prose).try_into().expect("one packet");
    assert_eq!(
        sent(&prose, &message),
        (vec!["6HmS"], &prose["SYN 6HmS: ".len()..])
    );
    let header = 0.."SYN 6HmS:".len();
    assert_eq!(placed_headers(&prose), [header]);
}

/// Every recipient-list or body defect refuses the whole packet, decided by the
/// first defect in source order, and never borrows a neighbor's body.
#[test]
fn defective_packets_refuse_whole_without_guessing() {
    assert_eq!(
        refusal("SYN 6HmS 7Bc 6HmS: twice."),
        MessageRefusal::Repeated {
            audience: Audience::Occupant(address("6HmS")),
            span: 13..17,
        }
    );
    assert_eq!(
        refusal("SYN 6HmS 0B: leading zero."),
        MessageRefusal::Recipient {
            span: 9..11,
            error: OccupantAddressError::LeadingZero("0B".to_owned()),
        }
    );
    assert_eq!(
        refusal("SYN bob 6HmS: vowel first."),
        MessageRefusal::Recipient {
            span: 4..7,
            error: OccupantAddressError::Alphabet("bob".to_owned()),
        }
    );
    for numbered in [
        "1. SYN bob 6HmS: numbered.",
        "  12) SYN 6HmS: numbered packet",
    ] {
        assert!(scan_messages(numbered).is_empty(), "published {numbered:?}");
    }
    assert_eq!(
        refusal("SYN 6HmS please review this"),
        MessageRefusal::Unterminated
    );
    assert_eq!(refusal("SYN 6HmS, : comma."), MessageRefusal::Unterminated);
    assert_eq!(refusal("SYN 6HmS:  \n\t\n"), MessageRefusal::EmptyBody);

    let prose = "SYN 6HmS:\nACK 1kJK6N3Rx: done\nSYN 7Bc: hi\nSYN 9x 9x: dup";
    let messages = scan_messages(prose);
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].publication, Err(MessageRefusal::EmptyBody));
    assert_eq!(sent(prose, &messages[1]), (vec!["7Bc"], "hi"));
    assert!(matches!(
        messages[2].publication,
        Err(MessageRefusal::Repeated { .. })
    ));
    assert_eq!(scan_acknowledgements(prose, &Live).len(), 1);
}

/// A header with no address-shaped recipient is ordinary prose using the word.
#[test]
fn prose_that_only_uses_the_word_is_not_a_packet() {
    for prose in [
        "SYN count: 3",
        "SYN only talks about a protocol name",
        "SYNS 6HmS: plural",
        "aSYN 6HmS: embedded",
        "SYN hello there",
        "SYN #1: first heading",
        "SYN #: hash only",
        "SYN #rooms-dVrD244Jd tag",
    ] {
        assert_eq!(scan_messages(prose), [], "published {prose:?}");
    }
}

/// An address that is also spelled like a steering key routes as an address
/// and retires nothing, even while that key is live.
#[test]
fn a_key_shaped_address_is_a_recipient_not_a_retirement() {
    let prose = "SYN dVhKT0dWx: hello";
    assert!("dVhKT0dWx".parse::<Key>().is_ok());

    let messages = scan_messages(prose);
    assert_eq!(messages.len(), 1);
    assert_eq!(sent(prose, &messages[0]), (vec!["dVhKT0dWx"], "hello"));
    assert_eq!(scan(prose, &Live), []);
}

/// A retirement's reply keeps wrapped lines and blank lines and ends only
/// before a placed packet or at the end of the prose. An unplaced SYN marker
/// remains reply prose.
#[test]
fn a_reply_clause_runs_to_the_next_placed_packet_or_the_end() {
    let prose = "ACK 1kJK6N3Rx: done here SYN 6HmS: private note";
    assert_eq!(clause(prose, "1kJK6N3Rx"), Some("done here"));
    let messages = scan_messages(prose);
    assert_eq!(messages.len(), 1);
    assert_eq!(sent(prose, &messages[0]), (vec!["6HmS"], "private note"));

    assert_eq!(
        clause("NAK 1kJK6N3Rx: **SYN 6HmS:** only this", "1kJK6N3Rx"),
        None
    );
    let narrated = "ACK 1kJK6N3Rx: done; SYN 6HmS please";
    assert_eq!(clause(narrated, "1kJK6N3Rx"), Some("done; SYN 6HmS please"));
    for marker in ["ACK", "FIN", "NAK"] {
        let prose =
            format!("{marker} 1kJK6N3Rx: first line\nwrapped line\n \t\r\nOperator report.");
        assert_eq!(
            clause(&prose, "1kJK6N3Rx"),
            Some("first line\nwrapped line\n \t\r\nOperator report."),
            "reply for {prose:?}"
        );
    }
    for (prose, expected) in [
        ("ACK 1kJK6N3Rx: done here\nSYN 6HmS: next line", "done here"),
        (
            "ACK 1kJK6N3Rx: done here `SYN 6HmS: inline`",
            "done here `SYN 6HmS: inline`",
        ),
        (
            "ACK 1kJK6N3Rx: done here; the example SYN 6HmS: narrated",
            "done here; the example SYN 6HmS: narrated",
        ),
        (
            "ACK 1kJK6N3Rx: done here, SYN count: 3",
            "done here, SYN count: 3",
        ),
    ] {
        assert_eq!(clause(prose, "1kJK6N3Rx"), Some(expected));
    }
}
