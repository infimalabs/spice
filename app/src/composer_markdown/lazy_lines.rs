//! Lane prose ends a list at an unindented line.
//!
//! CommonMark lets an unindented line continue the paragraph of the list item
//! above it. Hard-wrapped documents rely on that, but agents and operators
//! writing messages almost never do: they mean a new paragraph, and the list
//! that follows it a new list. A blank line written before each such line
//! makes the canonical parser read it that way, while a line indented under
//! its item still continues it. Block quotes keep their own lazy reading.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use wire::{MessageDirective, MessageSend};

use super::math::Normalized;

/// A block the parser has opened and not yet closed.
enum Block {
    /// A list item, with the column its content starts at once seen.
    Item(Option<usize>),
    Quote,
    Other,
}

/// `normalized` with a blank line before every lazy list continuation line,
/// its annotation offsets moved with the bytes they name.
pub(super) fn separate(normalized: Normalized<'_>, options: Options) -> Normalized<'_> {
    let breaks = lazy_lines(&normalized.markdown, options);
    if breaks.is_empty() {
        return normalized;
    }
    let markdown = &normalized.markdown;
    let mut separated = String::with_capacity(markdown.len() + breaks.len());
    let mut cursor = 0;
    for &line in &breaks {
        separated.push_str(&markdown[cursor..line]);
        separated.push('\n');
        cursor = line;
    }
    separated.push_str(&markdown[cursor..]);
    let shift = |offset: u32| {
        let inserted = breaks.partition_point(|&line| line <= offset as usize);
        offset + u32::try_from(inserted).expect("inserted breaks stay in the wire range")
    };
    Normalized {
        markdown: separated.into(),
        directives: normalized
            .directives
            .into_iter()
            .map(|directive| MessageDirective {
                start: shift(directive.start),
                end: shift(directive.end),
                ..directive
            })
            .collect(),
        sends: normalized
            .sends
            .into_iter()
            .map(|send| MessageSend {
                start: shift(send.start),
                end: shift(send.end),
                ..send
            })
            .collect(),
    }
}

/// The byte offset of each line that lazily continues a list item's
/// paragraph outside any block quote, in source order.
fn lazy_lines(markdown: &str, options: Options) -> Vec<usize> {
    let mut blocks = Vec::new();
    let mut after_break = None;
    let mut breaks = Vec::new();
    for (event, source) in Parser::new_ext(markdown, options).into_offset_iter() {
        let line = line_start(markdown, source.start);
        if let Some(Block::Item(content @ None)) = blocks.last_mut() {
            *content = Some(source.start - line);
        }
        if let Some(column) = after_break.take()
            && source.start - line < column
            && markdown[line..source.start].trim().is_empty()
        {
            breaks.push(line);
        }
        match event {
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => {}
            Event::Start(Tag::Item) => blocks.push(Block::Item(None)),
            Event::Start(Tag::BlockQuote(_)) => blocks.push(Block::Quote),
            Event::Start(_) => blocks.push(Block::Other),
            Event::End(_) => {
                blocks.pop();
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(Block::Item(Some(column))) = blocks.last()
                    && !blocks.iter().any(|block| matches!(block, Block::Quote))
                {
                    after_break = Some(*column);
                }
            }
            _ => {}
        }
    }
    breaks
}

fn line_start(markdown: &str, offset: usize) -> usize {
    markdown[..offset]
        .rfind('\n')
        .map_or(0, |newline| newline + 1)
}
