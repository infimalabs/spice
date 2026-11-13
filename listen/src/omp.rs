//! oh-my-pi's RPC frame grammar and the session file it persists beside it.
//!
//! A lane drives omp over `--mode rpc`, so the selected stream is one JSON
//! frame per line. `message_end` carries authored content, `get_state` declares
//! the session, `prompt_result` reports correlated completion and structured
//! provider failure, and `session_settled` marks whole-session quiescence. omp's
//! own session file persists the same message shape under a `message` envelope,
//! so one decoder reads both current surfaces.

use serde_json::Value;
use tape::Locus;

use crate::{
    Compaction, Dialog, Event, Events, Failure, Object, PromptOutcome, PromptStatus, Prose,
    ProseRole, ProviderError, Reasoning, ReasoningContent, ResidueKind, SessionDeclaration,
    SignOut, Tool, ToolMoment, ToolOutcome, ToolResult, TurnEnding, TurnMoment, Usage, array,
    media::{self, OwnedContent},
    object, optional_boolean, optional_string, optional_unsigned, required, string,
};

/// RPC protocol version a lane states at open, and the grammar below decodes.
///
/// omp answers `ready` on version 1 and negotiates 2. Version 2 is the one
/// whose oversized frames are chunked whole rather than compacted, shrunk,
/// or replaced by an overflow notice, so a lane that must read every record
/// it captures asks for it by name and refuses to run on anything else.
pub const PROTOCOL_VERSION: u8 = 2;

/// Prefix of the request identities a lane gives live setting changes.
///
/// The retune driver correlates those responses and answers each refusal to
/// its own caller, so the occupant keeps running at its previous value. Any
/// other refused command, a launch's opening effort included, is a failure.
pub const RETUNE_REQUEST_ID_PREFIX: &str = "spice-retune-";

/// Request identity of the launch's `spice://` host-URI registration.
///
/// The registration is feature detection: an omp that refuses it runs on
/// without the scheme, so its refusal is no failure of the lane.
pub const URI_SCHEMES_REQUEST_ID: &str = "spice-uri-schemes";

/// Request identity of the fast-mode selection a launch states after its
/// model and effort.
///
/// The driver admits current OMP's exact no-tier refusal as nonfatal and tells
/// it on the lane row. Every other refusal fails the opening.
pub const FAST_REQUEST_ID: &str = "spice-fast";

/// RPC frames and session entries that carry no listening moment of their own.
///
/// Every partial, lifecycle, and transport frame is named here rather than
/// left to the unknown arm, so a frame omp adds later is drift the residue
/// accounts for instead of silence.
const SILENT: &[&str] = &[
    // Transport and run delimiters. RecordContext consumes the run boundary
    // where one exists; the remaining frames only delimit content-bearing ones.
    "ready",
    "agent_start",
    "agent_end",
    "turn_start",
    "turn_end",
    "message_start",
    "tool_execution_start",
    "tool_execution_update",
    "tool_execution_end",
    "auto_compaction_start",
    "manual_compaction_start",
    "compaction_summary",
    "session_settled",
    // Retry, model, and UI state is either superseded by the settled message or
    // has no transcript moment. A terminal provider error is typed from
    // prompt_result instead of these intermediate status frames.
    "auto_retry_start",
    "auto_retry_end",
    "retry_fallback_applied",
    "retry_fallback_succeeded",
    "model_changed",
    "thinking_level_changed",
    "config_warnings_changed",
    "advisor_cost_changed",
    "advisor_yielded",
    "ttsr_triggered",
    "todo_reminder",
    "todo_auto_clear",
    "goal_updated",
    "irc_message",
    "extension_ui_request",
    "extension_error",
    "available_commands_update",
    "session_info_update",
    "session_shutdown",
    "subagent_event",
    "subagent_lifecycle",
    "subagent_progress",
    "host_tool_call",
    "host_tool_cancel",
    "host_uri_request",
    "host_uri_cancel",
    "config_update",
    // Session-file metadata changes how omp rebuilds provider context or
    // presents its own UI; none is authored conversation. Compaction itself is
    // decoded above, while branch/reset entries only describe rewinds around it.
    "title",
    "title_change",
    "model_change",
    "thinking_level_change",
    "custom",
    "custom_message",
    "branch_summary",
    "label",
    "credential_pin",
    "session_init",
    "mode_change",
    "model_usage",
    "reset_boundary",
    "service_tier_change",
    "ttsr_injection",
];

