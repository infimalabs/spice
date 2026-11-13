//! Operator-facing preview text for one tool moment.
//!
//! The lane status strip is read by a person, so a tool moment renders as the
//! work being done rather than as vendor bookkeeping: generic command-execution
//! calls suppress their taxonomy only when the command is already informative,
//! named shells retain their family, and results show the call they answer
//! instead of the `tool_use` id that joins them.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use wire::CanonicalUuid;

#[cfg(test)]
mod tests;

/// Longest preview one status line carries before it is elided.
///
/// The app renders the preview on one `nowrap` line that CSS clips at the
/// width it actually has. Every live tool event carries this text, so the
/// bound is the narrowest that still fills an ultrawide status line: about
/// 3200 CSS px of the ticker's monospace.
pub(in crate::resident) const MAX_CHARS: usize = 512;
const MIN_INFORMATIVE_TOKENS: usize = 2;
const MAX_EVENT_IDENTITY_CHARS: usize = 18;

/// Argument fields worth showing, most recognizable first.
///
/// `command` leads the list because a shell call is the moment this strip
/// exists for. A sorted JSON object cannot retain insertion order, so every
/// significant field is named explicitly.
const ARGUMENT_KEYS: [&str; 8] = [
    "command", "cmd", "path", "query", "url", "input", "prompt", "text",
];

/// Call previews held only while their calls are still outstanding.
///
/// A result record names no tool: `listen::ToolMoment::Result` carries the
/// vendor correlation id alone, because no dialect repeats the name on the
/// record that answers a call. Taking the entry on a result is what bounds this
/// map -- it holds one entry per call still in flight rather than one per call
/// ever observed.
///
/// The lane is part of the key because one resident watches every lane at once.
/// A vendor id is unique within a transcript, not across the fleet, and a strip
/// showing another lane's command would be worse than showing none.
#[derive(Debug, Default)]
pub(in crate::resident) struct CallPreviews {
    by_call: BTreeMap<(CanonicalUuid, String), Outstanding>,
    /// Outstanding calls that read a steering attachment the lane was sent.
    attachment_reads: BTreeSet<(CanonicalUuid, String)>,
}

/// One call still in flight: the preview its result answers with, and the
/// typed intent its call declared, which its updates do not repeat.
#[derive(Debug)]
struct Outstanding {
    preview: String,
    intent: Option<String>,
}

impl CallPreviews {
    pub(super) fn clear_lane(&mut self, lane: CanonicalUuid) {
        self.by_call.retain(|(candidate, _), _| *candidate != lane);
        self.attachment_reads
            .retain(|(candidate, _)| *candidate != lane);
    }

    pub(super) fn forget(&mut self, lane: CanonicalUuid, call_id: &str) {
        let key = (lane, call_id.to_owned());
        self.by_call.remove(&key);
        self.attachment_reads.remove(&key);
    }
}

/// Render one tool moment as the line an operator reads.
pub(super) fn activity(
    lane: CanonicalUuid,
    tool: &listen::Tool,
    previews: &mut CallPreviews,
) -> String {
    let key = (lane, tool.call_id.clone());
    match &tool.moment {
        listen::ToolMoment::Call {
            name,
            arguments,
            intent,
        } => {
            let intent = declared_intent(intent.as_deref());
            let preview = identified_preview(name, arguments, intent.as_deref(), &tool.call_id);
            if reads_steering_attachment(lane, arguments) {
                previews.attachment_reads.insert(key.clone());
            }
            previews.by_call.insert(
                key,
                Outstanding {
                    preview: preview.clone(),
                    intent,
                },
            );
            preview
        }
        listen::ToolMoment::Update { name, arguments } => {
            let outstanding = previews.by_call.get(&key);
            let intent = outstanding.and_then(|outstanding| outstanding.intent.clone());
            let progress = partial_result_text(arguments);
            let preview = progress
                .as_ref()
                .and_then(|_| outstanding.map(|outstanding| outstanding.preview.clone()))
                .unwrap_or_else(|| {
                    identified_preview(name, arguments, intent.as_deref(), &tool.call_id)
                });
            if reads_steering_attachment(lane, arguments) {
                previews.attachment_reads.insert(key.clone());
            }
            previews.by_call.insert(
                key,
                Outstanding {
                    preview: preview.clone(),
                    intent,
                },
            );
            progress.map_or_else(
                || flatten(&format!("{preview} update")),
                |progress| flatten(&progress_line(name, &preview, &progress)),
            )
        }
        listen::ToolMoment::Result(result) => {
            let outcome = match result.result {
                listen::ToolResult::Succeeded => "succeeded",
                listen::ToolResult::Failed => "failed",
                listen::ToolResult::Unreported => "completed",
            };
            previews.by_call.remove(&key).map_or_else(
                || flatten(&format!("{} {outcome}", event_identity(&tool.call_id))),
                |outstanding| flatten(&format!("{} {outcome}", outstanding.preview)),
            )
        }
    }
}

