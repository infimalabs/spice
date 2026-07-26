"""Assistant message envelopes streamed from the agent transcript.

Each transcript line becomes at most one envelope keyed `timestamp#offset`
(the byte offset doubles as a stable cursor). Visible envelopes are assistant
prose (ACK-segmented), final answers, plan updates, and compaction dividers;
tool calls and reasoning become *presence* records that carry activity previews
without consuming the visible message budget. Files larger than the tail cap are
scanned backwards in chunks so a season-long transcript stays cheap to page.
"""

from __future__ import annotations

import json
import re
from collections.abc import Sequence
from dataclasses import dataclass, field, replace
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, TypeVar

from spice.agent.driver import (
    AgentDriver,
    all_drivers,
    driver_for,
    driver_for_transcript,
)
from spice.agent.identity import canonical_thread_id
from spice.mail.feedback import supervisor_feedback_notices
from spice.serve.images import image_markdown, view_image_markdown
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
from spice.transcript.reader import (
    REVERSE_WINDOW_BYTES,
    TranscriptCursor,
    TranscriptEventReader,
    cursor_offset,
    locked_cursor,
    offset_after_line,
    render_cursor,
    transcript_file_identity,
    transcript_size,
)
from spice.transcript.timestamps import parse_timestamp

IMAGE_REFERENCE_RE = re.compile(r"!\[[^\]]*\]\((?:<[^>]*>|[^)]*)\)")

ACTIVE_ASSISTANT_SECONDS = 60
ACTIVEISH_ASSISTANT_SECONDS = 5 * 60
DEFAULT_MESSAGE_LIMIT = 200
MAX_MESSAGE_LIMIT = 400
_PREVIEW_MAX_CHARS = 120

_EventT = TypeVar("_EventT", bound=TranscriptEvent)
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


@dataclass
class RolloutCursor(TranscriptCursor):
    # Append-only transcripts: the initial no-`before` read seeds this cache;
    # same-size reads reuse it, and watcher growth reads extend it from
    # `offset` instead of rescanning the transcript tail.
    window: list[AssistantMessage] | None = field(default=None, repr=False)
    window_size: int = -1
    window_limit: int = -1
    removed_keys: list[str] = field(default_factory=list, repr=False)


@dataclass(frozen=True)
class TranscriptResolution:
    thread_id: str
    path: Path
    owner_driver: AgentDriver


@dataclass(frozen=True)
class AssistantMessageRead:
    items: list[AssistantMessage]
    error: str | None
    transcript: TranscriptResolution | None


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
    lines: list[str] = field(default_factory=list)
    spoken_lines: list[str] = field(default_factory=list)

    @property
    def marker(self) -> tuple[SpanKind, tuple[str, ...]]:
        return (self.kind, self.keys)

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


@dataclass
class _ToolPreviewIndex:
    """Call/output preview facts collected during one event projection pass."""

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


def resolve_thread_transcript(
    thread_id: str, repo_root: Path | None = None
) -> TranscriptResolution | None:
    """Locate a thread's transcript and the driver that owns it."""
    canonical = canonical_thread_id(thread_id)
    if not canonical:
        return None
    preferred = driver_for(repo_root)
    ordered = [preferred, *(d for d in all_drivers() if d.name != preferred.name)]
    for driver in ordered:
        try:
            return TranscriptResolution(
                thread_id=canonical,
                path=driver.thread_transcript_path(canonical),
                owner_driver=driver,
            )
        except (RuntimeError, SystemExit):
            continue
    return None


def assistant_messages_for_thread_id(
    thread_id: str,
    *,
    limit: int = DEFAULT_MESSAGE_LIMIT,
    after: str | None = None,
    before: str | None = None,
    append_only: bool = False,
    cursor: RolloutCursor | None = None,
    worktree_id: str | None = None,
    repo_root: Path | None = None,
) -> AssistantMessageRead:
    transcript = resolve_thread_transcript(thread_id, repo_root)
    if transcript is None or not transcript.path.is_file():
        return AssistantMessageRead(
            items=[],
            error=f"Could not resolve transcript for {thread_id}",
            transcript=transcript,
        )
    return AssistantMessageRead(
        items=read_assistant_messages(
            transcript.path,
            limit=limit,
            after=after,
            before=before,
            append_only=append_only,
            cursor=cursor,
            worktree_id=worktree_id,
            driver=transcript.owner_driver,
        ),
        error=None,
        transcript=transcript,
    )