/// Partials the frame that ends them restates whole, except a terminal text
/// update and an in-flight Spice job wait.
///
/// Provider adapters may normalize tool-adjacent commentary out of the
/// settled message, so capture keeps that one complete streamed revision as
/// evidence. Earlier cumulative updates remain omitted to avoid quadratic tape
/// growth. A job wait may remain open for minutes and its later terminal frame
/// cannot describe what the operator should see while it is blocked, so those
/// bounded progress frames are retained.
pub(crate) fn restated(record: &Object) -> bool {
    match record.get("type").and_then(Value::as_str) {
        Some("tool_execution_update") => {
            record.get("toolName").and_then(Value::as_str) != Some("job")
        }
        Some("message_update") => {
            record
                .get("assistantMessageEvent")
                .and_then(Value::as_object)
                .and_then(|event| event.get("type"))
                .and_then(Value::as_str)
                != Some("text_end")
        }
        _ => false,
    }
}

/// Withhold every reasoning payload one record carries, wherever it nests,
/// and report whether there was any.
///
/// Reasoning arrives as `thinking` content blocks inside messages, partials,
/// and message arrays, as `redactedThinking` blocks (`redacted_thinking` on
/// the Anthropic wire), and as the streamed
/// `thinking_start`, `thinking_delta`, and `thinking_end` updates. Each text
/// becomes `marker`; opaque signatures and redacted data are dropped, since
/// they are reasoning in another encoding.
pub(crate) fn withhold_reasoning(record: &mut Object, marker: &str) -> bool {
    let carried = match record.get("type").and_then(Value::as_str) {
        Some("thinking") => Some(Carried::Block),
        Some("redactedThinking" | "redacted_thinking") => Some(Carried::Redacted),
        Some("thinking_start" | "thinking_delta" | "thinking_end") => Some(Carried::Stream),
        _ => None,
    };
    let mut withheld = match carried {
        Some(Carried::Block) => {
            let text = mark(record, "thinking", marker);
            record.remove("thinkingSignature").is_some() | text
        }
        Some(Carried::Redacted) => record.remove("data").is_some(),
        Some(Carried::Stream) => mark(record, "delta", marker) | mark(record, "content", marker),
        None => false,
    };
    for value in record.values_mut() {
        withheld |= withhold_nested(value, marker);
    }
    withheld
}

fn withhold_nested(value: &mut Value, marker: &str) -> bool {
    match value {
        Value::Object(object) => withhold_reasoning(object, marker),
        Value::Array(items) => items.iter_mut().fold(false, |withheld, item| {
            withhold_nested(item, marker) | withheld
        }),
        _ => false,
    }
}

/// Where one object carries reasoning of its own.
#[derive(Clone, Copy)]
enum Carried {
    /// A `thinking` content block.
    Block,
    /// A `redactedThinking` content block.
    Redacted,
    /// A streamed thinking update.
    Stream,
}

fn mark(object: &mut Object, field: &str, marker: &str) -> bool {
    match object.get_mut(field) {
        Some(value @ Value::String(_)) => {
            *value = Value::String(marker.to_owned());
            true
        }
        _ => false,
    }
}