/// A named progress update's line: the short prefix naming the tool, then its
/// latest output, then the call's remaining context, so a narrow status bar
/// cuts the context before the output and the output still reads as that
/// tool's. An update without a recognizable tool keeps its output first.
fn progress_line(name: &str, preview: &str, progress: &str) -> String {
    let prefix = normalized_name(name);
    if unnamed(&prefix) {
        return format!("{progress} — {preview}");
    }
    let context = match preview.strip_prefix(prefix.as_str()) {
        Some(rest) if rest.starts_with(": ") => &rest[": ".len()..],
        // Only the preview's completion words follow the prefix.
        Some(rest) if rest.is_empty() || rest.starts_with(' ') => "",
        _ => preview,
    };
    if context.is_empty() {
        return format!("{prefix}: {progress}");
    }
    format!("{prefix}: {progress} — {context}")
}

/// A call's typed intent on one line, absent when it says nothing.
pub(super) fn declared_intent(intent: Option<&str>) -> Option<String> {
    intent.map(flatten).filter(|intent| !intent.is_empty())
}

/// Media a tool result shows as its own card.
///
/// A lane reading an attachment the operator sent it gets back that same image,
/// which the app already shows with the directive the lane acknowledges.
/// Showing it again as a lone tool image would put the operator's own picture
/// on the leading edge, directly above the acknowledgment that answered it.
pub(super) fn result_media(
    lane: CanonicalUuid,
    tool: &listen::Tool,
    previews: &mut CallPreviews,
) -> Vec<wire::Media> {
    let listen::ToolMoment::Result(outcome) = &tool.moment else {
        return Vec::new();
    };
    if previews
        .attachment_reads
        .remove(&(lane, tool.call_id.clone()))
    {
        return Vec::new();
    }
    outcome.media.clone()
}

