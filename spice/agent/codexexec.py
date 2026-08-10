"""Codex ``exec --json`` stdout adapter: CLI events to typed facts.

The exec stream is not the durable rollout transcript.  It has its own compact
envelope (``thread.started``, ``turn.*``, and ``item.*``), so it crosses the
driver seam through this adapter instead of teaching the rollout decoder a
second dialect.  Only completed agent-message items become operator prose:
Codex declares those items authoritative, and decoding an earlier lifecycle
record too would deliver one assistant message—and its ACK directives—twice.
"""

from __future__ import annotations

import json
from typing import Any, Callable

from spice.transcript.events import (
    UNLOCATED_SOURCE,
    AssistantText,
    CommandExecution,
    Compaction,
    LineStamper,
    Reasoning,
    ToolCall,
    TranscriptEvent,
    TurnBoundary,
    Unknown,
    WebSearch,
)


def codex_exec_line_events(
    raw: dict[str, Any], *, source: str = UNLOCATED_SOURCE, line: int = 0
) -> list[TranscriptEvent]:
    """Decode one parsed ``codex exec --json`` record in source order."""
    stamper = LineStamper(source=source, line=line, timestamp=None)
    outer_type = raw.get("type")
    if outer_type == "thread.started":
        # The launch binder reads ``thread_id`` from the raw startup-log head.
        return []
    if outer_type == "turn.started":
        return [
            TurnBoundary(
                at=stamper.stamp(),
                kind="started",
                turn_id=_optional_str(raw.get("turn_id")),
            )
        ]
    if outer_type == "turn.completed":
        return [
            TurnBoundary(
                at=stamper.stamp(),
                kind="completed",
                turn_id=_optional_str(raw.get("turn_id")),
            )
        ]
    if outer_type in {"turn.failed", "error"}:
        return [
            TurnBoundary(
                at=stamper.stamp(),
                kind="error",
                turn_id=_optional_str(raw.get("turn_id")),
            )
        ]
    if outer_type not in {"item.started", "item.completed"}:
        return [
            Unknown(
                at=stamper.stamp(),
                reason="unrecognized Codex exec event",
                raw_type=outer_type if isinstance(outer_type, str) else None,
            )
        ]

    item = raw.get("item")
    if not isinstance(item, dict):
        return [_unknown(stamper, "malformed Codex exec item", None)]
    item_type = item.get("type")
    completed = outer_type == "item.completed"
    if isinstance(item_type, str) and (decoder := _ITEM_DECODERS.get(item_type)):
        return decoder(stamper, item, completed)
    return [
        _unknown(
            stamper,
            "unrecognized Codex exec item",
            item_type if isinstance(item_type, str) else None,
        )
    ]


def _agent_message_events(
    stamper: LineStamper, item: dict[str, Any], completed: bool
) -> list[TranscriptEvent]:
    if not completed:
        return []
    text = item.get("text")
    if not isinstance(text, str):
        return [_unknown(stamper, "malformed Codex agent message", "agent_message")]
    phase = _optional_str(item.get("phase"))
    return [
        AssistantText(
            at=stamper.stamp(),
            text=text,
            final=phase == "final_answer",
            item_id=_optional_str(item.get("id")),
            phase=phase,
        )
    ]


def _reasoning_events(
    stamper: LineStamper, item: dict[str, Any], completed: bool
) -> list[TranscriptEvent]:
    if not completed or not (summary := _reasoning_summary(item.get("summary"))):
        return []
    return [
        Reasoning(
            at=stamper.stamp(),
            summary=summary,
            item_id=_optional_str(item.get("id")),
            content_present=bool(item.get("content")),
        )
    ]


def _compaction_events(
    stamper: LineStamper, item: dict[str, Any], completed: bool
) -> list[TranscriptEvent]:
    del item
    return [
        Compaction(
            at=stamper.stamp(),
            active=not completed,
            boundary=completed,
        )
    ]


def _command_events(
    stamper: LineStamper, item: dict[str, Any], completed: bool
) -> list[TranscriptEvent]:
    command = _command(item.get("command"))
    if not completed:
        return [
            _tool_call(
                stamper,
                item,
                name="command_execution",
                arguments=command,
            )
        ]
    return [
        CommandExecution(
            at=stamper.stamp(),
            command=command,
            cwd=_optional_str(item.get("cwd")),
            exit_code=_optional_int(item.get("exit_code")),
            status=_optional_str(item.get("status")),
            turn_id=None,
        )
    ]