/// The turn moment one session-file message marks.
///
/// The session file carries no lifecycle frames, so the operator prompt
/// opens a turn and the assistant message that stops closes it. A steer is
/// interjected inside the turn it addresses, and an assistant message that
/// stops for a tool call is still mid-turn.
pub(crate) fn session_turn(message: Option<&Object>) -> Option<TurnMoment> {
    let message = message?;
    let field = |key: &str| message.get(key).and_then(Value::as_str);
    match field("role")? {
        "user" if message.get("steering").and_then(Value::as_bool) != Some(true) => {
            Some(TurnMoment::Started)
        }
        "assistant"
            if field("stopReason").is_some_and(|reason| {
                matches!(reason, "stop" | "length" | "error" | "aborted")
            }) =>
        {
            Some(TurnMoment::Completed)
        }
        _ => None,
    }
}

/// How the assistant message one record settles stopped.
///
/// An unknown spelling is no ending rather than a guessed one, so a reader
/// that needs a clean stop never takes drift for it.
pub(crate) fn session_ending(message: Option<&Object>) -> Option<TurnEnding> {
    let message = message?;
    let field = |key: &str| message.get(key).and_then(Value::as_str);
    if field("role")? != "assistant" {
        return None;
    }
    match field("stopReason")? {
        "stop" => Some(TurnEnding::Stop),
        "toolUse" => Some(TurnEnding::ToolUse),
        "length" => Some(TurnEnding::Length),
        "error" => Some(TurnEnding::Error),
        "aborted" => Some(TurnEnding::Aborted),
        _ => None,
    }
}

/// The dialog one RPC frame opens that waits on an answer: a select, confirm,
/// input, or editor request, a call on a tool the host registered, or a read
/// or write of a URI scheme the host owns. Notices, status, widgets, titles,
/// and URL openings expect no reply.
pub(crate) fn dialog(record: &Object) -> Option<Dialog> {
    let field = |key: &str| record.get(key).and_then(Value::as_str);
    let id = field("id")?.to_owned();
    match field("type")? {
        "host_tool_call" => Some(Dialog::HostTool {
            id,
            tool: field("toolName")?.to_owned(),
            arguments: record
                .get("arguments")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        }),
        "host_uri_request" => Some(Dialog::HostUri {
            id,
            operation: field("operation")?.to_owned(),
            url: field("url")?.to_owned(),
            content: field("content").map(str::to_owned),
        }),
        "extension_ui_request" => match field("method")? {
            "confirm" => Some(Dialog::Confirm {
                id,
                title: field("title").unwrap_or_default().to_owned(),
                message: field("message").unwrap_or_default().to_owned(),
            }),
            "select" | "input" | "editor" => Some(Dialog::Prompt { id }),
            _ => None,
        },
        _ => None,
    }
}

/// The host call one RPC frame withdraws: omp stopped waiting on a host tool
/// call or a host URI request, so the host stops answering it.
pub(crate) fn withdrawn(record: &Object) -> Option<String> {
    let field = |key: &str| record.get(key).and_then(Value::as_str);
    matches!(field("type")?, "host_tool_cancel" | "host_uri_cancel")
        .then(|| field("targetId").map(str::to_owned))?
}

/// Whether this decoder explicitly handles or silences `discriminator`.
pub(crate) fn owns(discriminator: &str) -> bool {
    SILENT.contains(&discriminator)
        || matches!(
            discriminator,
            "message_end"
                | "message_update"
                | "response"
                | "session"
                | "auto_compaction_end"
                | "manual_compaction_end"
                | "compaction"
                | "prompt_result"
                | "rpc_frame_error"
                | "notice"
        )
}

