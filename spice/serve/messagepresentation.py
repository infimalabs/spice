"""Presentation models and renderers for transcript-backed assistant messages.

Transcript access, paging, and assembled-locus projection live in
:mod:`spice.serve.messages`. This module owns the envelopes emitted at that
boundary: ACK/NACK rendering, task and reply cards, supervisor feedback, and
compact activity previews.
"""

from __future__ import annotations

import json
import re
from collections.abc import Sequence
from dataclasses import dataclass, field
from typing import Any

from spice.mail.ackarchive import nack_response_is_honored
from spice.mail.feedback import supervisor_feedback_notices
from spice.serve.markdown import render_message_html
from spice.serve.taskdirectives import (
    _display_text_with_task_directives,
    _render_message_html_with_task_directives,
    _task_directive_count,
    _task_directive_html,
    _task_directive_summary,
)
from spice.transcript.assembly import (
    AssembledMessageReducer,
    ClassifiedSpan,
    DirectiveKind,
    SpanKind,
    span_disposition,
)
from spice.transcript.events import (
    UNLOCATED_SOURCE,
    AssistantText,
    Provenance,
    ToolCall,
    WebSearch,
)

IMAGE_REFERENCE_RE = re.compile(r"!\[[^\]]*\]\((?:<[^>]*>|[^)]*)\)")

_PREVIEW_MAX_CHARS = 120
_TEXT_SPAN_KINDS = frozenset({SpanKind.PROSE, SpanKind.ACK, SpanKind.NACK})
_TOOL_CALL_TYPES = frozenset({"function_call", "custom_tool_call"})
_TOOL_OUTPUT_TYPES = frozenset({"function_call_output", "custom_tool_call_output"})
_UPDATE_PLAN_TOOL = "update_plan"
_REASONING_KIND = "reasoning"
_WEB_SEARCH_KIND = "web_search_call"
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


@dataclass(slots=True)
class _TextGroup:
    """One prose or keyed-response run rebuilt from a locus' spans.

    The reducer splits a run wherever a control line interrupts it; a display
    wants the pieces whole again, so a group re-accumulates them and keeps the
    two readings apart: task directives stay in `body` to become cards, and
    every control line is gone from the `spoken` text a browser reads aloud.
    """

    kind: SpanKind
    keys: tuple[str, ...]
    event_identity: int = 0
    response_index: int | None = None
    lines: list[str] = field(default_factory=list)
    spoken_lines: list[str] = field(default_factory=list)

    @property
    def marker(self) -> tuple[int, int | None, SpanKind, tuple[str, ...]]:
        return (self.event_identity, self.response_index, self.kind, self.keys)

    @property
    def body(self) -> str:
        return "\n".join(self.lines).strip()

    @property
    def spoken(self) -> str:
        return "\n".join(self.spoken_lines).strip()


@dataclass(frozen=True, slots=True)
class _PresenceFacts:
    """The one activity a wordless locus reports, in wire-kind terms."""

    kind: str
    call_id: str = ""
    output_text: str = ""
    call: ToolCall | None = None
    search: WebSearch | None = None
    reasoning: str = ""


def _text_groups(spans: Sequence[ClassifiedSpan]) -> list[_TextGroup]:
    """Every prose and keyed-response run this locus carries, in source order.

    A span names the run it belongs to, control lines included, so a directive
    opening a refusal stays inside that refusal instead of being read as the
    start of some other run.
    """
    groups: list[_TextGroup] = []
    for span in spans:
        if span.kind not in _TEXT_SPAN_KINDS:
            continue
        marker = (id(span.event), span.response_index, span.kind, span.keys)
        if not groups or groups[-1].marker != marker:
            groups.append(
                _TextGroup(
                    event_identity=id(span.event),
                    response_index=span.response_index,
                    kind=span.kind,
                    keys=span.keys,
                )
            )
        _absorb_span(groups[-1], span)
    return [
        group
        for group in groups
        if group.body or group.spoken or group.response_index is not None
    ]


def _absorb_span(group: _TextGroup, span: ClassifiedSpan) -> None:
    if span.directive_kind is not None:
        if span.directive_kind is DirectiveKind.TASK:
            group.lines.append(span.text)
        return
    group.lines.append(span.text)
    group.spoken_lines.append(span.text)


def _supervisor_feedback_items(output: str) -> list[dict[str, Any]]:
    items: list[dict[str, Any]] = []
    for feedback in supervisor_feedback_notices(output):
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


