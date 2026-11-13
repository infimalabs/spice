//! Synchronous HTML projection of resident-authenticated quote trees.

use std::borrow::Cow;
use std::fmt::Write as _;

use pulldown_cmark::{Event as MarkdownEvent, Tag, TagEnd};
use wire::{Disposition, QuoteAttribution, QuoteAuthor, QuotePresentation, QuoteTarget};

/// The name an agent wears when its lane is absent from the roster.
const UNAVAILABLE: &str = "unavailable";

pub(super) fn push_typed_paragraph<'a>(events: &mut Vec<MarkdownEvent<'a>>, text: &'a str) {
    events.push(MarkdownEvent::Start(Tag::Paragraph));
    events.push(MarkdownEvent::Text(text.into()));
    events.push(MarkdownEvent::End(TagEnd::Paragraph));
}

pub(super) fn skip_review_envelope_event(event: &MarkdownEvent<'_>, depth: &mut usize) -> bool {
    if *depth == 0 {
        return false;
    }
    match event {
        MarkdownEvent::Start(Tag::BlockQuote(_)) => *depth += 1,
        MarkdownEvent::End(TagEnd::BlockQuote(_)) => *depth -= 1,
        _ => {}
    }
    true
}

/// Replace each structural quote link with its card. `speaker` is the lane the
/// surrounding signoff names, which a quote addressed only to it leaves implied.
pub(super) fn expand<'a>(
    events: Vec<MarkdownEvent<'a>>,
    presentations: &[QuotePresentation],
    agents: &super::QuoteAgents,
    sources: &super::QuoteSources,
    quote_depth: usize,
    speaker: Option<wire::CanonicalUuid>,
) -> Vec<MarkdownEvent<'a>> {
    if presentations.is_empty() {
        return events;
    }
    let mut expanded = Vec::with_capacity(events.len());
    let mut event = 0;
    let mut presentation = 0;
    let mut raw_quote_depth = 0usize;
    while event < events.len() {
        let node = structural_quote_target(&events[event..]).and_then(|target| {
            presentations
                .get(presentation)
                .filter(|node| presentation_target(node) == target)
        });
        let Some(node) = node else {
            match &events[event] {
                MarkdownEvent::Start(Tag::BlockQuote(_)) => raw_quote_depth += 1,
                MarkdownEvent::End(TagEnd::BlockQuote(_)) => {
                    raw_quote_depth = raw_quote_depth.saturating_sub(1);
                }
                _ => {}
            }
            expanded.push(events[event].clone());
            event += 1;
            continue;
        };
        expanded.push(MarkdownEvent::InlineHtml(
            render(
                node,
                agents,
                sources,
                quote_depth + raw_quote_depth + 1,
                speaker,
            )
            .into(),
        ));
        event += 3;
        presentation += 1;
    }
    expanded
}

/// The part of `agents` a card's quotes, directive contexts, and sent SYN
/// packets can render.
///
/// Authors resolve labels only among entries of the lane they name, so a card
/// rendered from this projection is identical to one rendered from the whole
/// roster, and a seat elsewhere changing occupancy leaves it untouched.
pub(crate) fn referenced_quote_agents(
    presentations: &[QuotePresentation],
    directives: &[wire::MessageDirective],
    sends: &[wire::MessageSend],
    agents: &super::QuoteAgents,
) -> super::QuoteAgents {
    let mut lanes = directives
        .iter()
        .flat_map(|directive| attribution_lanes(&directive.attribution))
        .chain(sends.iter().flat_map(|send| {
            std::iter::once(send.from)
                .chain(send.recipients.iter().map(|recipient| recipient.lane))
                .chain(send.lanes.iter().copied())
        }))
        .collect::<std::collections::BTreeSet<_>>();
    let mut pending = presentations.iter().collect::<Vec<_>>();
    while let Some(presentation) = pending.pop() {
        let (attribution, quotes) = match presentation {
            QuotePresentation::Resolved {
                attribution,
                quotes,
                ..
            } => (attribution, quotes.as_slice()),
            QuotePresentation::Media { attribution, .. } => (attribution, &[][..]),
            QuotePresentation::Unavailable { .. } => continue,
        };
        lanes.extend(attribution_lanes(attribution));
        pending.extend(quotes);
    }
    super::QuoteAgents {
        members: agents
            .members
            .iter()
            .filter(|((lane, _), _)| lanes.contains(lane))
            .map(|(key, agent)| (*key, agent.clone()))
            .collect(),
        principals: agents.principals.clone(),
    }
}

