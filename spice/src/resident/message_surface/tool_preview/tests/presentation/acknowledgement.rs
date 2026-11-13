use super::*;

fn quote_target(key: &str, text: &str) -> wire::QuoteTarget {
    wire::QuoteTarget::Steering {
        key: key.to_owned(),
        digest: wire::QuoteTarget::text_digest(text),
    }
}

fn published(millis: u16) -> wire::Instant {
    format!("2030-04-12T18:44:09.{millis:03}Z")
        .parse()
        .expect("fixture publication instant")
}

fn review_context() -> wire::MessageReview {
    wire::MessageReview {
        task: "APP-1kKRRsDRx".parse().expect("fixture review task"),
        finding: "preserve the finding".to_owned(),
        guidance: wire::REVIEW_FEEDBACK_GUIDANCE.to_owned(),
    }
}

fn directives(rows: &[(&str, &str)]) -> PresentationDirectives {
    PresentationDirectives::new(
        rows.iter()
            .map(|(key, text)| {
                (
                    key.parse::<steer::Key>().expect("fixture key parses"),
                    PresentationDirective {
                        actor: wire::Actor::TEST_PRINCIPAL,
                        attribution: operator_attribution(),
                        published: published(1),
                        fanout: None,
                        lane: lane(),
                        text: (*text).to_owned(),
                        authored: true,
                        taste: false,
                        review: None,
                        disposition: wire::Disposition::Live,
                        quote_target: quote_target(key, text),
                        media_quotes: Vec::new(),
                        retirement: None,
                    },
                )
            })
            .collect(),
    )
}

/// Retire one fixture directive at the header locus `event()` reads.
fn retire(directives: &mut PresentationDirectives, key: &str) {
    let key = key.parse::<steer::Key>().expect("fixture key parses");
    let directive = directives
        .directives
        .get_mut(&key)
        .expect("fixture directive");
    directive.disposition = wire::Disposition::Acked;
    directive.retirement = Some(wire::body::TranscriptLocus {
        source: None,
        member: 1,
        offset: 0,
        length: 1,
        event: 0,
        line: 1,
        column: 5,
    });
}

fn presented_text(
    text: &str,
    directives: &PresentationDirectives,
    event: PresentationEvent,
) -> Result<String, String> {
    present_steering_acknowledgements_with_directives(text, directives, event)
        .map(|presented| presented.text)
}

fn presented_acknowledgement(
    text: &str,
    directives: &PresentationDirectives,
    event: PresentationEvent,
) -> Option<wire::MessageAcknowledgement> {
    present_steering_acknowledgements(text, Some(directives), event)
        .expect("message presents")
        .acknowledgement
}
/// Badge of every directive context the answer presents, in text order.
fn presented_badges(
    text: &str,
    directives: &PresentationDirectives,
    event: PresentationEvent,
) -> Vec<Option<wire::DirectiveBadge>> {
    present_steering_acknowledgements(text, Some(directives), event)
        .expect("message presents")
        .directive_contexts
        .into_iter()
        .map(|context| context.badge)
        .collect()
}

#[test]
fn steering_acknowledgement_renders_key_context_and_agent_prose_once() {
    let directives = directives(&[("1kQnrzNZx", "Keep one tested, visible current path.")]);
    assert_eq!(
        presented_text(
            "ACK 1kQnrzNZx: Landed in the stream presentation path.",
            &directives,
            event(),
        )
        .expect("acknowledgment presents"),
        "> Keep one tested, visible current path.\n\nLanded in the stream presentation path.",
    );
}

#[test]
fn current_header_forms_hide_framing_and_capitalize_response_prose() {
    let directives = directives(&[("1kQnrzNZx", "Keep the current path.")]);
    for text in [
        "ACK 1kQnrzNZx: applied.",
        "ACK 1kQnrzNZx. applied.",
        "ACK 1kQnrzNZx applied.",
        "**ACK 1kQnrzNZx:** applied.",
        "**ACK** **1kQnrzNZx**:** applied.",
    ] {
        assert_eq!(
            presented_text(text, &directives, event()).expect("acknowledgment presents"),
            "> Keep the current path.\n\nApplied.",
            "wrong presentation for {text:?}",
        );
    }
}

#[test]
fn decorative_brackets_and_unicode_dashes_do_not_frame_presented_context() {
    let directives = directives(&[("1kQnrzNZx", "Keep the current path.")]);
    for text in [
        "[ACK 1kQnrzNZx] applied.",
        "[ACK 1kQnrzNZx] – applied.",
        "[ACK 1kQnrzNZx] — applied.",
        "— [ACK 1kQnrzNZx] — applied.",
    ] {
        assert_eq!(
            presented_text(text, &directives, event()).expect("acknowledgment presents"),
            "> Keep the current path.\n\nApplied.",
            "wrong presentation for {text:?}",
        );
    }
    assert_eq!(
        presented_text(
            "Context [kept] — ACK 1kQnrzNZx: applied.",
            &directives,
            event(),
        )
        .expect("acknowledgment after ordinary prose presents"),
        concat!(
            "Context [kept] —\n",
            "> Keep the current path.\n\n",
            "Applied."
        ),
    );
}