/// Latest non-empty text line from an omp partial tool result.
fn partial_result_text(arguments: &str) -> Option<String> {
    let Value::Object(result) = serde_json::from_str::<Value>(arguments).ok()? else {
        return None;
    };
    result
        .get("content")?
        .as_array()?
        .iter()
        .rev()
        .filter_map(Value::as_object)
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .flat_map(|text| text.lines().rev())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

fn reads_steering_attachment(lane: CanonicalUuid, arguments: &str) -> bool {
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(arguments) else {
        return false;
    };
    fields.values().any(|value| {
        value.as_str().is_some_and(|path| {
            crate::steering_command::media_projection::names_projection(path, lane)
        })
    })
}

fn identified_preview(name: &str, arguments: &str, intent: Option<&str>, call_id: &str) -> String {
    if unnamed(&normalized_name(name)) {
        return named_preview(
            &event_identity(call_id),
            &flatten(&argument_preview(arguments)),
            "activity",
        );
    }
    call_preview(name, arguments, intent)
}

fn unnamed(normalized: &str) -> bool {
    normalized.is_empty() || normalized.eq_ignore_ascii_case("tool")
}

fn event_identity(call_id: &str) -> String {
    let identity = flatten(call_id);
    let count = identity.chars().count();
    if count == 0 {
        return "activity".to_owned();
    }
    if count <= MAX_EVENT_IDENTITY_CHARS {
        return identity;
    }
    let leading = MAX_EVENT_IDENTITY_CHARS / 2;
    let trailing = MAX_EVENT_IDENTITY_CHARS - leading - 1;
    format!(
        "{}…{}",
        identity.chars().take(leading).collect::<String>(),
        identity.chars().skip(count - trailing).collect::<String>(),
    )
}

pub(super) fn message_text(tool: &listen::Tool, activity: String, media: &[wire::Media]) -> String {
    if media.is_empty()
        || matches!(
            &tool.moment,
            listen::ToolMoment::Result(outcome)
                if outcome.output.as_deref().is_some_and(|text| !text.trim().is_empty())
        )
    {
        activity
    } else {
        String::new()
    }
}

/// One call's preview; `intent` is the call's [`declared_intent`].
fn call_preview(name: &str, arguments: &str, intent: Option<&str>) -> String {
    let name = normalized_name(name);
    let target = targeted_preview(&name, arguments);
    let lowered = name.to_ascii_lowercase();
    let arguments = match (lowered.as_str(), intent) {
        ("job", Some(intent)) => intended(&name, intent, target.as_deref()),
        _ => target.unwrap_or_else(|| argument_preview(arguments)),
    };
    let fallback = match lowered.as_str() {
        "bash" | "zsh" => "shell",
        "shell" | "local shell" => "command",
        _ => "tool",
    };
    named_preview(&name, &arguments, fallback)
}

/// `INTENT (OP ID)`: a job call's declared purpose leads, because an
/// operation and an opaque job id say nothing a reader recognizes. The target
/// stays for correlation, so a purpose long enough to fill the line is elided
/// before the target is.
fn intended(name: &str, intent: &str, target: Option<&str>) -> String {
    let suffix = target.map_or_else(String::new, |target| format!(" ({target})"));
    let budget = MAX_CHARS
        .saturating_sub(name.chars().count() + ": ".len() + suffix.chars().count())
        .max(1);
    if intent.chars().count() <= budget {
        return format!("{intent}{suffix}");
    }
    let kept = intent.chars().take(budget - 1).collect::<String>();
    format!("{}\u{2026}{suffix}", kept.trim_end())
}

fn normalized_name(name: &str) -> String {
    flatten(name.trim())
}

fn named_preview(name: &str, arguments: &str, fallback: &str) -> String {
    let preview = match (name.is_empty(), arguments.is_empty()) {
        (false, false) => flatten(&format!("{name}: {arguments}")),
        (false, true) => name.to_owned(),
        (true, false) => flatten(arguments),
        (true, true) => String::new(),
    };
    complete_preview(&preview, fallback)
}

fn complete_preview(preview: &str, fallback: &str) -> String {
    if informative_tokens(preview) >= MIN_INFORMATIVE_TOKENS {
        return preview.to_owned();
    }
    if preview.is_empty() {
        return format!("{fallback} activity");
    }
    let completed = if preview.eq_ignore_ascii_case(fallback) {
        format!("{preview} activity")
    } else {
        format!("{preview} {fallback}")
    };
    if completed.chars().count() <= MAX_CHARS {
        completed
    } else {
        flatten(&format!("{fallback}: {preview}"))
    }
}

fn informative_tokens(preview: &str) -> usize {
    preview.split_whitespace().count()
}

/// The target a known harness tool acted on, where no single argument names it.
///
/// An `lsp` call names its operation in `action`, which the generic scan
/// would show alone; the symbol, query, and file it acted on are what a reader
/// recognizes. A `grep` pairs its pattern with the path it searched, which the
/// generic scan reduces to the path, and a `job` names its operation and job,
/// which follow the call's typed intent when it declares one.
fn targeted_preview(name: &str, raw: &str) -> Option<String> {
    let tool = name.to_ascii_lowercase();
    if !matches!(tool.as_str(), "lsp" | "grep" | "job") {
        return None;
    }
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(raw) else {
        return None;
    };
    let paired = |first: &str, joiner: &str, second: &str| {
        let first = text_field(&fields, first)?;
        Some(text_field(&fields, second).map_or_else(
            || first.to_owned(),
            |second| format!("{first}{joiner}{second}"),
        ))
    };
    match tool.as_str() {
        "lsp" => lsp_preview(&fields),
        "grep" => paired("pattern", " in ", "path"),
        _ => paired("op", " ", "id"),
    }
}

/// `references label (app/src/view.rs:120)`, `symbols app/src/browser.rs`,
/// `rename Foo -> Bar (src/a.rs:4)`, or `rename_file a.rs -> b.rs`.
fn lsp_preview(fields: &Map<String, Value>) -> Option<String> {
    let action = text_field(fields, "action")?;
    let renamed = text_field(fields, "new_name");
    let symbol = text_field(fields, "symbol");
    let subject = match (symbol, renamed) {
        (Some(symbol), Some(renamed)) => Some(format!("{symbol} -> {renamed}")),
        (Some(symbol), None) => Some(symbol.to_owned()),
        (None, _) => text_field(fields, "query").map(str::to_owned),
    };
    let location = text_field(fields, "file").map(|file| {
        let located = fields
            .get("line")
            .and_then(Value::as_u64)
            .map_or_else(|| file.to_owned(), |line| format!("{file}:{line}"));
        match (symbol, renamed) {
            (None, Some(renamed)) => format!("{located} -> {renamed}"),
            _ => located,
        }
    });
    Some(match (subject, location) {
        (Some(subject), Some(location)) => format!("{action} {subject} ({location})"),
        (Some(target), None) | (None, Some(target)) => format!("{action} {target}"),
        (None, None) => action.to_owned(),
    })
}

fn text_field<'a>(fields: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// The most readable single argument the vendor payload carries.
fn argument_preview(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        // A payload this resident cannot parse is still the vendor's own text;
        // its first line beats showing nothing at all.
        return raw.lines().next().unwrap_or_default().to_owned();
    };
    match value {
        Value::Object(fields) => object_preview(&fields),
        Value::Array(items) => joined(&items),
        Value::String(text) => text,
        Value::Null => "null".to_owned(),
        scalar => scalar.to_string(),
    }
}