/// The source cards referenced by one card's quotes and directive contexts.
pub(crate) fn referenced_quote_sources(
    presentations: &[QuotePresentation],
    directives: &[wire::MessageDirective],
    sources: &super::QuoteSources,
) -> super::QuoteSources {
    let mut referenced = super::QuoteSources::new();
    for directive in directives {
        let target = directive.quote_target.to_string();
        if let Some(source) = sources.get(&target) {
            referenced.insert(target, source.clone());
        }
    }
    let mut pending = presentations.iter().collect::<Vec<_>>();
    while let Some(presentation) = pending.pop() {
        let (target, quotes) = match presentation {
            QuotePresentation::Resolved { target, quotes, .. } => (target, quotes.as_slice()),
            QuotePresentation::Media { target, .. } => (target, &[][..]),
            QuotePresentation::Unavailable { .. } => continue,
        };
        if let Some(source) = sources.get(target) {
            referenced.insert(target.clone(), source.clone());
        }
        pending.extend(quotes);
    }
    referenced
}

/// The task badges caused by the directions one card's quotes and directive
/// contexts name, so a card holds only its own slice of the board's origin index.
pub(crate) fn referenced_quote_tasks(
    presentations: &[QuotePresentation],
    directives: &[wire::MessageDirective],
    tasks: &crate::runtime::QuoteTasks,
) -> crate::runtime::QuoteTasks {
    let mut referenced = crate::runtime::QuoteTasks::new();
    let mut take = |target: &QuoteTarget| {
        let key = match target {
            QuoteTarget::Steering { key, .. } | QuoteTarget::SteeringMedia { key, .. } => key,
            _ => return,
        };
        if let Some(handles) = tasks.get(key) {
            referenced.insert(key.clone(), handles.clone());
        }
    };
    for directive in directives {
        take(&directive.quote_target);
    }
    let mut pending = presentations.iter().collect::<Vec<_>>();
    while let Some(presentation) = pending.pop() {
        let (target, quotes) = match presentation {
            QuotePresentation::Resolved { target, quotes, .. } => (target, quotes.as_slice()),
            QuotePresentation::Media { target, .. } => (target, &[][..]),
            QuotePresentation::Unavailable { .. } => continue,
        };
        if let Ok(target) = target.parse() {
            take(&target);
        }
        pending.extend(quotes);
    }
    referenced
}

/// Every lane one attribution names: its author's, then each recipient's.
fn attribution_lanes(
    attribution: &QuoteAttribution,
) -> impl Iterator<Item = wire::CanonicalUuid> + '_ {
    author_lane(&attribution.author).into_iter().chain(
        attribution
            .recipients
            .iter()
            .map(|recipient| recipient.lane),
    )
}

/// The lane an author speaks for; the operator speaks for none.
const fn author_lane(author: &QuoteAuthor) -> Option<wire::CanonicalUuid> {
    match *author {
        QuoteAuthor::Agent { lane, .. }
        | QuoteAuthor::Directive {
            actor: wire::Actor::Lane(lane),
        } => Some(lane),
        QuoteAuthor::Directive { .. } => None,
    }
}

/// The lead of each hunk of quoted words, the same wherever the source
/// appears: source age, author name, then every recipient its record proves.
/// Where `speaker`, the lane the surrounding signoff names, is the sole
/// recipient, the arrow and receiver are left to that context: they stay in
/// the accessible name, and the hover title names both ends, but they take no
/// visible space.
pub(super) fn signoff_html(
    attribution: &QuoteAttribution,
    target: &str,
    agents: &super::QuoteAgents,
    sources: &super::QuoteSources,
    speaker: Option<wire::CanonicalUuid>,
) -> String {
    let Some(source) = sources.get(target) else {
        return String::new();
    };
    let age = quote_age_slot_html(&attribution.author, agents, source);
    let (name, author) = quote_author_name_html(&attribution.author, agents, source);
    let mut destination = String::new();
    for recipient in &attribution.recipients {
        push_recipient_separator(&mut destination);
        destination.push_str(&recipient_html(recipient, agents));
    }
    let implied = speaker
        .and_then(|lane| implied_recipient(attribution, lane, agents))
        .map(|receiver| match author {
            Some(author) => format!("{author} → {receiver}"),
            None => format!("→ {receiver}"),
        });
    packet_signoff_html(&age, &name, &destination, "incoming", implied.as_deref())
}

