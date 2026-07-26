"""Assistant message envelopes streamed from the agent transcript.

Each transcript line becomes at most one envelope keyed `timestamp#offset`
(the byte offset doubles as a stable cursor). Visible envelopes are assistant
prose (ACK-segmented), final answers, plan updates, and compaction dividers;
tool calls and reasoning become *presence* records that carry activity previews
without consuming the visible message budget. Files larger than the tail cap are
scanned backwards in chunks so a season-long transcript stays cheap to page.
"""

from __future__ import annotations

from dataclasses import dataclass, field, replace
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from spice.agent.driver import (
    AgentDriver,
    all_drivers,
    driver_for,
    driver_for_transcript,
)
from spice.agent.identity import canonical_thread_id
from spice.serve.images import (
    assistant_image_markdown,
    tool_output_image_markdown,
    view_image_markdown,
)
from spice.serve.messagepresentation import (
    _PRESENCE_PAYLOAD_TYPES,
    _SUPERVISOR_FEEDBACK_OUTPUT_TYPES,
    _SUPERVISOR_FEEDBACK_PREVIEW_PREFIXES,
    AssistantMessage,
    _assistant_message,
    _payload_call_id,
    _payload_output_text,
    _plan_items,
    _presence_message,
    _preview_for_presence,
    _preview_from_text,
    _remember_call_preview,
    _render_tool_output_preview,
    _simple_message,
    _supervisor_feedback_items as _supervisor_feedback_items,
    _supervisor_feedback_preview,
    reply_card_message as reply_card_message,
    task_card_message as task_card_message,
)
from spice.transcript.assembly import (
    AssembledMessage,
    AssembledMessageReducer,
)
from spice.transcript.reader import (
    REVERSE_WINDOW_BYTES,
    TranscriptCursor,
    TranscriptLine,
    cursor_offset,
    dispatch_records,
    locked_cursor,
    offset_after_line,
    read_bounded,
    read_forward,
    read_line,
    read_reverse_window,
    record_assistant_text,
    render_cursor,
    transcript_file_identity,
    transcript_size,
)
from spice.transcript.timestamps import parse_timestamp

