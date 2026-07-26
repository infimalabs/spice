"""Presentation projection for transcript-backed assistant messages.

Transcript access and paging live in :mod:`spice.serve.messages`. This module
owns event-to-envelope reduction, ACK/NACK rendering, task and reply cards,
supervisor feedback, and compact activity previews.
"""

from __future__ import annotations

import json
import re
from collections.abc import Sequence
from dataclasses import dataclass, field, replace
from typing import Any, TypeVar

from spice.mail.ackarchive import nack_response_is_honored
from spice.mail.ackgrammar import blank_edge_span
from spice.mail.feedback import supervisor_feedback_notices
from spice.serve.images import image_markdown, view_image_markdown
from spice.serve.markdown import render_message_html
from spice.serve.taskdirectives import (
    MarkedText,
    _display_text_with_task_directives,
    _render_message_html_with_task_directives,
    _task_directive_count,
    _task_directive_html,
    _task_directive_summary,
)
from spice.transcript.assembly import (
    AssembledMessage,
    AssembledMessageReducer,
    ClassifiedSpan,
    DirectiveKind,
    SpanKind,
    span_disposition,
)
from spice.transcript.events import (
    UNLOCATED_SOURCE,
    AssistantText,
    Compaction,
    Image,
    Provenance,
    Reasoning,
    ToolCall,
    ToolOutput,
    TranscriptEvent,
    WebSearch,
)
from spice.transcript.reader import render_cursor

IMAGE_REFERENCE_RE = re.compile(r"!\[[^\]]*\]\((?:<[^>]*>|[^)]*)\)")

# A refusal the archival authority left pending acked nothing and refused
# nothing, so its segment has neither ACK-state disposition. It still carries
# whatever the message captured, so it goes out under a third name. This is a
# wire and display state only: it is deliberately absent from ACK_DISPOSITIONS,
# which is the vocabulary of dispositions an ack record may be stored under.
SEGMENT_DISPOSITION_WITHHELD = "withheld"

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

_EventT = TypeVar("_EventT", bound=TranscriptEvent)


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

    @property
    def is_supervisor_feedback(self) -> bool:
        """Whether this presence envelope carries durable supervisor feedback."""
        return (
            self.kind.startswith("presence:")
            and self.source_kind in _TOOL_OUTPUT_TYPES
            and self.preview.startswith(_SUPERVISOR_FEEDBACK_PREVIEW_PREFIXES)
        )

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
    chunks: list[tuple[str, bool]] = field(default_factory=list)
    spoken_lines: list[str] = field(default_factory=list)

    @property
    def marker(self) -> tuple[int, int | None, SpanKind, tuple[str, ...]]:
        return (self.event_identity, self.response_index, self.kind, self.keys)

    @property
    def marked(self) -> MarkedText:
        """This run's body with the directive lines the reducer marked.

        A prose chunk carries however many lines it accumulated and a
        directive chunk is exactly one, so numbering the lines here is what
        turns the reducer's per-span decision into a position the display can
        use. The blank ends go the way the text form drops them, which no
        directive line is ever caught by.
        """
        flagged: list[tuple[str, bool]] = []
        for chunk, directive in self.chunks:
            if directive:
                flagged.append((chunk, True))
                continue
            flagged.extend((line, False) for line in chunk.split("\n"))
        start, stop = blank_edge_span([line for line, _ in flagged])
        flagged = flagged[start:stop]
        return MarkedText(
            text="\n".join(line for line, _ in flagged).rstrip(),
            directive_lines=frozenset(
                index for index, (_, directive) in enumerate(flagged) if directive
            ),
        )

    @property
    def body(self) -> str:
        return self.marked.text

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


