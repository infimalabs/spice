"""Assistant message envelopes streamed from the agent transcript.

Each transcript line becomes at most one envelope keyed `timestamp#offset`
(the byte offset doubles as a stable cursor). Visible envelopes are assistant
prose (ACK-segmented), final answers, plan updates, and compaction dividers;
tool calls and reasoning become *presence* records that carry activity previews
without consuming the visible message budget. Files larger than the tail cap are
scanned backwards in chunks so a season-long transcript stays cheap to page.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass, field, replace
from datetime import UTC, datetime
from pathlib import Path
from typing import TypeVar

from spice.agent.driver import (
    AgentDriver,
    all_drivers,
    driver_for,
    driver_for_transcript,
)
from spice.agent.identity import canonical_thread_id
from spice.serve.images import image_markdown, view_image_markdown
from spice.serve.messagepresentation import (
    _REASONING_KIND,
    _SUPERVISOR_FEEDBACK_PREVIEW_PREFIXES,
    _TOOL_CALL_TYPES,
    _TOOL_OUTPUT_TYPES,
    _WEB_SEARCH_KIND,
    AssistantMessage,
    _assistant_message,
    _plan_items,
    _PresenceFacts,
    _presence_message,
    _preview_for_presence,
    _preview_from_text,
    _remember_call_preview,
    _render_tool_output_preview,
    _simple_message,
    _supervisor_feedback_items as _supervisor_feedback_items,
    _supervisor_feedback_preview,
    _TextGroup,
    _text_groups,
    reply_card_message as reply_card_message,
    task_card_message as task_card_message,
)
from spice.transcript.assembly import (
    AssembledMessage,
    AssembledMessageReducer,
    ClassifiedSpan,
    SpanKind,
)
from spice.transcript.events import (
    AssistantText,
    Compaction,
    Image,
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

ACTIVE_ASSISTANT_SECONDS = 60
ACTIVEISH_ASSISTANT_SECONDS = 5 * 60
DEFAULT_MESSAGE_LIMIT = 200
MAX_MESSAGE_LIMIT = 400

_EventT = TypeVar("_EventT", bound=TranscriptEvent)


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