pub(crate) fn decode(record: &Object, locus: Locus) -> Events {
    match string(record, "type")? {
        "message_end" | "message" => message(object(record, "message")?, locus),
        "message_update" => message_update(record, locus),
        "tool_execution_update"
            if record.get("toolName").and_then(Value::as_str) == Some("job") =>
        {
            tool_update(record, locus)
        }
        "response" => response(record, locus),
        "session" => Ok(vec![Event::Session(SessionDeclaration {
            identity: string(record, "id")?.to_owned(),
            fresh: true,
            locus,
        })]),
        "auto_compaction_end" | "manual_compaction_end" | "compaction" => compaction(record, locus),
        "prompt_result" => prompt_result(record, locus),
        "rpc_frame_error" => Ok(vec![Event::Failure(Failure {
            message: string(record, "error")?.to_owned(),
            provider: None,
            locus,
        })]),
        "notice" => notice(record, locus),
        silent if SILENT.contains(&silent) => Ok(Vec::new()),
        other => Err(ResidueKind::Unknown {
            discriminator: other.to_owned(),
        }),
    }
}

/// Terminal lifecycle and structured provider cause from one accepted prompt.
pub(crate) fn prompt_outcome(record: &Object) -> Result<PromptOutcome, ResidueKind> {
    let status = match string(record, "status")? {
        "completed" => PromptStatus::Completed,
        "aborted" => PromptStatus::Aborted,
        "error" => PromptStatus::Error,
        other => return Err(drift(&format!("prompt_result:{other}"))),
    };
    Ok(PromptOutcome {
        agent_invoked: required(record, "agentInvoked")?.as_bool().ok_or(
            ResidueKind::Malformed {
                field: "prompt_result.agentInvoked",
                expected: "boolean",
            },
        )?,
        status,
        session_settled: required(record, "sessionSettled")?.as_bool().ok_or(
            ResidueKind::Malformed {
                field: "prompt_result.sessionSettled",
                expected: "boolean",
            },
        )?,
    })
}

fn prompt_result(record: &Object, locus: Locus) -> Events {
    let outcome = prompt_outcome(record)?;
    if outcome.status != PromptStatus::Error {
        return Ok(Vec::new());
    }
    let Some(error) = record.get("error") else {
        return Ok(Vec::new());
    };
    let error = error.as_object().ok_or(ResidueKind::Malformed {
        field: "prompt_result.error",
        expected: "object",
    })?;
    let http_status = optional_unsigned(error, "httpStatus")?
        .map(u16::try_from)
        .transpose()
        .map_err(|_| ResidueKind::Malformed {
            field: "prompt_result.error.httpStatus",
            expected: "HTTP status integer",
        })?;
    Ok(vec![Event::Failure(Failure {
        message: string(error, "message")?.to_owned(),
        provider: Some(ProviderError {
            provider: optional_string(error, "provider")?.map(str::to_owned),
            model: optional_string(error, "model")?.map(str::to_owned),
            http_status,
            retryable: required(error, "retryable")?
                .as_bool()
                .ok_or(ResidueKind::Malformed {
                    field: "prompt_result.error.retryable",
                    expected: "boolean",
                })?,
        }),
        locus,
    })])
}

fn message(message: &Object, locus: Locus) -> Events {
    match string(message, "role")? {
        "user" => user(message, locus),
        "assistant" => assistant(message, locus),
        "toolResult" => tool_result(message, locus),
        // Vendor notices to the model: background-job deliveries, `!command`
        // executions, extension-authored custom messages, and harness
        // reminders such as rule-violation notices.
        "custom" | "bashExecution" | "developer" => Ok(Vec::new()),
        other => Err(drift(&format!("message:{other}"))),
    }
}

/// One fully streamed assistant text block, before message settlement.
///
/// Its exact words may be normalized into reasoning by the provider adapter.
/// The message identity and content index in RecordContext let presentation
/// retain the authored block without displaying a second settled twin.
fn message_update(record: &Object, locus: Locus) -> Events {
    let update = object(record, "assistantMessageEvent")?;
    if string(update, "type")? != "text_end" {
        return Ok(Vec::new());
    }
    let text = string(update, "content")?.to_owned();
    let content = media::decompose(OwnedContent::AuthoredText(&text))?;
    Ok(vec![Event::StreamedProse(Prose {
        role: ProseRole::Assistant,
        media: content.media,
        external_media: content.external_media,
        text,
        phase: None,
        locus,
    })])
}

