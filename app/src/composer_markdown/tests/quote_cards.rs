use super::*;

fn quote_fixture_id(tail: &str) -> wire::CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131163{tail}")
        .parse()
        .expect("fixture identity is canonical")
}

fn nested_quote_sources(
    outer: &wire::QuoteTarget,
    inner: &wire::QuoteTarget,
    agent: QuoteAgent,
) -> QuoteSources {
    BTreeMap::from([
        (
            outer.to_string(),
            QuoteSource {
                card_id: Some("message-agent-outer-source".to_owned()),
                display_millis: 1_000,
                agent: Some(agent),
            },
        ),
        (
            inner.to_string(),
            QuoteSource {
                card_id: None,
                display_millis: 2_000,
                agent: None,
            },
        ),
    ])
}

fn assert_historical_agent_name_survives_reseating(
    outer: &wire::QuoteTarget,
    quotes: &[wire::QuotePresentation],
    sources: &QuoteSources,
    lane: wire::CanonicalUuid,
    source: wire::CanonicalUuid,
) {
    let successor = quote_fixture_id("e9");
    let agents = QuoteAgents::from_iter([(
        (lane, Some(successor)),
        QuoteAgent {
            accent: 4,
            label: "spice-k".to_owned(),
        },
    )]);
    let rendered = render_received_with_quotes(&outer.markdown(), &[], quotes, &agents, sources);
    assert!(rendered.html.contains(&format!(
        "class=\"message-quote-name\" data-quote-agent-lane=\"{lane}\" \
         data-quote-agent-occupant=\"{source}\" data-quote-author-state=\"historical\" \
         data-accent=\"3\">spice-k</span>"
    )));
    assert!(rendered.html.contains("data-quote-age-authored=\"1000\""));
}

#[test]
fn resolved_quote_links_render_complete_nested_cards_synchronously() {
    let inner_text = "authenticated inner direction";
    let inner_target = steering_target("dTVQdQfLw", inner_text);
    let outer_text = format!("before {} after", inner_target.markdown());
    let lane = quote_fixture_id("e6");
    let source = quote_fixture_id("e7");
    let outer_target = wire::QuoteTarget::Tape {
        lane,
        source,
        locus: wire::MessageLocus {
            member: 1,
            offset: 24,
            length: 80,
            event: 2,
        },
        digest: wire::QuoteTarget::text_digest(&outer_text),
    };
    let quotes = vec![wire::QuotePresentation::Resolved {
        target: outer_target.to_string(),
        text: outer_text,
        attribution: attributed(wire::QuoteAuthor::Agent {
            lane,
            occupant: source,
        }),
        disposition: None,
        quotes: vec![wire::QuotePresentation::Resolved {
            target: inner_target.to_string(),
            text: inner_text.to_owned(),
            attribution: attributed(principal(quote_fixture_id("f1"))),
            disposition: Some(wire::Disposition::Live),
            quotes: Vec::new(),
            media_quotes: Vec::new(),
        }],
        media_quotes: Vec::new(),
    }];

    let agent = QuoteAgent {
        accent: 3,
        label: "spice-k".to_owned(),
    };
    let mut agents = QuoteAgents::from_iter([((lane, Some(source)), agent.clone())]);
    agents.principals = serde_json::from_value(serde_json::json!([{
        "principal": quote_fixture_id("f1"),
        "code": "6wB",
        "name": "C Adams", "initials": "CA",
    }]))
    .expect("fixture principal directory decodes");
    let sources = nested_quote_sources(&outer_target, &inner_target, agent);
    assert_eq!(referenced_quote_sources(&quotes, &[], &sources), sources);
    let rendered =
        render_received_with_quotes(&outer_target.markdown(), &[], &quotes, &agents, &sources);

    for (needle, count) in [
        ("data-message-quote-depth=\"1\"", 2),
        ("data-message-quote-depth=\"2\"", 1),
        ("data-quote-state=\"resolved\"", 2),
    ] {
        assert_eq!(rendered.html.matches(needle).count(), count, "{needle}");
    }
    assert!(!rendered.html.contains("style=\"--message-quote-depth"));
    // Only the agent's quote wears its lane accent; the operator's nested quote stays neutral.
    let shell = " data-accent=\"3\"><div class=\"message-quote-body\">";
    assert_eq!(rendered.html.matches(shell).count(), 1);
    assert_eq!(
        rendered.html.matches("message-quote-age-slot").count(),
        3,
        "the outer source leads both hunks around the nested quote"
    );
    assert_eq!(rendered.html.matches("message-quote-name").count(), 3);
    assert!(
        rendered.html.contains(
            "data-quote-actor=\"principal\" role=\"img\" aria-label=\"C Adams\">CA</span>"
        ),
        "the operator's quote names the earliest principal by its recorded initials"
    );
    assert_eq!(rendered.html.matches("data-quote-source=").count(), 2);
    assert!(!rendered.html.contains("Original"));
    assert!(rendered.html.contains(inner_text));
    assert!(rendered.html.contains("data-quote-disposition=\"live\""));
    let outer_body = rendered
        .html
        .find("before ")
        .expect("outer authenticated prose renders");
    let outer_age = rendered
        .html
        .find("data-quote-age-authored=\"1000\"")
        .expect("outer age renders");
    let outer_name = rendered
        .html
        .find(">spice-k</span>")
        .expect("outer name renders");
    assert!(
        outer_age < outer_name && outer_name < outer_body,
        "the outer source attribution leads its first hunk"
    );

    assert_historical_agent_name_survives_reseating(&outer_target, &quotes, &sources, lane, source);
    assert_sourceless_quote_has_no_fabricated_byline(&outer_target, &quotes, &agents);
}