/// The sole recipient's name when it is `lane` under the name that lane's
/// own signoff wears; any other recipient set stays visible.
fn implied_recipient<'a>(
    attribution: &QuoteAttribution,
    lane: wire::CanonicalUuid,
    agents: &'a super::QuoteAgents,
) -> Option<&'a str> {
    let [recipient] = attribution.recipients.as_slice() else {
        return None;
    };
    let member = lane_member_agent(agents, lane).filter(|_| recipient.lane == lane)?;
    let label = match recipient.occupant {
        Some(occupant) => &occupant_agent(agents, lane, occupant)?.0.label,
        None => &member.label,
    };
    (*label == member.label).then_some(label.as_str())
}

/// One recipient: the exact occupant its record proves, else its lane.
fn recipient_html(recipient: &wire::QuoteRecipient, agents: &super::QuoteAgents) -> String {
    recipient.occupant.map_or_else(
        || lane_participant_html(recipient.lane, agents),
        |occupant| participant_html(recipient.lane, occupant, None, agents),
    )
}

/// A signoff whose receiver is already named around it keeps the arrow and
/// recipients for assistive technology, shows only the age and sender, and
/// carries the full direction, `implied`, as its hover title.
fn packet_signoff_html(
    age: &str,
    sender: &str,
    recipients: &str,
    direction: &str,
    implied: Option<&str>,
) -> String {
    if recipients.is_empty() {
        return format!("<span class=\"message-quote-signoff\">{age}{sender}</span>");
    }
    let destination = format!(
        "<span class=\"message-quote-direction\" role=\"img\" aria-label=\"sent to\">→</span>\
         <span class=\"message-quote-recipients\">{recipients}</span>"
    );
    let Some(implied) = implied else {
        return format!(
            "<span class=\"message-quote-signoff\" data-quote-direction=\"{direction}\">\
             {age}{sender}{destination}</span>"
        );
    };
    format!(
        "<span class=\"message-quote-signoff\" data-quote-direction=\"{direction}\" \
         data-quote-receiver=\"implied\" title=\"{}\">{age}{sender}\
         <span class=\"visually-hidden\">{destination}</span></span>",
        super::escape_html_attribute(implied)
    )
}

fn participant_html(
    lane: wire::CanonicalUuid,
    occupant: wire::CanonicalUuid,
    address: Option<wire::OccupantAddress>,
    agents: &super::QuoteAgents,
) -> String {
    let (agent, state) = occupant_agent(agents, lane, occupant)
        .map_or((None, "unresolved"), |(agent, state)| (Some(agent), state));
    let label = agent.map_or_else(
        || {
            Cow::Owned(
                address.map_or_else(|| UNAVAILABLE.to_owned(), |address| address.to_string()),
            )
        },
        |agent| Cow::Borrowed(agent.label.as_str()),
    );
    let recipient = address.is_some();
    let address = address.map_or_else(String::new, |address| {
        format!(" data-quote-recipient-address=\"{address}\" title=\"{address}\"")
    });
    let availability = (state == "unresolved" && recipient).then(|| {
        format!(
            " data-quote-recipient-state=\"unavailable\" \
             aria-label=\"{}, recipient unavailable\"",
            super::escape_html_attribute(&label),
        )
    });
    quote_name_html(
        &label,
        &format!(
            " data-quote-agent-lane=\"{lane}\" data-quote-agent-occupant=\"{occupant}\" \
             data-quote-author-state=\"{state}\"{address}{}",
            availability.unwrap_or_default()
        ),
        agent.map(|agent| agent.accent),
    )
}

/// A lane addressed as a whole, named by its team member, seated or vacant.
/// A lane gone from the roster reads as unavailable, never as raw identifiers.
fn lane_participant_html(lane: wire::CanonicalUuid, agents: &super::QuoteAgents) -> String {
    let member = lane_member_agent(agents, lane);
    let label = member.map_or(UNAVAILABLE, |agent| agent.label.as_str());
    quote_name_html(
        label,
        &format!(" data-quote-recipient-lane=\"{lane}\""),
        member.map(|agent| agent.accent),
    )
}

