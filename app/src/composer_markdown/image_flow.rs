//! Recursive attribution-hunk layout for message and quote Markdown.
//!
//! A speaker owns each direct run of content between nested quote blocks. Every
//! non-empty run leads with that speaker's controls and attribution, followed in
//! DOM order by all image stacks in the run and then its remaining content.

use std::ops::Range;

use pulldown_cmark::{Event as MarkdownEvent, Tag, TagEnd};

pub(super) const STACK_OPEN: &str = "<div class=\"message-image-stack\">";
pub(super) const STACK_CLOSE: &str = "</div>";
const HUNK_OPEN: &str = "<div class=\"message-hunk\">";
const HUNK_CONTENT_OPEN: &str = "<div class=\"message-hunk-content\">";
const HUNK_CLOSE: &str = "</div>";

/// Authenticated ownership and provenance for one Markdown layer.
#[derive(Clone)]
pub(super) struct Attribution {
    /// Stable browser control model key. Empty names the containing message.
    pub(super) controls_target: String,
    /// Source age and name, or the containing card's Yew signoff slot.
    pub(super) signoff: String,
    /// Settled disposition of a quoted source, repeated with every signoff.
    pub(super) disposition: String,
    /// The quoted directive's own kind, leading only the hunk where its words
    /// begin.
    pub(super) badge: String,
    /// Board provenance following the attributed layer's final word once.
    pub(super) task_tail: String,
    /// Whether the source lead repeats when nested quotes split this layer.
    pub(super) repeat: bool,
}

impl Attribution {
    fn lead_html(&self, quote_depth: usize, opening: bool) -> String {
        let show = opening || self.repeat;
        let controls = if show {
            format!(
                "<span class=\"message-controls-slot\" data-message-controls=\"{}\" \
                 data-message-quote-depth=\"{quote_depth}\"></span>",
                super::escape_html_attribute(&self.controls_target),
            )
        } else {
            String::new()
        };
        let attribution = if show {
            format!(
                "<p class=\"message-attribution\">{}{}{}</p>",
                self.signoff,
                self.disposition,
                if opening { self.badge.as_str() } else { "" },
            )
        } else {
            String::new()
        };
        format!("{controls}{attribution}")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    Paragraph,
    Stack,
    Quote,
    QuoteCard,
    Other,
}

/// Lay out one Markdown layer and every raw block quote it directly contains.
///
/// `quote_attributions` follows the layer's direct raw block quotes in document
/// order. Resolved quote cards render recursively before they become opaque HTML.
pub(super) fn flow<'a>(
    events: &[MarkdownEvent<'a>],
    attribution: Option<&Attribution>,
    quote_attributions: &[Option<Attribution>],
    quote_depth: usize,
) -> Vec<MarkdownEvent<'a>> {
    let mut quote_attributions = quote_attributions.iter();
    let blocks = prepare_blocks(events, &mut quote_attributions, quote_depth);
    lay_out_hunks(blocks, attribution, quote_depth)
}

fn prepare_blocks<'a, 'q>(
    events: &[MarkdownEvent<'a>],
    quote_attributions: &mut impl Iterator<Item = &'q Option<Attribution>>,
    quote_depth: usize,
) -> Vec<(Block, Vec<MarkdownEvent<'a>>)> {
    let mut prepared = Vec::new();
    for (kind, range) in blocks(events) {
        let block = &events[range];
        if kind == Block::Paragraph {
            let mut start = 1usize;
            for (index, event) in block[1..block.len() - 1].iter().enumerate() {
                let index = index + 1;
                if !matches!(event, MarkdownEvent::InlineHtml(html)
                    if html.starts_with("<div class=\"message-quote-card"))
                {
                    continue;
                }
                push_paragraph_segment(&mut prepared, block, start, index);
                prepared.push((Block::QuoteCard, vec![event.clone()]));
                start = index + 1;
            }
            push_paragraph_segment(&mut prepared, block, start, block.len() - 1);
        } else if kind == Block::Quote {
            let inner = &block[1..block.len() - 1];
            let attribution = quote_attributions.next().and_then(Option::as_ref);
            let quote_depth = quote_depth + 1;
            let body = lay_out_hunks(
                prepare_blocks(inner, &mut std::iter::empty(), quote_depth),
                attribution,
                quote_depth,
            );
            let mut quote = Vec::with_capacity(body.len() + 2);
            quote.push(block[0].clone());
            quote.extend(body);
            quote.push(block[block.len() - 1].clone());
            prepared.push((kind, quote));
        } else {
            prepared.push((kind, block.to_vec()));
        }
    }
    prepared
}

fn push_paragraph_segment<'a>(
    blocks: &mut Vec<(Block, Vec<MarkdownEvent<'a>>)>,
    paragraph: &[MarkdownEvent<'a>],
    start: usize,
    end: usize,
) {
    if start == end {
        return;
    }
    let mut segment = Vec::with_capacity(end - start + 2);
    segment.push(paragraph[0].clone());
    segment.extend(paragraph[start..end].iter().cloned());
    segment.push(paragraph[paragraph.len() - 1].clone());
    blocks.push((Block::Paragraph, segment));
}

fn lay_out_hunks<'a>(
    blocks: Vec<(Block, Vec<MarkdownEvent<'a>>)>,
    attribution: Option<&Attribution>,
    quote_depth: usize,
) -> Vec<MarkdownEvent<'a>> {
    let Some(attribution) = attribution else {
        return blocks.into_iter().flat_map(|(_, block)| block).collect();
    };
    let tail = (!attribution.task_tail.is_empty())
        .then(|| {
            blocks
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, (kind, block))| {
                    matches!(kind, Block::Paragraph | Block::Other)
                        .then(|| last_word_end(block))
                        .flatten()
                        .map(|end| (index, end))
                })
        })
        .flatten();
    let mut events = Vec::new();
    let mut hunk = Vec::new();
    let mut hunk_tail = None;
    let mut opened = false;
    for (index, (kind, block)) in blocks.into_iter().enumerate() {
        if matches!(kind, Block::Quote | Block::QuoteCard) {
            push_hunk(
                &mut events,
                &mut hunk,
                attribution,
                hunk_tail.take(),
                quote_depth,
                &mut opened,
            );
            events.extend(block);
        } else {
            if let Some((_, end)) = tail.filter(|(tail_block, _)| *tail_block == index) {
                hunk_tail = Some((hunk.len(), end));
            }
            hunk.push((kind, block));
        }
    }
    push_hunk(
        &mut events,
        &mut hunk,
        attribution,
        hunk_tail,
        quote_depth,
        &mut opened,
    );
    if tail.is_none() && !attribution.task_tail.is_empty() {
        events.push(MarkdownEvent::Html(
            format!(
                "<p class=\"message-quote-task-line\">{}</p>",
                attribution.task_tail
            )
            .into(),
        ));
    }
    events
}

