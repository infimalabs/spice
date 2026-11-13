//! Target-neutral Markdown composition and rendering for browser messages.
//!
//! The module owning the forms and cards is wasm-only, so every Markdown
//! operation that does not need a DOM lives here and is reachable from an
//! ordinary native test.
//! Nothing here may be duplicated there: a second copy is what let the shipped
//! quote drift from the tested one until the copies disagreed on blank input.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;

use pulldown_cmark::{CowStr, Event as MarkdownEvent, HeadingLevel, Options, Parser, Tag, TagEnd};
use wire::{Media, MessageDirective, MessageSend};

use crate::watchband;

mod compose;
pub(crate) use compose::{QuotedReply, block_quote, compose, compose_without_media};
mod code_blocks;
mod diagrams;
pub(crate) mod document;
mod image_flow;
pub(crate) mod inline;
mod math;
mod media_quotes;
mod quotes;
pub(crate) mod received;
mod tables;
pub(crate) mod task_text;
mod terminal;
#[cfg(test)]
pub(crate) use received::render_attributed_received;
pub(crate) use received::{render_received_with_media_quotes, render_received_with_quotes};

pub(crate) const QUOTE_ICON: &str = "<svg class=\"app-card-action__icon\" \
    width=\"15\" height=\"15\" viewBox=\"0 0 24 24\" aria-hidden=\"true\" focusable=\"false\">\
    <path d=\"M3.5 14.75C3.5 10 5.9 6.6 9.9 5.2l.75 1.6C8.3 8 7.1 9.6 6.95 11.1A3.75 3.75 0 1 1 3.5 14.75Z\
    M13 14.75C13 10 15.4 6.6 19.4 5.2l.75 1.6C17.8 8 16.6 9.6 16.45 11.1A3.75 3.75 0 1 1 13 14.75Z\" \
    fill=\"currentColor\" /></svg>";

/// The quote glyph, reduced and joined by a plus: quote into a new Draft.
pub(crate) const DRAFT_QUOTE_ICON: &str = "<svg class=\"app-card-action__icon\" \
    width=\"15\" height=\"15\" viewBox=\"0 0 24 24\" aria-hidden=\"true\" focusable=\"false\">\
    <path transform=\"translate(0 -1.5) scale(.72)\" \
    d=\"M3.5 14.75C3.5 10 5.9 6.6 9.9 5.2l.75 1.6C8.3 8 7.1 9.6 6.95 11.1A3.75 3.75 0 1 1 3.5 14.75Z\
    M13 14.75C13 10 15.4 6.6 19.4 5.2l.75 1.6C17.8 8 16.6 9.6 16.45 11.1A3.75 3.75 0 1 1 13 14.75Z\" \
    fill=\"currentColor\" />\
    <path d=\"M18 13.5v8M14 17.5h8\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2.2\" \
    stroke-linecap=\"round\" /></svg>";

/// The full drawer opener's chevron: it points toward the inline start, where
/// the opener travels; an open drawer's stylesheet turns it around.
pub(crate) const DRAWER_OPENER_ICON: &str = "<svg class=\"app-card-action__icon\" \
    width=\"15\" height=\"15\" viewBox=\"0 0 24 24\" aria-hidden=\"true\" focusable=\"false\">\
    <path d=\"M14.5 6.5 9 12l5.5 5.5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2.2\" \
    stroke-linecap=\"round\" stroke-linejoin=\"round\" /></svg>";

/// A virtual action drawer around `panel`: its full-size rounded opener is the
/// sole in-flow control and moves toward the inline start while every square
/// action appears between it and the fixed card edge. Every drawer shares one
/// `name`, so opening one closes any other.
pub(crate) fn drawer_html(panel: &str) -> String {
    format!(
        "<details class=\"message-actions-drawer\" name=\"message-actions-drawer\" \
         data-app-overlay-disclosure=\"\"><summary class=\"message-actions-opener\" \
         title=\"More actions\" aria-label=\"More actions\">{DRAWER_OPENER_ICON}</summary>\
         <span class=\"message-actions-drawer__panel\">{panel}</span></details>"
    )
}

/// The one parser option set for authored Markdown on every surface.
const OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_FOOTNOTES)
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_SMART_PUNCTUATION)
    .union(Options::ENABLE_MATH)
    .union(Options::ENABLE_GFM);

pub(crate) struct RenderedMarkdown {
    pub(crate) html: String,
    pub(crate) image_only: bool,
    pub(crate) image_count: usize,
    pub(crate) diagrams: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuoteAgent {
    pub(crate) accent: usize,
    pub(crate) label: String,
}

/// Everyone a quote byline can name. Each team member keys by its lane and the
/// occupant seated there now; a vacant member keys with no occupant, so a
/// message its lane sent still names the member while nobody sits in the seat.
/// Each principal is named by its recorded presentation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct QuoteAgents {
    pub(crate) members: BTreeMap<(wire::CanonicalUuid, Option<wire::CanonicalUuid>), QuoteAgent>,
    pub(crate) principals: wire::principal::PrincipalDirectory,
}

