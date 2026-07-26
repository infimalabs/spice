"""Presentation models and renderers for transcript-backed assistant messages.

Transcript access, paging, and record projection live in :mod:`spice.serve.messages`.
This module owns the envelopes produced at that boundary: ACK/NACK rendering,
task and reply cards, supervisor feedback, and compact activity previews.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from typing import Any

from spice.mail.ackstate import ACK_DISPOSITION_ACKED, ACK_DISPOSITION_REFUSED
from spice.mail.feedback import SupervisorFeedback, parse_supervisor_feedback_line
from spice.serve.markdown import render_message_html
from spice.serve.taskdirectives import (
    _display_text_with_task_directives,
    _render_message_html_with_task_directives,
    _task_directive_count,
    _task_directive_html,
    _task_directive_summary,
)
from spice.transcript.assembly import (
    AssembledMessage,
    AssembledMessageReducer,
    DirectiveKind,
    SpanKind,
)
from spice.transcript.events import AssistantText, Provenance

IMAGE_REFERENCE_RE = re.compile(r"!\[[^\]]*\]\((?:<[^>]*>|[^)]*)\)")

_PREVIEW_MAX_CHARS = 120
_PRESENCE_PAYLOAD_TYPES = frozenset(
    {
        "function_call",
        "function_call_output",
        "custom_tool_call",
        "custom_tool_call_output",
        "reasoning",
        "web_search_call",
    }
)
_SUPERVISOR_FEEDBACK_OUTPUT_TYPES = frozenset(
    {"function_call_output", "custom_tool_call_output"}
)
_SUPERVISOR_FEEDBACK_HEADING = "Supervisor Feedback"
_ACK_ALREADY_ACKED_KIND = "ack.already-acked"
_ACK_ARCHIVED_KIND = "ack.archived"
_ACK_ERROR_KIND = "ack.error"
_ACK_NOOP_KIND = "ack.noop"
_ACK_UNMATCHED_KIND = "ack.unmatched"
_TASK_CREATED_KIND = "task.created"
_TASK_ERROR_KIND = "task.error"
_SUPERVISOR_FEEDBACK_PREVIEW_PREFIXES = (
    "ACK ignored:",
    "Acknowledged:",
    "Acknowledged (no pending match):",
    "Acknowledgment failed:",
    "Already acknowledged:",
    "Task capture failed:",
    "Task captured:",
    "Tasks captured:",
)


@dataclass(frozen=True)
class AssistantMessage:
    key: str
    index: int
    timestamp: str
    text: str
    display_text: str
    display_html: str
    ack_count: int
    ack_keys: list[str]
    ack_utterances: list[str]
    nack_count: int = 0
    nack_keys: list[str] = field(default_factory=list)
    kind: str = "assistant"
    preview: str = ""
    image_only: bool = False
    source_kind: str = ""
    task_card_count: int = 0
    ack_segments: list[dict[str, Any]] = field(default_factory=list)
    preamble_html: str = ""
    plan_items: list[dict[str, str]] = field(default_factory=list)

    @property
    def speech_utterances(self) -> list[str]:
        return self.ack_utterances

    def to_payload(self) -> dict[str, Any]:
        return {
            "key": self.key,
            "index": self.index,
            "timestamp": self.timestamp,
            "kind": self.kind,
            "source_kind": self.source_kind,
            "text": self.text,
            "display_text": self.display_text,
            "display_html": self.display_html,
            "preamble_html": self.preamble_html,
            "preview": self.preview,
            "image_only": self.image_only,
            "task_card_count": self.task_card_count,
            "ack_count": self.ack_count,
            "ack_keys": self.ack_keys,
            "nack_count": self.nack_count,
            "nack_keys": self.nack_keys,
            "ack_utterances": self.ack_utterances,
            "ack_segments": self.ack_segments,
            "speech_utterances": self.speech_utterances,
            "plan_items": self.plan_items,
        }


def _payload_output_text(payload: dict[str, Any]) -> str:
    output = payload.get("output")
    if isinstance(output, str):
        return output
    if isinstance(output, list):
        parts: list[str] = []
        for item in output:
            if isinstance(item, str):
                parts.append(item)
            elif isinstance(item, dict) and isinstance(item.get("text"), str):
                parts.append(item["text"])
        return "\n".join(parts)
    return ""


def _supervisor_feedback_items(output: str) -> list[dict[str, Any]]:
    items: list[dict[str, Any]] = []
    for feedback in _supervisor_feedback_notices(output):
        if feedback.kind == _TASK_CREATED_KIND:
            handles = _feedback_string_list(feedback.fields.get("handles"))
            if handles:
                items.append(
                    {
                        "kind": _TASK_CREATED_KIND,
                        "label": "Task captured"
                        if len(handles) == 1
                        else "Tasks captured",
                        "detail": ", ".join(handles),
                        "handles": handles,
                    }
                )
        elif feedback.kind == _TASK_ERROR_KIND:
            items.append(
                {
                    "kind": _TASK_ERROR_KIND,
                    "label": "Task capture failed",
                    "detail": str(feedback.fields.get("error") or "").strip()
                    or "unknown error",
                }
            )
        elif feedback.kind == _ACK_ARCHIVED_KIND:
            keys = _feedback_string_list(feedback.fields.get("keys"))
            if keys:
                items.append(
                    {
                        "kind": _ACK_ARCHIVED_KIND,
                        "label": "Acknowledged",
                        "detail": ", ".join(keys),
                        "keys": keys,
                    }
                )
        elif feedback.kind == _ACK_ALREADY_ACKED_KIND:
            keys = _feedback_string_list(feedback.fields.get("keys"))
            if keys:
                items.append(
                    {
                        "kind": _ACK_ALREADY_ACKED_KIND,
                        "label": "Already acknowledged",
                        "detail": ", ".join(keys),
                        "keys": keys,
                    }
                )
        elif feedback.kind == _ACK_UNMATCHED_KIND:
            keys = _feedback_string_list(feedback.fields.get("keys"))
            if keys:
                items.append(
                    {
                        "kind": _ACK_UNMATCHED_KIND,
                        "label": "Acknowledged (no pending match)",
                        "detail": ", ".join(keys),
                        "keys": keys,
                    }
                )
        elif feedback.kind == _ACK_NOOP_KIND:
            detail = str(feedback.fields.get("message") or "").strip()
            items.append(
                {
                    "kind": _ACK_NOOP_KIND,
                    "label": "ACK ignored",
                    "detail": detail or "no inbox key found",
                }
            )
        elif feedback.kind == _ACK_ERROR_KIND:
            # A failed archival still matters when the keys are unreadable, so
            # this renders unconditionally rather than gating on `keys`.
            keys = _feedback_string_list(feedback.fields.get("keys"))
            error = str(feedback.fields.get("error") or "").strip() or "unknown error"
            items.append(
                {
                    "kind": _ACK_ERROR_KIND,
                    "label": "Acknowledgment failed",
                    "detail": f"{', '.join(keys)}: {error}" if keys else error,
                    "keys": keys,
                }
            )
    return items


def _supervisor_feedback_preview(payload: dict[str, Any]) -> str:
    if payload.get("type") not in _SUPERVISOR_FEEDBACK_OUTPUT_TYPES:
        return ""
    items = _supervisor_feedback_items(_payload_output_text(payload))
    return _preview_from_text(
        "\n".join(f"{item['label']}: {item['detail']}" for item in items)
    )


def _supervisor_feedback_notices(output: str) -> list[SupervisorFeedback]:
    notices: list[SupervisorFeedback] = []
    lines = output.replace("\r\n", "\n").replace("\r", "\n").split("\n")
    index = 0
    while index < len(lines):
        if lines[index].strip() != _SUPERVISOR_FEEDBACK_HEADING:
            index += 1
            continue
        index += 1
        while index < len(lines):
            line = lines[index]
            stripped = line.strip()
            if stripped == _SUPERVISOR_FEEDBACK_HEADING:
                break
            if not stripped:
                index += 1
                break
            if line == stripped:
                break
            feedback = parse_supervisor_feedback_line(stripped)
            if feedback is not None:
                notices.append(feedback)
            index += 1
    return notices


def _feedback_string_list(value: object) -> list[str]:
    if isinstance(value, list):
        return [str(item).strip() for item in value if str(item).strip()]
    if isinstance(value, str):
        return [item.strip() for item in value.split(",") if item.strip()]
    return []


def task_card_message(
    key: str,
    index: int,
    timestamp: str,
    fields: list[tuple[str, str]],
    *,
    source_kind: str,
    classes: list[str] | None = None,
    kicker: str = "Task capture",
) -> AssistantMessage:
    directive = {"classes": classes or [], "fields": fields, "kicker": kicker}
    display_text = _task_directive_summary(directive)
    display_html = _task_directive_html(directive)
    return AssistantMessage(
        key=key,
        index=index,
        timestamp=timestamp,
        text=display_text,
        display_text=display_text,
        display_html=display_html,
        ack_count=0,
        ack_keys=[],
        ack_utterances=[],
        kind="task_card",
        preview=_preview_from_text(display_text),
        source_kind=source_kind,
        task_card_count=1,
    )


def reply_card_message(
    key: str,
    index: int,
    timestamp: str,
    text: str,
    *,
    source_kind: str = "agent_reply",
    worktree_id: str | None = None,
) -> AssistantMessage:
    """Synthesize the lane card for one `spice agent reply` submission.

    The reply text is the same ACK/NACK grammar a prose reply would carry, so
    it runs through the ordinary assistant-message builder and renders exactly
    like a prose ACK -- acknowledgment quote plus ACK chip -- with no prose.
    """
    return _assistant_message(
        key,
        index,
        timestamp,
        text,
        kind="reply",
        source_kind=source_kind,
        worktree_id=worktree_id,
    )


def _plan_items(payload: dict[str, Any]) -> list[dict[str, str]] | None:
    if payload.get("type") not in ("function_call", "custom_tool_call"):
        return None
    if payload.get("name") != "update_plan":
        return None
    try:
        arguments = json.loads(payload.get("arguments") or "{}")
    except json.JSONDecodeError:
        return None
    raw_plan = arguments.get("plan") if isinstance(arguments, dict) else None
    if not isinstance(raw_plan, list):
        return None
    items: list[dict[str, str]] = []
    for entry in raw_plan:
        if isinstance(entry, dict):
            items.append(
                {
                    "step": str(entry.get("step") or ""),
                    "status": str(entry.get("status") or ""),
                }
            )
    return items


def _capitalize_first(text: str) -> str:
    first = text[:1]
    if not first.islower():
        return text
    return f"{first.title()}{text[1:]}"


def _replace_terminal_colon(text: str) -> str:
    content = text.rstrip()
    if not content.endswith(":"):
        return text
    return f"{content[:-1]}.{text[len(content) :]}"


def _normalize_terminal_colon_for_display(
    message: str, preamble: str, segment_bodies: list[str]
) -> tuple[str, list[str]]:
    if not message.rstrip().endswith(":"):
        return preamble, segment_bodies
    for body_index in range(len(segment_bodies) - 1, -1, -1):
        if segment_bodies[body_index]:
            segment_bodies[body_index] = _replace_terminal_colon(
                segment_bodies[body_index]
            )
            return preamble, segment_bodies
    return _replace_terminal_colon(preamble), segment_bodies


@dataclass
class _ClassifiedResponse:
    keys: tuple[str, ...]
    disposition: str
    visible_parts: list[str] = field(default_factory=list)
    spoken_parts: list[str] = field(default_factory=list)

    @property
    def visible_text(self) -> str:
        return "\n".join(self.visible_parts).strip()

    @property
    def spoken_text(self) -> str:
        return "\n".join(self.spoken_parts).strip()


def _classified_text_parts(
    message: AssembledMessage,
    source_event: AssistantText,
) -> tuple[str, list[_ClassifiedResponse]]:
    preamble_parts: list[str] = []
    responses: dict[int, _ClassifiedResponse] = {}
    for span in message.spans:
        if span.event is not source_event or span.kind is SpanKind.FINAL_ANSWER:
            continue
        visible = (
            span.text
            if span.kind is not SpanKind.DIRECTIVE
            or span.directive_kind is DirectiveKind.TASK
            else ""
        )
        if span.response_index is None:
            if span.kind not in {SpanKind.PROSE, SpanKind.DIRECTIVE}:
                raise ValueError(
                    f"unkeyed assistant span has response kind {span.kind}"
                )
            if visible:
                preamble_parts.append(visible)
            continue
        response_kind = span.response_kind
        if response_kind not in {SpanKind.ACK, SpanKind.NACK}:
            raise ValueError(
                f"response {span.response_index} has no ACK/NACK classification"
            )
        disposition = (
            ACK_DISPOSITION_REFUSED
            if response_kind is SpanKind.NACK
            else ACK_DISPOSITION_ACKED
        )
        response = responses.get(span.response_index)
        if response is None:
            response = _ClassifiedResponse(
                keys=span.keys,
                disposition=disposition,
            )
            responses[span.response_index] = response
        elif response.keys != span.keys or response.disposition != disposition:
            raise ValueError(
                f"inconsistent classification for response {span.response_index}"
            )
        if visible:
            response.visible_parts.append(visible)
        if span.kind in {SpanKind.ACK, SpanKind.NACK} and span.text:
            response.spoken_parts.append(span.text)
    return "\n".join(preamble_parts).strip(), list(responses.values())


def _classify_assistant_text(
    text: str,
    *,
    offset: int,
    timestamp: str,
    final: bool,
) -> tuple[AssembledMessage, AssistantText]:
    source_event = AssistantText(
        at=Provenance(
            source="<serve-message>",
            line=offset,
            ordinal=0,
            timestamp=timestamp or None,
            offset=offset,
        ),
        text=text,
        final=final,
    )
    reducer = AssembledMessageReducer()
    reducer.push(source_event)
    messages = reducer.finish()
    if len(messages) != 1:
        raise ValueError(f"assistant text assembled into {len(messages)} messages")
    return messages[0], source_event


def _assistant_message(
    key: str,
    offset: int,
    timestamp: str,
    text: str,
    *,
    kind: str,
    source_kind: str = "assistant_text",
    worktree_id: str | None = None,
    classified: AssembledMessage | None = None,
    source_event: AssistantText | None = None,
) -> AssistantMessage:
    if classified is None or source_event is None:
        classified, source_event = _classify_assistant_text(
            text,
            offset=offset,
            timestamp=timestamp,
            final=kind == "final",
        )
    if (
        source_event.text != text
        or source_event not in classified.assistant_text_events
    ):
        raise ValueError("assistant text does not match its classified source event")
    preamble, responses = _classified_text_parts(classified, source_event)
    segment_bodies = [response.visible_text for response in responses]
    preamble, segment_bodies = _normalize_terminal_colon_for_display(
        text, preamble, segment_bodies
    )
    ack_segments: list[dict[str, Any]] = []
    # `ack_keys` is the polarity-agnostic set of keys this message responded to
    # (ACK and NACK alike): it drives context fetch, cache retention, copy, and
    # pending-clear, all of which apply to a refusal exactly as to an ack. The
    # positive/negative split lives in ack_keys/nack_keys for tinting only.
    ack_keys: list[str] = []
    nack_keys: list[str] = []
    seen_keys: set[str] = set()
    acked_keys: set[str] = set()
    refused_keys: set[str] = set()
    ack_utterances: list[str] = []
    display_sources: list[str] = [preamble] if preamble else []
    display_parts: list[str] = (
        [_display_text_with_task_directives(preamble)] if preamble else []
    )
    task_card_count = _task_directive_count(preamble)
    for response, segment_body in zip(responses, segment_bodies, strict=True):
        refused = response.disposition == ACK_DISPOSITION_REFUSED
        # The ACK/NACK header is hidden in the UI, so capitalize the response's
        # first letter for display while keeping the spoken text verbatim.
        body = _capitalize_first(segment_body)
        task_card_count += _task_directive_count(body)
        display_body = _display_text_with_task_directives(body)
        ack_segments.append(
            {
                "keys": list(response.keys),
                "html": _render_message_html_with_task_directives(
                    body, worktree_id=worktree_id
                ),
                "disposition": response.disposition,
            }
        )
        for keyed in response.keys:
            if keyed not in seen_keys:
                seen_keys.add(keyed)
                ack_keys.append(keyed)
            (refused_keys if refused else acked_keys).add(keyed)
        spoken = response.spoken_text
        if spoken:
            ack_utterances.append(spoken)
        if body:
            display_sources.append(body)
        if display_body:
            display_parts.append(display_body)
    nack_keys = [
        key for key in ack_keys if key in refused_keys and key not in acked_keys
    ]
    display_text = "\n".join(display_parts)
    image_only = _image_only_markdown(display_text)
    preamble_html = (
        _render_message_html_with_task_directives(preamble, worktree_id=worktree_id)
        if preamble and responses
        else ""
    )
    display_source = "\n".join(display_sources)
    return AssistantMessage(
        key=key,
        index=offset,
        timestamp=timestamp,
        text=text,
        display_text=display_text,
        display_html=_render_message_html_with_task_directives(
            display_source, worktree_id=worktree_id
        ),
        ack_count=len(acked_keys),
        ack_keys=ack_keys,
        nack_count=len(nack_keys),
        nack_keys=nack_keys,
        ack_utterances=ack_utterances,
        kind=kind,
        preview="image" if image_only else _preview_from_text(display_text),
        image_only=image_only,
        source_kind=source_kind,
        task_card_count=task_card_count,
        ack_segments=ack_segments,
        preamble_html=preamble_html,
    )


def _simple_message(
    key: str, offset: int, timestamp: str, *, kind: str, text: str
) -> AssistantMessage:
    return AssistantMessage(
        key=key,
        index=offset,
        timestamp=timestamp,
        text=text,
        display_text=text,
        display_html=render_message_html(text),
        ack_count=0,
        ack_keys=[],
        ack_utterances=[],
        kind=kind,
        preview=text,
    )


def _presence_message(
    key: str,
    offset: int,
    timestamp: str,
    *,
    kind: str,
    preview: str,
    plan_items: list[dict[str, str]] | None = None,
) -> AssistantMessage:
    return AssistantMessage(
        key=key,
        index=offset,
        timestamp=timestamp,
        text="",
        display_text="",
        display_html="",
        ack_count=0,
        ack_keys=[],
        ack_utterances=[],
        kind=f"presence:{kind}",
        preview=preview,
        source_kind=kind,
        plan_items=plan_items or [],
    )


def _image_only_markdown(text: str) -> bool:
    stripped = text.strip()
    if not stripped:
        return False
    return not IMAGE_REFERENCE_RE.sub("", stripped).strip()


def _preview_from_text(text: str) -> str:
    flat = " ".join(text.split())
    if len(flat) > _PREVIEW_MAX_CHARS:
        return flat[: _PREVIEW_MAX_CHARS - 1].rstrip() + "…"
    return flat


def _preview_for_presence(
    payload: dict[str, Any],
    payload_type: str,
    *,
    call_previews: dict[str, str] | None = None,
) -> str:
    supervisor_feedback = _supervisor_feedback_preview(payload)
    if supervisor_feedback:
        return supervisor_feedback
    if payload_type == "reasoning":
        return _preview_from_text(_reasoning_summary_text(payload)) or "thinking"
    if payload_type in {"function_call", "custom_tool_call"}:
        return _preview_for_call(payload) or "tool call"
    if payload_type in {"function_call_output", "custom_tool_call_output"}:
        return _preview_for_tool_output(payload, call_previews=call_previews)
    if payload_type == "web_search_call":
        return _preview_for_web_search(payload) or "web search"
    return payload_type.replace("_", " ")


def _remember_call_preview(
    payload: dict[str, Any],
    payload_type: str,
    preview: str,
    call_previews: dict[str, str] | None,
) -> None:
    if call_previews is None:
        return
    if payload_type not in {"function_call", "custom_tool_call"}:
        return
    call_id = _payload_call_id(payload)
    if call_id and preview:
        call_previews[call_id] = preview


def _payload_call_id(payload: dict[str, Any]) -> str:
    for key in ("call_id", "id"):
        value = payload.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return ""


def _preview_for_tool_output(
    payload: dict[str, Any], *, call_previews: dict[str, str] | None
) -> str:
    output_preview = _preview_from_text(_payload_output_text(payload))
    call_preview = (
        call_previews.get(_payload_call_id(payload), "") if call_previews else ""
    )
    return _render_tool_output_preview(
        call_preview=call_preview,
        output_preview=output_preview,
    )


def _render_tool_output_preview(*, call_preview: str, output_preview: str) -> str:
    if call_preview and output_preview:
        return _preview_from_text(f"{call_preview} -> {output_preview}")
    if call_preview:
        return _preview_from_text(f"{call_preview} completed")
    if output_preview:
        return _preview_from_text(f"Tool output: {output_preview}")
    return "tool output"


def _reasoning_summary_text(payload: dict[str, Any]) -> str:
    summary = payload.get("summary")
    if not isinstance(summary, list):
        return ""
    for item in summary:
        if isinstance(item, dict):
            text = item.get("text") or item.get("summary") or ""
            if isinstance(text, str) and text.strip():
                return text
        elif isinstance(item, str) and item.strip():
            return item
    return ""


_PREVIEW_ARG_KEYS = ("cmd", "path", "query", "url", "input", "prompt", "text")


def _preview_for_call(payload: dict[str, Any]) -> str:
    name = str(payload.get("name") or "").strip().replace("_", " ")
    args_preview = _preview_args(payload.get("arguments"))
    if name and args_preview:
        return _preview_from_text(f"{name}: {args_preview}")
    return _preview_from_text(name or args_preview)


def _preview_args(raw: Any) -> str:
    if not isinstance(raw, str) or not raw:
        return ""
    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return raw.splitlines()[0]
    if isinstance(data, dict):
        command = data.get("command")
        if isinstance(command, list) and command:
            return " ".join(str(item) for item in command if item is not None)
        for key in _PREVIEW_ARG_KEYS:
            value = data.get(key)
            if isinstance(value, str) and value.strip():
                return value
        for value in data.values():
            if isinstance(value, str) and value.strip():
                return value
        return ""
    if isinstance(data, list):
        return " ".join(str(item) for item in data if item is not None)
    return data if isinstance(data, str) else ""


def _preview_for_web_search(payload: dict[str, Any]) -> str:
    action = payload.get("action")
    if isinstance(action, dict):
        query = action.get("query")
        if isinstance(query, str) and query.strip():
            return f"search: {query}"
    query = payload.get("query")
    if isinstance(query, str) and query.strip():
        return f"search: {query}"
    return ""