/// The accent slot of the speaker a quote's attribution names: the slot that
/// speaker's own top-level cards wear, carried on the quote's shell so its rail
/// wears it too. Operators and Spice speak in the neutral accent and have no
/// slot, as does a quote whose attribution is not shown.
pub(super) fn speaker_accent(
    author: &QuoteAuthor,
    target: &str,
    agents: &super::QuoteAgents,
    sources: &super::QuoteSources,
) -> Option<usize> {
    author_accent(author, agents, sources.get(target)?)
}

fn author_accent(
    author: &QuoteAuthor,
    agents: &super::QuoteAgents,
    source: &super::QuoteSource,
) -> Option<usize> {
    match author {
        QuoteAuthor::Agent { lane, occupant } => {
            agent_author(agents, source, *lane, *occupant).map(|(agent, _)| agent.accent)
        }
        QuoteAuthor::Directive {
            actor: wire::Actor::Lane(lane),
        } => lane_member_agent(agents, *lane).map(|agent| agent.accent),
        QuoteAuthor::Directive { .. } => None,
    }
}

/// The roster entry that names one occupant's prose, and whether it is that
/// occupant's current seat or a historical attribution: its own seat first,
/// then the agent its source card recorded, then its lane. A lane's name and
/// accent belong to the lane, so an occupant that has since left still reads
/// under them.
fn agent_author<'a>(
    agents: &'a super::QuoteAgents,
    source: &'a super::QuoteSource,
    lane: wire::CanonicalUuid,
    occupant: wire::CanonicalUuid,
) -> Option<(&'a super::QuoteAgent, &'static str)> {
    agents
        .members
        .get(&(lane, Some(occupant)))
        .map(|agent| (agent, "current"))
        .or_else(|| historical_agent(agents, source, lane).map(|agent| (agent, "historical")))
}

fn historical_agent<'a>(
    agents: &'a super::QuoteAgents,
    source: &'a super::QuoteSource,
    lane: wire::CanonicalUuid,
) -> Option<&'a super::QuoteAgent> {
    source
        .agent
        .as_ref()
        .or_else(|| lane_member_agent(agents, lane))
}

/// The lead of a packet the card's author sent: the card's live age, its kind
/// when not SYN, exact authoring occupant, an outgoing arrow, then every
/// addressed occupant, every lane reached as a whole, and the operator. A
/// quoted peer context uses the same direction lead with its own recorded
/// destination; badges never change either side's attribution.
fn send_signoff_html(
    send: &wire::MessageSend,
    agents: &super::QuoteAgents,
    card: Option<&super::QuoteSource>,
) -> String {
    let (sender, state) = occupant_agent(agents, send.from, send.sender)
        .map_or((None, "unresolved"), |(agent, state)| (Some(agent), state));
    let label = sender.map_or(UNAVAILABLE, |agent| agent.label.as_str());
    let sender = quote_name_html(
        label,
        &format!(
            " data-quote-author=\"agent\" data-quote-agent-lane=\"{}\" \
             data-quote-agent-occupant=\"{}\" data-quote-author-state=\"{state}\"",
            send.from, send.sender
        ),
        sender.map(|agent| agent.accent),
    );
    let mut recipients = String::new();
    for recipient in &send.recipients {
        push_recipient_separator(&mut recipients);
        recipients.push_str(&participant_html(
            recipient.lane,
            recipient.occupant,
            Some(recipient.address),
            agents,
        ));
    }
    for lane in &send.lanes {
        push_recipient_separator(&mut recipients);
        recipients.push_str(&lane_participant_html(*lane, agents));
    }
    if send.operator {
        push_recipient_separator(&mut recipients);
        recipients.push_str(&quote_name_html(
            "Operator",
            " data-quote-recipient=\"operator\"",
            None,
        ));
    }
    for context in &send.contexts {
        push_recipient_separator(&mut recipients);
        recipients.push_str(&super::task_chip_html(context, None, ""));
    }
    let author = QuoteAuthor::Agent {
        lane: send.from,
        occupant: send.sender,
    };
    let lead = card.map_or_else(String::new, |card| {
        quote_age_slot_html(&author, agents, card)
    });
    packet_signoff_html(&lead, &sender, &recipients, "outgoing", None)
}