#[test]
fn framing_trim_preserves_response_punctuation_and_structural_ranges() {
    let directives = directives(&[
        ("1kQwPZtKx", "Apply the current path."),
        ("1kQwPZtLx", "Keep the safety check."),
    ]);
    let presented = present_steering_acknowledgements(
        concat!(
            "[ACK 1kQwPZtKx] — applied [within scope] – keep — these.\n",
            "[NAK 1kQwPZtLx] – unsafe [without review] — retained."
        ),
        Some(&directives),
        event(),
    )
    .expect("framed acknowledgments present");

    assert_eq!(
        presented.text,
        concat!(
            "> Apply the current path.\n\n",
            "Applied [within scope] – keep — these.\n",
            "> Keep the safety check.\n\n",
            "Unsafe [without review] — retained."
        ),
    );
    assert!(wire::valid_message_directives(
        &presented.text,
        &presented.directive_contexts
    ));
}

#[test]
fn presented_response_prose_normalizes_only_a_terminal_colon() {
    let directives = directives(&[("1kQnrzNZx", "Keep the current path.")]);
    for (body, presented) in [
        ("implementing:", "Implementing."),
        ("implementing:\n", "Implementing.\n"),
        ("implementing.", "Implementing."),
        ("implementing", "Implementing"),
    ] {
        let text = format!("ACK 1kQnrzNZx: {body}");
        assert_eq!(
            presented_text(&text, &directives, event()).expect("acknowledgment presents"),
            format!("> Keep the current path.\n\n{presented}"),
            "wrong terminal punctuation for {body:?}",
        );
    }
}

#[test]
fn answer_dispositions_require_authenticated_directive_evidence() {
    use wire::MessageAcknowledgement as Acknowledgement;
    let directives = directives(&[("1kQnrzNZx", "Keep the current path.")]);
    let acknowledgement = |text: &str| presented_acknowledgement(text, &directives, event());
    assert_eq!(
        acknowledgement("ACK 1kQnrzNZx: applied."),
        Some(Acknowledgement::Pending)
    );
    assert_eq!(
        acknowledgement("NAK 1kQnrzNZx: unsafe."),
        Some(Acknowledgement::Declined),
        "a refusal is an answer, marked apart from credit",
    );
    for text in [
        "ACK not-a-key: prose only.",
        "NAK not-a-key: prose only.",
        "Ordinary prose mentions ACK without a key.",
        "Agent prose writes [ACK] itself.",
    ] {
        assert_eq!(
            acknowledgement(text),
            None,
            "unearned disposition for {text:?}"
        );
    }
}

#[test]
fn taste_acknowledgement_earns_a_typed_badge_without_a_marker() {
    let key = "dRyzkQp1P".parse::<steer::Key>().expect("taste key parses");
    let directive_text = "Keep one tested path.";
    let directives = PresentationDirectives::new(BTreeMap::from([(
        key,
        PresentationDirective {
            actor: wire::Actor::TEST_PRINCIPAL,
            attribution: operator_attribution(),
            published: published(1),
            fanout: None,
            lane: lane(),
            text: directive_text.to_owned(),
            authored: true,
            taste: true,
            review: None,
            disposition: wire::Disposition::Acked,
            quote_target: quote_target("dRyzkQp1P", directive_text),
            media_quotes: Vec::new(),
            retirement: Some(wire::body::TranscriptLocus {
                source: None,
                member: 1,
                offset: 0,
                length: 1,
                event: 0,
                line: 1,
                column: 5,
            }),
        },
    )]));
    let text = "ACK dRyzkQp1P: Applied.";

    assert_eq!(
        presented_badges(text, &directives, event()),
        [Some(wire::DirectiveBadge::Taste)],
        "the quoted reminder carries its kind without any marker",
    );
    assert_eq!(
        presented_acknowledgement(text, &directives, event()),
        Some(wire::MessageAcknowledgement::Acknowledged),
    );
    assert_eq!(
        presented_text(text, &directives, event()).expect("taste acknowledgement presents"),
        "> Keep one tested path.\n\nApplied.",
    );

    let mut literal = directives;
    for directive in literal.directives.values_mut() {
        directive.taste = false;
        directive.text = "[TASTE] Keep one tested path.".to_owned();
    }
    assert_eq!(
        presented_badges(text, &literal, event()),
        [None],
        "a literal marker in ordinary direction earns no badge",
    );
}

