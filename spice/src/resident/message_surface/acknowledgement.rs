//! Acknowledgement headers rewritten into quoted directive context, and SYN
//! packets rewritten into outgoing quotes.
//!
//! An assistant header such as `ACK KEY: applied` presents the directive it
//! names, quoted verbatim above the response prose, whenever the journal
//! allocated that key: a bare key is useless to the operator. One authenticated
//! message disposition records what the whole answer earned: acknowledged once
//! ordinary keys retire, pending while one remains held, and declined when any
//! owned header refuses its directive. The disposition is independent of the
//! presented prose and directive ranges. A key this response did not answer --
//! another lane's, source-bound review feedback from another source, or an
//! answer the journal recorded otherwise -- quotes its directive without
//! earning a disposition. Only a token no directive owns stays literal.
//! Presentation never grants repeated response retirement, a disposition, or
//! automatic-speech authority.
//!
//! A placed, well-formed `SYN SELECTOR ...: BODY` packet whose addressed
//! occupants each have a proved birth presents its body verbatim as one quote
//! block in place of the header. The block carries its addressed recipients,
//! operator destination, and distinct task/room contexts. The quote states
//! what the author addressed, not delivery: whether the resident published or
//! withheld the packet is the journal's record.
//!
//! An admitted invitation message has no header: its whole body is one such
//! quote, addressed to the lane its invitation reached and the task whose
//! room records it, and no header inside it is read.

use std::iter::Peekable;
use std::ops::Range;
use std::vec::IntoIter;

use super::*;

/// One directive context the resident inserted into a presented card.
#[derive(Debug)]
pub(super) struct PresentedDirectiveContext {
    pub(super) key: steer::Key,
    pub(super) range: Range<usize>,
}

/// One authored packet or invitation message the resident presented as an
/// outgoing quote block.
#[derive(Debug)]
pub(super) struct PresentedSend {
    pub(super) range: Range<usize>,
    pub(super) source: Range<usize>,
    pub(super) body: String,
    pub(super) recipients: Vec<wire::MessageRecipient>,
    pub(super) lanes: Vec<CanonicalUuid>,
    pub(super) operator: bool,
    pub(super) contexts: Vec<wire::TaskHandle>,
}

/// One placed packet in the source text, ready to present.
struct PlacedSend {
    /// Source offset where the packet's framing and header begin.
    start: usize,
    /// The packet's trimmed body in the source text.
    body: Range<usize>,
    recipients: Vec<wire::MessageRecipient>,
    operator: bool,
    contexts: Vec<wire::TaskHandle>,
}

/// One presented message and the quote blocks the resident itself wrote into it.
#[derive(Debug)]
pub(super) struct Presentation {
    pub(super) text: String,
    /// Exact directive contexts in source order. Quotation nested inside them
    /// is operator-authored journal text; anything outside them is agent prose.
    pub(super) contexts: Vec<PresentedDirectiveContext>,
    /// Exact outgoing packet quotes in source order, disjoint from `contexts`.
    pub(super) sends: Vec<PresentedSend>,
    /// Authenticated disposition of the whole answer. Mixed headers use
    /// declined, then pending, then acknowledged precedence.
    pub(super) acknowledgement: Option<wire::MessageAcknowledgement>,
}