fn object_preview(fields: &Map<String, Value>) -> String {
    // An argv array is the whole command, so it wins over any single field.
    if let Some(Value::Array(command)) = fields.get("command")
        && !command.is_empty()
    {
        return joined(command);
    }
    for key in ARGUMENT_KEYS {
        if let Some(Value::String(text)) = fields.get(key)
            && !text.trim().is_empty()
        {
            return text.clone();
        }
    }
    for value in fields.values() {
        if let Value::String(text) = value
            && !text.trim().is_empty()
        {
            return text.clone();
        }
    }
    for (key, value) in fields {
        let summary = match value {
            Value::Number(number) => format!("{key}={number}"),
            Value::Bool(boolean) => format!("{key}={boolean}"),
            Value::Array(items) => format!("{key}[{}]", items.len()),
            Value::Object(nested) => format!("{key}{{{}}}", nested.len()),
            Value::Null => format!("{key}=null"),
            Value::String(_) => continue,
        };
        return summary;
    }
    String::new()
}

fn joined(items: &[Value]) -> String {
    items
        .iter()
        .filter(|item| !item.is_null())
        .map(|item| match item {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Collapse a preview onto one bounded line.
pub(super) fn flatten(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_CHARS {
        return flat;
    }
    let kept = flat.chars().take(MAX_CHARS - 1).collect::<String>();
    format!("{}\u{2026}", kept.trim_end())
}