#[test]
fn review_feedback_context_carries_its_badge_without_a_marker() {
    let source = "00000000-0000-4000-8000-000000000002"
        .parse()
        .expect("fixture source");
    let key = "1kQnrzNZx"
        .parse::<steer::Key>()
        .expect("fixture key parses");
    let directives = PresentationDirectives::new(BTreeMap::from([(
        key,
        PresentationDirective {
            actor: wire::Actor::TEST_PRINCIPAL,
            attribution: operator_attribution(),
            published: published(1),
            fanout: None,
            lane: lane(),
            text: concat!(
                "APP-1kKRRsDRx\n\n",
                "> preserve the finding\n\n",
                "Keep working the current task."
            )
            .to_owned(),
            authored: true,
            taste: false,
            review: Some(review_context()),
            disposition: wire::Disposition::Acked,
            quote_target: quote_target(
                "1kQnrzNZx",
                concat!(
                    "APP-1kKRRsDRx\n\n",
                    "> preserve the finding\n\n",
                    "Keep working the current task."
                ),
            ),
            media_quotes: Vec::new(),
            retirement: Some(wire::body::TranscriptLocus {
                source: Some(source),
                member: 1,
                offset: 0,
                length: 1,
                event: 0,
                line: 1,
                column: 5,
            }),
        },
    )]));
    let text = "ACK 1kQnrzNZx: Internalized.";
    let mut original = event();
    original.source = Some(source);
    assert_eq!(
        presented_badges(text, &directives, original),
        [Some(wire::DirectiveBadge::Review)],
    );
    let presentation = present_steering_acknowledgements(text, Some(&directives), original)
        .expect("review message presents");
    assert_eq!(
        presentation.directive_contexts[0].review,
        Some(review_context()),
    );
    assert_eq!(
        presented_acknowledgement(text, &directives, original),
        Some(wire::MessageAcknowledgement::Acknowledged),
    );
    let mut other_source = original;
    other_source.source = Some(
        "00000000-0000-4000-8000-000000000003"
            .parse()
            .expect("other fixture source"),
    );
    assert_eq!(
        presented_badges(text, &directives, other_source),
        [Some(wire::DirectiveBadge::Review)],
        "the badge belongs to the sent feedback, not to whichever answer quotes it"
    );
}

#[test]
fn distributed_acknowledgements_render_every_context_without_repeating_the_message() {
    let text = concat!(
        "ACK 1kQwPZtKx: The current completion path is singular and visible.\n\n",
        "ACK 1kQwPZtLx: Historical records remain evidence only."
    );
    let directives = directives(&[
        ("1kQwPZtKx", "Keep one current completion path."),
        ("1kQwPZtLx", "Do not execute historical records."),
    ]);

    let rendered =
        presented_text(text, &directives, event()).expect("distributed acknowledgments present");
    assert_eq!(rendered.matches("The current completion path").count(), 1);
    assert_eq!(rendered.matches("Historical records").count(), 1);
    assert_eq!(rendered.matches("ACK 1kQwPZt").count(), 0);
    assert!(!rendered.contains("[ACK"));
    assert!(
        rendered.contains("> Keep one current completion path.\n\nThe current completion path")
    );
    assert!(rendered.contains("> Do not execute historical records.\n\nHistorical records"));
    assert_eq!(
        presented_acknowledgement(text, &directives, event()),
        Some(wire::MessageAcknowledgement::Pending),
    );
}

/// One header answering several directives quotes each of them, then opens its
/// one response with one marker beside the speaking agent.
#[test]
fn shared_acknowledgement_prose_renders_once_after_every_context() {
    let directives = directives(&[
        ("1kQwPZtKx", "Keep one current completion path."),
        ("1kQwPZtLx", "Do not execute historical records."),
    ]);

    assert_eq!(
        presented_text(
            "ACK 1kQwPZtKx 1kQwPZtLx: One response covers both directions.",
            &directives,
            event(),
        )
        .expect("shared acknowledgment presents"),
        concat!(
            "> Keep one current completion path.\n\n",
            "> Do not execute historical records.\n\n",
            "One response covers both directions."
        ),
    );
}

#[test]
fn a_partially_superseded_header_presents_each_key_only_on_its_winning_event() {
    let mut directives = directives(&[
        ("1kQwPZtKx", "Keep the current path."),
        ("1kQwPZtLx", "Keep the independent path."),
    ]);
    retire(&mut directives, "1kQwPZtKx");
    retire(&mut directives, "1kQwPZtLx");
    let first_key = "1kQwPZtKx"
        .parse::<steer::Key>()
        .expect("fixture key parses");
    let first_directive = directives
        .directives
        .get_mut(&first_key)
        .expect("fixture directive");
    first_directive.disposition = wire::Disposition::Declined;
    first_directive
        .retirement
        .as_mut()
        .expect("fixture retirement")
        .event = 1;

    let first = PresentationEvent {
        superseded_answers: std::slice::from_ref(&first_key),
        ..event()
    };
    assert_eq!(
        presented_text(
            "ACK 1kQwPZtKx 1kQwPZtLx: both accepted.",
            &directives,
            first,
        )
        .expect("winning part of shared acknowledgment presents"),
        "> Keep the independent path.\n\n1kQwPZtKx: both accepted.",
    );
    assert_eq!(
        presented_acknowledgement(
            "ACK 1kQwPZtKx 1kQwPZtLx: both accepted.",
            &directives,
            first,
        ),
        Some(wire::MessageAcknowledgement::Acknowledged),
    );

    let later = PresentationEvent {
        ordinal: 1,
        ..event()
    };
    assert_eq!(
        presented_text("NAK 1kQwPZtKx: declined in intent.", &directives, later)
            .expect("reanswering event presents"),
        "> Keep the current path.\n\nDeclined in intent.",
    );
    assert_eq!(
        presented_acknowledgement("NAK 1kQwPZtKx: declined in intent.", &directives, later),
        Some(wire::MessageAcknowledgement::Declined),
    );
}