@dataclass
class _ToolPreviewIndex:
    """Call/output preview facts collected across one paged presentation."""

    calls: dict[str, list[tuple[int, str]]] = field(default_factory=dict)
    outputs: dict[int, tuple[str, str]] = field(default_factory=dict)

    def observe(self, presence: _PresenceFacts, *, offset: int, preview: str) -> None:
        if presence.kind in _TOOL_CALL_TYPES:
            if presence.call_id and preview:
                self.calls.setdefault(presence.call_id, []).append((offset, preview))
            return
        if presence.kind not in _TOOL_OUTPUT_TYPES:
            return
        if _supervisor_feedback_preview(presence.output_text):
            return
        self.outputs[offset] = (
            presence.call_id,
            _preview_from_text(presence.output_text),
        )

    def resolve(self, messages: list[AssistantMessage]) -> list[AssistantMessage]:
        resolved: list[AssistantMessage] = []
        for message in messages:
            output = self.outputs.get(message.index)
            if output is None:
                resolved.append(message)
                continue
            call_id, output_preview = output
            call_preview = self._call_preview_before(call_id, message.index)
            resolved.append(
                replace(
                    message,
                    preview=_render_tool_output_preview(
                        call_preview=call_preview,
                        output_preview=output_preview,
                    ),
                )
            )
        return resolved

    def _call_preview_before(self, call_id: str, output_offset: int) -> str:
        candidates = (
            (offset, preview)
            for offset, preview in self.calls.get(call_id, ())
            if offset < output_offset
        )
        return max(candidates, default=(-1, ""))[1]


@dataclass
class MessagePresenter:
    """Project typed transcript facts into the public assistant-message model.

    Only a paged read earns the preview index. It alone meets a tool output
    whose call sits on a page it has yet to read, which one pass cannot pair on
    its own. Indexing anyway would rescan every tool output to answer a question
    a single-pass caller never asks, so that caller leaves the index unbuilt.
    """

    _preview_index: _ToolPreviewIndex | None = None

    @classmethod
    def paging(cls) -> MessagePresenter:
        """A presenter whose previews survive across successive read pages."""
        return cls(_preview_index=_ToolPreviewIndex())

    def project(
        self,
        events: Sequence[TranscriptEvent],
        *,
        worktree_id: str | None = None,
    ) -> list[AssistantMessage]:
        """Reduce one access pass into source-ordered message envelopes."""
        messages: list[AssistantMessage] = []
        call_previews: dict[str, str] = {}
        reducer = AssembledMessageReducer()

        def consume(assembled: AssembledMessage) -> None:
            message = self.present(
                assembled,
                worktree_id=worktree_id,
                call_previews=call_previews,
            )
            if message is not None:
                messages.append(message)

        for event in events:
            for assembled in reducer.push(event):
                consume(assembled)
        for assembled in reducer.finish():
            consume(assembled)
        return messages

    def present(
        self,
        message: AssembledMessage,
        *,
        worktree_id: str | None = None,
        call_previews: dict[str, str] | None = None,
    ) -> AssistantMessage | None:
        """Render one assembled transcript locus, if it carries presentation."""
        return _build_message(
            message,
            worktree_id=worktree_id,
            call_previews=call_previews,
            preview_index=self._preview_index,
        )

    def resolve(self, messages: list[AssistantMessage]) -> list[AssistantMessage]:
        """Resolve call previews whose output appeared on a newer read page."""
        if self._preview_index is None:
            return messages
        return self._preview_index.resolve(messages)

    @staticmethod
    def contains_tool_output_image(events: Sequence[TranscriptEvent]) -> bool:
        """Whether an event pass carries a tool-output image."""
        return any(
            isinstance(event, Image) and _is_tool_output_image(event)
            for event in events
        )


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
            group.chunks.append((span.text, True))
        return
    group.chunks.append((span.text, False))
    group.spoken_lines.append(span.text)


def _join_marked(parts: Sequence[MarkedText], separator: str) -> MarkedText:
    """One text from several, with every part's marks moved onto its new lines."""
    texts: list[str] = []
    directive_lines: set[int] = set()
    offset = 0
    for part in parts:
        directive_lines.update(offset + index for index in part.directive_lines)
        texts.append(part.text)
        offset += part.line_count + separator.count("\n") - 1
    return MarkedText(
        text=separator.join(texts), directive_lines=frozenset(directive_lines)
    )


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
    message: str, preamble: MarkedText, segment_bodies: list[MarkedText]
) -> tuple[MarkedText, list[MarkedText]]:
    if not message.rstrip().endswith(":"):
        return preamble, segment_bodies
    for body_index in range(len(segment_bodies) - 1, -1, -1):
        body = segment_bodies[body_index]
        if body.text:
            segment_bodies[body_index] = body.rewritten(
                _replace_terminal_colon(body.text)
            )
            return preamble, segment_bodies
    return preamble.rewritten(_replace_terminal_colon(preamble.text)), segment_bodies


