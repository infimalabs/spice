//! What opened a turn, told from the prompt that opened it.
//!
//! The record that opens a vendor turn is the prompt Spice delivered, so its
//! operator-role prose already says why the lane woke: the resident's notices
//! (an assignment, a resumed seat, a briefing, room catch-up, a finished
//! background job), the direction keys it delivers, or the peer notes it
//! carries. The turn boundary names those causes instead of a fixed label.

use super::direction;

#[cfg(test)]
mod tests;

const NOTICE_OPEN: &str = "<system-notice>";
const NOTICE_CLOSE: &str = "</system-notice>";
const PART_SEPARATOR: &str = " · ";
/// Characters one cause keeps before the ticker's own clipping takes over.
const PART_CHARS: usize = 120;

/// The causes one turn-opening record's operator prose names, in prompt
/// order: every notice, then every delivered directive. A prompt with neither
/// names its own first line unless it carries sealed steering (a replay) or
/// bootstrap scaffolding; a record with no operator prose names nothing.
pub(super) fn summary<'a>(events: impl IntoIterator<Item = &'a listen::Event>) -> String {
    let mut parts = Vec::new();
    for text in events.into_iter().filter_map(direction::operator_text) {
        prompt_causes(text, &mut parts);
    }
    parts.join(PART_SEPARATOR)
}

fn prompt_causes(text: &str, parts: &mut Vec<String>) {
    let mut outside = String::new();
    let mut rest = text;
    let named = parts.len();
    while let Some(open) = rest.find(NOTICE_OPEN) {
        outside.push_str(&rest[..open]);
        let body = &rest[open + NOTICE_OPEN.len()..];
        let Some(close) = body.find(NOTICE_CLOSE) else {
            rest = body;
            break;
        };
        let notice = &body[..close];
        if !is_seat_guide(notice)
            && let Some(cause) = notice_cause(notice)
        {
            parts.push(cause);
        }
        rest = &body[close + NOTICE_CLOSE.len()..];
    }
    outside.push_str(rest);
    if !steer::inbox::replays(text) {
        parts.extend(outside.lines().filter_map(directive_cause));
    }
    if parts.len() == named
        && steer::inbox::keys(&outside).next().is_none()
        && !outside.lines().any(crate::message_shape::is_skill_link)
        && let Some(line) = outside.lines().map(str::trim).find(|line| !line.is_empty())
    {
        parts.push(clip(line));
    }
}

/// The `spice://` guide rides beside briefings and resumed seats alike; it
/// tells a capability, never why the turn opened.
fn is_seat_guide(notice: &str) -> bool {
    crate::resource::SEAT_GUIDE
        .strip_prefix(NOTICE_OPEN)
        .and_then(|guide| guide.strip_suffix(NOTICE_CLOSE))
        == Some(notice)
}

/// A notice's `# heading` with the `- label: value` facts listed before its
/// first `##` section, or else its lead sentence.
fn notice_cause(body: &str) -> Option<String> {
    let mut lines = body
        .lines()
        .map(str::trim)
        .skip_while(|line| line.is_empty());
    let lead = lines.next()?;
    let Some(heading) = lead.strip_prefix("# ") else {
        let sentence = lead.split_once(". ").map_or(lead, |(sentence, _)| sentence);
        return Some(clip(sentence.trim_end_matches('.')));
    };
    let facts = lines
        .take_while(|line| !line.starts_with("## "))
        .filter_map(|line| line.strip_prefix("- "))
        .filter(|fact| {
            fact.split_once(": ")
                .is_some_and(|(label, _)| short_label(label))
        })
        .map(clip)
        .collect::<Vec<_>>();
    let heading = clip(heading);
    Some(if facts.is_empty() {
        heading
    } else {
        format!("{heading} ({})", facts.join("; "))
    })
}

fn short_label(label: &str) -> bool {
    label.split(' ').count() <= 4
        && label
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == ' ')
}

/// One sealed directive header: an operator direction by key, a peer note by
/// its sender's address.
fn directive_cause(line: &str) -> Option<String> {
    if !line.starts_with("directive=") {
        return None;
    }
    let field = |name: &str| {
        line.split(' ')
            .find_map(|field| field.strip_prefix(name)?.strip_prefix('='))
    };
    let key = field("key")?;
    Some(match field("sender") {
        Some(sender) => format!("peer note {key} from {sender}"),
        None => format!("direction {key}"),
    })
}

/// One cause on one line without code ticks, cut at a character boundary.
fn clip(text: &str) -> String {
    let flat = text.replace('`', "");
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(PART_CHARS) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}
