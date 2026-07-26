"""The Claude transcript dialect adapter: raw lines in, typed events out.

Claude writes one JSON line per transcript event, and every fact a consumer
cares about — prose, reasoning, tool calls, tool results, images, compaction
boundaries — arrives as blocks inside those lines. This module is the only place
that knows that shape. It decodes a line into the plane-neutral vocabulary in
`spice.transcript.events`, losslessly: a line carrying prose *and* a tool call
yields both, in source order. No canonical-dictionary compatibility projection
survives above or beside this adapter; every production consumer reads the
lossless typed facts.
"""

from __future__ import annotations

import json
from dataclasses import replace
from typing import Any

from spice.transcript.events import (
    UNLOCATED_SOURCE,
    AssistantText,
    Compaction,
    Image,
    LineStamper,
    Reasoning,
    ToolCall,
    ToolOutput,
    TranscriptEvent,
    Unknown,
    UserMessage,
)


def _claude_content_blocks(message: dict[str, Any]) -> list[dict[str, Any]]:
    content = message.get("content")
    if not isinstance(content, list):
        return []
    return [block for block in content if isinstance(block, dict)]


def claude_line_events(
    raw: dict[str, Any], *, source: str = UNLOCATED_SOURCE, line: int = 0
) -> list[TranscriptEvent]:
    """Decode one Claude transcript line into every event it carries, in order.

    Lossless by construction: an assistant line carrying prose, a tool call, and
    reasoning yields all three.
    """
    timestamp = raw.get("timestamp")
    stamper = LineStamper(
        source=source,
        line=line,
        timestamp=timestamp if isinstance(timestamp, str) else None,
    )
    rtype = raw.get("type")
    message = raw.get("message")
    if rtype == "assistant" and isinstance(message, dict):
        return _claude_assistant_events(stamper, message)
    if rtype == "user" and isinstance(message, dict):
        return _claude_user_events(stamper, message, raw.get("promptId"))
    compacting = _claude_compaction_activity(raw)
    if compacting is not None:
        return [Compaction(at=stamper.stamp(), active=compacting, boundary=False)]
    if _claude_is_compaction(raw):
        return [Compaction(at=stamper.stamp(), active=False, boundary=True)]
    return []


def _claude_assistant_events(
    stamper: LineStamper, message: dict[str, Any]
) -> list[TranscriptEvent]:
    final = message.get("stop_reason") == "end_turn"
    events: list[TranscriptEvent] = []
    for block in _claude_content_blocks(message):
        event = _claude_assistant_block_event(stamper, block, final=final)
        if event is not None:
            events.append(event)
    return _select_assistant_image_payload(events)


def _claude_assistant_block_event(
    stamper: LineStamper, block: dict[str, Any], *, final: bool
) -> TranscriptEvent | None:
    block_type = block.get("type")
    if block_type == "text":
        text = block.get("text")
        if not isinstance(text, str):
            return None
        return AssistantText(at=stamper.stamp(), text=text, final=final)
    if block_type == "thinking":
        summary = block.get("thinking")
        return Reasoning(
            at=stamper.stamp(), summary=summary if isinstance(summary, str) else ""
        )
    if block_type == "tool_use":
        payload = _claude_tool_call_payload(block)
        return ToolCall(
            at=stamper.stamp(),
            call_id=str(block.get("id") or ""),
            name=str(payload["name"]),
            arguments=str(payload["arguments"]),
        )
    if block_type == "image":
        url = _claude_image_url(block)
        return (
            None
            if url is None
            else Image(at=stamper.stamp(), url=url, role="assistant")
        )
    return Unknown(
        at=stamper.stamp(),
        reason="unrecognized assistant block",
        raw_type=block_type if isinstance(block_type, str) else None,
    )


def _claude_user_events(
    stamper: LineStamper, message: dict[str, Any], prompt_id: Any
) -> list[TranscriptEvent]:
    content = message.get("content")
    if isinstance(content, str):
        if not content.strip():
            return []
        return [
            UserMessage(
                at=stamper.stamp(),
                text=content,
                # A real user prompt carries Claude's per-turn id; tool-result
                # `user` lines do not, so turn boundaries land on actual prompts.
                prompt_id=prompt_id
                if isinstance(prompt_id, str) and prompt_id
                else None,
                transcript_kind="user",
            )
        ]
    if isinstance(content, list):
        block = next((item for item in content if isinstance(item, dict)), None)
        if block is not None and block.get("type") == "tool_result":
            return _claude_tool_result_events(stamper, block)
    return []


