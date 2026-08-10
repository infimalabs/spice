"""Typed decoding for the distinct ``codex exec --json`` event stream."""

from __future__ import annotations

from spice.agent.driver import CODEX_DRIVER
from spice.transcript.decode import decode_parsed_line, decode_stdout_parsed_line
from spice.transcript.events import (
    AssistantText,
    CommandExecution,
    Compaction,
    ContextUsage,
    FailureSignal,
    ToolCall,
    TurnBoundary,
    WebSearch,
)

SOURCE = "codex-exec.jsonl"
INPUT_TOKENS = 24_763
CACHED_INPUT_TOKENS = 24_448
OUTPUT_TOKENS = 122
REASONING_OUTPUT_TOKENS = 10
TOTAL_TOKENS = INPUT_TOKENS + OUTPUT_TOKENS


def _stdout(raw: dict) -> list:
    return decode_stdout_parsed_line(raw, CODEX_DRIVER, source=SOURCE, line=7)


def test_only_completed_agent_items_deliver_exact_prose_once() -> None:
    item = {
        "id": "item_7",
        "type": "agent_message",
        "phase": "commentary",
        "text": "ACK 1kLWspbN: exact text\nTASK title=kept whole | project=task.unit",
    }

    assert _stdout({"type": "item.started", "item": item}) == []
    events = _stdout({"type": "item.completed", "item": item})

    assert len(events) == 1
    message = events[0]
    assert isinstance(message, AssistantText)
    assert message.text == item["text"]
    assert message.item_id == "item_7"
    assert message.phase == "commentary"
    assert message.final is False
    assert message.at.source == SOURCE
    assert message.at.line == 7


def test_final_agent_item_retains_final_phase() -> None:
    events = _stdout(
        {
            "type": "item.completed",
            "item": {
                "id": "item_final",
                "type": "agent_message",
                "phase": "final_answer",
                "text": "Settled.",
            },
        }
    )

    assert len(events) == 1
    assert isinstance(events[0], AssistantText)
    assert events[0].final is True
    assert events[0].phase == "final_answer"


def test_command_start_is_activity_and_completion_is_execution_result() -> None:
    started = _stdout(
        {
            "type": "item.started",
            "item": {
                "id": "item_cmd",
                "type": "command_execution",
                "command": "spice task status",
                "cwd": "/repo",
                "status": "in_progress",
            },
        }
    )
    completed = _stdout(
        {
            "type": "item.completed",
            "item": {
                "id": "item_cmd",
                "type": "command_execution",
                "command": "spice task status",
                "cwd": "/repo",
                "status": "completed",
                "exit_code": 0,
            },
        }
    )

    assert len(started) == 1
    assert isinstance(started[0], ToolCall)
    assert (started[0].call_id, started[0].name, started[0].arguments) == (
        "item_cmd",
        "command_execution",
        "spice task status",
    )
    assert len(completed) == 1
    assert isinstance(completed[0], CommandExecution)
    assert completed[0].exit_code == 0
    assert completed[0].status == "completed"


def test_web_search_keeps_tool_activity_and_search_details() -> None:
    events = _stdout(
        {
            "type": "item.started",
            "item": {
                "id": "item_web",
                "type": "web_search",
                "query": "Codex JSON output",
                "action": {
                    "type": "search",
                    "queries": ["Codex JSON output", "codex exec --json"],
                },
            },
        }
    )

    assert [type(event) for event in events] == [ToolCall, WebSearch]
    search = events[1]
    assert isinstance(search, WebSearch)
    assert search.query == "Codex JSON output"
    assert search.queries == ("Codex JSON output", "codex exec --json")


def test_compaction_lifecycle_retains_active_and_boundary_signals() -> None:
    item = {"id": "item_compact", "type": "context_compaction"}

    started = _stdout({"type": "item.started", "item": item})
    completed = _stdout({"type": "item.completed", "item": item})

    assert len(started) == len(completed) == 1
    assert isinstance(started[0], Compaction)
    assert (started[0].active, started[0].boundary) == (True, False)
    assert isinstance(completed[0], Compaction)
    assert (completed[0].active, completed[0].boundary) == (False, True)


def test_turn_completion_attaches_cli_usage_without_changing_rollout_decode() -> None:
    raw = {
        "type": "turn.completed",
        "usage": {
            "input_tokens": INPUT_TOKENS,
            "cached_input_tokens": CACHED_INPUT_TOKENS,
            "output_tokens": OUTPUT_TOKENS,
            "reasoning_output_tokens": REASONING_OUTPUT_TOKENS,
        },
    }

    events = _stdout(raw)

    assert [type(event) for event in events] == [TurnBoundary, ContextUsage]
    usage = events[1]
    assert isinstance(usage, ContextUsage)
    assert usage.last.input_tokens == INPUT_TOKENS
    assert usage.last.cached_input_tokens == CACHED_INPUT_TOKENS
    assert usage.last.output_tokens == OUTPUT_TOKENS
    assert usage.last.reasoning_output_tokens == REASONING_OUTPUT_TOKENS
    assert usage.last.total_tokens == TOTAL_TOKENS
    # The durable Codex rollout decoder remains a different wire contract.
    assert decode_parsed_line(raw, CODEX_DRIVER, source=SOURCE, line=7) == []


def test_structural_usage_failure_keeps_turn_error_and_failure_family() -> None:
    events = _stdout(
        {
            "type": "turn.failed",
            "error": {
                "message": "The usage limit was reached.",
                "codexErrorInfo": {"type": "UsageLimitExceeded"},
            },
        }
    )

    assert [type(event) for event in events] == [TurnBoundary, FailureSignal]
    assert isinstance(events[0], TurnBoundary)
    assert events[0].kind == "error"
    assert isinstance(events[1], FailureSignal)
    assert events[1].kind == "out-of-credits"