fn push_recipient_separator(recipients: &mut String) {
    if !recipients.is_empty() {
        recipients.push_str("<span class=\"message-quote-recipient-separator\">, </span>");
    }
}

/// The accent slot of the occupant that sent one packet, which its quote's
/// rail wears as any quote wears its speaker's.
pub(super) fn send_accent(send: &wire::MessageSend, agents: &super::QuoteAgents) -> Option<usize> {
    occupant_agent(agents, send.from, send.sender).map(|(agent, _)| agent.accent)
}

/// A sent packet's body reads as the card author's own words, so its hunks
/// keep the containing message's controls under an outgoing sender-to-recipient
/// lead, with task/room destinations beside that lead.
pub(super) fn send_attribution(
    send: &wire::MessageSend,
    agents: &super::QuoteAgents,
    card: Option<&super::QuoteSource>,
) -> super::image_flow::Attribution {
    super::image_flow::Attribution {
        controls_target: String::new(),
        signoff: send_signoff_html(send, agents, card),
        disposition: String::new(),
        badge: String::new(),
        task_tail: String::new(),
        repeat: true,
    }
}

pub(super) fn directive_attribution(
    context: &wire::MessageDirective,
    agents: &super::QuoteAgents,
    sources: &super::QuoteSources,
    speaker: Option<wire::CanonicalUuid>,
) -> super::image_flow::Attribution {
    let target = context.quote_target.to_string();
    super::image_flow::Attribution {
        controls_target: target.clone(),
        signoff: signoff_html(&context.attribution, &target, agents, sources, speaker),
        disposition: String::new(),
        badge: directive_badges_html(context),
        task_tail: task_tail_html(&target),
        repeat: context.review.is_none(),
    }
}

/// One typed review finding, independent of the prose envelope that delivered
/// it. Its task leads the finding as structured context rather than recovered
/// Markdown.
pub(super) fn review_html(
    context: &wire::MessageDirective,
    agents: &super::QuoteAgents,
    sources: &super::QuoteSources,
    speaker: Option<wire::CanonicalUuid>,
) -> String {
    let review = context
        .review
        .as_ref()
        .expect("review presentation carries typed review context");
    let body = super::render_quote_card_body(
        &review.finding,
        &[],
        agents,
        sources,
        &[],
        super::Finish::QuoteCard {
            attribution: directive_attribution(context, agents, sources, speaker),
            leading_task: Some(&review.task),
            trailing_guidance: Some(&review.guidance),
            quote_depth: 1,
            speaker: author_lane(&context.attribution.author),
        },
    );
    format!(
        concat!(
            "<div class=\"message-quote-card message-review-card\" ",
            "data-message-review=\"{}\"><div class=\"message-quote-body\">{}</div></div>"
        ),
        super::escape_html_attribute(review.task.as_str()),
        body.html,
    )
}

/// Typed directive classifications, independent of authored prose.
fn directive_badges_html(context: &wire::MessageDirective) -> String {
    let mut badges = context.badge.map_or_else(String::new, directive_badge_html);
    if context.attribution.recipients.len() > 1
        && context
            .delivery
            .as_ref()
            .is_some_and(|delivery| delivery.fanout.is_some())
    {
        badges.push_str(
            "<span class=\"message-broadcast-pill\" data-message-classification=\"broadcast\" \
             aria-label=\"Broadcast classification\">BROADCAST</span>",
        );
    }
    let mut metadata = if badges.is_empty() {
        String::new()
    } else {
        format!(
            "<span class=\"message-classification-group\" role=\"group\" \
             aria-label=\"Directive classifications\">{badges}</span>"
        )
    };
    if let Some(line) = &context.line {
        metadata.push_str(&line_metadata_html(line));
    }
    metadata
}

/// The quoted directive's own kind, where its source marker once stood.
fn directive_badge_html(badge: wire::DirectiveBadge) -> String {
    let (class, classification, label, text) = match badge {
        wire::DirectiveBadge::Review => (
            "message-review-pill",
            "review",
            "Review classification",
            "REVIEW",
        ),
        wire::DirectiveBadge::Taste => (
            "message-taste-pill",
            "taste",
            "Taste classification",
            "TASTE",
        ),
    };
    format!(
        "<span class=\"{class}\" data-message-classification=\"{classification}\" \
         aria-label=\"{label}\">{text}</span>"
    )
}

