//! A directive context and every quote of its source sign off through one
//! attribution: author, arrow, and each recipient its record proves.

use super::*;

/// A quoted SYN names the occupant that wrote it while that occupant holds
/// the seat. Once it leaves, whether the lane is vacant or reseated, the
/// lane's own name and accent attribute it historically. A lane gone from
/// the roster reads as unavailable, never as raw identifiers.
#[test]
fn a_quoted_syn_context_keeps_its_lane_identity_across_seats() {
    let id = |suffix: &str| -> wire::CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131164{suffix}")
            .parse()
            .expect("fixture identity is canonical")
    };
    let (lane, author, successor) = (id("0a"), id("1a"), id("1b"));
    let quoted = "> hold the merge";
    let markdown = format!("{quoted}\n\nHeld.");
    let directives = [MessageDirective {
        start: 0,
        end: u32::try_from(quoted.len()).expect("fixture offset fits"),
        quote_target: steering_target("dTVQdQfLx", quoted),
        media_quotes: Vec::new(),
        attribution: attributed(wire::QuoteAuthor::Agent {
            lane,
            occupant: author,
        }),
        authored: "1970-01-01T00:00:01.000Z".parse().expect("fixture instant"),
        badge: None,
        review: None,
        line: None,
        delivery: None,
    }];
    let sources = BTreeMap::from([(
        directives[0].quote_target.to_string(),
        QuoteSource::directive(&directives[0]),
    )]);
    let agent = |label: &str| QuoteAgent {
        accent: 2,
        label: label.to_owned(),
    };
    let render = |agents: &QuoteAgents| {
        render_prose_card(&markdown, &directives, &[], &[], agents, &sources).html
    };
    let name = |state: &str, label: &str| {
        format!(
            "data-quote-agent-lane=\"{lane}\" data-quote-agent-occupant=\"{author}\" \
             data-quote-author-state=\"{state}\"{}>{label}</span>",
            if state == "unresolved" {
                ""
            } else {
                " data-accent=\"2\""
            }
        )
    };

    for (agents, state) in [
        (
            QuoteAgents::from_iter([((lane, Some(author)), agent("lane-name"))]),
            "current",
        ),
        (
            QuoteAgents::from_iter([((lane, Some(successor)), agent("lane-name"))]),
            "historical",
        ),
        (
            QuoteAgents::from_iter([((lane, None), agent("lane-name"))]),
            "historical",
        ),
    ] {
        let html = render(&agents);
        assert!(html.contains(&name(state, "lane-name")), "{html}");
    }
    let absent = render(&QuoteAgents::new());
    assert!(
        absent.contains(&name("unresolved", "unavailable")),
        "{absent}"
    );
}

#[test]
fn peer_context_destination_survives_badges_and_reseating() {
    let id = |suffix: &str| -> wire::CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131166{suffix}")
            .parse()
            .unwrap()
    };
    let (sender_lane, sender, recipient_lane, recipient, successor) =
        (id("0a"), id("1a"), id("0b"), id("1b"), id("2b"));
    let body = "> Check the boundary.";
    let markdown = format!("{body}\n\nChecked.");
    let mut context = MessageDirective {
        start: 0,
        end: u32::try_from(body.len()).unwrap(),
        quote_target: steering_target("dTVQdQfLx", body),
        media_quotes: Vec::new(),
        attribution: wire::QuoteAttribution {
            recipients: vec![wire::QuoteRecipient {
                lane: recipient_lane,
                occupant: Some(recipient),
            }],
            ..attributed(wire::QuoteAuthor::Agent {
                lane: sender_lane,
                occupant: sender,
            })
        },
        authored: "1970-01-01T00:00:01.000Z".parse().unwrap(),
        badge: None,
        review: None,
        line: None,
        delivery: None,
    };
    let sources = BTreeMap::from([(
        context.quote_target.to_string(),
        QuoteSource::directive(&context),
    )]);
    let agent = |label: &str| QuoteAgent {
        accent: 1,
        label: label.to_owned(),
    };
    for badge in [
        None,
        Some(wire::DirectiveBadge::Review),
        Some(wire::DirectiveBadge::Taste),
    ] {
        context.badge = badge;
        for (seat, expected_state) in [
            (Some(recipient), "current"),
            (None, "historical"),
            (Some(successor), "historical"),
        ] {
            let roster = QuoteAgents::from_iter([
                ((sender_lane, Some(sender)), agent("sender")),
                ((recipient_lane, seat), agent("recipient-lane")),
            ]);
            let projection =
                referenced_quote_agents(&[], std::slice::from_ref(&context), &[], &roster);
            let html = render_prose_card(
                &markdown,
                std::slice::from_ref(&context),
                &[],
                &[],
                &projection,
                &sources,
            )
            .html;
            assert!(html.contains("data-quote-direction=\"incoming\""), "{html}");
            let destination = html.split("message-quote-recipients").nth(1).unwrap();
            assert!(destination.contains(&format!("data-quote-agent-occupant=\"{recipient}\"")));
            assert!(destination.contains(&format!("data-quote-author-state=\"{expected_state}\"")));
            assert!(destination.contains(">recipient-lane</span>"));
            assert!(html.contains("Check the boundary."));
            assert!(html.contains("Checked."));
        }
    }
}