impl QuoteAgents {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl
    FromIterator<(
        (wire::CanonicalUuid, Option<wire::CanonicalUuid>),
        QuoteAgent,
    )> for QuoteAgents
{
    fn from_iter<I>(members: I) -> Self
    where
        I: IntoIterator<
            Item = (
                (wire::CanonicalUuid, Option<wire::CanonicalUuid>),
                QuoteAgent,
            ),
        >,
    {
        Self {
            members: members.into_iter().collect(),
            principals: wire::principal::PrincipalDirectory::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuoteSource {
    pub(crate) card_id: Option<String>,
    pub(crate) display_millis: i64,
    pub(crate) agent: Option<QuoteAgent>,
}

impl QuoteSource {
    pub(crate) fn directive(context: &MessageDirective) -> Self {
        Self {
            card_id: None,
            display_millis: context.authored.get().as_millisecond(),
            agent: None,
        }
    }
}

pub(crate) type QuoteSources = BTreeMap<String, QuoteSource>;

/// Borrowed identity and source authority used while presenting quotes, the
/// presented card's own source, and the lane that card speaks for.
#[derive(Clone, Copy)]
pub(crate) struct QuoteRenderContext<'a> {
    agents: &'a QuoteAgents,
    sources: &'a QuoteSources,
    card: &'a QuoteSource,
    speaker: Option<wire::CanonicalUuid>,
}

impl<'a> QuoteRenderContext<'a> {
    pub(crate) const fn new(
        agents: &'a QuoteAgents,
        sources: &'a QuoteSources,
        card: &'a QuoteSource,
        speaker: Option<wire::CanonicalUuid>,
    ) -> Self {
        Self {
            agents,
            sources,
            card,
            speaker,
        }
    }
}

pub(crate) use quotes::{
    referenced_quote_agents, referenced_quote_sources, referenced_quote_tasks,
};

/// Render received text through the one sanitized Markdown path used by cards.
#[cfg(test)]
pub(crate) fn render_received(markdown: &str) -> RenderedMarkdown {
    render_received_with_quotes(markdown, &[], &[], &QuoteAgents::new(), &BTreeMap::new())
}
/// Received Markdown after media expansion, with its annotations remapped.
pub(crate) struct ExpandedMedia<'a> {
    pub(crate) markdown: Cow<'a, str>,
    pub(crate) directives: Cow<'a, [MessageDirective]>,
    pub(crate) sends: Cow<'a, [MessageSend]>,
}

/// Expand authenticated display-only media while preserving authored labels,
/// titles, and exact annotation offsets.
pub(crate) fn expand_media_payloads<'a>(
    markdown: &'a str,
    media: &[Media],
    bindings: &[wire::MarkdownImageBinding],
    directives: &'a [MessageDirective],
    sends: &'a [MessageSend],
) -> Result<ExpandedMedia<'a>, wire::MediaError> {
    let mut expanded = ExpandedMedia {
        markdown: Cow::Borrowed(markdown),
        directives: Cow::Borrowed(directives),
        sends: Cow::Borrowed(sends),
    };
    if markdown.contains("](attachment:") {
        for item in media {
            let reference = item.reference_markdown()?;
            let replacement = item.markdown()?;
            let ranges = expanded
                .markdown
                .match_indices(&reference)
                .map(|(start, _)| start..start + reference.len())
                .collect::<Vec<_>>();
            replace_ranges(&mut expanded, &ranges, &replacement);
        }
    }
    wire::media::validate_markdown_image_bindings(bindings)?;
    for binding in bindings {
        let ranges = inline_image_destination_ranges(&expanded.markdown, &binding.destination);
        replace_ranges(&mut expanded, &ranges, &binding.media.data_url);
    }
    Ok(expanded)
}

fn replace_ranges(expanded: &mut ExpandedMedia<'_>, ranges: &[Range<usize>], replacement: &str) {
    if ranges.is_empty() {
        return;
    }
    let source = expanded.markdown.as_ref();
    let added = ranges.iter().fold(0usize, |added, range| {
        added.saturating_add(replacement.len().saturating_sub(range.len()))
    });
    let mut projected = String::with_capacity(source.len().saturating_add(added));
    let mut cursor = 0;
    for range in ranges {
        projected.push_str(&source[cursor..range.start]);
        projected.push_str(replacement);
        cursor = range.end;
    }
    projected.push_str(&source[cursor..]);

    let shift = |offset: u32| {
        let position = usize::try_from(offset).expect("validated annotation offset fits usize");
        let delta = ranges
            .iter()
            .take_while(|range| range.end <= position)
            .map(|range| {
                i64::try_from(replacement.len()).expect("media payload length fits i64")
                    - i64::try_from(range.len()).expect("media destination length fits i64")
            })
            .sum::<i64>();
        let shifted = i64::from(offset)
            .checked_add(delta)
            .expect("media annotation offset shift does not overflow");
        u32::try_from(shifted).expect("admitted media projection fits annotation offsets")
    };
    if !expanded.directives.is_empty() {
        expanded.directives = Cow::Owned(
            expanded
                .directives
                .iter()
                .map(|directive| MessageDirective {
                    start: shift(directive.start),
                    end: shift(directive.end),
                    ..directive.clone()
                })
                .collect(),
        );
    }
    if !expanded.sends.is_empty() {
        expanded.sends = Cow::Owned(
            expanded
                .sends
                .iter()
                .map(|send| MessageSend {
                    start: shift(send.start),
                    end: shift(send.end),
                    ..send.clone()
                })
                .collect(),
        );
    }
    expanded.markdown = Cow::Owned(projected);
}

fn inline_image_destination_ranges(markdown: &str, destination: &str) -> Vec<Range<usize>> {
    Parser::new_ext(markdown, Options::all())
        .into_offset_iter()
        .filter_map(|(event, source)| match event {
            MarkdownEvent::Start(Tag::Image {
                link_type: pulldown_cmark::LinkType::Inline,
                dest_url,
                ..
            }) if dest_url.as_ref() == destination => {
                inline_destination_range(&markdown[source.clone()])
                    .map(|range| source.start + range.start..source.start + range.end)
            }
            _ => None,
        })
        .collect()
}