/// A quoted peer input's conversation-line standing. The journal identity
/// remains available to assistive technology and browser automation without
/// exposing protocol transition vocabulary as visible message chrome.
fn line_metadata_html(line: &wire::intent::LineStanding) -> String {
    let standing = super::escape_html_attribute(&format!("Peer line: {}", line.standing()));
    format!(
        "<span class=\"visually-hidden\" data-message-classification=\"line\" \
         data-message-line=\"{root}\" aria-label=\"{standing}\">{standing}</span>",
        root = super::escape_html_attribute(&line.root),
    )
}

/// The roster entry naming one exact occupant with no source card to consult:
/// its own seat, else its lane, whose name and accent outlast any one seat.
fn occupant_agent(
    agents: &super::QuoteAgents,
    lane: wire::CanonicalUuid,
    occupant: wire::CanonicalUuid,
) -> Option<(&super::QuoteAgent, &'static str)> {
    agents
        .members
        .get(&(lane, Some(occupant)))
        .map(|agent| (agent, "current"))
        .or_else(|| lane_member_agent(agents, lane).map(|agent| (agent, "historical")))
}

/// Empty portal target that board provenance fills at the quoted content's
/// trailing edge after the card has rendered.
fn task_tail_html(target: &str) -> String {
    let key = match target.parse() {
        Ok(QuoteTarget::Steering { key, .. } | QuoteTarget::SteeringMedia { key, .. }) => key,
        _ => return String::new(),
    };
    format!(
        "<span class=\"message-quote-task-tail\" data-quote-tasks=\"{}\"></span>",
        super::escape_html_attribute(&key)
    )
}