/// A quoted peer input names its line by the opening input's key and carries
/// the standing every other surface prints for it; a directive on no line
/// carries no line classification.
#[test]
fn a_quoted_peer_input_names_its_line_by_key() {
    let id = |suffix: &str| -> wire::CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131168{suffix}")
            .parse()
            .unwrap()
    };
    let (sender_lane, sender, recipient_lane, recipient) = (id("0a"), id("1a"), id("0b"), id("1b"));
    let body = "> Nearly settled.";
    let markdown = format!("{body}\n\nGood.");
    let line = wire::intent::LineStanding {
        root: "dTVQdQfLw".to_owned(),
        parent: None,
        body: wire::body::PeerBodyKind::Continue,
        covered: vec!["dTVQdQfLw".to_owned()],
        admission: Some(wire::body::PeerAdmission::SameRound),
        answer: None,
        closed: None,
    };
    let mut context = MessageDirective {
        start: 0,
        end: u32::try_from(body.len()).unwrap(),
        quote_target: steering_target("dTVQdQfLx", body),
        media_quotes: Vec::new(),
        attribution: wire::QuoteAttribution {
            recipients: vec![wire::QuoteRecipient {
                lane: recipient_lane,
                occupant: Some(recipient),
            }],
            ..attributed(wire::QuoteAuthor::Agent {
                lane: sender_lane,
                occupant: sender,
            })
        },
        authored: "1970-01-01T00:00:01.000Z".parse().unwrap(),
        badge: None,
        review: None,
        line: Some(line.clone()),
        delivery: None,
    };
    let render = |context: &MessageDirective| {
        let sources = BTreeMap::from([(
            context.quote_target.to_string(),
            QuoteSource::directive(context),
        )]);
        let roster = QuoteAgents::from_iter([(
            (sender_lane, Some(sender)),
            QuoteAgent {
                accent: 1,
                label: "sender".to_owned(),
            },
        )]);
        let projection = referenced_quote_agents(&[], std::slice::from_ref(context), &[], &roster);
        render_prose_card(
            &markdown,
            std::slice::from_ref(context),
            &[],
            &[],
            &projection,
            &sources,
        )
        .html
    };
    let html = render(&context);
    assert!(
        html.contains(&format!(
            "class=\"visually-hidden\" data-message-classification=\"line\" \
             data-message-line=\"dTVQdQfLw\" aria-label=\"Peer line: {}\"",
            line.standing()
        )),
        "{html}"
    );
    assert!(
        !html.contains(">CONTINUES dTVQdQfLw</span>"),
        "protocol transitions stay out of visible message chrome: {html}"
    );
    context.line = None;
    assert!(
        !render(&context).contains("data-message-classification=\"line\""),
        "a directive on no line carries no line metadata"
    );
}

