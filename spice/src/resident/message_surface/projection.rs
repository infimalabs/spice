use super::*;
#[cfg(test)]
pub(in crate::resident) const TEST_TAPE_SOURCE: &str = "018f82d4-6f59-7d06-92e3-bce9131163e6";

#[cfg(test)]
pub(in crate::resident) fn publish_agent_messages(
    publisher: &impl AgentMessagePublisher,
    record: &lane::capture::CaptureRecord,
    state: &mut MessageState,
    steering: Option<&PresentationDirectives>,
) -> CommandResult<()> {
    publish_agent_messages_in(
        publisher,
        record,
        state,
        steering,
        Some(
            TEST_TAPE_SOURCE
                .parse()
                .expect("fixture tape source is canonical"),
        ),
        None,
        None,
    )
}

pub(super) fn publish_agent_messages_in(
    publisher: &impl AgentMessagePublisher,
    record: &lane::capture::CaptureRecord,
    state: &mut MessageState,
    steering: Option<&PresentationDirectives>,
    source: Option<CanonicalUuid>,
    observed: Option<wire::Instant>,
    media_source: Option<&std::path::Path>,
) -> CommandResult<()> {
    let Some(context) = record.decoded.context() else {
        return Ok(());
    };
    let source = source.or_else(|| state.observations.source(record.lane, record.locus));
    if state
        .presented
        .get(&(record.lane, record.occupant, source))
        .is_some_and(|next| (record.locus.member, record.locus.offset) < (next.member, next.offset))
    {
        return Ok(());
    }
    let mut events = Vec::new();
    for (event, decoded) in record.decoded.events().iter().enumerate() {
        let event = u32::try_from(event)
            .map_err(|error| store_fault("serve::message-locus-invalid", &error.to_string()))?;
        events.push((event, decoded));
    }
    publish_agent_record(
        publisher,
        AgentRecord {
            lane: record.lane,
            author: record.occupant,
            source,
            locus: record.locus,
            context,
            events: &events,
            observed,
            media_source,
            invitation: None,
        },
        state,
        steering,
    )
}

pub(super) fn publish_agent_record(
    publisher: &impl AgentMessagePublisher,
    record: AgentRecord<'_>,
    state: &mut MessageState,
    steering: Option<&PresentationDirectives>,
) -> CommandResult<()> {
    begin_agent_record(publisher, &record, state)?;
    prose_stream::observe(&record, state);
    for (position, &(ordinal, decoded)) in record.events.iter().enumerate() {
        if prose_stream::settled_block(&record, state, ordinal, decoded) {
            continue;
        }
        let answer_selection = prose_stream::answer_selection(&record, position, decoded);
        let suppress_answer = answer_selection.suppresses_event();
        let prose_origin = answer_selection.prose_origin(&record, state, ordinal, decoded);
        // Older tapes without a scoped message identity cannot safely join a
        // completed partial to its settled twin.
        if matches!(decoded, listen::Event::StreamedProse(_)) && prose_origin.is_none() {
            continue;
        }
        let event = AgentEvent {
            lane: record.lane,
            author: record.author,
            source: record.source,
            ordinal,
            decoded,
            authored: record.context.timestamp.as_deref(),
            observed: record.observed,
            observations: &state.observations,
            prose_origin,
            suppress_answer,
            superseded_answers: answer_selection.superseded(),
            invitation: record.invitation,
        };
        let markdown_images = state.media_hydration.observe(&event, record.media_source);
        let outbox = state.outbox_activity.authored(&event)?;
        let Some(projected) = project_agent_event(
            event,
            &mut state.previews,
            steering,
            markdown_images,
            outbox,
        )?
        else {
            continue;
        };
        let ProjectedMessage {
            published,
            terminal,
            pending,
        } = projected;
        if published.activity != AgentActivityKind::Prose {
            publish_message(publisher, published)?;
            continue;
        }
        if state
            .prose
            .get(&record.lane)
            .is_some_and(|previous| published.prose_superseded_by(&previous.published))
        {
            continue;
        }
        publish_message(publisher, published.clone())?;
        prose_stream::remember(&record, state, decoded);
        if let Some(pending) = pending {
            state.pending.remember(pending);
        }
        let terminal = terminal.unwrap_or_else(|| published.clone());
        state.prose.insert(
            record.lane,
            ProseState {
                published,
                terminal,
            },
        );
    }
    finish_agent_record(publisher, &record, state)
}