fn structural_quote_target<'event>(events: &'event [MarkdownEvent<'_>]) -> Option<&'event str> {
    match events {
        [
            MarkdownEvent::Start(Tag::Link { dest_url, .. }),
            MarkdownEvent::Text(label),
            MarkdownEvent::End(TagEnd::Link),
            ..,
        ] if label.as_ref() == "quote" && dest_url.starts_with("spice://quote/v1/") => {
            Some(dest_url.as_ref())
        }
        _ => None,
    }
}

fn presentation_target(presentation: &QuotePresentation) -> &str {
    match presentation {
        QuotePresentation::Resolved { target, .. }
        | QuotePresentation::Media { target, .. }
        | QuotePresentation::Unavailable { target, .. } => target,
    }
}

/// One quote card. Its signoff leaves a receiver implied by the surrounding
/// `speaker`; its own words surround nested quotes as its author's.
fn render(
    presentation: &QuotePresentation,
    agents: &super::QuoteAgents,
    sources: &super::QuoteSources,
    quote_depth: usize,
    speaker: Option<wire::CanonicalUuid>,
) -> String {
    match presentation {
        QuotePresentation::Resolved {
            target,
            text,
            attribution,
            disposition,
            quotes,
            media_quotes,
        } => {
            let disposition = disposition.map(disposition_html).unwrap_or_default();
            let signoff = signoff_html(attribution, target, agents, sources, speaker);
            let task_tail = task_tail_html(target);
            let body = super::render_quote_card_body(
                text,
                quotes,
                agents,
                sources,
                media_quotes,
                super::Finish::QuoteCard {
                    attribution: super::image_flow::Attribution {
                        controls_target: target.clone(),
                        signoff,
                        disposition,
                        badge: String::new(),
                        task_tail,
                        repeat: true,
                    },
                    leading_task: None,
                    trailing_guidance: None,
                    quote_depth,
                    speaker: author_lane(&attribution.author),
                },
            );
            let accent = speaker_accent(&attribution.author, target, agents, sources)
                .map_or_else(String::new, |accent| format!(" data-accent=\"{accent}\""));
            format!(
                "<div class=\"message-quote-card\" data-quote-state=\"resolved\" \
                 data-quote-target=\"{}\"{accent}><div class=\"message-quote-body\">{}</div></div>",
                super::escape_html_attribute(target),
                body.html,
            )
        }
        QuotePresentation::Media {
            target,
            media,
            attribution,
            disposition,
        } => {
            let disposition = disposition.map(disposition_html).unwrap_or_default();
            let signoff = signoff_html(attribution, target, agents, sources, speaker);
            let task_tail = task_tail_html(target);
            let own_quote = target.parse().ok().map(|target| wire::MediaQuote {
                target,
                name: media.name.clone(),
            });
            let markdown = media
                .markdown()
                .expect("resident-resolved media passed admission");
            let body = super::render_quote_card_body(
                &markdown,
                &[],
                agents,
                sources,
                own_quote.as_slice(),
                super::Finish::QuoteCard {
                    attribution: super::image_flow::Attribution {
                        controls_target: target.clone(),
                        signoff,
                        disposition,
                        badge: String::new(),
                        task_tail,
                        repeat: true,
                    },
                    leading_task: None,
                    trailing_guidance: None,
                    quote_depth,
                    speaker: author_lane(&attribution.author),
                },
            );
            let accent = speaker_accent(&attribution.author, target, agents, sources)
                .map_or_else(String::new, |accent| format!(" data-accent=\"{accent}\""));
            format!(
                "<div class=\"message-quote-card message-quote-card--media\" \
                 data-quote-state=\"resolved\" data-quote-target=\"{}\"{accent}>\
                 <div class=\"message-quote-body\">{}</div></div>",
                super::escape_html_attribute(target),
                body.html,
            )
        }
        QuotePresentation::Unavailable { target, reason } => {
            let reason = unavailable_key(*reason);
            format!(
                "<div class=\"message-quote-card message-quote-card--unavailable\" \
                 data-quote-state=\"unavailable\" data-quote-reason=\"{reason}\" \
                 data-quote-target=\"{}\"><span class=\"message-quote-unavailable\">\
                 Quote unavailable: {reason} · authorship unavailable</span></div>",
                super::escape_html_attribute(target),
            )
        }
    }
}

/// The author's name element, and the plain name assistive technology hears
/// for it; a principal with no recorded initials has neither.
fn quote_author_name_html<'a>(
    author: &QuoteAuthor,
    agents: &'a super::QuoteAgents,
    source: &'a super::QuoteSource,
) -> (String, Option<Cow<'a, str>>) {
    match author {
        QuoteAuthor::Agent { lane, occupant } => {
            let (agent, state) = agent_author(agents, source, *lane, *occupant)
                .map_or((None, "unresolved"), |(agent, state)| (Some(agent), state));
            let label = agent.map_or(UNAVAILABLE, |agent| agent.label.as_str());
            let html = quote_name_html(
                label,
                &format!(
                    " data-quote-agent-lane=\"{lane}\" data-quote-agent-occupant=\"{occupant}\" \
                     data-quote-author-state=\"{state}\""
                ),
                author_accent(author, agents, source),
            );
            (html, Some(Cow::Borrowed(label)))
        }
        // A principal directive names its author by the initials of the
        // recorded profile, and by nothing before that profile is known or
        // when it records no name.
        QuoteAuthor::Directive {
            actor: actor @ wire::Actor::Principal(_),
        } => agents
            .principals
            .identity(actor)
            .and_then(|identity| {
                let initials = identity.initials.as_deref()?;
                let name = identity.name.as_deref().unwrap_or(initials);
                let html = quote_name_html(
                    initials,
                    &format!(
                        " data-quote-author=\"directive\" data-quote-actor=\"principal\" \
                         role=\"img\" aria-label=\"{}\"",
                        super::escape_html_attribute(name),
                    ),
                    None,
                );
                Some((html, Some(Cow::Borrowed(name))))
            })
            .unwrap_or_default(),
        QuoteAuthor::Directive {
            actor: wire::Actor::Spice,
        } => (
            quote_name_html(
                "🌶️",
                " data-quote-author=\"directive\" data-quote-actor=\"spice\" aria-label=\"Spice\"",
                None,
            ),
            Some(Cow::Borrowed("Spice")),
        ),
        QuoteAuthor::Directive {
            actor: wire::Actor::Lane(lane),
        } => {
            // A lane directive with no recorded authoring occupant names its
            // sending lane: the lane's team member speaks for it, seated or
            // vacant. A SYN names its exact occupant as an `Agent` instead.
            let member = lane_member_agent(agents, *lane);
            let (label, state) = member.map_or_else(
                || {
                    (
                        Cow::Owned(format!("lane {}", crate::watchband::short(*lane))),
                        "lane",
                    )
                },
                |agent| (Cow::Borrowed(agent.label.as_str()), "current"),
            );
            let html = quote_name_html(
                &label,
                &format!(
                    " data-quote-author=\"directive\" data-quote-actor=\"lane:{lane}\" \
                     data-quote-author-state=\"{state}\""
                ),
                author_accent(author, agents, source),
            );
            (html, Some(label))
        }
    }
}