/// Without source chronology, no current timestamp or author name substitutes
/// for the missing evidence.
fn assert_sourceless_quote_has_no_fabricated_byline(
    outer: &wire::QuoteTarget,
    quotes: &[wire::QuotePresentation],
    agents: &QuoteAgents,
) {
    let sourceless =
        render_received_with_quotes(&outer.markdown(), &[], quotes, agents, &BTreeMap::new());
    assert!(!sourceless.html.contains("message-quote-name"));
    assert!(!sourceless.html.contains("message-quote-age-slot"));
    assert!(!sourceless.html.contains("data-quote-source="));
}

#[test]
fn directive_quote_cards_cover_every_disposition_and_board_backed_navigation() {
    let cases = [
        (wire::Disposition::Live, "live"),
        (wire::Disposition::Acked, "acked"),
        (wire::Disposition::Declined, "declined"),
        (wire::Disposition::Lapsed, "lapsed"),
        (wire::Disposition::Canceled, "canceled"),
        (wire::Disposition::DeadLettered, "dead-letter"),
    ];
    let keys = [
        "dTVQdQfLw",
        "dTVQdQfLx",
        "dTVQdQfLy",
        "dTVQdQfLz",
        "dTVQdQfMB",
        "dTVQdQfMC",
    ];
    let mut markdown = Vec::new();
    let mut quotes = Vec::new();
    for ((disposition, _), key) in cases.iter().zip(keys) {
        let text = format!("direction {key}");
        let target = steering_target(key, &text);
        markdown.push(target.markdown());
        quotes.push(wire::QuotePresentation::Resolved {
            target: target.to_string(),
            text,
            attribution: attributed(principal_fixture()),
            disposition: Some(*disposition),
            quotes: Vec::new(),
            media_quotes: Vec::new(),
        });
    }
    let spice_text = "direction from Spice";
    let spice_target = steering_target("dTVQdQfMD", spice_text);
    markdown.push(spice_target.markdown());
    quotes.push(wire::QuotePresentation::Resolved {
        target: spice_target.to_string(),
        text: spice_text.to_owned(),
        attribution: attributed(wire::QuoteAuthor::Directive {
            actor: wire::Actor::Spice,
        }),
        disposition: None,
        quotes: Vec::new(),
        media_quotes: Vec::new(),
    });
    let sources = BTreeMap::from([(
        spice_target.to_string(),
        quote_source("message-agent-spice-source", 3_000),
    )]);
    let rendered = render_received_with_quotes(
        &markdown.join(" "),
        &[],
        &quotes,
        &QuoteAgents::new(),
        &sources,
    );
    let html = rendered.html;

    for (_, key) in cases {
        assert!(
            html.contains(&format!("data-quote-disposition=\"{key}\"")),
            "{key}: {html}"
        );
    }
    assert_eq!(
        html.matches("class=\"message-ack-pill\"").count(),
        2,
        "ACK and NAK reuse the canonical acknowledgement pill"
    );
    let acknowledgement = html
        .find("data-quote-disposition=\"acked\"")
        .expect("acknowledged state renders");
    let acknowledged_prose = html
        .find("direction dTVQdQfLx")
        .expect("acknowledged prose renders");
    assert!(
        acknowledgement < acknowledged_prose,
        "the disposition leads its words: {html}"
    );
    for key in keys {
        assert!(
            html.contains(&format!(
                "direction {key}<span class=\"message-quote-task-tail\" \
                 data-quote-tasks=\"{key}\"></span></p>"
            )),
            "board provenance follows the quote's final word on its last line: {html}"
        );
    }
    assert!(html.contains(
        "<span class=\"message-quote-task-tail\" data-quote-tasks=\"dTVQdQfMD\"></span>"
    ));
    assert_eq!(html.matches("message-quote-age-slot").count(), 1);
    assert!(html.contains(
        "class=\"message-quote-name\" data-quote-author=\"directive\" \
         data-quote-actor=\"spice\" aria-label=\"Spice\">🌶️</span>"
    ));
    assert!(!html.contains(">Spice</span>"));
    assert!(!html.contains(">Operator</span>"));
    assert!(!html.contains("class=\"message-quote-disposition tag"));
}