fn finish_agent_record(
    publisher: &impl AgentMessagePublisher,
    record: &AgentRecord<'_>,
    state: &mut MessageState,
) -> CommandResult<()> {
    let successful_terminal = record
        .context
        .prompt_outcome
        .as_ref()
        .is_some_and(|outcome| {
            outcome.agent_invoked && outcome.status == listen::PromptStatus::Completed
        });
    if successful_terminal
        && let Some(previous) = state.prose.get(&record.lane)
        && previous.published.author == record.author
    {
        revise_final_message(publisher, previous.terminal.clone())?;
    }
    if record.context.session_settled
        || record
            .context
            .prompt_outcome
            .as_ref()
            .is_some_and(|outcome| outcome.session_settled)
    {
        state.prose.remove(&record.lane);
    }
    let source = record
        .source
        .or_else(|| state.observations.source(record.lane, record.locus));
    let next = listen::Position {
        member: record.locus.member,
        offset: record
            .locus
            .offset
            .checked_add(record.locus.length)
            .ok_or_else(|| {
                store_fault(
                    "serve::message-locus-invalid",
                    "record end exceeds the tape position range",
                )
            })?,
    };
    state
        .presented
        .entry((record.lane, record.author, source))
        .and_modify(|known| *known = (*known).max(next))
        .or_insert(next);
    Ok(())
}

fn begin_agent_record(
    publisher: &impl AgentMessagePublisher,
    record: &AgentRecord<'_>,
    state: &mut MessageState,
) -> CommandResult<()> {
    if record.context.turn == Some(listen::TurnMoment::Started) {
        state.prose.remove(&record.lane);
        state.messages.remove(&record.lane);
        publish_turn_boundary(publisher, record, &state.observations)?;
    } else if record
        .events
        .iter()
        .any(|(_, event)| matches!(event, listen::Event::Session(_)))
    {
        state.prose.remove(&record.lane);
        state.messages.remove(&record.lane);
        state.outbox_activity.clear_lane(record.lane);
    }
    Ok(())
}

fn publish_turn_boundary(
    publisher: &impl AgentMessagePublisher,
    record: &AgentRecord<'_>,
    observations: &MessageObservations,
) -> CommandResult<()> {
    let (authored, authored_synthetic) =
        parse_agent_message_authored(record.context.timestamp.as_deref(), record.locus, 0)?;
    let observed = authored_synthetic
        .then(|| {
            record
                .observed
                .or_else(|| observations.at(record.lane, record.locus))
        })
        .flatten();
    publish_message(
        publisher,
        wire::AgentMessage {
            lane: record.lane,
            author: record.author,
            locus: wire::MessageLocus {
                member: record.locus.member,
                offset: record.locus.offset,
                length: record.locus.length,
                event: TURN_BOUNDARY_EVENT,
            },
            prose_origin: None,
            authored,
            authored_synthetic,
            observed,
            activity: AgentActivityKind::Turn,
            compaction_method: None,
            final_answer: false,
            acknowledgement: None,
            text: super::turn_cause::summary(record.events.iter().map(|&(_, event)| event)),
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
        },
    )
}

fn publish_message(
    publisher: &impl AgentMessagePublisher,
    message: wire::AgentMessage,
) -> CommandResult<()> {
    publisher
        .publish(message)
        .map_err(|error| store_fault("serve::message-publish-failed", &error.to_string()))?;
    Ok(())
}

fn revise_final_message(
    publisher: &impl AgentMessagePublisher,
    message: wire::AgentMessage,
) -> CommandResult<()> {
    publisher
        .revise_final(message)
        .map_err(|error| store_fault("serve::message-publish-failed", &error.to_string()))?;
    Ok(())
}