pub(super) fn present_steering_acknowledgements_with_directives(
    text: &str,
    directives: &PresentationDirectives,
    event: PresentationEvent,
) -> Result<Presentation, String> {
    let standing = EventDirectives { directives, event };
    let mut rendered = String::new();
    let mut contexts = Vec::new();
    let mut sends = Vec::new();
    let mut placed = placed_sends(text, directives, event).into_iter().peekable();
    let mut answer_disposition: Option<wire::MessageAcknowledgement> = None;
    let mut cursor = 0;
    let mut changed = placed.peek().is_some();
    let mut capitalize_body = false;
    for acknowledgement in scan_presentations(text, &standing) {
        let separator = acknowledgement.separator;
        let packet = classify_packet(&standing, acknowledgement.recognitions);
        // Every allocated key a header names presents its directive; a header
        // whose tokens name no directive stays the agent's literal prose.
        if packet.states.is_empty() && packet.contexts.is_empty() {
            continue;
        }
        let start = presentation_framing_start(text, cursor, acknowledgement.start);
        let end = presentation_framing_end(text, acknowledgement.end);
        let framing_removed = start != acknowledgement.start || end != acknowledgement.end;
        let source = cursor..start;
        if text.get(source.clone()).is_none() {
            return Err("acknowledgment header crossed a non-character boundary".to_owned());
        }
        append_authored(
            &mut rendered,
            text,
            source,
            capitalize_body,
            &mut placed,
            &mut sends,
        );
        capitalize_body = false;
        if !packet.contexts.is_empty() {
            let keys = packet.contexts.iter().map(|(key, _)| key);
            append_directive_contexts(&mut rendered, directives, keys, &mut contexts)?;
        }
        let disposition = packet_acknowledgement(&packet);
        if let Some(disposition) = disposition
            && answer_disposition
                .is_none_or(|current| disposition.precedence() > current.precedence())
        {
            answer_disposition = Some(disposition);
        }
        cursor = end;
        if packet.missed.is_empty() {
            cursor = skip_horizontal_space(text, cursor);
            if !packet.contexts.is_empty() {
                cursor = skip_line_breaks(text, cursor);
            }
            capitalize_body = true;
        } else {
            // An authenticated disposition removes the authored ACK or NAK;
            // otherwise the header's literal word stays with tokens it failed
            // to own.
            if disposition.is_none() {
                let literal = packet
                    .marker
                    .ok_or_else(|| "presented acknowledgment header lost its marker".to_owned())?;
                rendered.push_str(acknowledgement_marker(literal)?);
            }
            for token in packet.missed {
                if rendered
                    .chars()
                    .next_back()
                    .is_some_and(|character| !character.is_whitespace())
                {
                    rendered.push(' ');
                }
                rendered.push_str(&token);
            }
            if let Some(separator) = separator {
                rendered.push(separator);
            } else if framing_removed
                && text
                    .get(cursor..)
                    .and_then(|tail| tail.chars().next())
                    .is_some_and(|character| !character.is_whitespace())
            {
                rendered.push(' ');
            }
        }
        changed = true;
    }

    if !changed {
        return Ok(Presentation {
            text: text.to_owned(),
            contexts,
            sends,
            acknowledgement: answer_disposition,
        });
    }
    if text.get(cursor..).is_none() {
        return Err("acknowledgment body crossed a non-character boundary".to_owned());
    }
    append_authored(
        &mut rendered,
        text,
        cursor..text.len(),
        capitalize_body,
        &mut placed,
        &mut sends,
    );
    Ok(Presentation {
        text: rendered,
        contexts,
        sends,
        acknowledgement: answer_disposition,
    })
}

/// An invitation's message presented as one outgoing quote of its whole body,
/// addressed to the lane its invitation reached in the room of its task. No
/// header inside the body is read: the invitation, not the text, addressed it.
pub(super) fn present_invitation(
    text: &str,
    invitation: &wire::outbox::Invitation,
) -> Presentation {
    let source = text.len() - text.trim_start().len()..text.trim_end().len();
    let body = text[source.clone()].to_owned();
    let mut rendered = String::new();
    append_acknowledgement_quote(&mut rendered, &body);
    Presentation {
        sends: vec![PresentedSend {
            range: 0..rendered.len(),
            source,
            body,
            recipients: Vec::new(),
            lanes: vec![invitation.recipient],
            operator: false,
            contexts: vec![invitation.task.clone()],
        }],
        text: rendered,
        contexts: Vec::new(),
        acknowledgement: None,
    }
}