def _ack_segment(
    segment: _TextGroup,
    body: MarkedText,
    *,
    withheld: bool,
    worktree_id: str | None,
) -> dict[str, Any]:
    """The wire record one keyed response contributes, honored or withheld.

    A withheld segment names no keys: the operator's items are still pending, so
    quoting them under this body would read as the answer the refusal did not
    give. It carries the body regardless, because the capture inside it landed.
    """
    return {
        "keys": [] if withheld else list(segment.keys),
        "html": _render_message_html_with_task_directives(
            body, worktree_id=worktree_id
        ),
        "disposition": SEGMENT_DISPOSITION_WITHHELD
        if withheld
        else span_disposition(segment.kind),
    }


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
    preamble = _join_marked(
        [
            group.marked
            for group in groups
            if group.kind is SpanKind.PROSE and group.body
        ],
        "\n\n",
    )
    segments = [group for group in groups if group.kind is not SpanKind.PROSE]
    segment_bodies = [group.marked for group in segments]
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
    display_sources: list[MarkedText] = [preamble] if preamble.text else []
    display_parts: list[str] = (
        [_display_text_with_task_directives(preamble)] if preamble.text else []
    )
    task_card_count = _task_directive_count(preamble)
    for segment, segment_body in zip(segments, segment_bodies, strict=True):
        refused = segment.kind is SpanKind.NACK
        # The archival authority leaves a reasonless NACK pending. It must not
        # become a refused/retired key merely because the transcript reducer
        # correctly identified its negative polarity.
        withheld = refused and not nack_response_is_honored(segment.body)
        # The ACK/NACK header is hidden in the UI, so capitalize the response's
        # first letter for display while keeping the spoken text verbatim.
        body = segment_body.rewritten(_capitalize_first(segment_body.text))
        # Withholding the refusal must not withhold what it captured, but a
        # refusal that said nothing at all captured nothing to show.
        if withheld and not body.text:
            continue
        task_card_count += _task_directive_count(body)
        display_body = _display_text_with_task_directives(body)
        ack_segments.append(
            _ack_segment(segment, body, withheld=withheld, worktree_id=worktree_id)
        )
        if not withheld:
            for keyed in segment.keys:
                if keyed not in seen_keys:
                    seen_keys.add(keyed)
                    ack_keys.append(keyed)
                (refused_keys if refused else acked_keys).add(keyed)
            if segment.spoken:
                ack_utterances.append(segment.spoken)
        if body.text:
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
        if preamble.text and segments
        else ""
    )
    display_source = _join_marked(display_sources, "\n")
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


def _build_message(
    message: AssembledMessage,
    *,
    worktree_id: str | None = None,
    call_previews: dict[str, str] | None = None,
    preview_index: _ToolPreviewIndex | None = None,
) -> AssistantMessage | None:
    """Project one assembled locus into the single envelope it can carry."""
    spans = message.spans
    at = message.at
    offset = at.offset if at.offset is not None else at.line
    timestamp = str(at.timestamp or "")
    key = render_cursor(timestamp, offset)
    compaction = _first_event(spans, Compaction)
    if compaction is not None:
        if not compaction.boundary:
            return None
        return _simple_message(
            key, offset, timestamp, kind="compaction", text="Context compacted"
        )
    visible = _visible_message(spans, key, offset, timestamp, worktree_id=worktree_id)
    if visible is not None:
        return visible
    return _presence_envelope(
        spans,
        key,
        offset,
        timestamp,
        call_previews=call_previews,
        preview_index=preview_index,
    )


def _visible_message(
    spans: Sequence[ClassifiedSpan],
    key: str,
    offset: int,
    timestamp: str,
    *,
    worktree_id: str | None,
) -> AssistantMessage | None:
    """The prose, picture, or picture request this locus shows the operator."""
    groups = _text_groups(spans)
    if groups:
        return _assistant_message(
            key,
            offset,
            timestamp,
            _source_text(spans),
            groups=groups,
            kind="final" if _has_final_answer(spans) else "assistant",
            source_kind="assistant_text",
            worktree_id=worktree_id,
        )
    images = [span.event for span in spans if isinstance(span.event, Image)]
    source_kind = _image_source_kind(images)
    if source_kind is not None:
        markdown = image_markdown(images, worktree_id=worktree_id, source_offset=offset)
        if markdown is not None:
            return _markdown_message(
                key, offset, timestamp, markdown, source_kind, worktree_id
            )
    call = _first_event(spans, ToolCall)
    view_markdown = None if call is None else view_image_markdown(call)
    if view_markdown is not None:
        return _markdown_message(
            key, offset, timestamp, view_markdown, "view_image_call", worktree_id
        )
    return None