/// Project one typed tape event through the sole live/restored message policy.
#[cfg(test)]
pub(super) fn publish_agent_event(
    publisher: &impl AgentMessagePublisher,
    event: AgentEvent<'_>,
    previews: &mut CallPreviews,
    steering: Option<&PresentationDirectives>,
) -> CommandResult<()> {
    if let Some(projected) = project_agent_event(event, previews, steering, Vec::new(), None)? {
        publish_message(publisher, projected.published)?;
    }
    Ok(())
}

struct ProjectedEvent {
    activity: AgentActivityKind,
    locus: listen::Locus,
    presentation: serve::stream::MessagePresentation,
    provider_error: Option<wire::AgentProviderError>,
    automatic_speech: Option<wire::AutomaticSpeech>,
    media: Vec<wire::Media>,
}

fn project_agent_event(
    event: AgentEvent<'_>,
    previews: &mut CallPreviews,
    steering: Option<&PresentationDirectives>,
    markdown_images: Vec<wire::MarkdownImageBinding>,
    outbox: Option<outbox::Authored>,
) -> CommandResult<Option<ProjectedMessage>> {
    let (projected, outbox_quote) = match outbox {
        Some(authored) => (
            project_outbox_transport(&event, previews, authored.text),
            Some(authored.quote),
        ),
        None => (project_event_body(&event, previews, steering)?, None),
    };
    let Some(projected) = projected else {
        return Ok(None);
    };
    let AgentEvent {
        lane,
        author,
        source,
        ordinal,
        decoded,
        authored,
        observed,
        observations,
        prose_origin,
        superseded_answers,
        invitation,
        ..
    } = event;
    let ProjectedEvent {
        activity,
        locus,
        presentation:
            serve::stream::MessagePresentation {
                text,
                acknowledgement,
                directive_contexts,
                sends,
            },
        provider_error,
        automatic_speech,
        media,
    } = projected;
    let presentation_event = |locus| PresentationEvent {
        lane,
        author: Some(author),
        source: source.or_else(|| observations.source(lane, locus)),
        locus,
        ordinal,
        superseded_answers,
    };
    let (authored, authored_synthetic) = parse_agent_message_authored(authored, locus, ordinal)?;
    let observed = authored_synthetic
        .then(|| observed.or_else(|| observations.at(lane, locus)))
        .flatten();
    wire::media::validate_media(&media)
        .map_err(|error| store_fault("serve::message-media-invalid", &error.to_string()))?;
    wire::media::validate_markdown_image_bindings(&markdown_images)
        .map_err(|error| store_fault("serve::message-media-invalid", &error.to_string()))?;
    let locus = wire::MessageLocus {
        member: locus.member,
        offset: locus.offset,
        length: locus.length,
        event: ordinal,
    };
    let quote_target = match outbox_quote {
        Some(quote) => Some(quote),
        None => agent_quote_target(decoded, lane, source, locus)?,
    };
    let media_quotes = agent_media_quotes(lane, source, locus, &media)?;
    let published = wire::AgentMessage {
        lane,
        author,
        locus,
        prose_origin,
        authored,
        authored_synthetic,
        observed,
        activity,
        compaction_method: match decoded {
            listen::Event::Compaction(compaction) => compaction.method.clone(),
            _ => None,
        },
        final_answer: false,
        acknowledgement,
        text,
        context_bytes: returned_bytes(decoded),
        provider_error,
        quote_target,
        directive_contexts,
        sends,
        quotes: Vec::new(),
        automatic_speech,
        media,
        media_quotes,
        markdown_images,
    };
    let pending = invitation
        .is_none()
        .then(|| pending_presentation(decoded, steering, presentation_event, published.locus))
        .flatten();
    let terminal = terminal_prose_card(&published, decoded, steering, presentation_event);

    Ok(Some(ProjectedMessage {
        published,
        terminal,
        pending,
    }))
}