#[test]
fn quote_levels_name_their_own_task_badges_at_every_occurrence() {
    let inner_text = "nested direction";
    let inner_target = steering_target("dTVQdQfLx", inner_text);
    let outer_text = format!("outer direction {}", inner_target.markdown());
    let outer_target = steering_target("dTVQdQfLw", &outer_text);
    let inner = wire::QuotePresentation::Resolved {
        target: inner_target.to_string(),
        text: inner_text.to_owned(),
        attribution: attributed(principal_fixture()),
        disposition: None,
        quotes: Vec::new(),
        media_quotes: Vec::new(),
    };
    let outer = wire::QuotePresentation::Resolved {
        target: outer_target.to_string(),
        text: outer_text,
        attribution: attributed(principal_fixture()),
        disposition: None,
        quotes: vec![inner],
        media_quotes: Vec::new(),
    };
    let sources = BTreeMap::from([
        (
            outer_target.to_string(),
            quote_source("message-agent-tasked-outer", 1_000),
        ),
        (
            inner_target.to_string(),
            quote_source("message-agent-tasked-inner", 2_000),
        ),
    ]);
    let rendered = render_received_with_quotes(
        &format!("{} {}", outer_target.markdown(), outer_target.markdown()),
        &[],
        &[outer.clone(), outer],
        &QuoteAgents::new(),
        &sources,
    );

    for (key, level) in [("dTVQdQfLw", "outer"), ("dTVQdQfLx", "nested")] {
        assert_eq!(
            rendered
                .html
                .matches(&format!("data-quote-tasks=\"{key}\""))
                .count(),
            2,
            "each {level} occurrence names its own task badges"
        );
    }
    assert_eq!(
        rendered
            .html
            .matches(
                "outer direction <span class=\"message-quote-task-tail\" \
                 data-quote-tasks=\"dTVQdQfLw\"></span></p>"
            )
            .count(),
        2,
        "a quote ending in a nested quote seats its provenance after its own last word: {}",
        rendered.html
    );
    assert!(!rendered.html.contains("message-quote-task-line"));
}