#[test]
fn non_key_text_after_a_recognized_header_remains_response_prose() {
    let directives = directives(&[("1kQwPZtKx", "Keep the recognized direction visible.")]);
    for marker in ["ACK", "NAK"] {
        let text = format!(
            "{marker} 1kQwPZtKx nope 1kQwPZtMx: Shared prose remains attached to the attempt."
        );
        let expected = concat!(
            "> Keep the recognized direction visible.\n\n",
            "Nope 1kQwPZtMx: Shared prose remains attached to the attempt.",
        );

        let rendered =
            presented_text(&text, &directives, event()).expect("mixed acknowledgment presents");
        assert_eq!(rendered, expected);
        assert_eq!(rendered.matches("1kQwPZtKx").count(), 0);
        assert!(!rendered.starts_with(marker));
        assert_eq!(rendered.matches("Nope").count(), 1);
        assert_eq!(rendered.matches("1kQwPZtMx").count(), 1);
        assert_eq!(rendered.matches("Shared prose").count(), 1);
    }
}

#[test]
fn mixed_acknowledgement_dispositions_keep_their_own_keys_and_contexts() {
    let directives = directives(&[
        ("1kQwPZtKx", "Apply the current path."),
        ("1kQwPZtLx", "Remove the safety check."),
    ]);
    assert_eq!(
        presented_text(
            "[ACK 1kQwPZtKx] — Applied.\n[NAK 1kQwPZtLx] – Unsafe.",
            &directives,
            event(),
        )
        .expect("mixed acknowledgments present"),
        concat!(
            "> Apply the current path.\n\nApplied.\n",
            "> Remove the safety check.\n\nUnsafe."
        ),
    );
    assert_eq!(
        presented_acknowledgement(
            "[ACK 1kQwPZtKx] — Applied.\n[NAK 1kQwPZtLx] – Unsafe.",
            &directives,
            event(),
        ),
        Some(wire::MessageAcknowledgement::Declined),
    );
}

#[test]
fn inline_packets_render_each_context_and_body_in_source_order() {
    let directives = directives(&[
        ("1kQwPZtKx", "Keep the first path."),
        ("1kQwPZtLx", "Keep the second path."),
    ]);

    assert_eq!(
        presented_text(
            "ACK 1kQwPZtKx: First applied. ACK 1kQwPZtLx: Second applied.",
            &directives,
            event(),
        )
        .expect("inline acknowledgments present"),
        concat!(
            "> Keep the first path.\n\n",
            "First applied.\n",
            "> Keep the second path.\n\n",
            "Second applied."
        ),
    );
}

/// Quoted context is structurally separated from list bodies without injected
/// protocol prose.
#[test]
fn directives_quote_verbatim_before_response_bodies() {
    let mut directives = directives(&[(
        "1kQwPZtKx",
        "> The original assertion remains visible.\n\napproved",
    )]);
    let quoted = "> > The original assertion remains visible.\n>\n> approved\n\n";
    assert_eq!(
        presented_text("ACK 1kQwPZtKx: Applied.", &directives, event())
            .expect("nested acknowledgment presents"),
        format!("{quoted}Applied."),
    );
    retire(&mut directives, "1kQwPZtKx");
    assert_eq!(
        presented_text("ACK 1kQwPZtKx: Applied.", &directives, event())
            .expect("retired nested acknowledgment presents"),
        format!("{quoted}Applied."),
    );
    for (response, presented) in [
        ("ACK 1kQwPZtKx: - first\n- second", "- first\n- second"),
        ("ACK 1kQwPZtKx:\n\n1. first", "1. first"),
        ("ACK 1kQwPZtKx:", ""),
    ] {
        assert_eq!(
            presented_text(response, &directives, event()).expect("acknowledgment presents"),
            format!("{quoted}{presented}"),
            "{response:?}",
        );
    }
}

