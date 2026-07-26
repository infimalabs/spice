"""The Codex transcript dialect adapter: raw lines to typed events.

Codex writes the dictionary vocabulary consumed by this adapter. It crosses
that dialect boundary once and emits only the shared lossless typed vocabulary;
there is no reverse compatibility projection or canonical dictionary seam.

Context usage joins this vocabulary through ``AgentDriver.context_snapshot_fields``
instead: that hook is the sole dialect-local usage decoder, and the driver
attaches its typed fact alongside the adapter events before consumers see the
line.
"""

from __future__ import annotations

from typing import Any, Literal

from spice.transcript.events import (
    UNLOCATED_SOURCE,
    AssistantText,
    CommandExecution,
    Compaction,
    Image,
    LineStamper,
    Reasoning,
    ToolCall,
    ToolOutput,
    ToolOutputType,
    TranscriptEvent,
    TurnBoundary,
    Unknown,
    UserMessage,
    WebSearch,
)

TurnMetadataKey = Literal["internal_chat_message_metadata_passthrough", "metadata"]


def codex_line_events(
    raw: dict[str, Any], *, source: str = UNLOCATED_SOURCE, line: int = 0
) -> list[TranscriptEvent]:
    """Decode every typed fact carried by one Codex JSONL object."""
    timestamp = raw.get("timestamp")
    stamper = LineStamper(
        source=source,
        line=line,
        timestamp=timestamp if isinstance(timestamp, str) else None,
    )
    outer_type = raw.get("type")
    if outer_type == "compacted":
        return [Compaction(at=stamper.stamp(), active=False, boundary=True)]

    payload = raw.get("payload")
    if not isinstance(payload, dict):
        return []
    payload_type = payload.get("type")
    if outer_type == "event_msg":
        return _codex_event_message_events(stamper, payload)
    if outer_type != "response_item":
        return []
    if payload_type == "message":
        return _codex_message_events(stamper, payload)
    if payload_type in {"function_call", "custom_tool_call"}:
        return _codex_tool_call_events(stamper, payload)
    if payload_type in {"function_call_output", "custom_tool_call_output"}:
        output_type: ToolOutputType = (
            "custom_tool_call_output"
            if payload_type == "custom_tool_call_output"
            else "function_call_output"
        )
        return _codex_tool_output_events(stamper, payload, output_type)
    if payload_type == "reasoning":
        return _codex_reasoning_events(stamper, payload)
    if payload_type == "web_search_call":
        return _codex_web_search_events(stamper, payload)
    return [
        Unknown(
            at=stamper.stamp(),
            reason="unrecognized Codex response item",
            raw_type=payload_type if isinstance(payload_type, str) else None,
        )
    ]


def _codex_event_message_events(
    stamper: LineStamper, payload: dict[str, Any]
) -> list[TranscriptEvent]:
    payload_type = payload.get("type")
    turn_id = _optional_str(payload.get("turn_id"))
    if payload_type == "token_count":
        # AgentDriver attaches the dialect-decoded ContextUsage fact.
        return []
    if payload_type == "task_started":
        return [
            TurnBoundary(
                at=stamper.stamp(),
                kind="started",
                turn_id=turn_id,
            )
        ]
    if payload_type == "task_complete":
        return [
            TurnBoundary(
                at=stamper.stamp(),
                kind="completed",
                turn_id=turn_id,
                last_assistant_message=_optional_str(payload.get("last_agent_message")),
            )
        ]
    if payload_type == "error":
        return [
            TurnBoundary(
                at=stamper.stamp(),
                kind="error",
                turn_id=turn_id,
            )
        ]
    if payload_type == "exec_command_end":
        return [
            CommandExecution(
                at=stamper.stamp(),
                command=_command_value(payload.get("command") or payload.get("cmd")),
                cwd=_optional_str(payload.get("cwd") or payload.get("workdir")),
                exit_code=_command_int(payload.get("exit_code")),
                status=_optional_str(payload.get("status")) or "completed",
                turn_id=turn_id,
            )
        ]
    if payload_type == "user_message":
        message = payload.get("message")
        if isinstance(message, str) and message:
            return [
                UserMessage(
                    at=stamper.stamp(),
                    text=message,
                    prompt_id=None,
                    phase="prompt",
                    turn_id=turn_id,
                    transcript_kind="event_msg",
                )
            ]
    return []


