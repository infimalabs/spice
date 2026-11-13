//! One structural classification for transcript prose consumers.

use serde::Serialize;

/// The one shape assigned to a transcript message from its text alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum MessageShape {
    /// Prose authored as part of the conversation.
    AuthoredAsk,
    /// Harness or skill bootstrap material rather than an authored ask.
    Scaffolding,
    /// A post-compaction summary used to reconstruct earlier intent.
    CompactionRehydration,
    /// A machine envelope, patch, diff, or tool result.
    ToolGeneratedEnvelope,
}

impl MessageShape {
    pub(crate) const fn is_authored(self) -> bool {
        matches!(self, Self::AuthoredAsk)
    }
}

/// One classification plus the exact authored fold consumers may inspect.
///
/// Mixed assistant text keeps every authored segment in order while excluding
/// only structurally delimited generated spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageClassification {
    pub(crate) shape: MessageShape,
    pub(crate) authored: Option<String>,
}

/// Classify one message using only its own text.
pub(crate) fn classify(text: &str) -> MessageClassification {
    let trimmed = text.trim();
    if is_rehydration(trimmed) {
        return MessageClassification {
            shape: MessageShape::CompactionRehydration,
            authored: None,
        };
    }

    let spans = structural_spans(text);
    if let Some(authored) = authored_fold(text, &spans) {
        return MessageClassification {
            shape: MessageShape::AuthoredAsk,
            authored: Some(authored),
        };
    }

    // Nothing authored remains, so a skill link marks the whole message as
    // bootstrap even when the resident framed a notice beside it.
    if spans.iter().any(|span| span.kind == SpanKind::Scaffolding) {
        return MessageClassification {
            shape: MessageShape::Scaffolding,
            authored: None,
        };
    }
    if spans.iter().any(|span| span.kind == SpanKind::Generated) {
        return generated();
    }

    MessageClassification {
        shape: MessageShape::AuthoredAsk,
        authored: Some(text.to_owned()),
    }
}

fn generated() -> MessageClassification {
    MessageClassification {
        shape: MessageShape::ToolGeneratedEnvelope,
        authored: None,
    }
}

fn machine_envelope_at(text: &str, start: usize) -> Option<usize> {
    if !text[start..].starts_with('<') || text[start..].starts_with("</") {
        return None;
    }
    let open_end = text[start..].find('>')? + start;
    let opening = &text[start + 1..open_end];
    let opening = opening.trim();
    if opening.ends_with('/') {
        let tag = opening.trim_end_matches('/').trim();
        return machine_tag(tag).then_some(open_end + 1);
    }
    let tag = opening.split_ascii_whitespace().next()?;
    if !machine_tag(tag) {
        return None;
    }
    let closing = format!("</{tag}>");
    text[open_end + 1..]
        .find(&closing)
        .map(|offset| open_end + 1 + offset + closing.len())
}