/// A card holds the Work of every direction it quotes, at any depth or as a
/// directive context, and no other entry of the board's origin index.
#[test]
fn a_cards_quote_tasks_follow_only_the_directions_it_quotes() {
    let inner_text = "nested direction";
    let inner = steering_target("dTVQdQfLx", inner_text);
    let outer_text = format!("outer direction {}", inner.markdown());
    let outer = steering_target("dTVQdQfLw", &outer_text);
    let context = steering_target("dTVQdQfLy", "acknowledged direction");
    let resolved =
        |target: &wire::QuoteTarget, text: &str, quotes| wire::QuotePresentation::Resolved {
            target: target.to_string(),
            text: text.to_owned(),
            attribution: attributed(principal_fixture()),
            disposition: None,
            quotes,
            media_quotes: Vec::new(),
        };
    let quotes = [resolved(
        &outer,
        &outer_text,
        vec![resolved(&inner, inner_text, Vec::new())],
    )];
    let directives = [MessageDirective {
        start: 0,
        end: 1,
        quote_target: context,
        media_quotes: Vec::new(),
        attribution: attributed(principal_fixture()),
        authored: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
        badge: None,
        review: None,
        line: None,
        delivery: None,
    }];
    let work = |handle: &str| vec![handle.parse::<wire::TaskHandle>().expect("fixture task")];
    let index = BTreeMap::from([
        ("dTVQdQfLw".to_owned(), work("OUTER-dTZQjKHhG")),
        ("dTVQdQfLx".to_owned(), work("INNER-dTZRdv3bF")),
        ("dTVQdQfLy".to_owned(), work("CONTEXT-dTZRfCQ43")),
        ("dTVQdQfLz".to_owned(), work("ELSEWHERE-dTZRjCd3s")),
    ]);

    let mut expected = index.clone();
    expected.remove("dTVQdQfLz");
    assert_eq!(
        referenced_quote_tasks(&quotes, &directives, &index),
        expected
    );
}

/// A card keeps the agents its quotes can render and nothing else: rendering
/// from that projection matches rendering from the whole roster, and a seat the
/// quotes never name changing occupancy leaves the card's projection, and so
/// its measurement, untouched.
#[test]
fn a_cards_quote_agents_follow_only_the_lanes_its_quotes_name() {
    let id = |suffix: &str| -> wire::CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131164{suffix}")
            .parse()
            .expect("fixture identity is canonical")
    };
    let (quoted, directing, unrelated, arriving) = (id("0a"), id("0b"), id("0c"), id("0d"));
    let (departed, successor) = (id("1a"), id("1b"));
    let inner_text = "direction from a lane";
    let inner_target = steering_target("dTVQdQfLw", inner_text);
    let outer_text = format!("before {} after", inner_target.markdown());
    let outer_target = tape_target(&outer_text);
    let quotes = vec![wire::QuotePresentation::Resolved {
        target: outer_target.to_string(),
        text: outer_text,
        attribution: attributed(wire::QuoteAuthor::Agent {
            lane: quoted,
            occupant: departed,
        }),
        disposition: None,
        quotes: vec![wire::QuotePresentation::Resolved {
            target: inner_target.to_string(),
            text: inner_text.to_owned(),
            attribution: attributed(wire::QuoteAuthor::Directive {
                actor: wire::Actor::Lane(directing),
            }),
            disposition: Some(wire::Disposition::Live),
            quotes: Vec::new(),
            media_quotes: Vec::new(),
        }],
        media_quotes: Vec::new(),
    }];
    let agent = |label: &str| QuoteAgent {
        accent: 2,
        label: label.to_owned(),
    };
    let roster = QuoteAgents::from_iter([
        ((quoted, Some(successor)), agent("quoted")),
        ((directing, Some(id("2b"))), agent("directing")),
        ((unrelated, Some(id("2c"))), agent("unrelated")),
    ]);
    let render = |agents: &QuoteAgents| {
        render_received_with_quotes(
            &outer_target.markdown(),
            &[],
            &quotes,
            agents,
            &BTreeMap::new(),
        )
        .html
    };

    let projection = referenced_quote_agents(&quotes, &[], &[], &roster);
    assert_eq!(render(&projection), render(&roster));

    let mut reseated = roster.clone();
    reseated.members.remove(&(unrelated, Some(id("2c"))));
    reseated
        .members
        .insert((unrelated, Some(id("3c"))), agent("unrelated"));
    reseated
        .members
        .insert((arriving, Some(id("3d"))), agent("arriving"));
    assert_eq!(
        referenced_quote_agents(&quotes, &[], &[], &reseated),
        projection
    );
}