fn inline_destination_range(image: &str) -> Option<Range<usize>> {
    let label_end = image.find("](")?;
    let mut start = label_end + 2;
    while image
        .as_bytes()
        .get(start)
        .is_some_and(u8::is_ascii_whitespace)
    {
        start += 1;
    }
    if image.as_bytes().get(start) == Some(&b'<') {
        let end = image[start + 1..].find('>')? + start + 1;
        return Some(start + 1..end);
    }
    let mut escaped = false;
    let mut parentheses = 0usize;
    for (relative, character) in image[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '(' => parentheses = parentheses.saturating_add(1),
            ')' if parentheses == 0 => return (relative != 0).then_some(start..start + relative),
            ')' => parentheses -= 1,
            character if character.is_ascii_whitespace() && parentheses == 0 => {
                return (relative != 0).then_some(start..start + relative);
            }
            _ => {}
        }
    }
    None
}

/// The source a card stands for when it sent no SYN, so its own source is
/// never named.
const SENDLESS_CARD: QuoteSource = QuoteSource {
    card_id: None,
    display_millis: 0,
    agent: None,
};

/// Render one assistant prose card: received text as
/// [`render_received_with_quotes`] renders it, except that terminal prose gains
/// a period when unpunctuated and presents a terminal colon as a period, the
/// card's attribution slots lead it and each answer after a quote chain, and
/// each SYN the card sent reads as an outgoing quote to its recipients.
#[cfg(test)]
pub(crate) fn render_prose_card(
    markdown: &str,
    directives: &[MessageDirective],
    sends: &[MessageSend],
    quotes: &[wire::QuotePresentation],
    quote_agents: &QuoteAgents,
    quote_sources: &QuoteSources,
) -> RenderedMarkdown {
    render_prose_card_with_media_quotes(
        markdown,
        directives,
        sends,
        quotes,
        QuoteRenderContext::new(quote_agents, quote_sources, &SENDLESS_CARD, None),
        &[],
    )
}

/// Render an assistant prose card with exact targets for its retained images.
pub(crate) fn render_prose_card_with_media_quotes(
    markdown: &str,
    directives: &[MessageDirective],
    sends: &[MessageSend],
    quotes: &[wire::QuotePresentation],
    quote_context: QuoteRenderContext<'_>,
    media_quotes: &[wire::MediaQuote],
) -> RenderedMarkdown {
    render(
        markdown,
        Annotations { directives, sends },
        quotes,
        quote_context.agents,
        quote_context.sources,
        media_quotes,
        Finish::ProseCard {
            card: quote_context.card,
            speaker: quote_context.speaker,
            punctuate: true,
        },
    )
}

/// Render one room card's authored words as written, with the card's own
/// signoff slot leading them as a prose card's does. `quotes` presents the
/// words' quote links, as the resident resolved them, bylined from `agents`
/// and `sources` as a message card's are.
pub(crate) fn render_room_card(
    markdown: &str,
    quotes: &[wire::QuotePresentation],
    agents: &QuoteAgents,
    sources: &QuoteSources,
) -> RenderedMarkdown {
    render(
        markdown,
        Annotations::default(),
        quotes,
        agents,
        sources,
        &[],
        Finish::ProseCard {
            card: &SENDLESS_CARD,
            speaker: None,
            punctuate: false,
        },
    )
}

/// Render one resolved or typed review quote as recursively attributed text
/// hunks. A typed review supplies its task separately from finding Markdown.
fn render_quote_card_body<'r>(
    markdown: &'r str,
    quotes: &[wire::QuotePresentation],
    quote_agents: &QuoteAgents,
    quote_sources: &QuoteSources,
    media_quotes: &[wire::MediaQuote],
    finish: Finish<'r>,
) -> RenderedMarkdown {
    render(
        markdown,
        Annotations::default(),
        quotes,
        quote_agents,
        quote_sources,
        media_quotes,
        finish,
    )
}

/// How a rendering presents its body.
///
/// A prose card's or quote card's `speaker` is the lane its own signoff
/// already names, so a quote inside it whose sole recipient is that lane
/// leaves the receiver implied rather than repeating it.
enum Finish<'r> {
    /// Received text as written.
    Received,
    /// Rack documents resolve destinations within their admitted root.
    Document {
        resolve: &'r dyn Fn(&str, bool) -> String,
    },
    /// Assistant prose split into attributed hunks around every quote, led
    /// where it sent a SYN by `card`, its own source.
    ProseCard {
        card: &'r QuoteSource,
        speaker: Option<wire::CanonicalUuid>,
        /// Whether unpunctuated terminal prose gains a period, as an
        /// assistant's final prose does; authored room words stay as written.
        punctuate: bool,
    },
    /// A quote card split into hunks attributed to its authenticated source.
    QuoteCard {
        attribution: image_flow::Attribution,
        /// Typed context leading the Markdown body without being recovered from
        /// its source envelope.
        leading_task: Option<&'r wire::TaskHandle>,
        /// Typed system guidance following the source's Markdown body.
        trailing_guidance: Option<&'r str>,
        /// Quote shells around this card's hunks, raw and authenticated alike.
        quote_depth: usize,
        speaker: Option<wire::CanonicalUuid>,
    },
    /// Authored task text nested under a detail's own headings, whose chips
    /// name only the tasks `resolves` admits.
    TaskText {
        resolves: &'r dyn Fn(&wire::TaskHandle) -> bool,
    },
}