fn user(message: &Object, locus: Locus) -> Events {
    let content = required(message, "content")?;
    if let Some(text) = content.as_str() {
        return Ok(vec![operator_prose(text, locus)?]);
    }
    let blocks = content.as_array().ok_or(ResidueKind::Malformed {
        field: "message.content",
        expected: "string or array",
    })?;
    let mut events = Vec::new();
    for value in blocks {
        let block = value.as_object().ok_or(ResidueKind::Malformed {
            field: "message.content[]",
            expected: "object",
        })?;
        match string(block, "type")? {
            "text" => events.push(operator_prose(string(block, "text")?, locus)?),
            "image" => {
                let content = media::decompose(OwnedContent::ProviderValue {
                    value,
                    default_name: "message-image",
                })?;
                if let Some(Event::Prose(prose)) = events.last_mut() {
                    if prose.media.len() + prose.external_media.len() < wire::media::MAX_MEDIA_ITEMS
                    {
                        prose.media.extend(content.media);
                        prose.external_media.extend(content.external_media);
                    }
                    wire::media::validate_media(&prose.media).map_err(|_| {
                        ResidueKind::Malformed {
                            field: "message.content[].media",
                            expected: "at most eight admitted images",
                        }
                    })?;
                } else if !content.media.is_empty() || !content.external_media.is_empty() {
                    events.push(Event::Prose(Prose {
                        role: ProseRole::Operator,
                        text: String::new(),
                        phase: Some("prompt".to_owned()),
                        media: content.media,
                        external_media: content.external_media,
                        locus,
                    }));
                }
            }
            other => return Err(drift(&format!("user:{other}"))),
        }
    }
    if events.is_empty() {
        return Err(ResidueKind::Absent {
            field: "message.content[]",
        });
    }
    Ok(events)
}

fn operator_prose(text: &str, locus: Locus) -> Result<Event, ResidueKind> {
    let content = media::decompose(OwnedContent::AuthoredText(text))?;
    Ok(Event::Prose(Prose {
        role: ProseRole::Operator,
        media: content.media,
        external_media: content.external_media,
        text: text.to_owned(),
        phase: Some("prompt".to_owned()),
        locus,
    }))
}

fn assistant(message: &Object, locus: Locus) -> Events {
    let phase = optional_string(message, "stopReason")?
        .is_some_and(|reason| matches!(reason, "stop" | "length"))
        .then(|| "final_answer".to_owned());
    let content = array(message, "content")?;
    let mut events = Vec::new();
    for value in content {
        let block = value.as_object().ok_or(ResidueKind::Malformed {
            field: "message.content[]",
            expected: "object",
        })?;
        match string(block, "type")? {
            "text" => {
                let text = string(block, "text")?.to_owned();
                let content = media::decompose(OwnedContent::AuthoredText(&text))?;
                events.push(Event::Prose(Prose {
                    role: ProseRole::Assistant,
                    media: content.media,
                    external_media: content.external_media,
                    text,
                    phase: phase.clone(),
                    locus,
                }));
            }
            "thinking" => events.push(Event::Reasoning(Reasoning {
                content: ReasoningContent::Plain(vec![string(block, "thinking")?.to_owned()]),
                locus,
            })),
            "toolCall" => events.push(Event::Tool(Tool {
                call_id: string(block, "id")?.to_owned(),
                moment: ToolMoment::Call {
                    name: string(block, "name")?.to_owned(),
                    arguments: arguments(block),
                    intent: optional_string(block, "intent")?.map(str::to_owned),
                },
                locus,
            })),
            other => return Err(drift(&format!("assistant:{other}"))),
        }
    }
    if let Some(usage) = message.get("usage").and_then(Value::as_object) {
        events.push(Event::Usage(usage_event(usage, locus)?));
    }
    Ok(events)
}