def read_assistant_messages(
    transcript_path: Path,
    *,
    limit: int = DEFAULT_MESSAGE_LIMIT,
    after: str | None = None,
    before: str | None = None,
    append_only: bool = False,
    cursor: RolloutCursor | None = None,
    worktree_id: str | None = None,
    driver: AgentDriver | None = None,
) -> list[AssistantMessage]:
    bounded = max(1, min(limit, MAX_MESSAGE_LIMIT))
    owner_driver = driver or driver_for_transcript(transcript_path)
    if cursor is not None:
        with locked_cursor(cursor):
            cursor.removed_keys = []
            return _read_locked(
                transcript_path,
                limit=bounded,
                after=after,
                before=before,
                append_only=append_only,
                cursor=cursor,
                worktree_id=worktree_id,
                driver=owner_driver,
            )
    return _read_locked(
        transcript_path,
        limit=bounded,
        after=after,
        before=before,
        append_only=append_only,
        cursor=None,
        worktree_id=worktree_id,
        driver=owner_driver,
    )


def _read_locked(
    transcript_path: Path,
    *,
    limit: int,
    after: str | None,
    before: str | None,
    append_only: bool,
    cursor: RolloutCursor | None,
    worktree_id: str | None,
    driver: AgentDriver,
) -> list[AssistantMessage]:
    if before is not None:
        end_offset = cursor_offset(before)
        if end_offset is None:
            return []
        return _read_window(
            transcript_path,
            limit=limit,
            end_offset=end_offset,
            cursor=None,
            worktree_id=worktree_id,
            driver=driver,
        )
    if (
        append_only
        and cursor is not None
        and (after is None or after == cursor.last_key)
    ):
        return _read_appended_window(
            transcript_path,
            limit=limit,
            cursor=cursor,
            worktree_id=worktree_id,
            driver=driver,
        )
    if cursor is not None and after and after == cursor.last_key:
        return _read_from_offset(
            transcript_path,
            start_offset=cursor.offset,
            limit=limit,
            cursor=cursor,
            worktree_id=worktree_id,
            driver=driver,
        )
    if after is not None:
        after_offset = cursor_offset(after)
        if after_offset is not None:
            return _read_from_offset(
                transcript_path,
                start_offset=offset_after_line(transcript_path, after_offset),
                limit=limit,
                cursor=cursor,
                worktree_id=worktree_id,
                driver=driver,
            )
    return _read_window(
        transcript_path,
        limit=limit,
        end_offset=None,
        cursor=cursor,
        worktree_id=worktree_id,
        driver=driver,
    )


def _read_from_offset(
    transcript_path: Path,
    *,
    start_offset: int,
    limit: int,
    cursor: RolloutCursor | None,
    worktree_id: str | None,
    driver: AgentDriver,
) -> list[AssistantMessage]:
    try:
        messages, end_offset = _read_chronological_from_offset(
            transcript_path,
            start_offset=start_offset,
            worktree_id=worktree_id,
            driver=driver,
        )
    except OSError:
        return []
    kept = _trim_chronological(messages, limit)
    if cursor is not None:
        cursor.offset = end_offset
        if messages:
            cursor.last_key = messages[-1].key
    return list(reversed(_collapse_view_image_pairs(kept)))


def _read_appended_window(
    transcript_path: Path,
    *,
    limit: int,
    cursor: RolloutCursor,
    worktree_id: str | None,
    driver: AgentDriver,
) -> list[AssistantMessage]:
    file_size = transcript_size(transcript_path)
    file_identity = transcript_file_identity(transcript_path)
    if file_size is None or file_identity is None:
        return []
    if (
        cursor.window is None
        or cursor.window_limit != limit
        or file_size < cursor.offset
        or file_size < cursor.window_size
        or (cursor.file_identity is not None and cursor.file_identity != file_identity)
    ):
        return _read_window(
            transcript_path,
            limit=limit,
            end_offset=None,
            cursor=cursor,
            worktree_id=worktree_id,
            driver=driver,
        )
    if file_size == cursor.window_size:
        cursor.offset = file_size
        return []
    read = _reader(transcript_path, driver).read("forward", cursor=cursor)
    if read.error is not None:
        return []
    if read.file_identity != file_identity:
        return _read_window(
            transcript_path,
            limit=limit,
            end_offset=None,
            cursor=cursor,
            worktree_id=worktree_id,
            driver=driver,
        )
    appended = _messages_from_events(read.events, worktree_id=worktree_id)
    end_offset = read.end_offset
    previous = list(reversed(cursor.window))
    previous_tail = previous[-1:] if previous else []
    combined = previous + appended
    window = _collapse_view_image_pairs(_trim_chronological(combined, limit))
    delta = _collapse_view_image_pairs(
        previous_tail + _trim_chronological(appended, limit)
    )
    tail_keys = {message.key for message in previous_tail}
    delta_keys = {message.key for message in delta}
    cursor.removed_keys = [
        message.key for message in previous_tail if message.key not in delta_keys
    ]
    cursor.offset = end_offset
    if appended:
        cursor.last_key = appended[-1].key
    cursor.window = list(reversed(window))
    cursor.window_size = end_offset
    cursor.window_limit = limit
    return [message for message in reversed(delta) if message.key not in tail_keys]