#[test]
fn unavailable_quote_cards_keep_the_target_structural_without_showing_it_as_prose() {
    let target = steering_target("dTVQdQfLw", "missing");
    let target_text = target.to_string();
    let unavailable = [wire::QuotePresentation::Unavailable {
        target: target_text.clone(),
        reason: wire::QuoteUnavailable::DepthExceeded,
    }];
    let rendered = render_received_with_quotes(
        &target.markdown(),
        &[],
        &unavailable,
        &QuoteAgents::new(),
        &BTreeMap::new(),
    );

    assert!(!rendered.html.contains("message-quote-signoff"));
    assert!(!rendered.html.contains("<a "));
    assert!(!rendered.html.contains("data-quote-tasks"));
    assert!(
        rendered
            .html
            .contains("data-quote-reason=\"depth-exceeded\"")
    );
    assert!(rendered.html.contains("data-quote-target=\"spice://quote/"));
    assert!(!rendered.html.contains("<code>"));
    assert!(!rendered.html.contains("message-quote-body"));
    let ordinary = render_received("> copied words");
    assert!(ordinary.html.contains("<blockquote>"));
    assert!(ordinary.html.contains("copied words"));
    assert!(!ordinary.html.contains("message-quote-card"));
}

#[test]
fn room_card_words_expand_resolved_quotes_and_keep_unresolved_ones_structural() {
    let (kept, missing) = (
        steering_target("dTVQdQfLv", "kept words"),
        steering_target("dTVQdQfLw", "missing"),
    );
    let quotes = [
        wire::QuotePresentation::Resolved {
            target: kept.to_string(),
            text: "kept words".to_owned(),
            attribution: attributed(principal_fixture()),
            disposition: None,
            quotes: Vec::new(),
            media_quotes: Vec::new(),
        },
        wire::QuotePresentation::Unavailable {
            target: missing.to_string(),
            reason: wire::QuoteUnavailable::Unknown,
        },
    ];
    let markdown = format!(
        "Carry these.\n\n{}\n\n{}",
        kept.markdown(),
        missing.markdown()
    );
    let html = render_room_card(&markdown, &quotes, &QuoteAgents::new(), &BTreeMap::new()).html;

    assert!(html.contains("data-quote-state=\"resolved\""), "{html}");
    assert!(html.contains("kept words"), "{html}");
    assert!(html.contains("data-quote-reason=\"unknown\""), "{html}");
    assert!(!html.contains("href=\"spice://quote"), "{html}");
    assert!(!html.contains(">spice://quote"), "{html}");
    assert!(!html.contains("[quote]"), "{html}");
}