/// Bytes of rendered output a tool result returned to the model. A call or a
/// progress update returns none: the model wrote the one and never reads the
/// other.
fn returned_bytes(decoded: &listen::Event) -> u64 {
    let listen::Event::Tool(listen::Tool {
        moment: listen::ToolMoment::Result(outcome),
        ..
    }) = decoded
    else {
        return 0;
    };
    outcome
        .output
        .as_ref()
        .map_or(0, |output| u64::try_from(output.len()).unwrap_or(u64::MAX))
}

fn project_outbox_transport(
    event: &AgentEvent<'_>,
    previews: &mut CallPreviews,
    text: String,
) -> Option<ProjectedEvent> {
    let listen::Event::Tool(tool) = event.decoded else {
        return None;
    };
    previews.forget(event.lane, &tool.call_id);
    Some(ProjectedEvent {
        activity: AgentActivityKind::OutboxTransport,
        locus: tool.locus,
        presentation: serve::stream::MessagePresentation::plain(text),
        provider_error: None,
        automatic_speech: None,
        media: Vec::new(),
    })
}

fn project_event_body(
    event: &AgentEvent<'_>,
    previews: &mut CallPreviews,
    steering: Option<&PresentationDirectives>,
) -> CommandResult<Option<ProjectedEvent>> {
    if !event.suppress_answer
        && let Some(prose) = spice::agent_prose::quoteable(event.decoded)
    {
        let presentation_event = PresentationEvent {
            lane: event.lane,
            author: Some(event.author),
            source: event
                .source
                .or_else(|| event.observations.source(event.lane, prose.locus)),
            locus: prose.locus,
            ordinal: event.ordinal,
            superseded_answers: event.superseded_answers,
        };
        let (presented, automatic_speech) =
            project_prose_presentation(prose, steering, presentation_event, event.invitation)?;
        return Ok(Some(ProjectedEvent {
            activity: AgentActivityKind::Prose,
            locus: prose.locus,
            presentation: presented,
            provider_error: None,
            automatic_speech,
            media: prose.media.clone(),
        }));
    }
    let projected = match event.decoded {
        listen::Event::Tool(tool) => {
            let activity = tool_preview::activity(event.lane, tool, previews);
            let media = tool_preview::result_media(event.lane, tool, previews);
            let text = tool_preview::message_text(tool, activity, &media);
            ProjectedEvent {
                activity: AgentActivityKind::Tool,
                locus: tool.locus,
                provider_error: None,
                presentation: serve::stream::MessagePresentation::plain(text),
                automatic_speech: None,
                media,
            }
        }
        listen::Event::Reasoning(listen::Reasoning {
            content: listen::ReasoningContent::Plain(parts),
            locus,
        }) => ProjectedEvent {
            activity: AgentActivityKind::Reasoning,
            provider_error: None,
            locus: *locus,
            presentation: serve::stream::MessagePresentation::plain(reasoning_preview(parts)),
            automatic_speech: None,
            media: Vec::new(),
        },
        listen::Event::Compaction(compaction) => ProjectedEvent {
            activity: AgentActivityKind::Compaction,
            locus: compaction.locus,
            presentation: serve::stream::MessagePresentation::plain(
                compaction
                    .summary
                    .clone()
                    .filter(|summary| !summary.trim().is_empty())
                    .unwrap_or_else(|| "Context compacted".to_owned()),
            ),
            provider_error: None,
            automatic_speech: None,
            media: Vec::new(),
        },
        listen::Event::Session(session) if session.fresh => ProjectedEvent {
            activity: AgentActivityKind::Session,
            locus: session.locus,
            presentation: serve::stream::MessagePresentation::plain("Fresh session".to_owned()),
            provider_error: None,
            automatic_speech: None,
            media: Vec::new(),
        },
        listen::Event::Failure(failure) => ProjectedEvent {
            activity: AgentActivityKind::Failure,
            locus: failure.locus,
            presentation: serve::stream::MessagePresentation::plain(failure.message.clone()),
            provider_error: failure
                .provider
                .as_ref()
                .map(|provider| wire::AgentProviderError {
                    provider: provider.provider.clone(),
                    model: provider.model.clone(),
                    http_status: provider.http_status,
                    retryable: provider.retryable,
                }),
            automatic_speech: None,
            media: Vec::new(),
        },
        listen::Event::SignOut(sign_out) => ProjectedEvent {
            activity: AgentActivityKind::SignOut,
            provider_error: None,
            locus: sign_out.locus,
            presentation: serve::stream::MessagePresentation::plain(format!(
                "{}\n\nSign in from a terminal with `{}`.",
                sign_out.message.trim_end(),
                sign_out.login
            )),
            automatic_speech: None,
            media: Vec::new(),
        },
        listen::Event::Prose(prose) if prose.role == listen::ProseRole::Operator => {
            return project_direction(event, prose, steering);
        }
        _ => return Ok(None),
    };
    Ok(Some(projected))
}