/// Quote every directive one header answers as one block, recording each
/// context's exact range.
fn append_directive_contexts<'k>(
    rendered: &mut String,
    directives: &PresentationDirectives,
    keys: impl Iterator<Item = &'k steer::Key>,
    contexts: &mut Vec<PresentedDirectiveContext>,
) -> Result<(), String> {
    start_quote_block(rendered);
    for (index, key) in keys.enumerate() {
        let directive = directives
            .directives
            .get(key)
            .ok_or_else(|| format!("canonical acknowledgment recognition lost directive {key}"))?;
        if index > 0 {
            rendered.push_str("\n\n");
        }
        let start = rendered.len();
        append_acknowledgement_quote(rendered, &directive.text);
        contexts.push(PresentedDirectiveContext {
            key: key.clone(),
            range: start..rendered.len(),
        });
    }
    rendered.push_str("\n\n");
    Ok(())
}

/// Every presentable SYN packet in `text`, in source order. Only an event
/// whose authoring occupant is known presents packets. One unproved occupant
/// address keeps its whole packet literal; operator and room-only routes need
/// no occupant proof.
fn placed_sends(
    text: &str,
    directives: &PresentationDirectives,
    event: PresentationEvent,
) -> Vec<PlacedSend> {
    if event.author.is_none() || !text.contains(steer::ack::scan::message::MARKER) {
        return Vec::new();
    }
    steer::ack::scan::scan_messages(text)
        .into_iter()
        .filter_map(|message| {
            let publication = message.publication.ok()?;
            let routes = &publication.routes;
            let recipients = routes
                .iter()
                .filter_map(|route| match route.audience {
                    Some(steer::ack::scan::Audience::Occupant(address)) => {
                        Some(directives.recipients.get(&address).copied())
                    }
                    Some(steer::ack::scan::Audience::Input) | None => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let operator = routes
                .iter()
                .any(|route| route.audience == Some(steer::ack::scan::Audience::Input));
            let mut contexts = Vec::new();
            for context in routes.iter().filter_map(|route| route.context.as_ref()) {
                if !contexts.contains(context) {
                    contexts.push(context.clone());
                }
            }
            Some(PlacedSend {
                start: send_framing_start(text, message.header.start),
                body: publication.body,
                recipients,
                operator,
                contexts,
            })
        })
        .collect()
}

/// List or bracket framing that opens a packet's line belongs to the header,
/// so the quote that replaces the header never leaves an empty list item.
fn send_framing_start(text: &str, start: usize) -> usize {
    let line_start = text[..start].rfind('\n').map_or(0, |newline| newline + 1);
    let framing = text[line_start..start].trim();
    let ordinal = framing.strip_suffix(['.', ')']).is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    });
    if framing.is_empty() || ordinal || matches!(framing, "-" | "*" | "+" | "[") {
        line_start
    } else {
        start
    }
}

/// Copy one source segment the acknowledgement pass keeps, presenting every
/// authored packet wholly inside it as an outgoing quote block.
fn append_authored(
    rendered: &mut String,
    text: &str,
    segment: Range<usize>,
    capitalize: bool,
    placed: &mut Peekable<IntoIter<PlacedSend>>,
    sends: &mut Vec<PresentedSend>,
) {
    // A packet whose header an acknowledgement header consumed presents
    // nothing here.
    while placed
        .next_if(|send| send.body.start < segment.start)
        .is_some()
    {}
    let mut cursor = segment.start;
    let mut capitalize = capitalize;
    while let Some(send) = placed.next_if(|send| send.body.end <= segment.end) {
        let prose = &text[cursor..send.start.max(cursor)];
        if !prose.is_empty() {
            append_presented_source(rendered, prose, capitalize);
            capitalize = false;
        }
        start_quote_block(rendered);
        let start = rendered.len();
        append_acknowledgement_quote(rendered, &text[send.body.clone()]);
        sends.push(PresentedSend {
            range: start..rendered.len(),
            source: send.body.clone(),
            body: text[send.body.clone()].to_owned(),
            recipients: send.recipients,
            lanes: Vec::new(),
            operator: send.operator,
            contexts: send.contexts,
        });
        cursor =
            skip_line_breaks(text, skip_horizontal_space(text, send.body.end)).min(segment.end);
        // A following block must not continue this quote lazily.
        if !text[cursor..].trim().is_empty() {
            rendered.push_str("\n\n");
        }
    }
    append_presented_source(rendered, &text[cursor..segment.end], capitalize);
}