fn machine_tag(opening: &str) -> bool {
    let Some(tag) = opening.split_ascii_whitespace().next() else {
        return false;
    };
    let mut bytes = tag.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_rehydration(text: &str) -> bool {
    intent_section(text, numbered_heading, PRIMARY_INTENT).is_some() || {
        let lower = text.to_ascii_lowercase();
        lower.contains("session is being continued from a previous conversation")
            && (lower.contains("summary") || lower.contains("context"))
    }
}

pub(crate) fn is_skill_link(line: &str) -> bool {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("[$") else {
        return false;
    };
    let Some((label, target)) = rest.split_once("](") else {
        return false;
    };
    let Some(target) = target.strip_suffix(')') else {
        return false;
    };
    // CommonMark lets a destination sit in angle brackets; vendors use that
    // form for absolute skill paths.
    let target = target
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
        .unwrap_or(target);
    !label.is_empty()
        && !target.is_empty()
        && !target.chars().any(char::is_whitespace)
        && target.ends_with("SKILL.md")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Span {
    start: usize,
    end: usize,
    kind: SpanKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpanKind {
    Generated,
    Scaffolding,
}

fn structural_spans(text: &str) -> Vec<Span> {
    let mut spans = machine_envelope_spans(text);
    spans.extend(line_structural_spans(text));
    merge_spans(spans)
}

fn machine_envelope_spans(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find('<') {
        let start = cursor + relative;
        if let Some(end) = machine_envelope_at(text, start) {
            spans.push(Span {
                start,
                end,
                kind: SpanKind::Generated,
            });
            cursor = end;
        } else {
            cursor = start + '<'.len_utf8();
        }
    }
    spans
}

fn line_structural_spans(text: &str) -> Vec<Span> {
    let lines = line_offsets(text);
    let mut spans = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let (start, line) = lines[index];
        let stripped = line.trim();
        if stripped.is_empty() {
            index += 1;
            continue;
        }
        if is_skill_link(stripped) {
            spans.push(Span {
                start,
                end: start + line.len(),
                kind: SpanKind::Scaffolding,
            });
            index += 1;
            continue;
        }
        if stripped.starts_with("*** Begin Patch")
            && let Some(end_index) = patch_end(&lines, index)
        {
            let (end_start, end_line) = lines[end_index];
            spans.push(Span {
                start,
                end: end_start + end_line.len(),
                kind: SpanKind::Generated,
            });
            index = end_index + 1;
            continue;
        }
        if stripped.starts_with("diff --git ") {
            let end_index = diff_end(&lines, index);
            let (end_start, end_line) = lines[end_index];
            spans.push(Span {
                start,
                end: end_start + end_line.len(),
                kind: SpanKind::Generated,
            });
            index = end_index + 1;
            continue;
        }
        if terminal_output_start(stripped) {
            spans.push(Span {
                start,
                end: text.len(),
                kind: SpanKind::Generated,
            });
            break;
        }
        if complete_machine_line(stripped)
            || generated_line(stripped)
            || complete_bracketed_notice(stripped)
        {
            spans.push(Span {
                start,
                end: start + line.len(),
                kind: SpanKind::Generated,
            });
        }
        index += 1;
    }
    spans
}

fn line_offsets(text: &str) -> Vec<(usize, &str)> {
    let mut offset = 0;
    text.split_inclusive('\n')
        .map(|line| {
            let start = offset;
            offset += line.len();
            (start, line)
        })
        .collect()
}

fn patch_end(lines: &[(usize, &str)], start: usize) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, (_, line))| line.trim().starts_with("*** End Patch"))
        .map(|(index, _)| index)
}

fn diff_end(lines: &[(usize, &str)], start: usize) -> usize {
    let mut end = start;
    for (index, (_, line)) in lines.iter().enumerate().skip(start + 1) {
        if diff_continuation(line) {
            end = index;
        } else {
            break;
        }
    }
    end
}

fn diff_continuation(line: &str) -> bool {
    let stripped = line.trim_end();
    let trimmed = stripped.trim_start();
    trimmed.starts_with("diff --git ")
        || trimmed.starts_with("index ")
        || trimmed.starts_with("new file mode ")
        || trimmed.starts_with("deleted file mode ")
        || trimmed.starts_with("similarity index ")
        || trimmed.starts_with("rename from ")
        || trimmed.starts_with("rename to ")
        || trimmed.starts_with("--- ")
        || trimmed.starts_with("+++ ")
        || trimmed.starts_with("@@ ")
        || trimmed.starts_with("\\ No newline at end of file")
        || stripped.starts_with(['+', '-', ' '])
}

fn generated_line(line: &str) -> bool {
    line.eq_ignore_ascii_case("apply patch")
        || line.starts_with("patch:")
        || line.starts_with("index ")
        || line.starts_with("--- a/")
        || line.starts_with("+++ b/")
        || line.starts_with("@@ ")
}

fn terminal_output_start(line: &str) -> bool {
    line.starts_with("tool output:")
        || line.starts_with("tool_result:")
        || line.starts_with("Process exited with code ")
        || line.starts_with("Command exited with code ")
}