#[test]
fn quoted_marker_history_stays_literal() {
    let text = "ACK 1kQwPZtKx: closing the drawer.";
    let mut quoted = directives(&[(
        "1kQwPZtKx",
        concat!(
            "> > [ACK] Button should close the drawer\n",
            "> > > [NAK] Keep the old drawer\n",
            "> [ACK PENDING] Waiting then\n",
            "Ship it",
        ),
    )]);

    let pending = present_steering_acknowledgements(text, Some(&quoted), event())
        .expect("quoted history presents");
    assert!(
        pending
            .text
            .contains("[ACK] Button should close the drawer")
    );
    assert!(pending.text.contains("[NAK] Keep the old drawer"));
    assert!(pending.text.contains("[ACK PENDING] Waiting then"));
    assert_eq!(
        pending.acknowledgement,
        Some(wire::MessageAcknowledgement::Pending),
    );

    retire(&mut quoted, "1kQwPZtKx");
    let acknowledged = present_steering_acknowledgements(text, Some(&quoted), event())
        .expect("retired quoted history presents");
    assert_eq!(acknowledged.text, pending.text);
    assert_eq!(
        acknowledged.acknowledgement,
        Some(wire::MessageAcknowledgement::Acknowledged),
    );
}

#[test]
fn unknown_key_preserves_the_original_message_across_replay() {
    let text = "[ACK 1kQwPZtKx] — No directive owns this key.";
    let directives = PresentationDirectives::default();
    let first = presented_text(text, &directives, event()).expect("unknown key remains visible");
    let replayed =
        presented_text(text, &directives, event()).expect("unknown key replay remains visible");

    assert_eq!(first, text);
    assert_eq!(replayed, first);
}

#[test]
fn system_acknowledgment_badge_reaches_app_without_speech_eligibility() {
    let key = "dRyzkGX8b"
        .parse::<steer::Key>()
        .expect("current system key parses");
    let directives = PresentationDirectives::new(BTreeMap::from([(
        key,
        PresentationDirective {
            actor: wire::Actor::TEST_PRINCIPAL,
            attribution: operator_attribution(),
            published: published(1),
            fanout: None,
            lane: lane(),
            text: "Accepted lane renewal context.".to_owned(),
            authored: false,
            taste: false,
            review: None,
            disposition: wire::Disposition::Acked,
            quote_target: quote_target("dRyzkGX8b", "Accepted lane renewal context."),
            media_quotes: Vec::new(),
            retirement: Some(wire::body::TranscriptLocus {
                source: None,
                member: 1,
                offset: 0,
                length: 1,
                event: 0,
                line: 1,
                column: 5,
            }),
        },
    )]));
    let prose = listen::Event::Prose(listen::Prose {
        role: listen::ProseRole::Assistant,
        text: "ACK dRyzkGX8b: Accepted revision 44.".to_owned(),
        phase: None,
        media: Vec::new(),
        external_media: Vec::new(),
        locus: event().locus,
    });
    let publisher = Messages::default();

    publish_agent_event(
        &publisher,
        AgentEvent {
            lane: lane(),
            author: lane(),
            source: Some(TEST_TAPE_SOURCE.parse().expect("fixture source")),
            ordinal: 0,
            decoded: &prose,
            authored: None,
            observed: None,
            observations: &MessageObservations::default(),
            prose_origin: None,
            suppress_answer: false,
            superseded_answers: &[],
            invitation: None,
        },
        &mut CallPreviews::default(),
        Some(&directives),
    )
    .expect("system acknowledgment publishes");

    let messages = publisher.0.lock().expect("message lock");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "Accepted revision 44.");
    assert_eq!(
        messages[0].acknowledgement,
        Some(wire::MessageAcknowledgement::Acknowledged),
    );
    assert!(messages[0].automatic_speech.is_none());
}

#[test]
fn a_spoken_acknowledgement_reads_pending_until_the_journal_retires_its_key() {
    let key = "1kQnrzNZx"
        .parse::<steer::Key>()
        .expect("fixture key parses");
    let text = "ACK 1kQnrzNZx: Applied:";
    let mut directives = directives(&[("1kQnrzNZx", "Keep the current path.")]);
    assert_eq!(
        presented_text(text, &directives, event()).expect("pending acknowledgment presents"),
        "> Keep the current path.\n\nApplied.",
    );
    let locus = wire::MessageLocus {
        member: 1,
        offset: 0,
        length: 1,
        event: 0,
    };
    let registered = pending::PendingPresentation::of(text, Some(&directives), event(), locus)
        .expect("a pending key registers its card");
    assert_eq!(registered.keys(), &BTreeSet::from([key]));
    let publisher = Messages::default();
    let mut state = MessageState::default();
    let record = capture_record(
        br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"ACK 1kQnrzNZx: Applied:"}]}}"#,
        0,
    );
    publish_agent_messages(&publisher, &record, &mut state, Some(&directives))
        .expect("pending acknowledgment publishes");
    assert_eq!(
        state.pending.len(),
        1,
        "the published card waits on its key"
    );
    let messages = publisher.0.lock().expect("message lock");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "> Keep the current path.\n\nApplied.");
    let [context] = messages[0].directive_contexts.as_slice() else {
        panic!("one directive context should be quoteable")
    };
    assert_eq!(
        &messages[0].text[context.start as usize..context.end as usize],
        "> Keep the current path."
    );
    assert_eq!(
        context.quote_target,
        quote_target("1kQnrzNZx", "Keep the current path.")
    );
    assert_eq!(
        messages[0].acknowledgement,
        Some(wire::MessageAcknowledgement::Pending),
    );
    drop(messages);

    retire(&mut directives, "1kQnrzNZx");
    assert_eq!(
        presented_text(text, &directives, event()).expect("retired acknowledgment presents"),
        "> Keep the current path.\n\nApplied.",
    );
    assert_eq!(
        presented_acknowledgement(text, &directives, event()),
        Some(wire::MessageAcknowledgement::Acknowledged),
    );
    assert!(
        pending::PendingPresentation::of(text, Some(&directives), event(), locus).is_none(),
        "a retired key leaves nothing to settle"
    );
}