def _read_chronological_from_offset(
    transcript_path: Path,
    *,
    start_offset: int,
    worktree_id: str | None,
    driver: AgentDriver,
) -> tuple[list[AssistantMessage], int]:
    read = _reader(transcript_path, driver).read(
        "forward",
        cursor=TranscriptCursor(offset=start_offset),
    )
    return (
        _messages_from_events(read.events, worktree_id=worktree_id),
        read.end_offset,
    )


def _read_window(
    transcript_path: Path,
    *,
    limit: int,
    end_offset: int | None,
    cursor: RolloutCursor | None,
    worktree_id: str | None,
    driver: AgentDriver,
) -> list[AssistantMessage]:
    """Newest-first window ending at `end_offset` (or EOF), tail-scanned."""
    file_size = transcript_size(transcript_path)
    file_identity = transcript_file_identity(transcript_path)
    if file_size is None or file_identity is None:
        return []
    if (
        end_offset is None
        and cursor is not None
        and cursor.window is not None
        and cursor.window_size == file_size
        and cursor.window_limit == limit
        and cursor.file_identity == file_identity
    ):
        return list(cursor.window)
    reader = _reader(transcript_path, driver)
    read = reader.read(
        "reverse",
        end_offset=end_offset,
        max_bytes=REVERSE_WINDOW_BYTES,
    )
    preview_index = _ToolPreviewIndex()
    scanned = _messages_from_events(
        read.events,
        worktree_id=worktree_id,
        preview_index=preview_index,
    )
    visible_count = sum(not message.kind.startswith("presence:") for message in scanned)
    scan_start = read.access_start_offset
    while visible_count < limit and scan_start > 0:
        older = reader.read(
            "bounded",
            start_offset=max(0, scan_start - REVERSE_WINDOW_BYTES),
            end_offset=scan_start,
            align_partial_start=True,
        )
        if older.access_start_offset >= scan_start:
            break
        if older.events:
            projected = _messages_from_events(
                older.events,
                worktree_id=worktree_id,
                preview_index=preview_index,
            )
            scanned[0:0] = projected
            visible_count += sum(
                not message.kind.startswith("presence:") for message in projected
            )
        scan_start = older.access_start_offset
    scanned = preview_index.resolve(scanned)
    visible = [
        message for message in scanned if not message.kind.startswith("presence:")
    ]
    presence = [message for message in scanned if message.kind.startswith("presence:")]
    kept = list(visible[-limit:])
    if end_offset is None:
        kept.extend(_kept_presence_messages(presence))
    kept.sort(key=lambda message: message.index)
    kept = _collapse_view_image_pairs(kept)
    if end_offset is not None and _line_has_tool_output_image(
        transcript_path, end_offset, driver=driver
    ):
        kept = _drop_trailing_view_image_call(kept)
    result = list(reversed(kept))
    if cursor is not None and end_offset is None:
        cursor.offset = file_size
        cursor.last_key = kept[-1].key if kept else None
        cursor.window = result
        cursor.window_size = file_size
        cursor.window_limit = limit
        cursor.file_identity = read.file_identity
    return result


def _reader(transcript_path: Path, driver: AgentDriver) -> TranscriptEventReader:
    return TranscriptEventReader(transcript_path, driver, source_actor=None)