/// Hunk ownership is the renderer's durable layout contract: controls and
/// attribution lead every direct run, each quote owns its lead, and the
/// surrounding speaker restarts after every interruption. The browser mounts
/// the message's semantic badges through each direct hunk's signoff slot.
#[test]
fn attribution_hunks_restart_after_more_than_one_quote() {
    let first_quote = "> first quoted words";
    let second_quote = "> second quoted words";
    let markdown = format!(
        "first answer\n\n![first](a.png)\n\n{first_quote}\n\n\
         second answer\n\n![second](b.png)\n\n{second_quote}\n\nthird answer"
    );
    let directive = |key, quoted: &str, authored: &str| {
        let start = markdown.find(quoted).expect("fixture quote");
        MessageDirective {
            start: u32::try_from(start).expect("fixture offset fits"),
            end: u32::try_from(start + quoted.len()).expect("fixture offset fits"),
            quote_target: steering_target(key, quoted),
            media_quotes: Vec::new(),
            attribution: attributed(principal_fixture()),
            authored: authored.parse().expect("fixture instant"),
            badge: None,
            review: None,
            line: None,
            delivery: None,
        }
    };
    let directives = [
        directive("dTVQdQfLx", first_quote, "1970-01-01T00:00:01.000Z"),
        directive("dTVQdQfLy", second_quote, "1970-01-01T00:00:02.000Z"),
    ];
    let targets = directives
        .iter()
        .map(|directive| directive.quote_target.to_string())
        .collect::<Vec<_>>();
    let sources = directives
        .iter()
        .map(|directive| {
            (
                directive.quote_target.to_string(),
                QuoteSource::directive(directive),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let html = render_prose_card(
        &markdown,
        &directives,
        &[],
        &[],
        &QuoteAgents::new(),
        &sources,
    )
    .html;

    assert_eq!(html.matches("class=\"message-hunk\"").count(), 5, "{html}");
    let answer_controls = html
        .match_indices("data-message-controls=\"\"")
        .map(|(position, _)| position)
        .collect::<Vec<_>>();
    let answer_attributions = html
        .match_indices(SIGNOFF_SLOT)
        .map(|(position, _)| position)
        .collect::<Vec<_>>();
    assert_eq!(answer_controls.len(), 3, "{html}");
    assert_eq!(
        html.matches("data-message-quote-depth=\"0\"").count(),
        3,
        "{html}"
    );
    assert_eq!(
        html.matches("data-message-quote-depth=\"1\"").count(),
        2,
        "{html}"
    );
    assert_eq!(answer_attributions.len(), 3, "{html}");

    let first_words = html.find("first answer").expect("first answer");
    let first_quote_controls = html
        .find(&format!(
            "data-message-controls=\"{}\"",
            escape_html_attribute(&targets[0])
        ))
        .expect("first quote controls");
    let first_quote_words = html.find("first quoted words").expect("first quote words");
    let second_words = html.find("second answer").expect("second answer");
    let second_quote_controls = html
        .find(&format!(
            "data-message-controls=\"{}\"",
            escape_html_attribute(&targets[1])
        ))
        .expect("second quote controls");
    let second_quote_words = html
        .find("second quoted words")
        .expect("second quote words");
    let third_words = html.find("third answer").expect("third answer");
    assert!(
        answer_controls[0] < answer_attributions[0]
            && answer_attributions[0] < first_words
            && first_words < first_quote_controls
            && first_quote_controls < first_quote_words
            && first_quote_words < answer_controls[1]
            && answer_controls[1] < answer_attributions[1]
            && answer_attributions[1] < second_words
            && second_words < second_quote_controls
            && second_quote_controls < second_quote_words
            && second_quote_words < answer_controls[2]
            && answer_controls[2] < answer_attributions[2]
            && answer_attributions[2] < third_words,
        "{html}"
    );
}

/// A typed review replaces its prose delivery envelope with one finding card.
/// Its authenticated source, kind, and task lead the typed finding once, while
/// typed handling guidance closes the card; discarded envelope text cannot
/// become presentation structure.
#[test]
fn typed_review_replaces_its_delivery_envelope() {
    let review = "> APP-1kKRRsDRx\n>\n> > stale string finding\n>\n> stale envelope guidance";
    let plain = "> Keep one path.";
    let markdown = format!("{review}\n\n{plain}\n\nInternalized.");
    let directive = |key, quoted: &str, badge| {
        let start = markdown.find(quoted).expect("fixture quote");
        MessageDirective {
            start: u32::try_from(start).expect("fixture offset fits"),
            end: u32::try_from(start + quoted.len()).expect("fixture offset fits"),
            quote_target: steering_target(key, quoted),
            media_quotes: Vec::new(),
            attribution: attributed(wire::QuoteAuthor::Directive {
                actor: wire::Actor::Spice,
            }),
            authored: "1970-01-01T00:00:01.000Z".parse().expect("fixture instant"),
            badge,
            review: (badge == Some(wire::DirectiveBadge::Review)).then(|| wire::MessageReview {
                task: "APP-1kKRRsDRx".parse().expect("fixture task"),
                finding: "typed missing coverage\n\n> nested typed evidence".to_owned(),
                guidance: "Verbalize this guidance; follow-ups are already boarded.".to_owned(),
            }),
            line: None,
            delivery: None,
        }
    };
    let directives = [
        directive("dTVQdQfLx", review, Some(wire::DirectiveBadge::Review)),
        directive("dTVQdQfLy", plain, None),
    ];
    let sources = directives
        .iter()
        .map(|directive| {
            (
                directive.quote_target.to_string(),
                QuoteSource::directive(directive),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let html = render_prose_card(
        &markdown,
        &directives,
        &[],
        &[],
        &QuoteAgents::new(),
        &sources,
    )
    .html;

    assert_eq!(
        html.matches("class=\"message-review-pill\"").count(),
        1,
        "{html}"
    );
    let review_controls = format!("data-message-controls=\"{}\"", directives[0].quote_target,);
    assert_eq!(html.matches(&review_controls).count(), 1, "{html}");
    assert!(!html.contains("message-taste-pill"), "{html}");
    let review_quote = html.find("data-message-review=").expect("review quote");
    let first_hunk = html.find("class=\"message-hunk\"").expect("first hunk");
    let signoff = html.find("message-quote-signoff").expect("quote signoff");
    let badge = html.find("class=\"message-review-pill\"").expect("badge");
    let handle = html
        .find("class=\"message-task-chip\"")
        .expect("task handle");
    let finding = html.find("typed missing coverage").expect("typed finding");
    let guidance = html
        .find("Verbalize this guidance; follow-ups are already boarded.")
        .expect("typed guidance");
    let plain_words = html.find("Keep one path.").expect("plain quote");
    assert!(
        review_quote < first_hunk
            && first_hunk < signoff
            && signoff < badge
            && badge < handle
            && handle < finding
            && finding < guidance
            && guidance < plain_words,
        "{html}"
    );
    assert_eq!(html.matches("<blockquote").count(), 2, "{html}");
    assert!(!html.contains("stale string finding"), "{html}");
    assert!(!html.contains("stale envelope guidance"), "{html}");
}

/// An outgoing SYN names its exact sender, every addressed occupant in header
/// order, every lane reached as a whole, the operator, and its room context
/// without exposing protocol syntax. A departed recipient and a vacant lane
/// both keep their lane's name, and a lane gone from the roster falls back to
/// its address.
#[test]
fn a_sent_syn_names_every_destination_without_its_protocol_header() {
    let id = |suffix: &str| -> wire::CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131165{suffix}")
            .parse()
            .expect("fixture identity is canonical")
    };
    let (sender_lane, sender) = (id("0a"), id("1a"));
    let (seated_lane, seated) = (id("0b"), id("1b"));
    let (departed_lane, departed, successor) = (id("0c"), id("1c"), id("2c"));
    let (vacant_lane, vacated) = (id("0d"), id("1d"));
    let (absent_lane, absent) = (id("0e"), id("1e"));
    let invited_lane = id("0f");
    let recipient = |address: &str, lane, occupant| wire::MessageRecipient {
        address: address.parse().expect("fixture address parses"),
        lane,
        occupant,
    };
    let body = "> Please check the parser.";
    let markdown = format!("Asking for review.\n{body}");
    let start = markdown.find(body).expect("fixture body");
    let sends = [wire::MessageSend {
        start: u32::try_from(start).expect("fixture offset fits"),
        end: u32::try_from(markdown.len()).expect("fixture offset fits"),
        from: sender_lane,
        sender,
        recipients: vec![
            recipient("6Jv5", seated_lane, seated),
            recipient("6K26", departed_lane, departed),
            recipient("6KB3", vacant_lane, vacated),
            recipient("6KC4", absent_lane, absent),
        ],
        lanes: vec![invited_lane],
        operator: true,
        contexts: vec![
            "ROOMS-dVrD244Jd"
                .parse()
                .expect("fixture task handle parses"),
        ],
        source: wire::rooms::TextRange {
            start: 0,
            end: u64::try_from("Please check the parser.".len()).expect("fixture length fits"),
        },
        body: "Please check the parser.".to_owned(),
    }];
    let agent = |accent: usize, label: &str| QuoteAgent {
        accent,
        label: label.to_owned(),
    };
    let agents = QuoteAgents::from_iter([
        ((sender_lane, Some(sender)), agent(1, "sender-agent")),
        ((seated_lane, Some(seated)), agent(2, "seated-agent")),
        ((departed_lane, Some(successor)), agent(3, "departed-lane")),
        ((vacant_lane, None), agent(4, "vacant-lane")),
        ((invited_lane, None), agent(5, "invited-lane")),
    ]);
    let html = render_prose_card(
        &markdown,
        &[],
        &sends,
        &[],
        &referenced_quote_agents(&[], &[], &sends, &agents),
        &QuoteSources::new(),
    )
    .html;

    assert!(!html.contains("SYN 6Jv5"), "{html}");
    let (lead, recipients) = html
        .split_once("data-quote-direction=\"outgoing\"")
        .and_then(|(_, signoff)| signoff.split_once("message-quote-recipients"))
        .expect("outgoing signoff names its recipients");
    assert!(lead.contains(">sender-agent</span>"), "{html}");
    let words = recipients.find("Please check the parser.").expect("body");
    let named = recipients[..words]
        .split("message-quote-recipient-separator")
        .collect::<Vec<_>>();
    assert_eq!(named.len(), 7, "{html}");
    for (name, (occupant, label, state)) in named[..4].iter().zip([
        (seated, ">seated-agent</span>", "current"),
        (departed, ">departed-lane</span>", "historical"),
        (vacated, ">vacant-lane</span>", "historical"),
        (absent, ">6KC4</span>", "unresolved"),
    ]) {
        assert!(name.contains(&format!("data-quote-agent-occupant=\"{occupant}\"")));
        assert!(name.contains(label), "{name}");
        assert!(name.contains(&format!("data-quote-author-state=\"{state}\"")));
    }
    assert!(
        named[..3]
            .iter()
            .all(|name| !name.contains("data-quote-recipient-state")),
        "{html}"
    );
    assert!(
        named[3].contains("data-quote-recipient-state=\"unavailable\""),
        "{html}"
    );
    assert!(
        named[3].contains("aria-label=\"6KC4, recipient unavailable\""),
        "{html}"
    );
    assert!(
        named[4].contains(&format!("data-quote-recipient-lane=\"{invited_lane}\"")),
        "{html}"
    );
    assert!(named[4].contains(">invited-lane</span>"), "{html}");
    assert!(
        named[5].contains("data-quote-recipient=\"operator\""),
        "{html}"
    );
    assert!(named[5].contains(">Operator</span>"), "{html}");
    assert!(
        named[6].contains("data-task-handle=\"ROOMS-dVrD244Jd\""),
        "{html}"
    );
}

/// A card whose words are only a SYN it sent has no unquoted run to carry its
/// own lead, so the outgoing lead itself opens with the card's live age.
#[test]
fn a_card_that_only_sent_a_syn_leads_it_with_the_cards_age() {
    let (lane, sender) = (quote_fixture_id("0a"), quote_fixture_id("1a"));
    let markdown = "> Fixed in review.";
    let sends = [wire::MessageSend {
        start: 0,
        end: u32::try_from(markdown.len()).expect("fixture offset fits"),
        from: lane,
        sender,
        recipients: Vec::new(),
        lanes: Vec::new(),
        operator: true,
        contexts: Vec::new(),
        source: wire::rooms::TextRange {
            start: 0,
            end: u64::try_from("Fixed in review.".len()).expect("fixture length fits"),
        },
        body: "Fixed in review.".to_owned(),
    }];
    let card = quote_source("message-sent-card", 4_000);
    let html = render_prose_card_with_media_quotes(
        markdown,
        &[],
        &sends,
        &[],
        QuoteRenderContext::new(&QuoteAgents::new(), &QuoteSources::new(), &card, None),
        &[],
    )
    .html;

    let lead = html
        .split_once("data-quote-direction=\"outgoing\">")
        .and_then(|(_, signoff)| signoff.split_once("class=\"message-quote-name\""))
        .map(|(lead, _)| lead)
        .expect("outgoing signoff names its sender");
    assert!(lead.contains("data-quote-age-authored=\"4000\""), "{html}");
    assert!(
        lead.contains("data-quote-source=\"message-sent-card\""),
        "{html}"
    );
}

#[test]
fn retained_images_expose_their_exact_quote_target_without_replacing_preview_opening() {
    let media = wire::Media::from_base64("capture.png", "image/png", "iVBORw0KGgo=")
        .expect("fixture media");
    let target = wire::QuoteTarget::SteeringMedia {
        key: "dTVLR0qL0".to_owned(),
        ordinal: 0,
        digest: media.identity().expect("media identity"),
    };
    let media_quote = wire::MediaQuote {
        target: target.clone(),
        name: media.name.clone(),
    };

    let rendered = render_received_with_media_quotes(
        &media.markdown().expect("fixture markdown"),
        &[],
        &[],
        &QuoteAgents::new(),
        &QuoteSources::new(),
        std::slice::from_ref(&media_quote),
    );

    assert!(rendered.html.contains("data-image-preview=\"\""));
    assert!(rendered.html.contains("data-image-quote-action=\"\""));
    assert!(
        rendered
            .html
            .contains(&format!("data-image-quote-target=\"{target}\""))
    );
    assert!(
        rendered
            .html
            .contains("data-image-quote-name=\"capture.png\"")
    );
}
