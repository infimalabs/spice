"""Assistant message envelopes streamed from the agent transcript.

Each transcript line becomes at most one envelope keyed `timestamp#offset`
(the byte offset doubles as a stable cursor). Visible envelopes are assistant
prose (ACK-segmented, SAY-scrubbed), final answers, plan updates, and
compaction dividers; tool calls and reasoning become *presence* records that
carry activity previews without consuming the visible message budget. Files
larger than the tail cap are scanned backwards in chunks so a season-long
transcript stays cheap to page.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from threading import RLock
from typing import Any

from spice.agent.driver import DRIVER
from spice.mail.acks import split_ack_message
from spice.mail.watch import (
    extract_assistant_text,
    extract_say_directives,
    scrub_say_headers,
    strip_directive_lines,
)
from spice.serve.images import (
    assistant_image_markdown,
    tool_output_image_markdown,
    view_image_markdown,
)
from spice.serve.markdown import render_message_html

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
    say_count: int
    say_utterances: list[str]
    kind: str = "assistant"
    preview: str = ""
    image_only: bool = False
    source_kind: str = ""
    ack_segments: list[dict[str, Any]] = field(default_factory=list)
    preamble_html: str = ""
    plan_items: list[dict[str, str]] = field(default_factory=list)

    @property
    def speech_utterances(self) -> list[str]:
        return [*self.say_utterances, *self.ack_utterances]

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
            "ack_count": self.ack_count,
            "ack_keys": self.ack_keys,
            "ack_utterances": self.ack_utterances,
            "ack_segments": self.ack_segments,
            "say_count": self.say_count,
            "say_utterances": self.say_utterances,
            "speech_utterances": self.speech_utterances,
            "plan_items": self.plan_items,
        }


@dataclass
class RolloutCursor:
    offset: int = 0
    last_key: str | None = None
    lock: RLock = field(default_factory=RLock, repr=False)


def transcript_path_for_thread(thread_id: str) -> Path | None:
    try:
        return DRIVER.thread_transcript_path(thread_id)
    except (RuntimeError, SystemExit):
        return None


def assistant_messages_for_thread_id(
    thread_id: str,
    *,
    limit: int = DEFAULT_MESSAGE_LIMIT,
    after: str | None = None,
    before: str | None = None,
    cursor: RolloutCursor | None = None,
    worktree_id: str | None = None,
) -> tuple[list[AssistantMessage], str | None]:
    path = transcript_path_for_thread(thread_id)
    if path is None or not path.is_file():
        return [], f"Could not resolve transcript for {thread_id}"
    return (
        read_assistant_messages(
            path,
            limit=limit,
            after=after,
            before=before,
            cursor=cursor,
            worktree_id=worktree_id,
        ),
        None,
    )


def read_assistant_messages(
    transcript_path: Path,
    *,
    limit: int = DEFAULT_MESSAGE_LIMIT,
    after: str | None = None,
    before: str | None = None,
    cursor: RolloutCursor | None = None,
    worktree_id: str | None = None,
) -> list[AssistantMessage]:
    bounded = max(1, min(limit, MAX_MESSAGE_LIMIT))
    if cursor is not None:
        with cursor.lock:
            return _read_locked(
                transcript_path,
                limit=bounded,
                after=after,
                before=before,
                cursor=cursor,
                worktree_id=worktree_id,
            )
    return _read_locked(
        transcript_path,
        limit=bounded,
        after=after,
        before=before,
        cursor=None,
        worktree_id=worktree_id,
    )


def read_metric_messages_from_offset(
    transcript_path: Path,
    *,
    start_offset: int,
    worktree_id: str | None = None,
) -> tuple[list[AssistantMessage], int]:
    """Read metric-relevant transcript records from a byte offset to EOF."""
    messages: list[AssistantMessage] = []
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
            message = _build_message(line_offset, line, worktree_id=worktree_id)
            if message is not None:
                messages.append(message)


def _read_locked(
    transcript_path: Path,
    *,
    limit: int,
    after: str | None,
    before: str | None,
    cursor: RolloutCursor | None,
    worktree_id: str | None,
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
        )
    if cursor is not None and after and after == cursor.last_key:
        return _read_from_offset(
            transcript_path,
            start_offset=cursor.offset,
            limit=limit,
            cursor=cursor,
            worktree_id=worktree_id,
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
            )
    return _read_window(
        transcript_path,
        limit=limit,
        end_offset=None,
        cursor=cursor,
        worktree_id=worktree_id,
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
) -> list[AssistantMessage]:
    messages: list[AssistantMessage] = []
    try:
        file_size = transcript_path.stat().st_size
        if file_size < start_offset:
            start_offset = 0
        with transcript_path.open(encoding="utf-8", errors="replace") as handle:
            handle.seek(start_offset)
            while True:
                line_offset = handle.tell()
                line = handle.readline()
                if not line:
                    break
                message = _build_message(line_offset, line, worktree_id=worktree_id)
                if message is not None:
                    messages.append(message)
                    messages = _trim_chronological(messages, limit)
            if cursor is not None:
                cursor.offset = handle.tell()
                if messages:
                    cursor.last_key = messages[-1].key
    except OSError:
        return []
    return list(reversed(_collapse_view_image_pairs(messages)))


def _read_window(
    transcript_path: Path,
    *,
    limit: int,
    end_offset: int | None,
    cursor: RolloutCursor | None,
    worktree_id: str | None,
) -> list[AssistantMessage]:
    """Newest-first window ending at `end_offset` (or EOF), tail-scanned."""
    try:
        file_size = transcript_path.stat().st_size
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
            transcript_path, end_offset
        ):
            kept = _drop_trailing_view_image_call(kept)
        if cursor is not None and end_offset is None:
            cursor.offset = file_size
            cursor.last_key = kept[-1].key if kept else None
    except OSError:
        return []
    return list(reversed(kept))


def _scan_span(
    transcript_path: Path,
    *,
    start: int,
    end: int,
    limit: int,
    worktree_id: str | None,
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
            message = _build_message(line_offset, line, worktree_id=worktree_id)
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


def _line_has_tool_output_image(transcript_path: Path, offset: int) -> bool:
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
    event = DRIVER.normalize_transcript_line(loaded)
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
    offset: int, line: str, *, worktree_id: str | None = None
) -> AssistantMessage | None:
    loaded = _load_json_line(line)
    if loaded is None:
        return None
    event = DRIVER.normalize_transcript_line(loaded)
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
    text = extract_assistant_text(line)
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
    preamble, segments = split_ack_message(text)
    preamble = scrub_say_headers(preamble)
    say_utterances = extract_say_directives(text)
    ack_segments: list[dict[str, Any]] = []
    ack_keys: list[str] = []
    seen_keys: set[str] = set()
    ack_utterances: list[str] = []
    display_parts: list[str] = [preamble] if preamble else []
    for segment in segments:
        # The ACK header is hidden in the UI, so capitalize the response's
        # first letter for display while keeping the spoken text verbatim.
        body = _capitalize_first(scrub_say_headers(segment.content))
        ack_segments.append(
            {
                "keys": list(segment.keys),
                "html": render_message_html(body, worktree_id=worktree_id),
            }
        )
        for ack_key in segment.keys:
            if ack_key not in seen_keys:
                seen_keys.add(ack_key)
                ack_keys.append(ack_key)
        spoken = strip_directive_lines(segment.content)
        if spoken:
            ack_utterances.append(spoken)
        if body:
            display_parts.append(body)
    display_text = "\n".join(display_parts)
    image_only = _image_only_markdown(display_text)
    preamble_html = (
        render_message_html(preamble, worktree_id=worktree_id)
        if preamble and segments
        else ""
    )
    return AssistantMessage(
        key=key,
        index=offset,
        timestamp=timestamp,
        text=text,
        display_text=display_text,
        display_html=render_message_html(display_text, worktree_id=worktree_id),
        ack_count=len(ack_keys),
        ack_keys=ack_keys,
        ack_utterances=ack_utterances,
        say_count=len(say_utterances),
        say_utterances=say_utterances,
        kind=kind,
        preview="image" if image_only else _preview_from_text(display_text),
        image_only=image_only,
        source_kind=source_kind,
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
        say_count=0,
        say_utterances=[],
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
        say_count=0,
        say_utterances=[],
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