fn complete_machine_line(line: &str) -> bool {
    machine_envelope_at(line, 0).is_some_and(|end| end == line.len())
}

fn complete_bracketed_notice(line: &str) -> bool {
    let Some(body) = line
        .strip_prefix('[')
        .and_then(|body| body.strip_suffix(']'))
    else {
        return false;
    };
    let Some((head, detail)) = body.split_once(':') else {
        return false;
    };
    matches!(
        head.trim(),
        "tool-call" | "tool-result" | "harness-retry" | "harness-notice"
    ) && !detail.trim().is_empty()
}

fn merge_spans(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_by_key(|span| (span.start, span.end));
    let mut merged: Vec<Span> = Vec::new();
    for span in spans {
        let Some(last) = merged.last_mut() else {
            merged.push(span);
            continue;
        };
        if span.start <= last.end {
            last.end = last.end.max(span.end);
            if span.kind == SpanKind::Generated {
                last.kind = SpanKind::Generated;
            }
        } else {
            merged.push(span);
        }
    }
    merged
}

fn authored_fold(text: &str, spans: &[Span]) -> Option<String> {
    if spans.is_empty() {
        return (!text.is_empty()).then(|| text.to_owned());
    }
    let mut authored = String::new();
    let mut cursor = 0;
    for span in spans {
        if cursor < span.start {
            authored.push_str(&text[cursor..span.start]);
        }
        cursor = span.end;
    }
    if cursor < text.len() {
        authored.push_str(&text[cursor..]);
    }
    let authored = authored.trim();
    (!authored.is_empty()).then(|| authored.to_owned())
}

/// Recover the primary intent section or return a visible unrecoverable marker.
///
/// Claude Code opens its summary with a numbered `Primary Request and Intent`
/// section and omp with a markdown `## Goal` section; each section ends at the
/// next heading of its own style.
pub(crate) fn rehydration_intent(text: &str) -> String {
    let Some((style, heading, inline)) = INTENT_SECTIONS.iter().find_map(|&(style, name)| {
        intent_section(text, style, name).map(|(heading, inline)| (style, heading, inline))
    }) else {
        return UNRECOVERABLE_REHYDRATION_INTENT.to_owned();
    };
    let body = text[heading..]
        .lines()
        .take_while(|line| style(line).is_none())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let intent = [inline, body.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    if intent.is_empty() {
        UNRECOVERABLE_REHYDRATION_INTENT.to_owned()
    } else {
        intent
    }
}

pub(crate) const UNRECOVERABLE_REHYDRATION_INTENT: &str = "[unparseable compaction-summary intent]";

/// A heading style: the heading's name and any inline text after a colon.
type Heading = fn(&str) -> Option<(&str, &str)>;

const PRIMARY_INTENT: &str = "Primary Request and Intent";

/// Each vendor's primary intent section, in its own heading style.
const INTENT_SECTIONS: [(Heading, &str); 2] = [
    (numbered_heading, PRIMARY_INTENT),
    (markdown_heading, "Goal"),
];

fn intent_section<'a>(text: &'a str, style: Heading, name: &str) -> Option<(usize, &'a str)> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if let Some((heading, inline)) = style(line)
            && heading.eq_ignore_ascii_case(name)
        {
            return Some((offset + line.len(), inline));
        }
        offset += line.len();
    }
    None
}

fn numbered_heading(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || !matches!(line.as_bytes().get(digits), Some(b'.' | b')')) {
        return None;
    }
    let body = line[digits + 1..].trim().trim_matches('*');
    let (heading, inline) = body.split_once(':').unwrap_or((body, ""));
    Some((heading.trim().trim_matches('*'), inline.trim()))
}