/// The authenticated operator direction one operator-role record delivered to
/// its lane, or nothing when it delivered none.
fn project_direction(
    event: &AgentEvent<'_>,
    prose: &listen::Prose,
    steering: Option<&PresentationDirectives>,
) -> CommandResult<Option<ProjectedEvent>> {
    let presentation_event = PresentationEvent {
        lane: event.lane,
        author: Some(event.author),
        source: event
            .source
            .or_else(|| event.observations.source(event.lane, prose.locus)),
        locus: prose.locus,
        ordinal: event.ordinal,
        superseded_answers: &[],
    };
    let presentation = direction::present(&prose.text, steering, presentation_event)
        .map_err(|detail| store_fault("serve::message-steering-presentation-invalid", &detail))?;
    Ok(presentation.map(|presentation| ProjectedEvent {
        activity: AgentActivityKind::Direction,
        locus: prose.locus,
        presentation,
        provider_error: None,
        automatic_speech: None,
        media: Vec::new(),
    }))
}

fn agent_quote_target(
    decoded: &listen::Event,
    lane: CanonicalUuid,
    source: Option<CanonicalUuid>,
    locus: wire::MessageLocus,
) -> CommandResult<Option<wire::QuoteTarget>> {
    let Some(text) = spice::agent_prose::quoteable_text(decoded) else {
        return Ok(None);
    };
    let source = source.ok_or_else(|| {
        store_fault(
            "serve::message-source-unavailable",
            "quoteable agent text has no durable tape source",
        )
    })?;
    Ok(Some(wire::QuoteTarget::Tape {
        lane,
        source,
        locus,
        digest: wire::QuoteTarget::text_digest(text),
    }))
}

fn agent_media_quotes(
    lane: CanonicalUuid,
    source: Option<CanonicalUuid>,
    locus: wire::MessageLocus,
    media: &[wire::Media],
) -> CommandResult<Vec<wire::MediaQuote>> {
    if media.is_empty() {
        return Ok(Vec::new());
    }
    let source = source.ok_or_else(|| {
        store_fault(
            "serve::message-source-unavailable",
            "agent media has no durable tape source",
        )
    })?;
    spice::quote::media_quotes(
        &wire::QuoteTarget::Tape {
            lane,
            source,
            locus,
            digest: wire::QuoteTarget::text_digest(""),
        },
        media,
    )
    .map_err(|error| store_fault("serve::message-media-invalid", &format!("{error:?}")))
}

fn project_prose_presentation(
    prose: &listen::Prose,
    steering: Option<&PresentationDirectives>,
    event: PresentationEvent,
    invitation: Option<&wire::outbox::Invitation>,
) -> CommandResult<(
    serve::stream::MessagePresentation,
    Option<wire::AutomaticSpeech>,
)> {
    let presented = match invitation {
        Some(invitation) => present_invitation_message(&prose.text, invitation, event)?,
        None => present_steering_acknowledgements(&prose.text, steering, event)?,
    };
    let automatic_speech = automatic_speech::classify(prose, steering, event);
    Ok((presented, automatic_speech))
}