impl<'r> Finish<'r> {
    const fn source_card(&self) -> Option<&'r QuoteSource> {
        match self {
            Self::ProseCard { card, .. } => Some(*card),
            Self::Received
            | Self::QuoteCard { .. }
            | Self::TaskText { .. }
            | Self::Document { .. } => None,
        }
    }

    /// The lane this rendering's surrounding signoff names, if any.
    const fn speaker(&self) -> Option<wire::CanonicalUuid> {
        match self {
            Self::ProseCard { speaker, .. } | Self::QuoteCard { speaker, .. } => *speaker,
            Self::Received | Self::TaskText { .. } | Self::Document { .. } => None,
        }
    }

    const fn leading_task(&self) -> Option<&'r wire::TaskHandle> {
        match self {
            Self::QuoteCard { leading_task, .. } => *leading_task,
            Self::Received
            | Self::ProseCard { .. }
            | Self::TaskText { .. }
            | Self::Document { .. } => None,
        }
    }

    const fn trailing_guidance(&self) -> Option<&'r str> {
        match self {
            Self::QuoteCard {
                trailing_guidance, ..
            } => *trailing_guidance,
            Self::Received
            | Self::ProseCard { .. }
            | Self::TaskText { .. }
            | Self::Document { .. } => None,
        }
    }

    fn attribution(self, events: &mut [MarkdownEvent<'_>]) -> Option<image_flow::Attribution> {
        match self {
            Self::Received | Self::TaskText { .. } | Self::Document { .. } => None,
            Self::ProseCard { punctuate, .. } => {
                if punctuate {
                    terminal::period(events);
                }
                Some(card_attribution())
            }
            Self::QuoteCard { attribution, .. } => Some(attribution),
        }
    }
}

fn card_attribution() -> image_flow::Attribution {
    image_flow::Attribution {
        controls_target: String::new(),
        signoff: SIGNOFF_SLOT.to_owned(),
        disposition: String::new(),
        badge: String::new(),
        task_tail: String::new(),
        repeat: true,
    }
}

/// Headings in task text nest under the detail's own section headings.
const TASK_TEXT_HEADING: HeadingLevel = HeadingLevel::H5;

/// The resident's annotations of one message body: the directives it carried
/// and the sends it made. They travel together through math normalization and
/// event parsing.
#[derive(Clone, Copy, Default)]
struct Annotations<'a> {
    directives: &'a [MessageDirective],
    sends: &'a [MessageSend],
}

fn render<'r>(
    markdown: &'r str,
    annotations: Annotations<'r>,
    quotes: &[wire::QuotePresentation],
    quote_agents: &QuoteAgents,
    quote_sources: &QuoteSources,
    media_quotes: &[wire::MediaQuote],
    finish: Finish<'r>,
) -> RenderedMarkdown {
    let math = math::normalize(markdown, annotations.directives, annotations.sends, OPTIONS);
    let markdown = math.markdown;
    let mut media_quotes = media_quotes::Matcher::new(media_quotes, &math.directives);
    let (events, outer_quotes) = parse_events(
        &markdown,
        math.directives,
        math.sends,
        quote_agents,
        quote_sources,
        &finish,
    );
    let events = coalesce_text_events(events);
    let quote_depth = match &finish {
        Finish::QuoteCard { quote_depth, .. } => *quote_depth,
        Finish::Received
        | Finish::ProseCard { .. }
        | Finish::TaskText { .. }
        | Finish::Document { .. } => 0,
    };
    let resolves: &dyn Fn(&wire::TaskHandle) -> bool = match &finish {
        Finish::TaskText { resolves } => *resolves,
        Finish::Document { .. } => &|_| false,
        Finish::Received | Finish::ProseCard { .. } | Finish::QuoteCard { .. } => &|_| true,
    };
    let heading = matches!(finish, Finish::TaskText { .. }).then_some(TASK_TEXT_HEADING);
    let events = quotes::expand(
        events,
        quotes,
        quote_agents,
        quote_sources,
        quote_depth,
        finish.speaker(),
    );
    let (events, diagrams) = diagrams::extract(events);
    let has_diagrams = !diagrams.is_empty();
    let image_count = markdown_image_only_count(&events, has_diagrams);
    let image_only = image_count > 0;
    // Diagram controls and nested block structure must not collapse into an image row.
    let mut events =
        decorate_markdown_images(events, image_only && !has_diagrams, &mut media_quotes);
    let attribution = finish.attribution(&mut events);
    let quote_attributions = outer_quotes
        .iter()
        .map(|quote| quote.attribution.clone())
        .collect::<Vec<_>>();
    let events = image_flow::flow(
        &events,
        attribution.as_ref(),
        &quote_attributions,
        quote_depth,
    );
    // Code blocks and link text stay literal: a chip inside a link would be a
    // control nested in another.
    let mut literal = 0usize;
    let events = events.into_iter().flat_map(|event| match event {
        MarkdownEvent::Start(Tag::CodeBlock(_) | Tag::Link { .. }) => {
            literal += 1;
            vec![event]
        }
        MarkdownEvent::End(TagEnd::CodeBlock | TagEnd::Link) => {
            literal = literal.saturating_sub(1);
            vec![event]
        }
        _ if literal > 0 => vec![event],
        MarkdownEvent::Start(Tag::Heading {
            level,
            id,
            classes,
            attrs,
        }) => vec![MarkdownEvent::Start(Tag::Heading {
            level: heading.unwrap_or(level),
            id,
            classes,
            attrs,
        })],
        MarkdownEvent::End(TagEnd::Heading(level)) => {
            vec![MarkdownEvent::End(TagEnd::Heading(
                heading.unwrap_or(level),
            ))]
        }
        event => inline_task_chips(event, resolves),
    });
    let mut events = code_blocks::copyable(events);
    bind_adjacent_task_chip_punctuation(&mut events);
    accent_directive_contexts(&mut events, &outer_quotes);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, tables::lay_out_tables(events));
    RenderedMarkdown {
        html,
        image_only,
        image_count,
        diagrams,
    }
}