ACTIVE_ASSISTANT_SECONDS = 60
ACTIVEISH_ASSISTANT_SECONDS = 5 * 60
DEFAULT_MESSAGE_LIMIT = 200
MAX_MESSAGE_LIMIT = 400


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
    """Call/output preview facts collected during one record projection pass."""

    calls: dict[str, list[tuple[int, str]]] = field(default_factory=dict)
    outputs: dict[int, tuple[str, str]] = field(default_factory=dict)

    def observe(
        self,
        *,
        offset: int,
        payload: dict[str, Any],
        payload_type: str,
        preview: str,
    ) -> None:
        call_id = _payload_call_id(payload)
        if payload_type in {"function_call", "custom_tool_call"}:
            if call_id and preview:
                self.calls.setdefault(call_id, []).append((offset, preview))
            return
        if payload_type not in {"function_call_output", "custom_tool_call_output"}:
            return
        if _supervisor_feedback_preview(payload):
            return
        self.outputs[offset] = (
            call_id,
            _preview_from_text(_payload_output_text(payload)),
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
    read = read_forward(transcript_path, cursor=cursor)
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
    appended = _messages_from_records(
        read.records,
        driver=driver,
        worktree_id=worktree_id,
    )
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
    read = read_forward(
        transcript_path,
        cursor=TranscriptCursor(offset=start_offset),
    )
    return (
        _messages_from_records(
            read.records,
            driver=driver,
            worktree_id=worktree_id,
        ),
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
    read = read_reverse_window(
        transcript_path,
        end_offset=end_offset,
        max_bytes=REVERSE_WINDOW_BYTES,
    )
    preview_index = _ToolPreviewIndex()
    scanned = _messages_from_records(
        read.records,
        driver=driver,
        worktree_id=worktree_id,
        preview_index=preview_index,
    )
    visible_count = sum(not message.kind.startswith("presence:") for message in scanned)
    scan_start = read.access_start_offset
    while visible_count < limit and scan_start > 0:
        older = read_bounded(
            transcript_path,
            start_offset=max(0, scan_start - REVERSE_WINDOW_BYTES),
            end_offset=scan_start,
            align_partial_start=True,
        )
        if older.access_start_offset >= scan_start:
            break
        if older.records:
            projected = _messages_from_records(
                older.records,
                driver=driver,
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


def _messages_from_records(
    records: tuple[TranscriptLine, ...],
    *,
    driver: AgentDriver,
    worktree_id: str | None,
    preview_index: _ToolPreviewIndex | None = None,
) -> list[AssistantMessage]:
    messages: list[AssistantMessage] = []
    call_previews: dict[str, str] = {}

    def consume(record: TranscriptLine) -> None:
        message = _build_message(
            record,
            driver=driver,
            worktree_id=worktree_id,
            call_previews=call_previews,
            preview_index=preview_index,
        )
        if message is not None:
            messages.append(message)

    dispatch_records(records, consume)
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
    record = read_line(transcript_path, offset)
    loaded = record.parsed if record is not None else None
    if loaded is None:
        return False
    event = driver.normalize_transcript_line(loaded)
    if event is None or event.get("type") != "response_item":
        return False
    payload = event.get("payload")
    if not isinstance(payload, dict):
        return False
    markdown = tool_output_image_markdown(payload, worktree_id=None, source_offset=None)
    return markdown is not None


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
        and message.source_kind in _SUPERVISOR_FEEDBACK_OUTPUT_TYPES
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


def _classified_record_text(
    record: TranscriptLine,
    driver: AgentDriver,
) -> AssembledMessage | None:
    reducer = AssembledMessageReducer()
    messages: list[AssembledMessage] = []
    for event in record_assistant_text(record, driver):
        messages.extend(reducer.push(event))
    messages.extend(reducer.finish())
    if not messages:
        return None
    if len(messages) != 1:
        raise ValueError(
            f"one transcript record assembled into {len(messages)} text messages"
        )
    return messages[0]


def _build_message(
    record: TranscriptLine,
    *,
    driver: AgentDriver,
    worktree_id: str | None = None,
    call_previews: dict[str, str] | None = None,
    preview_index: _ToolPreviewIndex | None = None,
) -> AssistantMessage | None:
    loaded = record.parsed
    if loaded is None:
        return None
    event = driver.normalize_transcript_line(loaded)
    if event is None:
        return None
    offset = record.offset
    timestamp = str(event.get("timestamp") or "")
    key = render_cursor(timestamp, offset)
    if event.get("type") == "compacted":
        return _simple_message(
            key, offset, timestamp, kind="compaction", text="Context compacted"
        )
    if event.get("type") != "response_item":
        return None
    payload = event.get("payload") or {}
    classified = _classified_record_text(record, driver)
    source_event = (
        next(
            (event for event in classified.assistant_text_events if event.text),
            None,
        )
        if classified is not None
        else None
    )
    text = source_event.text if source_event is not None else None
    source_kind = "assistant_text"
    if text is None:
        text = assistant_image_markdown(
            payload, worktree_id=worktree_id, source_offset=offset
        )
        source_kind = "assistant_image"
    if text is None:
        text = tool_output_image_markdown(
            payload, worktree_id=worktree_id, source_offset=offset
        )
        source_kind = "tool_output_image"
    if text is None:
        text = view_image_markdown(payload)
        source_kind = "view_image_call"
    if text is not None:
        kind = "final" if payload.get("phase") == "final_answer" else "assistant"
        return _assistant_message(
            key,
            offset,
            timestamp,
            text,
            kind=kind,
            source_kind=source_kind,
            worktree_id=worktree_id,
            classified=classified if source_event is not None else None,
            source_event=source_event,
        )
    plan_items = _plan_items(payload)
    if plan_items is not None:
        return _presence_message(
            key,
            offset,
            timestamp,
            kind="update_plan",
            preview="to-do list update",
            plan_items=plan_items,
        )
    payload_type = payload.get("type")
    if isinstance(payload_type, str) and payload_type in _PRESENCE_PAYLOAD_TYPES:
        preview = _preview_for_presence(
            payload, payload_type, call_previews=call_previews
        )
        _remember_call_preview(payload, payload_type, preview, call_previews)
        if preview_index is not None:
            preview_index.observe(
                offset=offset,
                payload=payload,
                payload_type=payload_type,
                preview=preview,
            )
        return _presence_message(
            key,
            offset,
            timestamp,
            kind=payload_type,
            preview=preview,
        )
    return None