/// Reuse the protocol scanner with display standing, not retirement authority.
pub(super) fn scan_presentations(
    text: &str,
    standing: &EventDirectives<'_>,
) -> Vec<steer::ack::scan::Acknowledgement> {
    steer::ack::scan::scan_acknowledgements(text, &DisplayStanding(standing))
}

/// Display standing: every allocated key and the lane seal read as live, so the
/// scanner reports each with the header's disposition and presentation decides
/// what it earned.
struct DisplayStanding<'a>(&'a EventDirectives<'a>);

impl steer::Standing for DisplayStanding<'_> {
    fn disposition(&self, key: &steer::Key) -> Option<wire::Disposition> {
        (!self.0.event.superseded_answers.contains(key)
            && (self.0.directives.directives.contains_key(key) || self.0.is_lane_seal(key)))
        .then_some(wire::Disposition::Live)
    }
}

/// How one authored directive presents inside an acknowledgement quote.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ContextState {
    /// Positively acknowledged and retired by the journal for this lane.
    Acknowledged,
    /// Positively acknowledged in prose while the journal still holds the key.
    Pending,
    /// Declined here: the lane answered and refused.
    Declined,
    /// Referenced context this response did not answer.
    Unmarked,
}

/// One acknowledgement header sorted into what it answered, quotes, and missed.
struct ClassifiedPacket {
    /// Disposition the header's marker spells.
    marker: Option<wire::Disposition>,
    /// States authenticated by this event, including non-authored system keys
    /// and the lane seal, which deliberately have no quoted context.
    states: Vec<ContextState>,
    contexts: Vec<(steer::Key, ContextState)>,
    missed: Vec<String>,
}

fn classify_packet(
    standing: &EventDirectives<'_>,
    recognitions: Vec<steer::ack::scan::Recognition>,
) -> ClassifiedPacket {
    let mut packet = ClassifiedPacket {
        marker: None,
        states: Vec::new(),
        contexts: Vec::new(),
        missed: Vec::new(),
    };
    for recognition in recognitions {
        let (key, disposition) = match recognition.outcome {
            steer::ack::scan::Outcome::Retired {
                key, disposition, ..
            } => (key, disposition),
            steer::ack::scan::Outcome::Missed { token, .. } => {
                packet.missed.push(token);
                continue;
            }
        };
        packet.marker.get_or_insert(disposition);
        let presents = standing.presents(&key, disposition, recognition.locus);
        if presents {
            packet
                .states
                .push(standing.context_state(&key, disposition));
        }
        if standing.authored(&key) {
            if !packet.contexts.iter().any(|(known, _)| known == &key) {
                let state = if presents {
                    standing.context_state(&key, disposition)
                } else {
                    ContextState::Unmarked
                };
                packet.contexts.push((key, state));
            }
        } else if !presents {
            packet.missed.push(key.to_string());
        }
    }
    packet
}

fn append_presented_source(rendered: &mut String, source: &str, capitalize: bool) {
    if !capitalize {
        rendered.push_str(source);
        return;
    }
    let terminal = source.trim_end_matches(char::is_whitespace).len();
    let (body, trailing) = source.split_at(terminal);
    let (body, normalize_colon) = body
        .strip_suffix(':')
        .map_or((body, false), |body| (body, true));
    let mut characters = body.chars();
    if let Some(first) = characters.next() {
        rendered.extend(first.to_uppercase());
        rendered.push_str(characters.as_str());
    }
    if normalize_colon {
        rendered.push('.');
    }
    rendered.push_str(trailing);
}

fn start_quote_block(rendered: &mut String) {
    while rendered.ends_with([' ', '\t']) {
        rendered.pop();
    }
    if !rendered.is_empty() && !rendered.ends_with('\n') {
        rendered.push('\n');
    }
}

