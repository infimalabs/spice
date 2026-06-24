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
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from threading import RLock
from typing import Any

from spice.agent.driver import (
    ALL_DRIVERS,
    AgentDriver,
    driver_for,
    driver_for_transcript,
)
from spice.agent.identity import canonical_thread_id
from spice.mail.feedback import SupervisorFeedback, parse_supervisor_feedback_line
from spice.mail.acks import split_ack_message
from spice.mail.watch import (
    extract_assistant_text,
    strip_app_directive_lines,
)
from spice.serve.images import (
    assistant_image_markdown,
    tool_output_image_markdown,
    view_image_markdown,
)
from spice.serve.markdown import render_message_html
from spice.serve.taskdirectives import (
    _display_text_with_task_directives,
    _render_message_html_with_task_directives,
    _strip_task_directive_lines,
    _task_directive_count,
    _task_directive_html,
    _task_directive_summary,
)

IMAGE_REFERENCE_RE = re.compile(r"!\[[^\]]*\]\((?:<[^>]*>|[^)]*)\)")
TAIL_SCAN_CHUNK_BYTES = 1024 * 1024
TAIL_SCAN_MAX_BYTES = 8 * 1024 * 1024

ACTIVE_ASSISTANT_SECONDS = 60
ACTIVEISH_ASSISTANT_SECONDS = 5 * 60
DEFAULT_MESSAGE_LIMIT = 200
MAX_MESSAGE_LIMIT = 400
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
_ACK_UNMATCHED_KIND = "ack.unmatched"
_TASK_CREATED_KIND = "task.created"
_TASK_ERROR_KIND = "task.error"


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
            "ack_utterances": self.ack_utterances,
            "ack_segments": self.ack_segments,
            "speech_utterances": self.speech_utterances,
            "plan_items": self.plan_items,
        }


@dataclass
class RolloutCursor:
    offset: int = 0
    last_key: str | None = None
    lock: RLock = field(default_factory=RLock, repr=False)
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


def resolve_thread_transcript(
    thread_id: str, repo_root: Path | None = None
) -> TranscriptResolution | None:
    """Locate a thread's transcript and the driver that owns it."""
    canonical = canonical_thread_id(thread_id)
    if not canonical:
        return None
    preferred = driver_for(repo_root)
    ordered = [preferred, *(d for d in ALL_DRIVERS if d is not preferred)]
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
        with cursor.lock:
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