def _codex_message_events(
    stamper: LineStamper, payload: dict[str, Any]
) -> list[TranscriptEvent]:
    role = payload.get("role")
    content = payload.get("content")
    if not isinstance(role, str) or not isinstance(content, list):
        return [_unknown(stamper, "malformed Codex message", "message")]
    item_id = _optional_str(payload.get("id"))
    phase = _optional_str(payload.get("phase"))
    turn_id, metadata_key = _turn_metadata(payload)
    events: list[TranscriptEvent] = []
    for payload_index, block in enumerate(content):
        if not isinstance(block, dict):
            events.append(_unknown(stamper, "malformed Codex message block", None))
            continue
        block_type = block.get("type")
        text = block.get("text")
        if isinstance(text, str):
            # Text is the discriminant, not the declared type. Codex writes the
            # type on every block it emits itself, but transcripts in the wild
            # carry bare `{"text": ...}` blocks. Requiring the type here would file
            # real assistant text as `Unknown` and silently stop delivering it.
            content_type = block_type if isinstance(block_type, str) else "text"
            if role == "assistant":
                events.append(
                    AssistantText(
                        at=stamper.stamp(),
                        text=text,
                        final=phase == "final_answer",
                        item_id=item_id,
                        content_type=content_type,
                        phase=phase,
                        turn_id=turn_id,
                        turn_metadata_key=metadata_key,
                    )
                )
            else:
                events.append(
                    UserMessage(
                        at=stamper.stamp(),
                        text=text,
                        prompt_id=None,
                        role=role,
                        item_id=item_id,
                        content_type=content_type,
                        phase=phase,
                        turn_id=turn_id,
                        turn_metadata_key=metadata_key,
                    )
                )
            continue
        image_url = _codex_image_url(block.get("image_url"))
        if isinstance(block_type, str) and image_url is not None:
            events.append(
                Image(
                    at=stamper.stamp(),
                    url=image_url,
                    content_type=block_type,
                    detail=_optional_str(block.get("detail")),
                    role=role,
                    item_id=item_id,
                    payload_index=payload_index,
                    turn_id=turn_id,
                    turn_metadata_key=metadata_key,
                )
            )
            continue
        events.append(
            _unknown(
                stamper,
                "unrecognized Codex message block",
                block_type if isinstance(block_type, str) else None,
            )
        )
    return events


def _codex_tool_call_events(
    stamper: LineStamper, payload: dict[str, Any]
) -> list[TranscriptEvent]:
    payload_type = payload.get("type")
    custom = payload_type == "custom_tool_call"
    argument_key = "input" if custom else "arguments"
    name = payload.get("name")
    arguments = payload.get(argument_key)
    if not isinstance(name, str) or not isinstance(arguments, str):
        return [
            _unknown(
                stamper,
                "malformed Codex tool call",
                payload_type if isinstance(payload_type, str) else None,
            )
        ]
    turn_id, metadata_key = _turn_metadata(payload)
    return [
        ToolCall(
            at=stamper.stamp(),
            call_id=str(payload.get("call_id") or ""),
            name=name,
            arguments=arguments,
            item_id=_optional_str(payload.get("id")),
            custom=custom,
            status=_optional_str(payload.get("status")),
            namespace=_optional_str(payload.get("namespace")),
            turn_id=turn_id,
            turn_metadata_key=metadata_key,
        )
    ]