/// One outermost raw block quote, in document order.
struct OuterQuote {
    /// Attribution repeated around each direct run that this directive or
    /// sent SYN owns.
    attribution: Option<image_flow::Attribution>,
    /// The accent slot of the speaker a directive context or sent SYN
    /// attributes.
    accent: Option<usize>,
}

/// Parse sanitized Markdown, inline each exact classification as its pill, and
/// record every outermost directive context's and sent SYN's hunk attribution.
/// A prose card's own source leads each SYN it sent, and its speaker is the
/// lane each directive context's receiver may already be implied by.
fn parse_events<'m>(
    markdown: &'m str,
    directives: Vec<MessageDirective>,
    sends: Vec<MessageSend>,
    quote_agents: &QuoteAgents,
    quote_sources: &QuoteSources,
    finish: &Finish<'m>,
) -> (Vec<MarkdownEvent<'m>>, Vec<OuterQuote>) {
    let card = finish.source_card();
    // The resident writes every directive context and every sent SYN body as
    // one outermost block quote beginning exactly at its start.
    let mut contexts = directives
        .into_iter()
        .map(|directive| (directive.start as usize, directive))
        .collect::<BTreeMap<_, _>>();
    let mut sends = sends
        .into_iter()
        .map(|send| (send.start as usize, send))
        .collect::<BTreeMap<_, _>>();
    let mut outer_quotes = Vec::new();
    let mut quote_depth = 0usize;
    let mut skipped_review_depth = 0usize;
    let mut omitted_image = false;
    let mut events = Vec::new();
    if let Some(task) = finish.leading_task() {
        quotes::push_typed_paragraph(&mut events, task.as_str());
    }
    for (event, source) in Parser::new_ext(markdown, OPTIONS).into_offset_iter() {
        if quotes::skip_review_envelope_event(&event, &mut skipped_review_depth) {
            continue;
        }
        match &event {
            MarkdownEvent::Start(Tag::BlockQuote(_)) => {
                if quote_depth == 0 {
                    if let Some(context) = contexts.remove(&source.start) {
                        if context.review.is_some() {
                            events.push(MarkdownEvent::InlineHtml(
                                quotes::review_html(
                                    &context,
                                    quote_agents,
                                    quote_sources,
                                    finish.speaker(),
                                )
                                .into(),
                            ));
                            skipped_review_depth = 1;
                            continue;
                        }
                        outer_quotes.push(OuterQuote {
                            accent: quotes::speaker_accent(
                                &context.attribution.author,
                                &context.quote_target.to_string(),
                                quote_agents,
                                quote_sources,
                            ),
                            attribution: Some(quotes::directive_attribution(
                                &context,
                                quote_agents,
                                quote_sources,
                                finish.speaker(),
                            )),
                        });
                    } else if let Some(send) = sends.remove(&source.start) {
                        outer_quotes.push(OuterQuote {
                            accent: quotes::send_accent(&send, quote_agents),
                            attribution: Some(quotes::send_attribution(&send, quote_agents, card)),
                        });
                    } else {
                        outer_quotes.push(OuterQuote {
                            attribution: None,
                            accent: None,
                        });
                    }
                }
                quote_depth += 1;
            }
            MarkdownEvent::End(TagEnd::BlockQuote(_)) => quote_depth -= 1,
            _ => {}
        }
        let event = match finish {
            Finish::Document { resolve } => document::sanitize(event, *resolve, &mut omitted_image),
            _ => sanitize_markdown_event(event),
        };
        events.push(event);
    }
    if let Some(guidance) = finish.trailing_guidance() {
        quotes::push_typed_paragraph(&mut events, guidance);
    }
    (events, outer_quotes)
}