/// The keys an assistant prose card acknowledged ahead of the journal.
fn pending_presentation<'a>(
    decoded: &listen::Event,
    steering: Option<&PresentationDirectives>,
    presentation_event: impl Fn(listen::Locus) -> PresentationEvent<'a>,
    locus: wire::MessageLocus,
) -> Option<pending::PendingPresentation> {
    let prose = spice::agent_prose::quoteable(decoded)?;
    pending::PendingPresentation::of(
        &prose.text,
        steering,
        presentation_event(prose.locus),
        locus,
    )
}

/// The final-answer twin a mid-turn prose card carries for the turn end.
///
/// Only assistant prose short of its final answer has one; the twin repeats
/// the card with the final-answer phase so its automatic speech is classified
/// the way the terminal record will be.
fn terminal_prose_card<'a>(
    published: &wire::AgentMessage,
    decoded: &listen::Event,
    steering: Option<&PresentationDirectives>,
    presentation_event: impl FnOnce(listen::Locus) -> PresentationEvent<'a>,
) -> Option<wire::AgentMessage> {
    if published.activity != AgentActivityKind::Prose || published.final_answer {
        return None;
    }
    let prose = decoded.prose()?;
    let mut terminal_prose = prose.clone();
    terminal_prose.phase = Some("final_answer".to_owned());
    let mut terminal = published.clone();
    terminal.final_answer = true;
    terminal.automatic_speech = automatic_speech::classify(
        &terminal_prose,
        steering,
        presentation_event(terminal_prose.locus),
    );
    Some(terminal)
}

/// Strip preview for one reasoning event.
///
/// The narration is flattened to one bounded line the way a tool preview is.
/// A thinking block the vendor redacted to its signature previews as empty
/// text, so the strip still shows the lane thinking without inventing words;
/// opaque reasoning never reaches here because it carries no text at all.
fn reasoning_preview(parts: &[String]) -> String {
    tool_preview::flatten(&parts.join(" "))
}

fn parse_agent_message_authored(
    timestamp: Option<&str>,
    locus: listen::Locus,
    event: u32,
) -> CommandResult<(wire::Instant, bool)> {
    match timestamp {
        Some(timestamp) => timestamp
            .parse::<wire::Instant>()
            .map(|authored| (authored, false))
            .map_err(|error| store_fault("serve::message-authored-invalid", &error.to_string())),
        None => synthetic_agent_message_authored(locus, event).map(|authored| (authored, true)),
    }
}

fn synthetic_agent_message_authored(
    locus: listen::Locus,
    event: u32,
) -> CommandResult<wire::Instant> {
    // Some transcript records omit vendor time. The wire identity still
    // needs a stable ordering key, so derive one from durable tape coordinates;
    // presentation must use `observed` and never expose this surrogate as time.
    const BASE_MILLIS: i64 = 946_684_800_000;
    const MEMBER_MILLIS: i64 = 86_400_000;

    let member = locus.member.saturating_sub(1);
    let member = i64::try_from(member).map_err(|error| {
        store_fault(
            "serve::message-authored-unrepresentable",
            &error.to_string(),
        )
    })?;
    let offset = i64::try_from(locus.offset).map_err(|error| {
        store_fault(
            "serve::message-authored-unrepresentable",
            &error.to_string(),
        )
    })?;
    let delta = member
        .checked_mul(MEMBER_MILLIS)
        .and_then(|millis| millis.checked_add(offset))
        .and_then(|millis| millis.checked_add(i64::from(event)))
        .ok_or_else(|| {
            store_fault(
                "serve::message-authored-unrepresentable",
                "transcript locus cannot fit a synthetic authored instant",
            )
        })?;
    let millis = BASE_MILLIS.checked_add(delta).ok_or_else(|| {
        store_fault(
            "serve::message-authored-unrepresentable",
            "transcript locus cannot fit a synthetic authored instant",
        )
    })?;
    let authored = jiff::Timestamp::from_millisecond(millis).map_err(|error| {
        store_fault(
            "serve::message-authored-unrepresentable",
            &error.to_string(),
        )
    })?;
    wire::Instant::new(authored).map_err(|error| {
        store_fault(
            "serve::message-authored-unrepresentable",
            &error.to_string(),
        )
    })
}