fn tool_update(record: &Object, locus: Locus) -> Events {
    let partial = required(record, "partialResult")?;
    let arguments = serde_json::to_string(partial).map_err(|_| ResidueKind::Malformed {
        field: "partialResult",
        expected: "JSON value",
    })?;
    Ok(vec![Event::Tool(Tool {
        call_id: string(record, "toolCallId")?.to_owned(),
        moment: ToolMoment::Update {
            name: string(record, "toolName")?.to_owned(),
            arguments,
        },
        locus,
    })])
}

fn tool_result(message: &Object, locus: Locus) -> Events {
    let content = media::decompose(OwnedContent::ProviderValue {
        value: required(message, "content")?,
        default_name: "tool-image",
    })?;
    let result = match optional_boolean(message, "isError")? {
        Some(true) => ToolResult::Failed,
        Some(false) => ToolResult::Succeeded,
        None => ToolResult::Unreported,
    };
    let exit_code = message
        .get("details")
        .and_then(Value::as_object)
        .and_then(|details| details.get("exitCode"))
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok());
    Ok(vec![Event::Tool(Tool {
        call_id: string(message, "toolCallId")?.to_owned(),
        moment: ToolMoment::Result(ToolOutcome {
            result,
            output: Some(content.text),
            exit_code,
            media: content.media,
            external_media: content.external_media,
        }),
        locus,
    })])
}

/// Correlation and disposition carried by one RPC command response.
pub(crate) fn command_response(
    record: &Object,
) -> Result<Option<crate::CommandResponse>, ResidueKind> {
    if record.get("type").and_then(Value::as_str) != Some("response") {
        return Ok(None);
    }
    let success = required(record, "success")?
        .as_bool()
        .ok_or(ResidueKind::Malformed {
            field: "success",
            expected: "boolean",
        })?;
    let command = string(record, "command")?;
    let served_models = if success && command == "get_available_models" {
        Some(served_models(object(record, "data")?)?)
    } else {
        None
    };
    Ok(Some(crate::CommandResponse {
        id: optional_string(record, "id")?.map(str::to_owned),
        command: command.to_owned(),
        success,
        error: optional_string(record, "error")?.map(str::to_owned),
        served_models,
    }))
}

/// Each model a `get_available_models` answer lists, as `set_model` names it.
fn served_models(data: &Object) -> Result<Vec<crate::ServedModel>, ResidueKind> {
    array(data, "models")?
        .iter()
        .map(|model| {
            let model = model.as_object().ok_or(ResidueKind::Malformed {
                field: "models",
                expected: "array of objects",
            })?;
            Ok(crate::ServedModel {
                provider: string(model, "provider")?.to_owned(),
                id: string(model, "id")?.to_owned(),
            })
        })
        .collect()
}

/// A command reply. Only `get_state` carries content: the exact session
/// identity a resumed launch names again. A refused command is a failure the
/// lane must see, except a live retune its own driver answers and the
/// host-URI registration a lane runs on without. A busy prompt refusal states
/// that it accepted nothing and names both deterministic repairs.
fn response(record: &Object, locus: Locus) -> Events {
    let response = command_response(record)?.expect("response records carry response context");
    let command = response.command.as_str();
    if !response.success {
        let reason = response.error.as_deref().unwrap_or("no reason given");
        if response.id.as_deref().is_some_and(|id| {
            id.starts_with(RETUNE_REQUEST_ID_PREFIX)
                || id == URI_SCHEMES_REQUEST_ID
                || id == FAST_REQUEST_ID
        }) {
            return Ok(Vec::new());
        }
        let mut message =
            if command == "prompt" && reason.starts_with("Agent is already processing") {
                "prompt not accepted: agent is already processing; queue it with steer/followUp or \
             retry after completion"
                    .to_owned()
            } else {
                format!("{command} refused: {reason}")
            };
        if command == "negotiate_protocol" {
            // The wire spice cannot read is not a wire it may run on, so the
            // refusal states what the lane asked for rather than leaving the
            // operator to infer it from omp's own wording.
            message.push_str(&format!(
                "; this lane requires RPC protocol version {PROTOCOL_VERSION}"
            ));
        }
        return Ok(vec![Event::Failure(Failure {
            message,
            provider: None,
            locus,
        })]);
    }
    if command != "get_state" {
        return Ok(Vec::new());
    }
    let state = object(record, "data")?;
    Ok(vec![Event::Session(SessionDeclaration {
        identity: string(state, "sessionId")?.to_owned(),
        fresh: optional_unsigned(state, "messageCount")? == Some(0),
        locus,
    })])
}