#[test]
fn a_narrated_messages_acknowledging_intent_presents_no_card() {
    let directives = directives(&[("1kQnrzNZx", "Keep the current path.")]);
    let publisher = Messages::default();
    let mut state = MessageState::default();
    let narrated = capture_record(
        br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Keeping the path and checking the log."},{"type":"toolCall","id":"call-1","name":"shell","arguments":{"command":"git status"},"intent":"ACK 1kQnrzNZx: Applied, checking the log"},{"type":"toolCall","id":"call-2","name":"read","arguments":{"path":"a"},"intent":"Reading the next file"}],"stopReason":"toolUse"}}"#,
        0,
    );
    publish_agent_messages(&publisher, &narrated, &mut state, Some(&directives))
        .expect("narrated message publishes");

    let messages = publisher.0.lock().expect("message lock");
    let cards = messages
        .iter()
        .filter(|message| message.activity == wire::AgentActivityKind::Prose)
        .collect::<Vec<_>>();
    assert!(
        cards.is_empty(),
        "a tool intent presents no card: {cards:?}"
    );
    assert!(
        state.pending.is_empty(),
        "an intent opens no pending acknowledgement"
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.text.contains("Reading the next file")),
        "an intent without a header stays tool activity"
    );
}

#[test]
fn every_allocated_key_a_header_names_presents_its_directive_without_earning_credit() {
    let other_lane = "00000000-0000-4000-8000-000000000009"
        .parse()
        .expect("other lane");
    let other_source = "00000000-0000-4000-8000-000000000003"
        .parse()
        .expect("other source");
    let retired_at = wire::body::TranscriptLocus {
        source: Some(other_source),
        member: 1,
        offset: 0,
        length: 1,
        event: 0,
        line: 1,
        column: 5,
    };
    let mut directives = directives(&[
        ("1kQwPZtKx", "Another lane's live direction."),
        ("1kQwPZtLx", "Another lane's settled direction."),
        ("1kQwPZtMx", "APP-1kKRRsDRx\n\nKeep working."),
        ("1kQwPZtNx", "Refused earlier on this lane."),
    ]);
    for (key, lane, disposition, review) in [
        ("1kQwPZtKx", other_lane, wire::Disposition::Live, false),
        ("1kQwPZtLx", other_lane, wire::Disposition::Acked, false),
        ("1kQwPZtMx", lane(), wire::Disposition::Acked, true),
        ("1kQwPZtNx", lane(), wire::Disposition::Declined, false),
    ] {
        let directive = directives
            .directives
            .get_mut(&key.parse().expect("fixture key"))
            .expect("fixture directive");
        directive.lane = lane;
        directive.disposition = disposition;
        directive.review = review.then(review_context);
        directive.retirement = (disposition != wire::Disposition::Live).then_some(retired_at);
    }
    for (text, presented) in [
        (
            "ACK 1kQwPZtKx: noted.",
            "> Another lane's live direction.\n\nNoted.",
        ),
        (
            "ACK 1kQwPZtLx is done.",
            "> Another lane's settled direction.\n\nIs done.",
        ),
        (
            "ACK 1kQwPZtMx: again.",
            "> APP-1kKRRsDRx\n>\n> Keep working.\n\nAgain.",
        ),
        (
            "ACK 1kQwPZtNx: after all.",
            "> Refused earlier on this lane.\n\nAfter all.",
        ),
        (
            "ACK 1kQwPZtKx 1kQwPZtZx: mixed.",
            "> Another lane's live direction.\n\n1kQwPZtZx: mixed.",
        ),
    ] {
        assert_eq!(
            presented_text(text, &directives, event()).expect("referenced key presents"),
            presented,
            "wrong presentation for {text:?}",
        );
        let review = text.contains("1kQwPZtMx");
        assert_eq!(
            presented_badges(text, &directives, event()),
            [review.then_some(wire::DirectiveBadge::Review)],
            "a quoted directive carries its own kind: {text:?}",
        );
        let locus = wire::MessageLocus {
            member: 1,
            offset: 0,
            length: 1,
            event: 0,
        };
        assert!(
            pending::PendingPresentation::of(text, Some(&directives), event(), locus).is_none(),
            "a reference never waits on another answer: {text:?}",
        );
    }
}