fn quote_name_html(label: &str, data: &str, accent: Option<usize>) -> String {
    let accent = accent.map_or_else(String::new, |accent| format!(" data-accent=\"{accent}\""));
    format!(
        "<span class=\"message-quote-name\"{data}{accent}>{}</span>",
        escape_html_text(label),
    )
}

fn quote_age_slot_html(
    author: &QuoteAuthor,
    agents: &super::QuoteAgents,
    source: &super::QuoteSource,
) -> String {
    let mut data = match author {
        QuoteAuthor::Agent { lane, occupant } => {
            let state = if agents.members.contains_key(&(*lane, Some(*occupant))) {
                "current"
            } else {
                "historical"
            };
            format!(
                " data-quote-author=\"agent\" data-quote-agent-lane=\"{lane}\" \
                 data-quote-agent-occupant=\"{occupant}\" data-quote-author-state=\"{state}\""
            )
        }
        QuoteAuthor::Directive { actor } => {
            let actor = actor.to_string();
            format!(
                " data-quote-author=\"directive\" data-quote-actor=\"{}\"",
                super::escape_html_attribute(&actor),
            )
        }
    };
    let accent = match author {
        QuoteAuthor::Agent { lane, .. } => historical_agent(agents, source, *lane),
        QuoteAuthor::Directive {
            actor: wire::Actor::Lane(lane),
        } => source
            .agent
            .as_ref()
            .or_else(|| lane_member_agent(agents, *lane)),
        QuoteAuthor::Directive { .. } => source.agent.as_ref(),
    }
    .map(|agent| agent.accent);
    if let Some(accent) = accent {
        write!(data, " data-accent=\"{accent}\"").expect("writing HTML to a String cannot fail");
    }
    let source_data = source.card_id.as_ref().map_or_else(String::new, |card_id| {
        format!(
            " data-quote-source=\"{}\"",
            super::escape_html_attribute(card_id)
        )
    });
    format!(
        "<span class=\"message-quote-age-slot\" data-quote-age-authored=\"{}\"{source_data}{data}></span>",
        source.display_millis,
    )
}

/// The team member for one lane, seated or vacant, which speaks for a
/// directive that lane sent with no recorded occupant: such a directive names
/// the lane, never an occupant.
fn lane_member_agent(
    agents: &super::QuoteAgents,
    lane: wire::CanonicalUuid,
) -> Option<&super::QuoteAgent> {
    agents
        .members
        .iter()
        .find_map(|((agent_lane, _), agent)| (*agent_lane == lane).then_some(agent))
}

fn disposition_html(disposition: Disposition) -> String {
    let (key, label) = match disposition {
        Disposition::Live => ("live", "LIVE"),
        Disposition::Acked => {
            return super::acknowledgement_pill_html_with_attributes(
                "ack",
                "acked",
                "Acknowledgement classification",
                "ACK",
                " data-quote-disposition=\"acked\"",
            );
        }
        Disposition::Declined => {
            return super::acknowledgement_pill_html_with_attributes(
                "nak",
                "declined",
                "Declined classification",
                "NAK",
                " data-quote-disposition=\"declined\"",
            );
        }
        Disposition::Lapsed => ("lapsed", "LAPSED"),
        Disposition::DeadLettered => ("dead-letter", "DEAD LETTER"),
        Disposition::Canceled => ("canceled", "CANCELED"),
    };
    format!(
        "<span class=\"message-quote-disposition\" \
         data-quote-disposition=\"{key}\">{label}</span>"
    )
}

const fn unavailable_key(reason: wire::QuoteUnavailable) -> &'static str {
    match reason {
        wire::QuoteUnavailable::Malformed => "malformed",
        wire::QuoteUnavailable::Unknown => "unknown",
        wire::QuoteUnavailable::Unreadable => "unreadable",
        wire::QuoteUnavailable::DigestMismatch => "digest-mismatch",
        wire::QuoteUnavailable::DepthExceeded => "depth-exceeded",
    }
}

fn escape_html_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(character),
        }
    }
    escaped
}