/// A delivered principal direction reads principal → every receiving member,
/// and its rail stays neutral just as it does wherever that source is quoted.
#[test]
fn a_delivered_principal_direction_keeps_a_neutral_rail_and_names_every_receiver() {
    let id = |suffix: &str| -> wire::CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131167{suffix}")
            .parse()
            .unwrap()
    };
    let (first_lane, first, second_lane, second) = (id("0a"), id("1a"), id("0b"), id("1b"));
    let body = "> Ship the projection.";
    let context = MessageDirective {
        start: 0,
        end: u32::try_from(body.len()).unwrap(),
        quote_target: steering_target("dTVQdQfLy", body),
        media_quotes: Vec::new(),
        attribution: wire::QuoteAttribution {
            recipients: vec![
                wire::QuoteRecipient {
                    lane: first_lane,
                    occupant: Some(first),
                },
                wire::QuoteRecipient {
                    lane: second_lane,
                    occupant: Some(second),
                },
            ],
            ..attributed(principal_fixture())
        },
        authored: "1970-01-01T00:00:01.000Z".parse().unwrap(),
        badge: None,
        review: None,
        line: None,
        delivery: Some(wire::MessageDelivery {
            fanout: Some(id("ff")),
            provisional: false,
        }),
    };
    let sources = BTreeMap::from([(
        context.quote_target.to_string(),
        QuoteSource::directive(&context),
    )]);
    let agent = |accent: usize, label: &str| QuoteAgent {
        accent,
        label: label.to_owned(),
    };
    let roster = QuoteAgents::from_iter([
        ((first_lane, Some(first)), agent(3, "first")),
        ((second_lane, Some(second)), agent(5, "second")),
    ]);
    let agents = referenced_quote_agents(&[], std::slice::from_ref(&context), &[], &roster);

    let html =
        render_received_with_quotes(body, std::slice::from_ref(&context), &[], &agents, &sources)
            .html;

    assert!(html.contains("<blockquote>"), "{html}");
    assert!(!html.contains("<blockquote data-accent="), "{html}");
    assert!(
        html.contains("class=\"message-broadcast-pill\" data-message-classification=\"broadcast\""),
        "{html}"
    );
    let destination = html.split("message-quote-recipients").nth(1).unwrap();
    let first = destination.find(">first</span>").expect("first receiver");
    let second = destination.find(">second</span>").expect("second receiver");
    assert!(first < second, "{destination}");
    assert_provisional_direction_matches_delivered(&context, body, &agents);
}

fn assert_provisional_direction_matches_delivered(
    context: &MessageDirective,
    body: &str,
    agents: &QuoteAgents,
) {
    let mut provisional = context.clone();
    provisional
        .delivery
        .as_mut()
        .expect("fixture delivery")
        .provisional = true;
    let card_id = Some("message-direction-dTVQdQfLy".to_owned());
    let delivered_sources = BTreeMap::from([(
        context.quote_target.to_string(),
        QuoteSource {
            card_id: card_id.clone(),
            ..QuoteSource::directive(context)
        },
    )]);
    let provisional_sources = BTreeMap::from([(
        provisional.quote_target.to_string(),
        QuoteSource {
            card_id,
            ..QuoteSource::directive(&provisional)
        },
    )]);
    let delivered_html = render_received_with_quotes(
        body,
        std::slice::from_ref(context),
        &[],
        agents,
        &delivered_sources,
    )
    .html;
    let provisional_html = render_received_with_quotes(
        body,
        std::slice::from_ref(&provisional),
        &[],
        agents,
        &provisional_sources,
    )
    .html;
    assert_eq!(
        provisional_html, delivered_html,
        "delivery enrichment must not replace the raw body or reload its images"
    );
    assert!(
        !provisional_html.contains("message-queued-pill"),
        "{provisional_html}"
    );
    assert!(!provisional_html.contains(">QUEUED<"), "{provisional_html}");
    let signoff = provisional_html
        .split("message-quote-signoff")
        .nth(1)
        .expect("provisional card signs off like a delivered one");
    assert!(
        signoff.contains("data-quote-age-authored=\"1000\""),
        "{signoff}"
    );
    assert!(
        signoff.contains("data-quote-source=\"message-direction-dTVQdQfLy\""),
        "{signoff}"
    );
    assert!(
        signoff.contains("aria-label=\"sent to\">→</span>"),
        "{signoff}"
    );
    let destination = signoff.split("message-quote-recipients").nth(1).unwrap();
    assert!(destination.contains(">first</span>"), "{destination}");
    assert!(destination.contains(">second</span>"), "{destination}");
    assert!(
        provisional_html.contains(&format!(
            "data-message-controls=\"{}\"",
            provisional.quote_target
        )),
        "{provisional_html}"
    );
}