def _supervisor_feedback_preview(output: str) -> str:
    items = _supervisor_feedback_items(output)
    return _preview_from_text(
        "\n".join(f"{item['label']}: {item['detail']}" for item in items)
    )


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
    it goes through the same reducer a transcript line does and renders exactly
    like a prose ACK -- acknowledgment quote plus ACK chip -- with no prose.
    """
    return _assistant_message(
        key,
        index,
        timestamp,
        text,
        groups=_text_groups(_submitted_spans(text, index, timestamp)),
        kind="reply",
        source_kind=source_kind,
        worktree_id=worktree_id,
    )


def _submitted_spans(
    text: str, index: int, timestamp: str
) -> tuple[ClassifiedSpan, ...]:
    """Classify text an operator submitted, which has no transcript line yet."""
    reducer = AssembledMessageReducer()
    reducer.push(
        AssistantText(
            at=Provenance(
                source=UNLOCATED_SOURCE,
                line=index,
                ordinal=0,
                timestamp=timestamp,
                offset=index,
            ),
            text=text,
            final=False,
        )
    )
    assembled = reducer.finish()
    return assembled[0].spans if assembled else ()


def _plan_items(call: ToolCall) -> list[dict[str, str]] | None:
    if call.name != _UPDATE_PLAN_TOOL:
        return None
    try:
        arguments = json.loads(call.arguments or "{}")
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


def _assistant_message(
    key: str,
    offset: int,
    timestamp: str,
    text: str,
    *,
    groups: Sequence[_TextGroup],
    kind: str,
    source_kind: str = "assistant_text",
    worktree_id: str | None = None,
) -> AssistantMessage:
    preamble = "\n\n".join(
        group.body for group in groups if group.kind is SpanKind.PROSE and group.body
    )
    segments = [group for group in groups if group.kind is not SpanKind.PROSE]
    segment_bodies = [group.body for group in segments]
    preamble, segment_bodies = _normalize_terminal_colon_for_display(
        text, preamble, segment_bodies
    )
    ack_segments: list[dict[str, Any]] = []
    # `ack_keys` is the polarity-agnostic set of keys this message validly
    # responded to (ACK and reason-bearing NACK alike): it drives context fetch,
    # cache retention, copy, and pending-clear. The positive/negative split
    # lives in ack_keys/nack_keys for tinting only.
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
    for segment, segment_body in zip(segments, segment_bodies, strict=True):
        refused = segment.kind is SpanKind.NACK
        if refused and not nack_response_is_honored(segment.body):
            # The archival authority leaves a reasonless NACK pending. It must
            # not become a refused/retired key merely because the transcript
            # reducer correctly identified its negative polarity.
            continue
        # The ACK/NACK header is hidden in the UI, so capitalize the response's
        # first letter for display while keeping the spoken text verbatim.
        body = _capitalize_first(segment_body)
        task_card_count += _task_directive_count(body)
        display_body = _display_text_with_task_directives(body)
        ack_segments.append(
            {
                "keys": list(segment.keys),
                "html": _render_message_html_with_task_directives(
                    body, worktree_id=worktree_id
                ),
                "disposition": span_disposition(segment.kind),
            }
        )
        for keyed in segment.keys:
            if keyed not in seen_keys:
                seen_keys.add(keyed)
                ack_keys.append(keyed)
            (refused_keys if refused else acked_keys).add(keyed)
        if segment.spoken:
            ack_utterances.append(segment.spoken)
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
        if preamble and segments
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
    presence: _PresenceFacts,
    *,
    call_previews: dict[str, str] | None = None,
) -> str:
    supervisor_feedback = _supervisor_feedback_preview(presence.output_text)
    if supervisor_feedback:
        return supervisor_feedback
    if presence.call is not None:
        return _preview_for_call(presence.call) or "tool call"
    if presence.search is not None:
        return _preview_for_web_search(presence.search) or "web search"
    if presence.kind == _REASONING_KIND:
        return _preview_from_text(presence.reasoning) or "thinking"
    if presence.kind in _TOOL_OUTPUT_TYPES:
        return _preview_for_tool_output(presence, call_previews=call_previews)
    return presence.kind.replace("_", " ")


def _remember_call_preview(
    presence: _PresenceFacts,
    preview: str,
    call_previews: dict[str, str] | None,
) -> None:
    if call_previews is None or presence.call is None:
        return
    if presence.call_id and preview:
        call_previews[presence.call_id] = preview


def _preview_for_tool_output(
    presence: _PresenceFacts, *, call_previews: dict[str, str] | None
) -> str:
    output_preview = _preview_from_text(presence.output_text)
    call_preview = call_previews.get(presence.call_id, "") if call_previews else ""
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


_PREVIEW_ARG_KEYS = ("cmd", "path", "query", "url", "input", "prompt", "text")


def _preview_for_call(call: ToolCall) -> str:
    name = call.name.strip().replace("_", " ")
    args_preview = _preview_args(call.arguments)
    if name and args_preview:
        return _preview_from_text(f"{name}: {args_preview}")
    return _preview_from_text(name or args_preview)


def _preview_args(raw: str) -> str:
    if not raw:
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


def _preview_for_web_search(search: WebSearch) -> str:
    query = search.query or ""
    if query.strip():
        return f"search: {query}"
    return ""