def _codex_tool_output_events(
    stamper: LineStamper,
    payload: dict[str, Any],
    output_type: ToolOutputType,
) -> list[TranscriptEvent]:
    call_id = str(payload.get("call_id") or "")
    item_id = _optional_str(payload.get("id"))
    turn_id, metadata_key = _turn_metadata(payload)
    output = payload.get("output")
    if isinstance(output, str):
        return [
            ToolOutput(
                at=stamper.stamp(),
                call_id=call_id,
                content=output,
                failed=False,
                item_id=item_id,
                tool_output_type=output_type,
                turn_id=turn_id,
                turn_metadata_key=metadata_key,
            )
        ]
    if not isinstance(output, list):
        return [_unknown(stamper, "malformed Codex tool output", "output")]
    if not output:
        return [
            ToolOutput(
                at=stamper.stamp(),
                call_id=call_id,
                content="",
                failed=False,
                item_id=item_id,
                output_is_list=True,
                tool_output_type=output_type,
                turn_id=turn_id,
                turn_metadata_key=metadata_key,
            )
        ]
    events: list[TranscriptEvent] = []
    for payload_index, block in enumerate(output):
        if not isinstance(block, dict):
            events.append(_unknown(stamper, "malformed Codex output block", None))
            continue
        block_type = block.get("type")
        text = block.get("text")
        if isinstance(block_type, str) and isinstance(text, str):
            events.append(
                ToolOutput(
                    at=stamper.stamp(),
                    call_id=call_id,
                    content=text,
                    failed=False,
                    item_id=item_id,
                    content_type=block_type,
                    output_is_list=True,
                    tool_output_type=output_type,
                    turn_id=turn_id,
                    turn_metadata_key=metadata_key,
                )
            )
            continue
        image_url = _codex_image_url(block.get("image_url"))
        if isinstance(block_type, str) and image_url is not None:
            events.append(
                Image(
                    at=stamper.stamp(),
                    url=image_url,
                    content_type=block_type,
                    detail=_optional_str(block.get("detail")),
                    item_id=item_id,
                    call_id=call_id,
                    tool_output_type=output_type,
                    payload_index=payload_index,
                    turn_id=turn_id,
                    turn_metadata_key=metadata_key,
                )
            )
            continue
        events.append(
            _unknown(
                stamper,
                "unrecognized Codex output block",
                block_type if isinstance(block_type, str) else None,
            )
        )
    return events


def _codex_reasoning_events(
    stamper: LineStamper, payload: dict[str, Any]
) -> list[TranscriptEvent]:
    summary = payload.get("summary")
    if not isinstance(summary, list):
        return [_unknown(stamper, "malformed Codex reasoning", "reasoning")]
    item_id = _optional_str(payload.get("id"))
    encrypted = _optional_str(payload.get("encrypted_content"))
    content_present = "content" in payload
    turn_id, metadata_key = _turn_metadata(payload)
    if not summary:
        return [
            Reasoning(
                at=stamper.stamp(),
                summary="",
                item_id=item_id,
                summary_type=None,
                encrypted_content=encrypted,
                content_present=content_present,
                turn_id=turn_id,
                turn_metadata_key=metadata_key,
            )
        ]
    events: list[TranscriptEvent] = []
    for block in summary:
        if not isinstance(block, dict) or not isinstance(block.get("text"), str):
            events.append(_unknown(stamper, "malformed reasoning summary", None))
            continue
        events.append(
            Reasoning(
                at=stamper.stamp(),
                summary=block["text"],
                item_id=item_id,
                summary_type=_optional_str(block.get("type")),
                encrypted_content=encrypted,
                content_present=content_present,
                turn_id=turn_id,
                turn_metadata_key=metadata_key,
            )
        )
    return events


def _codex_web_search_events(
    stamper: LineStamper, payload: dict[str, Any]
) -> list[TranscriptEvent]:
    action = payload.get("action")
    if action is not None and not isinstance(action, dict):
        return [_unknown(stamper, "malformed Codex web search", "web_search_call")]
    action = action or {}
    raw_queries = action.get("queries")
    queries = (
        tuple(query for query in raw_queries if isinstance(query, str))
        if isinstance(raw_queries, list)
        else ()
    )
    return [
        WebSearch(
            at=stamper.stamp(),
            status=_optional_str(payload.get("status")),
            action_type=_optional_str(action.get("type")),
            query=_optional_str(action.get("query")),
            queries=queries,
            url=_optional_str(action.get("url")),
            pattern=_optional_str(action.get("pattern")),
        )
    ]


def _turn_metadata(
    payload: dict[str, Any],
) -> tuple[str | None, TurnMetadataKey | None]:
    for key in ("internal_chat_message_metadata_passthrough", "metadata"):
        metadata = payload.get(key)
        if isinstance(metadata, dict):
            turn_id = metadata.get("turn_id")
            if isinstance(turn_id, str):
                return turn_id, key
    return None, None


def _unknown(stamper: LineStamper, reason: str, raw_type: str | None) -> Unknown:
    return Unknown(at=stamper.stamp(), reason=reason, raw_type=raw_type)


def _optional_str(value: Any) -> str | None:
    return value if isinstance(value, str) else None


def _codex_image_url(value: Any) -> str | None:
    raw = value.get("url") if isinstance(value, dict) else value
    return raw if isinstance(raw, str) and raw else None


def _command_value(value: Any) -> str:
    if isinstance(value, list):
        return " ".join(str(part) for part in value)
    return value if isinstance(value, str) else "-"


def _command_int(value: Any) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, str):
        try:
            return int(value)
        except ValueError:
            return None
    return None