/// A direct operator direction reads author → delivered lane on its own card
/// and on every structural quote of it, top-level or nested in agent prose,
/// while that lane is seated, vacant, reseated, or gone from the roster.
#[test]
fn a_quoted_direct_direction_names_the_same_two_ends_as_its_card() {
    let id = |suffix: &str| -> wire::CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131168{suffix}")
            .parse()
            .unwrap()
    };
    let (lane, receiver, successor, agent_lane, agent_seat) =
        (id("0a"), id("1a"), id("2a"), id("0b"), id("1b"));
    let text = "How is it going?";
    let body = format!("> {text}");
    let direction = steering_target("dTVQdQfLv", text);
    // One attribution, derived once from the direction's record, carried by
    // its card and by every quote of it.
    let attribution = wire::QuoteAttribution {
        recipients: vec![wire::QuoteRecipient {
            lane,
            occupant: None,
        }],
        ..attributed(principal(id("f1")))
    };
    let context = MessageDirective {
        start: 0,
        end: u32::try_from(body.len()).unwrap(),
        quote_target: direction.clone(),
        media_quotes: Vec::new(),
        attribution: attribution.clone(),
        authored: "1970-01-01T00:00:01.000Z".parse().unwrap(),
        badge: None,
        review: None,
        line: None,
        delivery: Some(wire::MessageDelivery {
            fanout: None,
            provisional: false,
        }),
    };
    let quoted = wire::QuotePresentation::Resolved {
        target: direction.to_string(),
        text: text.to_owned(),
        attribution,
        disposition: Some(wire::Disposition::Acked),
        quotes: Vec::new(),
        media_quotes: Vec::new(),
    };
    let reply = format!("Looking good. {}", direction.markdown());
    let outer = wire::QuoteTarget::Tape {
        lane: agent_lane,
        source: agent_seat,
        locus: wire::MessageLocus {
            member: 1,
            offset: 24,
            length: 80,
            event: 2,
        },
        digest: wire::QuoteTarget::text_digest(&reply),
    };
    let quotes = [
        quoted.clone(),
        wire::QuotePresentation::Resolved {
            target: outer.to_string(),
            text: reply,
            attribution: attributed(wire::QuoteAuthor::Agent {
                lane: agent_lane,
                occupant: agent_seat,
            }),
            disposition: None,
            quotes: vec![quoted],
            media_quotes: Vec::new(),
        },
    ];
    let markdown = format!("{}\n\n{}", direction.markdown(), outer.markdown());
    let sources = BTreeMap::from([
        (direction.to_string(), QuoteSource::directive(&context)),
        (
            outer.to_string(),
            quote_source("message-agent-reply", 2_000),
        ),
    ]);
    for (seat, recipient) in [
        (Some(Some(receiver)), "Solo"),
        (Some(None), "Solo"),
        (Some(Some(successor)), "Solo"),
        (None, "unavailable"),
    ] {
        let agent = |accent, label: &str| QuoteAgent {
            accent,
            label: label.to_owned(),
        };
        let solo = seat.map(|seat| ((lane, seat), agent(4, "Solo")));
        let kite = ((agent_lane, Some(agent_seat)), agent(3, "Kite"));
        let roster = principal_roster(solo.into_iter().chain([kite]), id("f1"));
        let expected = ("CA".to_owned(), recipient.to_owned());

        let card_agents =
            referenced_quote_agents(&[], std::slice::from_ref(&context), &[], &roster);
        let card = render_received_with_quotes(
            &body,
            std::slice::from_ref(&context),
            &[],
            &card_agents,
            &sources,
        )
        .html;
        assert_eq!(
            signoff_ends(&card),
            std::slice::from_ref(&expected),
            "{card}"
        );

        let quote_agents = referenced_quote_agents(&quotes, &[], &[], &roster);
        let quoting =
            render_received_with_quotes(&markdown, &[], &quotes, &quote_agents, &sources).html;
        assert_eq!(
            signoff_ends(&quoting),
            [expected.clone(), expected],
            "top-level and nested quotes name both ends: {quoting}"
        );
        assert!(!quoting.contains("data-message-packet="), "{quoting}");
    }
}