#[test]
fn repeated_acknowledgement_keeps_context_without_reearning_speech() {
    let key = "1kQwPZtKx"
        .parse::<steer::Key>()
        .expect("fixture key parses");
    let retirement = wire::body::TranscriptLocus {
        source: None,
        member: 1,
        offset: 0,
        length: 1,
        event: 0,
        line: 1,
        column: 5,
    };
    let directives = PresentationDirectives::new(BTreeMap::from([(
        key,
        PresentationDirective {
            actor: wire::Actor::TEST_PRINCIPAL,
            attribution: operator_attribution(),
            published: published(1),
            fanout: None,
            lane: lane(),
            text: "Keep the original response context.".to_owned(),
            authored: true,
            taste: false,
            review: None,
            disposition: wire::Disposition::Acked,
            quote_target: quote_target("1kQwPZtKx", "Keep the original response context."),
            media_quotes: Vec::new(),
            retirement: Some(retirement),
        },
    )]));
    let text = "ACK 1kQwPZtKx: Applied.";

    assert_eq!(
        presented_text(text, &directives, event()).expect("retirement locus presents"),
        "> Keep the original response context.\n\nApplied.",
    );
    let mut later = event();
    later.locus = listen::Locus::new(1, 8, 1);
    assert_eq!(
        presented_text(text, &directives, later).expect("repeated acknowledgement presents"),
        "> Keep the original response context.\n\nApplied.",
    );
    assert_eq!(
        presented_acknowledgement(text, &directives, later),
        Some(wire::MessageAcknowledgement::Acknowledged),
    );
    assert!(
        automatic_speech::classify(
            &listen::Prose {
                role: listen::ProseRole::Assistant,
                text: text.to_owned(),
                phase: None,
                media: Vec::new(),
                external_media: Vec::new(),
                locus: later.locus,
            },
            Some(&directives),
            later,
        )
        .is_none(),
        "displaying the retired context does not authorize automatic speech",
    );
}

#[test]
fn current_keys_present_every_known_context() {
    let settled = "1kR5x8yRx"
        .parse::<steer::Key>()
        .expect("settled fixture key parses");
    let current = "1kR5xJDDx"
        .parse::<steer::Key>()
        .expect("current fixture key parses");
    let retirement = |offset, column| wire::body::TranscriptLocus {
        source: None,
        member: 1,
        offset,
        length: 1,
        event: 0,
        line: 1,
        column,
    };
    let directives = PresentationDirectives::new(BTreeMap::from([
        (
            settled,
            PresentationDirective {
                actor: wire::Actor::TEST_PRINCIPAL,
                attribution: operator_attribution(),
                published: published(1),
                fanout: None,
                lane: lane(),
                text: "Continue".to_owned(),
                authored: true,
                taste: false,
                review: None,
                disposition: wire::Disposition::Acked,
                quote_target: quote_target("1kR5x8yRx", "Continue"),
                media_quotes: Vec::new(),
                retirement: Some(retirement(8, 5)),
            },
        ),
        (
            current,
            PresentationDirective {
                actor: wire::Actor::TEST_PRINCIPAL,
                attribution: operator_attribution(),
                published: published(2),
                fanout: None,
                lane: lane(),
                text: "Keep one deterministic path.".to_owned(),
                authored: true,
                taste: true,
                review: None,
                disposition: wire::Disposition::Acked,
                quote_target: quote_target("1kR5xJDDx", "Keep one deterministic path."),
                media_quotes: Vec::new(),
                retirement: Some(retirement(0, 15)),
            },
        ),
    ]));
    let text = concat!(
        "ACK 1kR5x8yRx 1kR5xJDDx: Continuing with the explicit two-context contract ",
        "only."
    );

    let presented = present_steering_acknowledgements(text, Some(&directives), event())
        .expect("current keys present");
    assert_eq!(
        presented.text,
        concat!(
            "> Continue\n\n",
            "> Keep one deterministic path.\n\n",
            "Continuing with the explicit two-context contract only."
        ),
    );
    assert_eq!(
        presented
            .directive_contexts
            .iter()
            .map(|context| context.authored)
            .collect::<Vec<_>>(),
        [published(1), published(2)],
        "each context keeps its directive's publication instant"
    );
    assert_eq!(
        presented
            .directive_contexts
            .iter()
            .map(|context| context.badge)
            .collect::<Vec<_>>(),
        [None, Some(wire::DirectiveBadge::Taste)],
    );
    assert_eq!(
        presented.acknowledgement,
        Some(wire::MessageAcknowledgement::Acknowledged),
    );
}

