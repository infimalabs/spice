//! Model-friendly TeX delimiters before the canonical Markdown parser.

use std::borrow::Cow;
use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};
use wire::{MessageDirective, MessageSend};

pub(super) struct Normalized<'a> {
    pub(super) markdown: Cow<'a, str>,
    pub(super) directives: Vec<MessageDirective>,
    pub(super) sends: Vec<MessageSend>,
}

struct Replacement {
    source: Range<usize>,
    rendered: &'static str,
}

/// Pulldown-cmark owns dollar-delimited math, while models commonly emit the
/// equivalent TeX `\(...\)` and `\[...\]` forms. Rewrite only delimiters that
/// the first Markdown parse exposed as prose, so code and escaped literals keep
/// their authored bytes. Annotation offsets move with the length-changing
/// inline form before later structural normalization runs.
pub(super) fn normalize<'a>(
    markdown: &'a str,
    directives: &[MessageDirective],
    sends: &[MessageSend],
    options: Options,
) -> Normalized<'a> {
    if !markdown.contains("\\(") && !markdown.contains("\\[") {
        return unchanged(markdown, directives, sends);
    }

    let mut prose = Vec::new();
    let mut protected = Vec::new();
    let mut code_block = false;
    for (event, source) in Parser::new_ext(markdown, options).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                code_block = true;
                protected.push(source);
            }
            Event::End(pulldown_cmark::TagEnd::CodeBlock) => code_block = false,
            Event::Code(_) | Event::Html(_) | Event::InlineHtml(_) => protected.push(source),
            Event::Text(_) if !code_block => prose.push(source),
            _ => {}
        }
    }

    let mut replacements = Vec::new();
    collect_pairs(
        markdown,
        b'[',
        b']',
        "$$",
        &prose,
        &protected,
        &mut replacements,
    );
    collect_pairs(
        markdown,
        b'(',
        b')',
        "$",
        &prose,
        &protected,
        &mut replacements,
    );
    replacements.sort_by_key(|replacement| replacement.source.start);
    if replacements.is_empty() {
        return unchanged(markdown, directives, sends);
    }

    let removed = replacements
        .iter()
        .map(|replacement| replacement.source.len() - replacement.rendered.len())
        .sum::<usize>();
    let mut normalized = String::with_capacity(markdown.len() - removed);
    let mut cursor = 0usize;
    for replacement in &replacements {
        normalized.push_str(&markdown[cursor..replacement.source.start]);
        normalized.push_str(replacement.rendered);
        cursor = replacement.source.end;
    }
    normalized.push_str(&markdown[cursor..]);

    let directives = directives
        .iter()
        .map(|directive| MessageDirective {
            start: remap_offset(directive.start, &replacements),
            end: remap_offset(directive.end, &replacements),
            ..directive.clone()
        })
        .collect();
    let sends = sends
        .iter()
        .map(|send| MessageSend {
            start: remap_offset(send.start, &replacements),
            end: remap_offset(send.end, &replacements),
            ..send.clone()
        })
        .collect();
    Normalized {
        markdown: Cow::Owned(normalized),
        directives,
        sends,
    }
}

fn unchanged<'a>(
    markdown: &'a str,
    directives: &[MessageDirective],
    sends: &[MessageSend],
) -> Normalized<'a> {
    Normalized {
        markdown: Cow::Borrowed(markdown),
        directives: directives.to_vec(),
        sends: sends.to_vec(),
    }
}

fn collect_pairs(
    markdown: &str,
    opening: u8,
    closing: u8,
    rendered: &'static str,
    prose: &[Range<usize>],
    protected: &[Range<usize>],
    replacements: &mut Vec<Replacement>,
) {
    let mut cursor = 0usize;
    while let Some(start) = next_delimiter(markdown, cursor, opening, prose) {
        let Some(end) = next_delimiter(markdown, start + 2, closing, prose) else {
            break;
        };
        let expression = start..end + 2;
        if !protected
            .iter()
            .any(|range| range.start < expression.end && expression.start < range.end)
        {
            replacements.push(Replacement {
                source: start..start + 2,
                rendered,
            });
            replacements.push(Replacement {
                source: end..end + 2,
                rendered,
            });
        }
        cursor = end + 2;
    }
}

fn next_delimiter(
    markdown: &str,
    mut cursor: usize,
    symbol: u8,
    prose: &[Range<usize>],
) -> Option<usize> {
    let bytes = markdown.as_bytes();
    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'\\'
            && bytes[cursor + 1] == symbol
            && slash_run_is_active(bytes, cursor)
            && prose_contains(prose, cursor + 1)
        {
            return Some(cursor);
        }
        cursor += 1;
    }
    None
}

fn prose_contains(prose: &[Range<usize>], position: usize) -> bool {
    let candidate = prose.partition_point(|range| range.end <= position);
    prose
        .get(candidate)
        .is_some_and(|range| range.start <= position && position < range.end)
}

fn slash_run_is_active(bytes: &[u8], slash: usize) -> bool {
    let mut start = slash;
    while start > 0 && bytes[start - 1] == b'\\' {
        start -= 1;
    }
    (slash - start + 1) % 2 == 1
}

fn remap_offset(offset: u32, replacements: &[Replacement]) -> u32 {
    let original = offset as usize;
    let mut delta = 0_i64;
    for replacement in replacements {
        if original < replacement.source.start {
            break;
        }
        if original < replacement.source.end {
            let within = (original - replacement.source.start).min(replacement.rendered.len());
            let mapped = replacement.source.start as i64 + delta + within as i64;
            return u32::try_from(mapped).expect("normalized math offset stays in the wire range");
        }
        delta += replacement.rendered.len() as i64 - replacement.source.len() as i64;
    }
    let mapped = original as i64 + delta;
    u32::try_from(mapped).expect("normalized math offset stays in the wire range")
}