/// Open each directive context whose speaker has an accent slot with that
/// slot, as a quote card carries it on its own shell, so the context's rail
/// wears its speaker's accent.
fn accent_directive_contexts(events: &mut [MarkdownEvent<'_>], outer_quotes: &[OuterQuote]) {
    let mut outer_quotes = outer_quotes.iter();
    let mut depth = 0usize;
    for event in events {
        match event {
            MarkdownEvent::Start(Tag::BlockQuote(kind)) => {
                let accent = if depth == 0 {
                    outer_quotes.next().and_then(|quote| quote.accent)
                } else {
                    None
                };
                depth += 1;
                if let (None, Some(accent)) = (kind, accent) {
                    *event = MarkdownEvent::Html(
                        format!("<blockquote data-accent=\"{accent}\">\n").into(),
                    );
                }
            }
            MarkdownEvent::End(TagEnd::BlockQuote(_)) => depth -= 1,
            _ => {}
        }
    }
}

/// Where a prose card's own attribution mounts (`browser::mermaid`).
const SIGNOFF_SLOT: &str = "<span class=\"message-signoff-slot\" data-signoff-slot=\"\"></span>";

fn coalesce_text_events(events: Vec<MarkdownEvent<'_>>) -> Vec<MarkdownEvent<'_>> {
    let mut coalesced = Vec::with_capacity(events.len());
    for event in events {
        if let (Some(MarkdownEvent::Text(previous)), MarkdownEvent::Text(next)) =
            (coalesced.last_mut(), &event)
        {
            let mut joined = previous.to_string();
            joined.push_str(next);
            *previous = joined.into();
        } else {
            coalesced.push(event);
        }
    }
    coalesced
}

fn acknowledgement_pill_html_with_attributes(
    classification: &str,
    acknowledgement: &str,
    label: &str,
    text: &str,
    attributes: &str,
) -> String {
    format!(
        "<span class=\"message-ack-pill\" data-message-classification=\"{classification}\" \
         data-acknowledgement=\"{acknowledgement}\"{attributes} aria-label=\"{label}\">\
         <span class=\"message-ack-label\">{text}</span></span>"
    )
}
/// Whether two rendered messages have identical measured content.
///
/// Message-level acknowledgment badges are mounted in hunk signoffs, outside
/// this rendered Markdown, so settlement leaves this geometry unchanged.
#[cfg(target_arch = "wasm32")]
pub(crate) fn same_measured_shape(left: &RenderedMarkdown, right: &RenderedMarkdown) -> bool {
    left.html == right.html
        && left.image_only == right.image_only
        && left.image_count == right.image_count
        && left.diagrams == right.diagrams
}

fn markdown_image_only_count(events: &[MarkdownEvent<'_>], has_diagrams: bool) -> usize {
    let mut image_depth = 0usize;
    let mut images = 0usize;
    for event in events {
        match event {
            MarkdownEvent::Start(Tag::Image { .. }) => {
                image_depth += 1;
                images += 1;
            }
            MarkdownEvent::End(TagEnd::Image) => image_depth = image_depth.saturating_sub(1),
            _ if image_depth > 0 => {}
            // Raw HTML was converted to text before extraction; these are owned slots.
            MarkdownEvent::Html(_) => images += 1,
            MarkdownEvent::Start(Tag::BlockQuote(_) | Tag::List(_) | Tag::Item)
            | MarkdownEvent::End(TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item)
                if has_diagrams => {}
            MarkdownEvent::Start(Tag::Paragraph)
            | MarkdownEvent::End(TagEnd::Paragraph)
            | MarkdownEvent::Start(Tag::Link { .. })
            | MarkdownEvent::End(TagEnd::Link)
            | MarkdownEvent::SoftBreak
            | MarkdownEvent::HardBreak => {}
            MarkdownEvent::Text(text) if text.trim().is_empty() => {}
            _ => return 0,
        }
    }
    images
}

fn decorate_markdown_images<'a>(
    events: Vec<MarkdownEvent<'a>>,
    image_only: bool,
    media_quotes: &mut media_quotes::Matcher,
) -> Vec<MarkdownEvent<'a>> {
    if image_only {
        return decorate_image_only_markdown(events, media_quotes);
    }
    let mut decorated = Vec::with_capacity(events.len());
    let mut index = 0usize;
    let mut image_stack = false;
    while index < events.len() {
        match &events[index] {
            MarkdownEvent::Start(Tag::Paragraph) => {
                image_stack = paragraph_is_image_stack(&events[index + 1..]);
                if image_stack {
                    decorated.push(MarkdownEvent::InlineHtml(image_flow::STACK_OPEN.into()));
                } else {
                    decorated.push(events[index].clone());
                }
            }
            MarkdownEvent::End(TagEnd::Paragraph) if image_stack => {
                decorated.push(MarkdownEvent::InlineHtml(image_flow::STACK_CLOSE.into()));
                image_stack = false;
            }
            MarkdownEvent::Start(Tag::Link { .. })
                if linked_image_end(&events, index).is_some() =>
            {
                let image_index = index + 1;
                let image_end =
                    push_decorated_image(&events, image_index, &mut decorated, media_quotes);
                index = image_end + 1;
            }
            MarkdownEvent::Start(Tag::Image { .. }) => {
                index = push_decorated_image(&events, index, &mut decorated, media_quotes);
            }
            _ => decorated.push(events[index].clone()),
        }
        index += 1;
    }
    decorated
}

fn decorate_image_only_markdown<'a>(
    events: Vec<MarkdownEvent<'a>>,
    media_quotes: &mut media_quotes::Matcher,
) -> Vec<MarkdownEvent<'a>> {
    let mut decorated = Vec::with_capacity(events.len());
    decorated.push(MarkdownEvent::InlineHtml(image_flow::STACK_OPEN.into()));
    let mut index = 0usize;
    while index < events.len() {
        match &events[index] {
            MarkdownEvent::Start(Tag::Image { .. }) => {
                index = push_decorated_image(&events, index, &mut decorated, media_quotes);
            }
            MarkdownEvent::Start(Tag::Paragraph)
            | MarkdownEvent::End(TagEnd::Paragraph)
            | MarkdownEvent::Start(Tag::Link { .. })
            | MarkdownEvent::End(TagEnd::Link)
            | MarkdownEvent::SoftBreak
            | MarkdownEvent::HardBreak => {}
            MarkdownEvent::Text(text) if text.trim().is_empty() => {}
            _ => debug_assert!(
                false,
                "image-only classification admitted a non-image event"
            ),
        }
        index += 1;
    }
    decorated.push(MarkdownEvent::InlineHtml(image_flow::STACK_CLOSE.into()));
    decorated
}