def _file_change_events(
    stamper: LineStamper, item: dict[str, Any], completed: bool
) -> list[TranscriptEvent]:
    return (
        []
        if completed
        else [
            _tool_call(
                stamper,
                item,
                name="apply_patch",
                arguments=_json_value(item.get("changes")),
            )
        ]
    )


def _named_tool_events(
    stamper: LineStamper, item: dict[str, Any], completed: bool
) -> list[TranscriptEvent]:
    if completed:
        return []
    server = _optional_str(item.get("server"))
    item_type = _optional_str(item.get("type")) or "tool_call"
    tool = _optional_str(item.get("tool")) or item_type
    return [
        _tool_call(
            stamper,
            item,
            name=f"{server}.{tool}" if server else tool,
            arguments=_json_value(item.get("arguments")),
            namespace=server,
        )
    ]


def _web_search_events(
    stamper: LineStamper, item: dict[str, Any], completed: bool
) -> list[TranscriptEvent]:
    if completed:
        return []
    query = _optional_str(item.get("query"))
    action = item.get("action")
    action_fields = action if isinstance(action, dict) else {}
    queries = action_fields.get("queries")
    return [
        _tool_call(
            stamper,
            item,
            name="web_search",
            arguments=_json_value(action or query or ""),
        ),
        WebSearch(
            at=stamper.stamp(),
            status=_optional_str(item.get("status")) or "in_progress",
            action_type=_optional_str(action_fields.get("type")),
            query=query or _optional_str(action_fields.get("query")),
            queries=(
                tuple(value for value in queries if isinstance(value, str))
                if isinstance(queries, list)
                else ()
            ),
            url=_optional_str(action_fields.get("url")),
            pattern=_optional_str(action_fields.get("pattern")),
        ),
    ]


def _image_view_events(
    stamper: LineStamper, item: dict[str, Any], completed: bool
) -> list[TranscriptEvent]:
    return (
        []
        if completed
        else [
            _tool_call(
                stamper,
                item,
                name="view_image",
                arguments=_json_value({"path": item.get("path")}),
            )
        ]
    )


def _quiet_item_events(
    stamper: LineStamper, item: dict[str, Any], completed: bool
) -> list[TranscriptEvent]:
    del stamper, item, completed
    return []


def _tool_call(
    stamper: LineStamper,
    item: dict[str, Any],
    *,
    name: str,
    arguments: str,
    namespace: str | None = None,
) -> ToolCall:
    item_id = _optional_str(item.get("id"))
    return ToolCall(
        at=stamper.stamp(),
        call_id=item_id or "",
        name=name,
        arguments=arguments,
        item_id=item_id,
        status=_optional_str(item.get("status")) or "in_progress",
        namespace=namespace,
    )


def _unknown(stamper: LineStamper, reason: str, raw_type: str | None) -> Unknown:
    return Unknown(at=stamper.stamp(), reason=reason, raw_type=raw_type)


def _optional_str(value: object) -> str | None:
    return value if isinstance(value, str) and value else None


def _optional_int(value: object) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def _command(value: object) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return " ".join(str(part) for part in value)
    return ""


def _json_value(value: object) -> str:
    if isinstance(value, str):
        return value
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def _reasoning_summary(value: object) -> str:
    if isinstance(value, str):
        return value
    if not isinstance(value, list):
        return ""
    parts: list[str] = []
    for part in value:
        if isinstance(part, str):
            parts.append(part)
        elif isinstance(part, dict) and isinstance(part.get("text"), str):
            parts.append(part["text"])
    return "\n".join(parts)


ItemDecoder = Callable[
    [LineStamper, dict[str, Any], bool],
    list[TranscriptEvent],
]
_ITEM_DECODERS: dict[str, ItemDecoder] = {
    "agent_message": _agent_message_events,
    "reasoning": _reasoning_events,
    "context_compaction": _compaction_events,
    "command_execution": _command_events,
    "file_change": _file_change_events,
    "mcp_tool_call": _named_tool_events,
    "dynamic_tool_call": _named_tool_events,
    "collab_tool_call": _named_tool_events,
    "web_search": _web_search_events,
    "image_view": _image_view_events,
    "plan": _quiet_item_events,
    "plan_update": _quiet_item_events,
}