fn markdown_heading(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let hashes = line.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&hashes) || line.as_bytes().get(hashes) != Some(&b' ') {
        return None;
    }
    Some((line[hashes..].trim(), ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_structural_shapes_are_exclusive_and_unknown_envelopes_are_generated() {
        let rows = [
            ("Please finish the task.", MessageShape::AuthoredAsk),
            (
                "[$spice](.agents/skills/spice/SKILL.md)",
                MessageShape::Scaffolding,
            ),
            (
                "[$spice](</repo/.git/.spice-install/agents/skills/spice/SKILL.md>)\n\n<system-notice>\n# Spice briefing\n- held\n</system-notice>",
                MessageShape::Scaffolding,
            ),
            (
                "1. Primary Request and Intent\nKeep going.\n2. Work Completed\nNone.",
                MessageShape::CompactionRehydration,
            ),
            (
                "<future-harness-block>generated</future-harness-block>",
                MessageShape::ToolGeneratedEnvelope,
            ),
        ];
        for (text, expected) in rows {
            assert_eq!(classify(text).shape, expected);
        }
    }

    #[test]
    fn malformed_envelopes_remain_authored_but_complete_unknown_tags_do_not() {
        assert_eq!(
            classify("<future-harness-block>generated").shape,
            MessageShape::AuthoredAsk
        );
        assert_eq!(
            classify("<future-harness-block>x</different-block>").shape,
            MessageShape::AuthoredAsk
        );
    }

    #[test]
    fn generated_diff_body_is_removed_from_authored_segments() {
        let text = "I made the focused repair.\ndiff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-old\n+new";
        let classified = classify(text);
        assert_eq!(classified.shape, MessageShape::AuthoredAsk);
        assert_eq!(
            classified.authored.as_deref(),
            Some("I made the focused repair.")
        );
        assert_eq!(
            classify("*** Begin Patch\n*** End Patch").shape,
            MessageShape::ToolGeneratedEnvelope
        );
    }

    #[test]
    fn ordinary_bracketed_prose_and_harness_discussion_remain_authored() {
        for text in [
            "[Please keep this bracketed ask.]",
            "The malformed tool call was discussed, then I retried with valid syntax.",
            "Here is a quoted tool result: `tool_result: not a real envelope`.",
        ] {
            let classified = classify(text);
            assert_eq!(classified.shape, MessageShape::AuthoredAsk);
            assert_eq!(classified.authored.as_deref(), Some(text));
        }
        assert_eq!(
            classify("[tool-call: malformed retry]").shape,
            MessageShape::ToolGeneratedEnvelope
        );
    }

    #[test]
    fn mixed_structural_segments_preserve_every_authored_segment() {
        let classified = classify(concat!(
            "First authored line.\n",
            "[$spice](.agents/skills/spice/SKILL.md)\n",
            "*** Begin Patch\n",
            "+generated\n",
            "*** End Patch\n",
            "Second authored line.\n",
            "<future-harness-block>generated</future-harness-block>\n",
            "Third authored line."
        ));
        assert_eq!(classified.shape, MessageShape::AuthoredAsk);
        assert_eq!(
            classified.authored.as_deref(),
            Some("First authored line.\nSecond authored line.\nThird authored line.")
        );
    }

    #[test]
    fn diff_blocks_stop_before_following_authored_prose() {
        let classified = classify(concat!(
            "Before diff.\n",
            "diff --git a/a.rs b/a.rs\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
            "After diff."
        ));
        assert_eq!(
            classified.authored.as_deref(),
            Some("Before diff.\nAfter diff.")
        );
    }

    #[test]
    fn rehydration_intent_reads_each_vendor_section_or_marks_it_missing() {
        assert_eq!(
            rehydration_intent(
                "This session is being continued from a previous conversation summary"
            ),
            UNRECOVERABLE_REHYDRATION_INTENT
        );
        assert_eq!(
            rehydration_intent(
                "1. Primary Request and Intent\n- preserve the ask\n2. Work Completed\n- none"
            ),
            "- preserve the ask"
        );
        assert_eq!(
            rehydration_intent(
                "## Goal\n- preserve the ask\n\n## Progress\n### Done\n- none\n1. not a heading"
            ),
            "- preserve the ask"
        );
        assert_eq!(
            classify("## Goal\nShip the fix.").shape,
            MessageShape::AuthoredAsk
        );
    }
}