/// Where board provenance follows a block's final word: after its last prose
/// text and any inline markup closing around it, outside code blocks.
fn last_word_end(block: &[MarkdownEvent<'_>]) -> Option<usize> {
    let mut code = false;
    let mut end = None;
    for (index, event) in block.iter().enumerate() {
        match event {
            MarkdownEvent::Start(Tag::CodeBlock(_)) => code = true,
            MarkdownEvent::End(TagEnd::CodeBlock) => code = false,
            MarkdownEvent::Text(_) | MarkdownEvent::Code(_) | MarkdownEvent::InlineMath(_)
                if !code =>
            {
                end = Some(index + 1);
            }
            MarkdownEvent::End(
                TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Superscript
                | TagEnd::Subscript
                | TagEnd::Link,
            ) if end == Some(index) => end = Some(index + 1),
            _ => {}
        }
    }
    end
}

/// Emit one hunk. `tail` names the hunk block and event position after which
/// the speaker's board provenance follows its final word; `opened` records
/// whether an earlier hunk of this layer already led with the opening badge.
fn push_hunk<'a>(
    events: &mut Vec<MarkdownEvent<'a>>,
    hunk: &mut Vec<(Block, Vec<MarkdownEvent<'a>>)>,
    attribution: &Attribution,
    tail: Option<(usize, usize)>,
    quote_depth: usize,
    opened: &mut bool,
) {
    if hunk.is_empty() {
        return;
    }
    events.push(MarkdownEvent::Html(HUNK_OPEN.into()));
    events.push(MarkdownEvent::Html(
        attribution.lead_html(quote_depth, !*opened).into(),
    ));
    *opened = true;
    let mut stacks = Vec::new();
    let mut content = Vec::new();
    for (index, (kind, mut block)) in hunk.drain(..).enumerate() {
        if kind == Block::Stack {
            stacks.push(block);
        } else if let Some((_, end)) = tail.filter(|(tail_block, _)| *tail_block == index) {
            content.extend(block.drain(..end));
            content.push(MarkdownEvent::InlineHtml(
                attribution.task_tail.clone().into(),
            ));
            content.extend(block);
        } else {
            content.extend(block);
        }
    }
    if !stacks.is_empty() {
        events.push(MarkdownEvent::InlineHtml(STACK_OPEN.into()));
        for stack in stacks {
            events.extend(stack[1..stack.len() - 1].iter().cloned());
        }
        events.push(MarkdownEvent::InlineHtml(STACK_CLOSE.into()));
    }
    if !content.is_empty() {
        events.push(MarkdownEvent::Html(HUNK_CONTENT_OPEN.into()));
        events.extend(content);
        events.push(MarkdownEvent::Html(HUNK_CLOSE.into()));
    }
    events.push(MarkdownEvent::Html(HUNK_CLOSE.into()));
}

/// The layer's direct child blocks, in order.
fn blocks(events: &[MarkdownEvent<'_>]) -> Vec<(Block, Range<usize>)> {
    let mut blocks = Vec::new();
    let mut index = 0usize;
    while index < events.len() {
        let (kind, end) = match &events[index] {
            MarkdownEvent::Start(tag) => {
                let mut depth = 0usize;
                let mut end = index;
                for (offset, event) in events[index..].iter().enumerate() {
                    match event {
                        MarkdownEvent::Start(_) => depth += 1,
                        MarkdownEvent::End(_) => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        end = index + offset;
                        break;
                    }
                }
                let kind = match tag {
                    Tag::BlockQuote(_) => Block::Quote,
                    Tag::Paragraph => Block::Paragraph,
                    _ => Block::Other,
                };
                (kind, end)
            }
            MarkdownEvent::InlineHtml(html)
                if html.starts_with("<div class=\"message-quote-card") =>
            {
                (Block::QuoteCard, index)
            }
            MarkdownEvent::InlineHtml(html) if html.as_ref() == STACK_OPEN => {
                let end = events[index + 1..]
                    .iter()
                    .position(|event| {
                        matches!(event, MarkdownEvent::InlineHtml(html) if html.as_ref() == STACK_CLOSE)
                    })
                    .map_or(events.len() - 1, |offset| index + 1 + offset);
                (Block::Stack, end)
            }
            _ => (Block::Other, index),
        };
        blocks.push((kind, index..end + 1));
        index = end + 1;
    }
    blocks
}