/// The direction both implied-receiver tests quote.
const IMPLIED_BODY: &str = "> Cover the design controller.";

/// Hiro's lane and seat, another lane, and a roster naming both and an
/// operator principal whose name needs attribute escaping.
fn implied_roster() -> ([wire::CanonicalUuid; 3], QuoteAgents) {
    let id = |suffix: &str| -> wire::CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131169{suffix}")
            .parse()
            .unwrap()
    };
    let (hiro, hiro_seat, other) = (id("0a"), id("1a"), id("0b"));
    let agent = |accent, label: &str| QuoteAgent {
        accent,
        label: label.to_owned(),
    };
    let mut roster = QuoteAgents::from_iter([
        ((hiro, Some(hiro_seat)), agent(1, "Hiro")),
        ((other, Some(id("1b"))), agent(2, "Other")),
    ]);
    roster.principals = serde_json::from_value(serde_json::json!([{
        "principal": id("f1"),
        "code": "6wB",
        "name": "C \"Ace\" & Co", "initials": "CA",
    }]))
    .expect("fixture principal directory decodes");
    ([hiro, hiro_seat, other], roster)
}

/// The hover title an implied-receiver signoff from the fixture operator to
/// Hiro wears: both ends, escaped.
const IMPLIED_TITLE: &str = "title=\"C &quot;Ace&quot; &amp; Co → Hiro\"";

/// The operator's direction to `recipients`, as a card quotes it.
fn direction_to(recipients: &[wire::CanonicalUuid]) -> MessageDirective {
    MessageDirective {
        start: 0,
        end: u32::try_from(IMPLIED_BODY.len()).unwrap(),
        quote_target: steering_target("dTVQdQfLz", IMPLIED_BODY),
        media_quotes: Vec::new(),
        attribution: wire::QuoteAttribution {
            recipients: recipients
                .iter()
                .map(|&lane| wire::QuoteRecipient {
                    lane,
                    occupant: None,
                })
                .collect(),
            ..attributed(principal(
                "018f82d4-6f59-7d06-92e3-bce9131169f1"
                    .parse()
                    .expect("fixture principal is canonical"),
            ))
        },
        authored: "1970-01-01T00:00:01.000Z".parse().unwrap(),
        badge: None,
        review: None,
        line: None,
        delivery: None,
    }
}

/// A prose card's own lane already names the receiver of a quote addressed
/// only to it, so that signoff shows its sender alone while the arrow and
/// receiver stay in its accessible name and hover title. Several receivers,
/// or a card speaking for another lane, still show them.
#[test]
fn a_quote_to_the_card_speaker_leaves_its_receiver_implied() {
    let ([hiro, _, other], roster) = implied_roster();
    let markdown = format!("{IMPLIED_BODY}\n\nCovered.");
    let card = QuoteSource {
        card_id: None,
        display_millis: 0,
        agent: None,
    };
    let render = |recipients: &[wire::CanonicalUuid], speaker| {
        let context = direction_to(recipients);
        let sources = BTreeMap::from([(
            context.quote_target.to_string(),
            QuoteSource::directive(&context),
        )]);
        render_prose_card_with_media_quotes(
            &markdown,
            std::slice::from_ref(&context),
            &[],
            &[],
            QuoteRenderContext::new(&roster, &sources, &card, Some(speaker)),
            &[],
        )
        .html
    };

    let implied = render(&[hiro], hiro);
    let (_, hidden) = implied
        .split_once(&format!("data-quote-receiver=\"implied\" {IMPLIED_TITLE}"))
        .expect("the implied signoff's hover title names its speaker and receiver");
    let (_, hidden) = hidden
        .split_once("<span class=\"visually-hidden\"><span class=\"message-quote-direction\"")
        .expect("the arrow stays for assistive technology only");
    assert!(hidden.contains(">Hiro</span>"), "{implied}");
    for (recipients, speaker) in [(&[hiro][..], other), (&[hiro, other][..], hiro)] {
        let shown = render(recipients, speaker);
        assert!(!shown.contains("data-quote-receiver"), "{shown}");
        assert!(!shown.contains("visually-hidden"), "{shown}");
        assert!(!shown.contains(" → Hiro\""), "{shown}");
        assert!(shown.contains(">Hiro</span>"), "{shown}");
    }
}