def _presence_envelope(
    spans: Sequence[ClassifiedSpan],
    key: str,
    offset: int,
    timestamp: str,
    *,
    call_previews: dict[str, str] | None,
    preview_index: _ToolPreviewIndex | None,
) -> AssistantMessage | None:
    """The activity record a locus with nothing visible still contributes."""
    presence = _presence_facts(spans)
    if presence is None:
        return None
    if presence.call is not None:
        plan_items = _plan_items(presence.call)
        if plan_items is not None:
            return _presence_message(
                key,
                offset,
                timestamp,
                kind="update_plan",
                preview="to-do list update",
                plan_items=plan_items,
            )
    preview = _preview_for_presence(presence, call_previews=call_previews)
    _remember_call_preview(presence, preview, call_previews)
    if preview_index is not None:
        preview_index.observe(presence, offset=offset, preview=preview)
    return _presence_message(
        key, offset, timestamp, kind=presence.kind, preview=preview
    )


def _markdown_message(
    key: str,
    offset: int,
    timestamp: str,
    markdown: str,
    source_kind: str,
    worktree_id: str | None,
) -> AssistantMessage:
    """One picture rendered as the whole of an otherwise wordless message."""
    group = _TextGroup(kind=SpanKind.PROSE, keys=())
    group.chunks.append((markdown, False))
    group.spoken_lines.append(markdown)
    return _assistant_message(
        key,
        offset,
        timestamp,
        markdown,
        groups=[group],
        kind="assistant",
        source_kind=source_kind,
        worktree_id=worktree_id,
    )


def _first_event(
    spans: Sequence[ClassifiedSpan], wanted: type[_EventT]
) -> _EventT | None:
    return next(
        (span.event for span in spans if isinstance(span.event, wanted)),
        None,
    )


def _has_final_answer(spans: Sequence[ClassifiedSpan]) -> bool:
    return any(span.kind is SpanKind.FINAL_ANSWER for span in spans)


def _source_text(spans: Sequence[ClassifiedSpan]) -> str:
    """The prose exactly as written, with each block of a line kept apart."""
    texts: list[str] = []
    previous: AssistantText | None = None
    for span in spans:
        event = span.event
        if isinstance(event, AssistantText) and event is not previous:
            previous = event
            if event.text.strip():
                texts.append(event.text)
    return "\n\n".join(texts).strip()


def _is_tool_output_image(image: Image) -> bool:
    return image.tool_output_type == "function_call_output"


def _image_source_kind(images: Sequence[Image]) -> str | None:
    """Where a locus' pictures came from, or None when it shows none."""
    if not images:
        return None
    if any(_is_tool_output_image(image) for image in images):
        return "tool_output_image"
    if all(image.role in (None, "assistant") for image in images):
        return "assistant_image"
    return None


def _presence_facts(spans: Sequence[ClassifiedSpan]) -> _PresenceFacts | None:
    """The single activity a locus reports, tool results before requests."""
    outputs = [span.event for span in spans if isinstance(span.event, ToolOutput)]
    if outputs:
        return _PresenceFacts(
            kind=outputs[0].tool_output_type,
            call_id=_event_call_id(outputs[0]),
            output_text="\n".join(output.content for output in outputs),
        )
    call = _first_event(spans, ToolCall)
    if call is not None:
        return _PresenceFacts(
            kind="custom_tool_call" if call.custom else "function_call",
            call_id=_event_call_id(call),
            call=call,
        )
    search = _first_event(spans, WebSearch)
    if search is not None:
        return _PresenceFacts(kind=_WEB_SEARCH_KIND, search=search)
    reasonings = [span.event for span in spans if isinstance(span.event, Reasoning)]
    if reasonings:
        summary = next(
            (event.summary for event in reasonings if event.summary.strip()), ""
        )
        return _PresenceFacts(kind=_REASONING_KIND, reasoning=summary)
    return None


def _event_call_id(event: ToolCall | ToolOutput) -> str:
    for value in (event.call_id, event.item_id):
        if value and value.strip():
            return value.strip()
    return ""