/// A committed compaction, however it was started.
///
/// omp announces an automatic and a manual compaction with the same shape:
/// `result` carries the committed entry's fields, and an aborted, skipped, or
/// result-less end names an attempt that committed nothing. The preserved
/// session entry keeps the same fields at its top level. A remote compaction
/// announces only its short summary.
fn compaction(record: &Object, locus: Locus) -> Events {
    let kind = record.get("type").and_then(Value::as_str);
    if optional_boolean(record, "aborted")? == Some(true)
        || optional_boolean(record, "skipped")? == Some(true)
        || (kind != Some("compaction") && record.get("result").and_then(Value::as_object).is_none())
    {
        return Ok(Vec::new());
    }
    let body = record
        .get("result")
        .and_then(Value::as_object)
        .unwrap_or(record);
    let summary = ["summary", "shortSummary"]
        .into_iter()
        .find_map(|field| body.get(field).and_then(Value::as_str))
        .map(str::to_owned);
    Ok(vec![Event::Compaction(Compaction {
        summary,
        method: compaction_method(record, body),
        locus,
    })])
}

/// Successful strategy named by either spelling OMP has emitted.
fn compaction_method(record: &Object, body: &Object) -> Option<String> {
    for source in [record, body] {
        for field in ["method", "action"] {
            if let Some(method) = source.get(field).and_then(Value::as_str) {
                let method = method.trim();
                if !method.is_empty() {
                    return Some(method.to_owned());
                }
            }
        }
    }
    None
}

/// Notice source omp gives the warning it raises when it signs an account out.
const SIGN_OUT_SOURCE: &str = "auth";

/// Terminal counterpart of the `/login` command omp's sign-out notice names.
const LOGIN_COMMAND: &str = "omp login";

fn notice(record: &Object, locus: Locus) -> Events {
    if optional_string(record, "source")? == Some(SIGN_OUT_SOURCE) {
        return Ok(vec![Event::SignOut(SignOut {
            message: string(record, "message")?.to_owned(),
            login: LOGIN_COMMAND.to_owned(),
            locus,
        })]);
    }
    if optional_string(record, "level")? != Some("error") {
        return Ok(Vec::new());
    }
    Ok(vec![Event::Failure(Failure {
        message: string(record, "message")?.to_owned(),
        provider: None,
        locus,
    })])
}

fn usage_event(usage: &Object, locus: Locus) -> Result<Usage, ResidueKind> {
    Ok(Usage {
        turns: 1,
        input_tokens: optional_unsigned(usage, "input")?.unwrap_or(0),
        cached_input_tokens: optional_unsigned(usage, "cacheRead")?.unwrap_or(0),
        cache_write_input_tokens: optional_unsigned(usage, "cacheWrite")?.unwrap_or(0),
        output_tokens: optional_unsigned(usage, "output")?.unwrap_or(0),
        reasoning_output_tokens: None,
        locus,
    })
}

/// The vendor call payload as JSON, or an empty object when it carries none.
fn arguments(block: &Object) -> String {
    block
        .get("arguments")
        .filter(|arguments| arguments.is_object())
        .map_or_else(|| "{}".to_owned(), ToString::to_string)
}

fn drift(discriminator: &str) -> ResidueKind {
    ResidueKind::VersionDrift {
        discriminator: discriminator.to_owned(),
    }
}