/// A quote Hiro spoke surrounds a nested direction to Hiro as Hiro's card
/// does, while the same direction quoted at the top of received text has no
/// speaker around it and names Hiro.
#[test]
fn a_quote_nested_in_its_receivers_quote_leaves_that_receiver_implied() {
    let ([hiro, hiro_seat, _], roster) = implied_roster();
    let context = direction_to(&[hiro]);
    let quoted = wire::QuotePresentation::Resolved {
        target: context.quote_target.to_string(),
        text: IMPLIED_BODY.trim_start_matches("> ").to_owned(),
        attribution: context.attribution.clone(),
        disposition: None,
        quotes: Vec::new(),
        media_quotes: Vec::new(),
    };
    let reply = format!("On it. {}", context.quote_target.markdown());
    let outer = wire::QuoteTarget::Tape {
        lane: hiro,
        source: hiro_seat,
        locus: wire::MessageLocus {
            member: 1,
            offset: 24,
            length: 80,
            event: 2,
        },
        digest: wire::QuoteTarget::text_digest(&reply),
    };
    let quotes = [
        quoted.clone(),
        wire::QuotePresentation::Resolved {
            target: outer.to_string(),
            text: reply,
            attribution: attributed(wire::QuoteAuthor::Agent {
                lane: hiro,
                occupant: hiro_seat,
            }),
            disposition: None,
            quotes: vec![quoted],
            media_quotes: Vec::new(),
        },
    ];
    let sources = BTreeMap::from([
        (
            context.quote_target.to_string(),
            QuoteSource::directive(&context),
        ),
        (
            outer.to_string(),
            quote_source("message-agent-reply", 2_000),
        ),
    ]);
    let markdown = format!(
        "{}\n\n{}",
        context.quote_target.markdown(),
        outer.markdown()
    );
    let html = render_received_with_quotes(&markdown, &[], &quotes, &roster, &sources).html;
    let visible = html
        .find("data-quote-direction=\"incoming\">")
        .unwrap_or_else(|| panic!("the top-level quote names its receiver: {html}"));
    let implied = html
        .find(&format!("data-quote-receiver=\"implied\" {IMPLIED_TITLE}"))
        .unwrap_or_else(|| panic!("the nested quote leaves its receiver implied: {html}"));
    assert!(visible < implied, "{html}");
    assert_eq!(html.matches("data-quote-receiver").count(), 1, "{html}");
}

/// Each signoff that names a recipient, as (author, recipient) labels.
fn signoff_ends(html: &str) -> Vec<(String, String)> {
    let label = |name: &str| {
        name.rsplit_once('>')
            .map_or(name, |(_, label)| label)
            .to_owned()
    };
    html.split("class=\"message-quote-signoff\"")
        .skip(1)
        .filter_map(|signoff| {
            let (lead, recipients) = signoff.split_once("class=\"message-quote-recipients\">")?;
            if lead.contains("message-quote-body") {
                return None;
            }
            let (_, author) = lead.split_once("class=\"message-quote-name\"")?;
            let (author, _) = author.split_once("</span>")?;
            let (recipient, _) = recipients.split_once("</span>")?;
            Some((label(author), label(recipient)))
        })
        .collect()
}

/// `agents`, with `principal` as the operator principal named "CA".
fn principal_roster(
    agents: impl IntoIterator<
        Item = (
            (wire::CanonicalUuid, Option<wire::CanonicalUuid>),
            QuoteAgent,
        ),
    >,
    principal: wire::CanonicalUuid,
) -> QuoteAgents {
    let mut roster = QuoteAgents::from_iter(agents);
    roster.principals = serde_json::from_value(serde_json::json!([{
        "principal": principal,
        "code": "6wB",
        "name": "C Adams", "initials": "CA",
    }]))
    .expect("fixture principal directory decodes");
    roster
}