/// Decorative punctuation that models sometimes wrap around a protocol header
/// is not authored response prose. Consume it only when the whole current-line
/// prefix is framing, so brackets and dashes after real words remain content.
fn presentation_framing_start(text: &str, floor: usize, start: usize) -> usize {
    let Some(prefix) = text.get(..start) else {
        return start;
    };
    let line_start = prefix
        .rfind('\n')
        .map_or(0, |newline| newline + 1)
        .max(floor);
    let Some(framing) = text.get(line_start..start) else {
        return start;
    };
    let mut decorated = false;
    for character in framing.chars() {
        match character {
            ' ' | '\t' => {}
            '[' | '\u{2013}' | '\u{2014}' => decorated = true,
            _ => return start,
        }
    }
    if decorated { line_start } else { start }
}

/// A closing square bracket can hide the ordinary separator from the protocol
/// scanner. Remove that close and any adjacent Unicode dash before the body is
/// capitalized and before Markdown turns inserted directive context into a
/// structural quote card.
fn presentation_framing_end(text: &str, end: usize) -> usize {
    let mut cursor = skip_horizontal_space(text, end);
    let mut bracketed = false;
    while text.get(cursor..).is_some_and(|tail| tail.starts_with(']')) {
        cursor += ']'.len_utf8();
        cursor = skip_horizontal_space(text, cursor);
        bracketed = true;
    }
    if !bracketed {
        return end;
    }
    while let Some(character) = text.get(cursor..).and_then(|tail| tail.chars().next()) {
        if !matches!(character, '\u{2013}' | '\u{2014}') {
            break;
        }
        cursor += character.len_utf8();
        cursor = skip_horizontal_space(text, cursor);
    }
    cursor
}

fn skip_horizontal_space(text: &str, mut cursor: usize) -> usize {
    while text
        .as_bytes()
        .get(cursor)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        cursor += 1;
    }
    cursor
}

fn skip_line_breaks(text: &str, mut cursor: usize) -> usize {
    while let Some(tail) = text.get(cursor..) {
        if tail.starts_with("\r\n") {
            cursor += 2;
        } else if tail.starts_with('\n') || tail.starts_with('\r') {
            cursor += 1;
        } else {
            break;
        }
    }
    cursor
}

fn acknowledgement_marker(disposition: wire::Disposition) -> Result<&'static str, String> {
    match disposition {
        wire::Disposition::Acked => Ok("ACK"),
        wire::Disposition::Declined => Ok("NAK"),
        wire::Disposition::Live
        | wire::Disposition::Lapsed
        | wire::Disposition::DeadLettered
        | wire::Disposition::Canceled => Err(format!(
            "acknowledgment scanner returned nonterminal disposition {disposition:?}"
        )),
    }
}

/// The one disposition a header contributes to its containing message.
///
/// Every quoted directive decides it, regardless of flavor: a refusal is an
/// answer the operator must see at a glance, so declined wins; a key the
/// journal still holds reads pending, so spoken acknowledgement never reads as
/// credit before steer::acked lands; otherwise retired keys read acknowledged.
/// A bare acknowledgement of the lane seal quotes nothing and still answers.
fn packet_acknowledgement(packet: &ClassifiedPacket) -> Option<wire::MessageAcknowledgement> {
    use wire::MessageAcknowledgement as Acknowledgement;
    if packet.states.contains(&ContextState::Declined) {
        Some(Acknowledgement::Declined)
    } else if packet.states.contains(&ContextState::Pending) {
        Some(Acknowledgement::Pending)
    } else if packet.states.contains(&ContextState::Acknowledged) {
        Some(Acknowledgement::Acknowledged)
    } else {
        None
    }
}

pub(super) fn append_acknowledgement_quote(rendered: &mut String, directive: &str) {
    for (index, line) in directive.split('\n').enumerate() {
        if index > 0 {
            rendered.push('\n');
        }
        rendered.push('>');
        if !line.is_empty() {
            rendered.push(' ');
        }
        rendered.push_str(line);
    }
}