#[test]
fn inline_current_control_and_direction_share_classification() {
    let system = "dRyzkGX8b"
        .parse::<steer::Key>()
        .expect("current system key parses");
    let direction = "1kQwPZtKx"
        .parse::<steer::Key>()
        .expect("current direction key parses");
    let retirement = |column| wire::body::TranscriptLocus {
        source: None,
        member: 1,
        offset: 0,
        length: 1,
        event: 0,
        line: 1,
        column,
    };
    let directives = PresentationDirectives::new(BTreeMap::from([
        (
            system,
            PresentationDirective {
                actor: wire::Actor::TEST_PRINCIPAL,
                attribution: operator_attribution(),
                published: published(1),
                fanout: None,
                lane: lane(),
                text: "Accepted lane renewal context.".to_owned(),
                authored: false,
                taste: false,
                review: None,
                disposition: wire::Disposition::Acked,
                quote_target: quote_target("dRyzkGX8b", "Accepted lane renewal context."),
                media_quotes: Vec::new(),
                retirement: Some(retirement(13)),
            },
        ),
        (
            direction,
            PresentationDirective {
                actor: wire::Actor::TEST_PRINCIPAL,
                attribution: operator_attribution(),
                published: published(2),
                fanout: None,
                lane: lane(),
                text: "Keep direction readable.".to_owned(),
                authored: true,
                taste: false,
                review: None,
                disposition: wire::Disposition::Acked,
                quote_target: quote_target("1kQwPZtKx", "Keep direction readable."),
                media_quotes: Vec::new(),
                retirement: Some(retirement(23)),
            },
        ),
    ]));
    let text = "Before. ACK dRyzkGX8b 1kQwPZtKx: applied. After.";

    assert_eq!(
        presented_text(text, &directives, event()).expect("inline acknowledgment presents"),
        "Before.\n> Keep direction readable.\n\nApplied. After.",
    );
    assert_eq!(
        presented_acknowledgement(text, &directives, event()),
        Some(wire::MessageAcknowledgement::Acknowledged),
    );
}

#[test]
fn acknowledged_context_survives_transcript_source_replacement() {
    let expected_source = "00000000-0000-4000-8000-000000000002"
        .parse()
        .expect("fixture source");
    let key = "1kQwPZtKx"
        .parse::<steer::Key>()
        .expect("fixture key parses");
    let directives = PresentationDirectives::new(BTreeMap::from([(
        key,
        PresentationDirective {
            actor: wire::Actor::TEST_PRINCIPAL,
            attribution: operator_attribution(),
            published: published(1),
            fanout: None,
            lane: lane(),
            text: "Keep the original response context.".to_owned(),
            authored: true,
            taste: false,
            review: None,
            disposition: wire::Disposition::Acked,
            quote_target: quote_target("1kQwPZtKx", "Keep the original response context."),
            media_quotes: Vec::new(),
            retirement: Some(wire::body::TranscriptLocus {
                source: Some(expected_source),
                member: 1,
                offset: 0,
                length: 1,
                event: 0,
                line: 1,
                column: 5,
            }),
        },
    )]));
    let text = "ACK 1kQwPZtKx: Applied.";
    let expected = "> Keep the original response context.\n\nApplied.";

    let mut original = event();
    original.source = Some(expected_source);
    assert_eq!(
        presented_text(text, &directives, original).expect("original source presents"),
        expected,
    );

    let mut replacement = event();
    replacement.source = Some(
        "00000000-0000-4000-8000-000000000003"
            .parse()
            .expect("replacement source"),
    );
    assert_eq!(
        presented_text(text, &directives, replacement).expect("replacement source presents"),
        expected,
    );
    assert_eq!(
        presented_acknowledgement(text, &directives, replacement),
        Some(wire::MessageAcknowledgement::Acknowledged),
    );
    assert!(
        automatic_speech::classify(
            &listen::Prose {
                role: listen::ProseRole::Assistant,
                text: text.to_owned(),
                phase: None,
                media: Vec::new(),
                external_media: Vec::new(),
                locus: replacement.locus,
            },
            Some(&directives),
            replacement,
        )
        .is_none(),
        "a later source may present context but cannot re-earn acknowledgement speech",
    );
    assert_eq!(
        presented_text(text, &directives, event()).expect("source-less history presents"),
        expected,
    );
}

#[test]
fn repeated_lane_seal_acknowledgements_render_without_fabricating_a_directive() {
    let seal = steer::Key::for_identity(lane());
    let text = format!("ACK {seal}: Continue.");
    let expected = "Continue.";
    let directives = PresentationDirectives::default();

    let first = event();
    let mut later = event();
    later.source = Some(
        "00000000-0000-4000-8000-000000000003"
            .parse()
            .expect("later transcript source"),
    );
    later.locus = listen::Locus::new(1, 8, 1);

    for acknowledged in [first, later] {
        assert_eq!(
            presented_text(&text, &directives, acknowledged,)
                .expect("lane seal acknowledgement presents"),
            expected,
        );
        assert_eq!(
            presented_acknowledgement(&text, &directives, acknowledged),
            Some(wire::MessageAcknowledgement::Acknowledged),
        );
        assert!(
            automatic_speech::classify(
                &listen::Prose {
                    role: listen::ProseRole::Assistant,
                    text: text.clone(),
                    phase: None,
                    media: Vec::new(),
                    external_media: Vec::new(),
                    locus: acknowledged.locus,
                },
                Some(&directives),
                acknowledged,
            )
            .is_none(),
            "the lane seal is presentation identity, not retirement evidence",
        );
    }
}