def _claude_tool_result_events(
    stamper: LineStamper, block: dict[str, Any]
) -> list[TranscriptEvent]:
    call_id = str(block.get("tool_use_id") or "")
    body = block.get("content")
    events: list[TranscriptEvent] = [
        ToolOutput(
            at=stamper.stamp(),
            call_id=call_id,
            content=body if isinstance(body, str) else "",
            failed=bool(block.get("is_error")),
            tool_output_type="function_call_output",
        )
    ]
    payload_index = 0
    for item in body if isinstance(body, list) else []:
        if isinstance(item, dict) and item.get("type") == "image":
            url = _claude_image_url(item)
            if url is not None:
                events.append(
                    Image(
                        at=stamper.stamp(),
                        url=url,
                        call_id=call_id,
                        tool_output_type="function_call_output",
                        payload_index=payload_index,
                    )
                )
                payload_index += 1
    return events


def _select_assistant_image_payload(
    events: list[TranscriptEvent],
) -> list[TranscriptEvent]:
    """Mark the image retained by Claude's historical response-item selection."""
    if any(isinstance(event, AssistantText) and event.text.strip() for event in events):
        return events
    for index, event in enumerate(events):
        if isinstance(event, ToolCall):
            return events
        if isinstance(event, Image):
            events[index] = replace(event, payload_index=0)
            return events
    return events


def _claude_image_url(block: dict[str, Any]) -> str | None:
    """Resolved image URL from a Claude image block, or None if unreadable.

    Claude stores `{source:{type:"base64",media_type,data}}` (or a `url`
    source); both resolve to a `data:`/http URL the existing image extraction
    already understands.
    """
    source = block.get("source")
    if not isinstance(source, dict):
        return None
    if source.get("type") == "url":
        url = source.get("url")
        return str(url) if url else None
    media_type = source.get("media_type")
    data = source.get("data")
    if not isinstance(media_type, str) or not isinstance(data, str):
        return None
    return f"data:{media_type};base64,{data}"


def _claude_tool_call_payload(block: dict[str, Any]) -> dict[str, Any]:
    name = str(block.get("name") or "tool")
    raw_input = block.get("input")
    arguments = raw_input if isinstance(raw_input, dict) else {}
    if name == "TodoWrite":
        return {
            "type": "function_call",
            "name": "update_plan",
            "arguments": json.dumps({"plan": _claude_plan_steps(arguments)}),
        }
    return {
        "type": "function_call",
        "name": name,
        "arguments": json.dumps(arguments),
    }


def _claude_plan_steps(arguments: dict[str, Any]) -> list[dict[str, str]]:
    todos = arguments.get("todos")
    if not isinstance(todos, list):
        return []
    steps: list[dict[str, str]] = []
    for todo in todos:
        if isinstance(todo, dict):
            steps.append(
                {
                    "step": str(todo.get("content") or todo.get("activeForm") or ""),
                    "status": str(todo.get("status") or ""),
                }
            )
    return steps


def _claude_is_compaction(raw: dict[str, Any]) -> bool:
    if raw.get("type") == "summary":
        return True
    return raw.get("type") == "system" and raw.get("subtype") == "compact_boundary"


def _claude_compaction_activity(raw: dict[str, Any]) -> bool | None:
    """True while a compaction runs, False when it settles, None otherwise.

    Resuming a large thread compacts before the agent can act at all, and the
    CLI narrates that on bare status lines: `status: "compacting"` when the
    compaction starts and a `compact_result` when it succeeds or fails. The
    boundary and summary lines that `_claude_is_compaction` matches only ever
    arrive *after* a compaction produced new context, so they cannot tell a
    supervisor that a silent process is busy rather than wedged.
    """
    if raw.get("type") != "system" or raw.get("subtype") != "status":
        return None
    if raw.get("status") == "compacting":
        return True
    if raw.get("compact_result") is not None:
        return False
    return None