def read_metric_messages_from_offset(
    transcript_path: Path,
    *,
    start_offset: int,
    worktree_id: str | None = None,
) -> tuple[list[AssistantMessage], int]:
    """Read metric-relevant transcript records from a byte offset to EOF."""
    messages: list[AssistantMessage] = []
    driver = driver_for_transcript(transcript_path)
    file_size = transcript_path.stat().st_size
    if file_size < start_offset:
        start_offset = 0
    with transcript_path.open(encoding="utf-8", errors="replace") as handle:
        handle.seek(start_offset)
        while True:
            line_offset = handle.tell()
            line = handle.readline()
            if not line:
                return messages, handle.tell()
            message = _build_message(
                line_offset, line, driver=driver, worktree_id=worktree_id
            )
            if message is not None:
                messages.append(message)


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
        end_offset = _key_offset(before)
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
        after_offset = _key_offset(after)
        if after_offset is not None:
            return _read_from_offset(
                transcript_path,
                start_offset=_offset_after_line(transcript_path, after_offset),
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


def _offset_after_line(transcript_path: Path, line_offset: int) -> int:
    try:
        with transcript_path.open("rb") as handle:
            handle.seek(line_offset)
            handle.readline()
            return handle.tell()
    except OSError:
        return line_offset


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
    try:
        file_size = transcript_path.stat().st_size
    except OSError:
        return []
    if (
        cursor.window is None
        or cursor.window_limit != limit
        or file_size < cursor.offset
        or file_size < cursor.window_size
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
    try:
        appended, end_offset = _read_chronological_from_offset(
            transcript_path,
            start_offset=cursor.offset,
            worktree_id=worktree_id,
            driver=driver,
        )
    except OSError:
        return []
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
    file_size = transcript_path.stat().st_size
    if file_size < start_offset:
        start_offset = 0
    messages: list[AssistantMessage] = []
    with transcript_path.open(encoding="utf-8", errors="replace") as handle:
        handle.seek(start_offset)
        while True:
            line_offset = handle.tell()
            line = handle.readline()
            if not line:
                return messages, handle.tell()
            message = _build_message(
                line_offset, line, driver=driver, worktree_id=worktree_id
            )
            if message is not None:
                messages.append(message)


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
    try:
        file_size = transcript_path.stat().st_size
        if (
            end_offset is None
            and cursor is not None
            and cursor.window is not None
            and cursor.window_size == file_size
            and cursor.window_limit == limit
        ):
            return list(cursor.window)
        scan_end = file_size if end_offset is None else min(end_offset, file_size)
        start = max(0, scan_end - TAIL_SCAN_MAX_BYTES)
        newest: list[AssistantMessage] = []
        latest_presence: AssistantMessage | None = None
        while True:
            newest, latest_presence = _scan_span(
                transcript_path,
                start=start,
                end=scan_end,
                limit=limit,
                worktree_id=worktree_id,
                driver=driver,
            )
            if len(newest) >= limit or start == 0:
                break
            start = max(0, start - TAIL_SCAN_MAX_BYTES)
        kept = list(newest)
        if latest_presence is not None and end_offset is None:
            kept.append(latest_presence)
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
        return result
    except OSError:
        return []


def _scan_span(
    transcript_path: Path,
    *,
    start: int,
    end: int,
    limit: int,
    worktree_id: str | None,
    driver: AgentDriver,
) -> tuple[list[AssistantMessage], AssistantMessage | None]:
    visible: list[AssistantMessage] = []
    latest_presence: AssistantMessage | None = None
    with transcript_path.open(encoding="utf-8", errors="replace") as handle:
        handle.seek(start)
        if start:
            handle.readline()  # skip the partial line at the chunk boundary
        while True:
            line_offset = handle.tell()
            if line_offset >= end:
                break
            line = handle.readline()
            if not line:
                break
            message = _build_message(
                line_offset, line, driver=driver, worktree_id=worktree_id
            )
            if message is None:
                continue
            if message.kind.startswith("presence:"):
                latest_presence = message
                continue
            visible.append(message)
    return visible[-limit:], latest_presence


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
    try:
        with transcript_path.open(encoding="utf-8", errors="replace") as handle:
            handle.seek(offset)
            line = handle.readline()
    except OSError:
        return False
    loaded = _load_json_line(line)
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
    """Keep the newest visible records plus one newest presence record."""
    latest_presence = next(
        (
            message
            for message in reversed(messages)
            if message.kind.startswith("presence:")
        ),
        None,
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
    if latest_presence is not None:
        kept.append(latest_presence)
    return sorted(
        {message.key: message for message in kept}.values(),
        key=lambda message: message.index,
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
    offset: int, line: str, *, driver: AgentDriver, worktree_id: str | None = None
) -> AssistantMessage | None:
    loaded = _load_json_line(line)
    if loaded is None:
        return None
    event = driver.normalize_transcript_line(loaded)
    if event is None:
        return None
    timestamp = str(event.get("timestamp") or "")
    key = f"{timestamp}#{offset}" if timestamp else str(offset)
    if event.get("type") == "compacted":
        return _simple_message(
            key, offset, timestamp, kind="compaction", text="Context compacted"
        )
    if event.get("type") != "response_item":
        return None
    payload = event.get("payload") or {}
    text = extract_assistant_text(line, driver)
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
        return _presence_message(
            key,
            offset,
            timestamp,
            kind=payload_type,
            preview=_preview_for_presence(payload, payload_type),
        )
    return None


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
                        "kind": "task_created",
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
                    "kind": "task_error",
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
                        "kind": "ack_archived",
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
                        "kind": "ack_already_acked",
                        "label": "ACK already consumed",
                        "detail": ", ".join(keys),
                        "keys": keys,
                    }
                )
        elif feedback.kind == _ACK_UNMATCHED_KIND:
            keys = _feedback_string_list(feedback.fields.get("keys"))
            if keys:
                items.append(
                    {
                        "kind": "ack_unmatched",
                        "label": "Acknowledged (no pending match)",
                        "detail": ", ".join(keys),
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
) -> AssistantMessage:
    directive = {"fields": fields}
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


def _key_offset(key: str) -> int | None:
    raw = key.rsplit("#", 1)[-1]
    try:
        offset = int(raw)
    except ValueError:
        return None
    return offset if offset >= 0 else None


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


def _assistant_message(
    key: str,
    offset: int,
    timestamp: str,
    text: str,
    *,
    kind: str,
    source_kind: str = "assistant_text",
    worktree_id: str | None = None,
) -> AssistantMessage:
    preamble, segments = split_ack_message(text, drop_task_directives=False)
    preamble = strip_app_directive_lines(preamble)
    ack_segments: list[dict[str, Any]] = []
    ack_keys: list[str] = []
    seen_keys: set[str] = set()
    ack_utterances: list[str] = []
    display_sources: list[str] = [preamble] if preamble else []
    display_parts: list[str] = (
        [_display_text_with_task_directives(preamble)] if preamble else []
    )
    task_card_count = _task_directive_count(preamble)
    for segment in segments:
        # The ACK header is hidden in the UI, so capitalize the response's
        # first letter for display while keeping the spoken text verbatim.
        body = _capitalize_first(strip_app_directive_lines(segment.content))
        task_card_count += _task_directive_count(body)
        display_body = _display_text_with_task_directives(body)
        ack_segments.append(
            {
                "keys": list(segment.keys),
                "html": _render_message_html_with_task_directives(
                    body, worktree_id=worktree_id
                ),
            }
        )
        for ack_key in segment.keys:
            if ack_key not in seen_keys:
                seen_keys.add(ack_key)
                ack_keys.append(ack_key)
        spoken = _strip_task_directive_lines(strip_app_directive_lines(segment.content))
        if spoken:
            ack_utterances.append(spoken)
        if body:
            display_sources.append(body)
        if display_body:
            display_parts.append(display_body)
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
        ack_count=len(ack_keys),
        ack_keys=ack_keys,
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


def _preview_for_presence(payload: dict[str, Any], payload_type: str) -> str:
    supervisor_feedback = _supervisor_feedback_preview(payload)
    if supervisor_feedback:
        return supervisor_feedback
    if payload_type == "reasoning":
        return _preview_from_text(_reasoning_summary_text(payload)) or "thinking"
    if payload_type in {"function_call", "custom_tool_call"}:
        return _preview_for_call(payload) or "tool call"
    if payload_type in {"function_call_output", "custom_tool_call_output"}:
        return "tool output"
    if payload_type == "web_search_call":
        return _preview_for_web_search(payload) or "web search"
    return payload_type.replace("_", " ")


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


_PREVIEW_ARG_KEYS = ("path", "query", "url", "input", "prompt", "text")


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


def _load_json_line(line: str) -> dict[str, Any] | None:
    try:
        loaded = json.loads(line)
    except json.JSONDecodeError:
        return None
    return loaded if isinstance(loaded, dict) else None


def parse_timestamp(raw: str) -> datetime | None:
    if not raw:
        return None
    try:
        parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=UTC)
    return parsed.astimezone(UTC)