fn linked_image_end(events: &[MarkdownEvent<'_>], index: usize) -> Option<usize> {
    if !matches!(
        events.get(index),
        Some(MarkdownEvent::Start(Tag::Link { .. }))
    ) || !matches!(
        events.get(index + 1),
        Some(MarkdownEvent::Start(Tag::Image { .. }))
    ) {
        return None;
    }
    let image_end = events[index + 2..]
        .iter()
        .position(|event| matches!(event, MarkdownEvent::End(TagEnd::Image)))?
        + index
        + 2;
    matches!(
        events.get(image_end + 1),
        Some(MarkdownEvent::End(TagEnd::Link))
    )
    .then_some(image_end + 1)
}

fn push_decorated_image<'a>(
    events: &[MarkdownEvent<'a>],
    index: usize,
    decorated: &mut Vec<MarkdownEvent<'a>>,
    media_quotes: &mut media_quotes::Matcher,
) -> usize {
    let MarkdownEvent::Start(Tag::Image {
        dest_url, title, ..
    }) = &events[index]
    else {
        return index;
    };
    let end = events[index + 1..]
        .iter()
        .position(|event| matches!(event, MarkdownEvent::End(TagEnd::Image)))
        .map_or(events.len(), |offset| index + offset + 1);
    let alt = image_alt_text(&events[index + 1..end]);
    let media_quote = media_quotes.take(dest_url, &alt);
    decorated.push(MarkdownEvent::InlineHtml(
        media_quotes::image_preview_html(dest_url, title, &alt, media_quote).into(),
    ));
    end
}

fn paragraph_is_image_stack(events: &[MarkdownEvent<'_>]) -> bool {
    let mut image_depth = 0usize;
    let mut images = 0usize;
    for event in events {
        match event {
            MarkdownEvent::End(TagEnd::Paragraph) if image_depth == 0 => return images > 0,
            MarkdownEvent::Start(Tag::Image { .. }) => {
                image_depth += 1;
                images += 1;
            }
            MarkdownEvent::End(TagEnd::Image) => image_depth = image_depth.saturating_sub(1),
            _ if image_depth > 0 => {}
            MarkdownEvent::SoftBreak | MarkdownEvent::HardBreak => {}
            MarkdownEvent::Text(text) if text.trim().is_empty() => {}
            _ => return false,
        }
    }
    false
}

fn image_alt_text(events: &[MarkdownEvent<'_>]) -> String {
    let mut alt = String::new();
    for event in events {
        match event {
            MarkdownEvent::Text(text) | MarkdownEvent::Code(text) => alt.push_str(text),
            MarkdownEvent::SoftBreak | MarkdownEvent::HardBreak => alt.push(' '),
            _ => {}
        }
    }
    alt
}

fn data_image_has_malformed_base64(source: &str) -> bool {
    source.split_once(',').is_some_and(|(metadata, payload)| {
        let metadata = metadata.to_ascii_lowercase();
        metadata.starts_with("data:image/")
            && metadata.ends_with(";base64")
            && !canonical_base64_payload(payload)
    })
}

fn canonical_base64_payload(payload: &str) -> bool {
    if payload.is_empty() || !payload.len().is_multiple_of(4) {
        return false;
    }
    let body = payload.trim_end_matches('=');
    let padding = payload.len() - body.len();
    padding <= 2
        && body
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
}

fn escape_html_attribute(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// Chip every mentioned task `resolves` admits, in prose or as a whole code span.
fn inline_task_chips<'e>(
    event: MarkdownEvent<'e>,
    resolves: &dyn Fn(&wire::TaskHandle) -> bool,
) -> Vec<MarkdownEvent<'e>> {
    match event {
        MarkdownEvent::Text(text) => inline_task_text(text, resolves),
        MarkdownEvent::Code(code) => match watchband::canonical_task_handle(&code) {
            Some(task) if resolves(&task) => {
                vec![MarkdownEvent::InlineHtml(
                    task_chip_html(&task, None, "").into(),
                )]
            }
            _ => vec![MarkdownEvent::Code(code)],
        },
        other => vec![other],
    }
}

const TASK_CHIP_HTML_START: &str = "<span class=\"message-task-chip\"";
const TASK_CHIP_RUN_OPEN: &str = "<span class=\"message-task-chip-run\">";
const TASK_CHIP_RUN_CLOSE: &str = "</span>";

fn bind_adjacent_task_chip_punctuation(events: &mut [MarkdownEvent<'_>]) {
    for index in 0..events.len().saturating_sub(1) {
        let [MarkdownEvent::InlineHtml(chip), MarkdownEvent::Text(text)] =
            &mut events[index..=index + 1]
        else {
            continue;
        };
        if !chip.starts_with(TASK_CHIP_HTML_START) {
            continue;
        }
        let punctuation_end = task_chip_punctuation_end(text, 0);
        if punctuation_end == 0 {
            continue;
        }
        let mut run = String::with_capacity(
            TASK_CHIP_RUN_OPEN.len() + chip.len() + punctuation_end + TASK_CHIP_RUN_CLOSE.len(),
        );
        run.push_str(TASK_CHIP_RUN_OPEN);
        run.push_str(chip);
        run.push_str(&text[..punctuation_end]);
        run.push_str(TASK_CHIP_RUN_CLOSE);
        let remainder = (punctuation_end < text.len()).then(|| text[punctuation_end..].to_owned());
        *chip = run.into();
        *text = remainder.map_or(CowStr::Borrowed(""), CowStr::from);
    }
}

fn inline_task_text<'e>(
    text: CowStr<'e>,
    resolves: &dyn Fn(&wire::TaskHandle) -> bool,
) -> Vec<MarkdownEvent<'e>> {
    let mentions = watchband::task_mentions(&text)
        .into_iter()
        .filter(|(_, task)| resolves(task))
        .collect::<Vec<_>>();
    if mentions.is_empty() {
        return vec![MarkdownEvent::Text(text)];
    }
    let mut events = Vec::new();
    let mut cursor = 0;
    for (range, task) in mentions {
        if cursor < range.start {
            events.push(MarkdownEvent::Text(
                text[cursor..range.start].to_owned().into(),
            ));
        }
        let punctuation_end = task_chip_punctuation_end(&text, range.end);
        let context = task_chip_context(&text, &range);
        events.push(MarkdownEvent::InlineHtml(
            task_chip_html(&task, context.as_deref(), &text[range.end..punctuation_end]).into(),
        ));
        cursor = punctuation_end;
    }
    if cursor < text.len() {
        events.push(MarkdownEvent::Text(text[cursor..].to_owned().into()));
    }
    events
}

fn task_chip_punctuation_end(text: &str, start: usize) -> usize {
    text[start..]
        .char_indices()
        .take_while(|(_, character)| {
            matches!(
                character,
                '.' | ',' | ';' | ':' | '!' | '?' | '…' | ')' | ']' | '}' | '\'' | '"' | '’' | '”'
            )
        })
        .last()
        .map_or(start, |(offset, character)| {
            start + offset + character.len_utf8()
        })
}

const TASK_CHIP_CONTEXT_CHARS: usize = 180;

fn task_chip_context(text: &str, mention: &std::ops::Range<usize>) -> Option<String> {
    let mut compact = String::with_capacity(text.len() - mention.len());
    for word in text[..mention.start]
        .split_whitespace()
        .chain(text[mention.end..].split_whitespace())
    {
        let word = if compact.is_empty() {
            word.trim_start_matches(task_context_edge)
        } else {
            word
        };
        if word.is_empty() {
            continue;
        }
        if !compact.is_empty() {
            compact.push(' ');
        }
        compact.push_str(word);
    }
    let trimmed = compact.trim_end_matches(task_context_edge).len();
    compact.truncate(trimmed);
    if compact.is_empty() {
        return None;
    }
    if compact.chars().count() > TASK_CHIP_CONTEXT_CHARS {
        let end = compact
            .char_indices()
            .nth(TASK_CHIP_CONTEXT_CHARS - 1)
            .map_or(compact.len(), |(index, _)| index);
        compact.truncate(end);
        while compact.ends_with(' ') {
            compact.pop();
        }
        compact.push('…');
    }
    Some(compact)
}

fn task_context_edge(character: char) -> bool {
    matches!(
        character,
        ',' | '.' | ';' | ':' | '·' | '—' | '–' | '-' | '|'
    )
}

pub(super) fn task_chip_html(
    task: &wire::TaskHandle,
    context: Option<&str>,
    trailing_punctuation: &str,
) -> String {
    // TaskHandle parsing admits only the canonical ASCII handle grammar, so
    // these fixed attributes cannot acquire markup from authored prose. The
    // trailing text is restricted to punctuation that is safe in HTML text.
    let handle = task.to_string();
    let title = context
        .map(escape_html_attribute)
        .unwrap_or_else(|| format!("Open task {handle}"));
    let (run_open, run_close) = if trailing_punctuation.is_empty() {
        ("", "")
    } else {
        (TASK_CHIP_RUN_OPEN, TASK_CHIP_RUN_CLOSE)
    };
    format!(
        "{run_open}{TASK_CHIP_HTML_START} data-task-handle=\"{handle}\" role=\"button\" \
         tabindex=\"0\" aria-label=\"Open task {handle}\" title=\"{title}\">\
         <span class=\"message-task-chip-label\">{handle}</span></span>\
         {trailing_punctuation}{run_close}"
    )
}

fn sanitize_markdown_event(event: MarkdownEvent<'_>) -> MarkdownEvent<'_> {
    match event {
        MarkdownEvent::Start(tag) => MarkdownEvent::Start(sanitize_markdown_tag(tag)),
        MarkdownEvent::Html(raw) | MarkdownEvent::InlineHtml(raw) => MarkdownEvent::Text(raw),
        other => other,
    }
}

fn sanitize_markdown_tag(tag: Tag<'_>) -> Tag<'_> {
    match tag {
        Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        } => Tag::Link {
            link_type,
            dest_url: sanitize_markdown_url(dest_url, false),
            title,
            id,
        },
        Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        } => Tag::Image {
            link_type,
            dest_url: sanitize_markdown_url(dest_url, true),
            title,
            id,
        },
        other => other,
    }
}

fn sanitize_markdown_url(url: CowStr<'_>, image: bool) -> CowStr<'_> {
    let candidate = url.trim().to_ascii_lowercase();
    let safe_scheme = candidate.starts_with("https://")
        || candidate.starts_with("http://")
        || (!image
            && (candidate.starts_with("mailto:") || candidate.starts_with("spice://quote/v1/")))
        || (image && candidate.starts_with("data:image/"));
    let relative = !candidate.starts_with("//")
        && !candidate
            .split(['/', '#', '?'])
            .next()
            .is_some_and(|head| head.contains(':'));
    if safe_scheme || relative {
        url
    } else {
        CowStr::Borrowed("")
    }
}

#[cfg(test)]
mod tests;