def _messages_from_events(
    events: Sequence[TranscriptEvent],
    *,
    worktree_id: str | None,
    preview_index: _ToolPreviewIndex | None = None,
) -> list[AssistantMessage]:
    """Project one access pass of typed facts into envelopes, in source order."""
    messages: list[AssistantMessage] = []
    call_previews: dict[str, str] = {}
    reducer = AssembledMessageReducer()

    def consume(assembled: AssembledMessage) -> None:
        message = _build_message(
            assembled,
            worktree_id=worktree_id,
            call_previews=call_previews,
            preview_index=preview_index,
        )
        if message is not None:
            messages.append(message)

    for event in events:
        for assembled in reducer.push(event):
            consume(assembled)
    for assembled in reducer.finish():
        consume(assembled)
    return messages


def _collapse_view_image_pairs(
    messages: list[AssistantMessage],
) -> list[AssistantMessage]:
    """Drop a `view_image` call immediately followed by its output image."""
    collapsed: list[AssistantMessage] = []
    index = 0
    while index < len(messages):
        message = messages[index]
        follower = messages[index + 1] if index + 1 < len(messages) else None
        if (
            message.source_kind == "view_image_call"
            and follower is not None
            and follower.source_kind == "tool_output_image"
        ):
            index += 1
            continue
        collapsed.append(message)
        index += 1
    return collapsed


def _drop_trailing_view_image_call(
    messages: list[AssistantMessage],
) -> list[AssistantMessage]:
    if messages and messages[-1].source_kind == "view_image_call":
        return messages[:-1]
    return messages


def _line_has_tool_output_image(
    transcript_path: Path, offset: int, *, driver: AgentDriver
) -> bool:
    """The paging boundary line pairs with a trailing `view_image` call."""
    read = _reader(transcript_path, driver).read(
        "bounded", start_offset=offset, end_offset=offset + 1
    )
    return any(
        isinstance(event, Image) and _is_tool_output_image(event)
        for event in read.events
    )


def _trim_chronological(
    messages: list[AssistantMessage], limit: int
) -> list[AssistantMessage]:
    """Keep newest visible records plus retained presence records."""
    kept_presence = _kept_presence_messages(
        [message for message in messages if message.kind.startswith("presence:")]
    )
    kept: list[AssistantMessage] = []
    visible = 0
    for message in reversed(messages):
        if message.kind.startswith("presence:"):
            continue
        if visible >= limit:
            continue
        kept.append(message)
        visible += 1
    kept.extend(kept_presence)
    return sorted(
        {message.key: message for message in kept}.values(),
        key=lambda message: message.index,
    )


def _kept_presence_messages(messages: list[AssistantMessage]) -> list[AssistantMessage]:
    feedback = [
        message for message in messages if _is_supervisor_feedback_presence(message)
    ]
    latest_presence = next(
        (
            message
            for message in reversed(messages)
            if not _is_supervisor_feedback_presence(message)
        ),
        None,
    )
    kept = [*feedback]
    if latest_presence is not None:
        kept.append(latest_presence)
    return sorted(
        {message.key: message for message in kept}.values(),
        key=lambda message: message.index,
    )


def _is_supervisor_feedback_presence(message: AssistantMessage) -> bool:
    return (
        message.kind.startswith("presence:")
        and message.source_kind in _TOOL_OUTPUT_TYPES
        and message.preview.startswith(_SUPERVISOR_FEEDBACK_PREVIEW_PREFIXES)
    )


def activity_status(messages: list[AssistantMessage]) -> str:
    if not messages:
        return "unknown"
    timestamp = parse_timestamp(messages[0].timestamp)
    if timestamp is None:
        return "unknown"
    age_seconds = (datetime.now(UTC) - timestamp).total_seconds()
    if age_seconds < ACTIVE_ASSISTANT_SECONDS:
        return "active"
    if age_seconds < ACTIVEISH_ASSISTANT_SECONDS:
        return "active-ish"
    return "inactive"


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
    group.lines.append(markdown)
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
        marker = (span.kind, span.keys)
        if not groups or groups[-1].marker != marker:
            groups.append(_TextGroup(kind=span.kind, keys=span.keys))
        _absorb_span(groups[-1], span)
    return [group for group in groups if group.body or group.spoken]


def _absorb_span(group: _TextGroup, span: ClassifiedSpan) -> None:
    if span.directive_kind is not None:
        if span.directive_kind is DirectiveKind.TASK:
            group.lines.append(span.text)
        return
    group.lines.append(span.text)
    group.spoken_lines.append(span.text)


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
    for segment, segment_body in zip(segments, segment_bodies, strict=True):
        refused = segment.kind is SpanKind.NACK
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
