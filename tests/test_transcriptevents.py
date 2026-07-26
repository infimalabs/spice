"""The closed typed transcript event vocabulary and the Claude decode onto it."""

from __future__ import annotations

import dataclasses
from typing import get_args

import pytest

from spice.agent.claudetranscript import claude_line_events
from spice.agent.driver import CLAUDE_DRIVER
from spice.transcript.events import (
    AssistantText,
    CommandExecution,
    Compaction,
    ContextUsage,
    Image,
    LineStamper,
    Provenance,
    Reasoning,
    TokenUsage,
    ToolCall,
    ToolOutput,
    TranscriptEvent,
    TurnBoundary,
    Unknown,
    UserMessage,
    WebSearch,
)

SOURCE = "/transcripts/session.jsonl"
FIRST_LINE = 4
SECOND_LINE = 9
STAMP_TIMESTAMP = "2026-07-25T21:30:00.000Z"

THINKING = "weighing two decode shapes"
PROSE = "checking the seam"
TOOL_ARGUMENTS = '{"command": "ls"}'
PNG_DATA = "iVBORw0KGgo="
PNG_URL = f"data:image/png;base64,{PNG_DATA}"


def _provenance(line: int, ordinal: int) -> Provenance:
    return Provenance(
        source=SOURCE, line=line, ordinal=ordinal, timestamp=STAMP_TIMESTAMP
    )


def _one_of_every_kind(at: Provenance) -> list[object]:
    return [
        AssistantText(at=at, text=PROSE, final=True),
        Reasoning(at=at, summary=THINKING),
        ToolCall(at=at, call_id="call-1", name="Bash", arguments=TOOL_ARGUMENTS),
        ToolOutput(
            at=at,
            call_id="call-1",
            content="events.py",
            failed=False,
            tool_output_type="function_call_output",
        ),
        CommandExecution(
            at=at,
            command="git status",
            cwd="/repo",
            exit_code=0,
            status="completed",
            turn_id="turn-1",
        ),
        Image(at=at, url=PNG_URL),
        UserMessage(at=at, text="drain the board", prompt_id="prompt-7"),
        TurnBoundary(at=at, kind="started", turn_id="turn-1"),
        Compaction(at=at, active=True, boundary=False),
        WebSearch(at=at, status="completed", action_type="search", query="spice"),
        ContextUsage(
            at=at,
            last=TokenUsage(
                input_tokens=7,
                cached_input_tokens=3,
                cache_write_input_tokens=0,
                output_tokens=2,
                reasoning_output_tokens=1,
                total_tokens=9,
            ),
            cumulative=None,
            model_context_window=258_400,
        ),
        Unknown(at=at, reason="malformed json", raw_type=None),
    ]


def _assistant_line(*blocks: dict, stop_reason: str | None = None) -> dict:
    message: dict = {"content": list(blocks)}
    if stop_reason is not None:
        message["stop_reason"] = stop_reason
    return {
        "type": "assistant",
        "timestamp": STAMP_TIMESTAMP,
        "message": message,
    }


def test_every_event_kind_carries_full_provenance() -> None:
    at = _provenance(FIRST_LINE, ordinal=0)
    events = _one_of_every_kind(at)
    assert {type(event) for event in events} == set(get_args(TranscriptEvent))
    for event in events:
        assert event.at.source == SOURCE
        assert event.at.line == FIRST_LINE
        assert event.at.ordinal == 0
        assert event.at.timestamp == STAMP_TIMESTAMP


def test_every_event_kind_is_a_frozen_typed_record() -> None:
    at = _provenance(FIRST_LINE, ordinal=0)
    for event in _one_of_every_kind(at):
        assert dataclasses.is_dataclass(event)
        with pytest.raises(dataclasses.FrozenInstanceError):
            event.at = _provenance(SECOND_LINE, ordinal=1)


def test_event_fields_keep_their_decoded_values() -> None:
    at = _provenance(FIRST_LINE, ordinal=0)
    call = ToolCall(at=at, call_id="call-1", name="Bash", arguments=TOOL_ARGUMENTS)
    output = ToolOutput(
        at=at,
        call_id="call-1",
        content="events.py",
        failed=True,
        tool_output_type="function_call_output",
    )
    assert (call.call_id, call.name, call.arguments) == (
        "call-1",
        "Bash",
        TOOL_ARGUMENTS,
    )
    assert (output.call_id, output.content, output.failed) == (
        "call-1",
        "events.py",
        True,
    )
    output_type_field = next(
        field
        for field in dataclasses.fields(ToolOutput)
        if field.name == "tool_output_type"
    )
    assert output_type_field.default is dataclasses.MISSING


def test_stamper_hands_out_ascending_ordinals_for_one_line() -> None:
    stamper = LineStamper(source=SOURCE, line=FIRST_LINE, timestamp=STAMP_TIMESTAMP)
    stamps = [stamper.stamp(), stamper.stamp(), stamper.stamp()]
    assert [stamp.ordinal for stamp in stamps] == [0, 1, 2]
    assert stamps[0] != stamps[1]
    for stamp in stamps:
        assert stamp.line == FIRST_LINE
        assert stamp.timestamp == STAMP_TIMESTAMP


def test_separate_lines_stamp_independent_ordinal_runs() -> None:
    first = LineStamper(source=SOURCE, line=FIRST_LINE, timestamp=STAMP_TIMESTAMP)
    second = LineStamper(source=SOURCE, line=SECOND_LINE, timestamp=STAMP_TIMESTAMP)
    first.stamp()
    assert first.stamp().ordinal == 1
    assert second.stamp().ordinal == 0


def test_multi_block_line_decodes_to_every_block_in_source_order() -> None:
    raw = _assistant_line(
        {"type": "text", "text": PROSE},
        {
            "type": "tool_use",
            "id": "call-1",
            "name": "Bash",
            "input": {"command": "ls"},
        },
        {"type": "thinking", "thinking": THINKING},
    )
    events = claude_line_events(raw, source=SOURCE, line=FIRST_LINE)
    assert [type(event) for event in events] == [AssistantText, ToolCall, Reasoning]
    assert [event.at.ordinal for event in events] == [0, 1, 2]
    assert [event.at.line for event in events] == [FIRST_LINE] * len(events)
    assert events[0].text == PROSE
    assert events[1].name == "Bash"
    assert events[1].arguments == TOOL_ARGUMENTS
    assert events[2].summary == THINKING


def test_decode_keeps_the_blocks_the_canonical_dict_collapses_away() -> None:
    raw = _assistant_line(
        {"type": "text", "text": PROSE},
        {
            "type": "tool_use",
            "id": "call-1",
            "name": "Bash",
            "input": {"command": "ls"},
        },
        {"type": "thinking", "thinking": THINKING},
    )
    events = claude_line_events(raw, source=SOURCE, line=FIRST_LINE)
    canonical = CLAUDE_DRIVER.normalize_transcript_line(raw)

    # The dict seam carries exactly one item: the text that wins the collapse.
    assert canonical["payload"]["content"] == [{"type": "text", "text": PROSE}]
    # The typed decode carries all three, so the tool call survives the same line.
    assert len(events) == 3
    assert [event.name for event in events if isinstance(event, ToolCall)] == ["Bash"]


def test_canonical_dict_still_ranks_text_over_tool_call_over_image() -> None:
    text_and_call = CLAUDE_DRIVER.normalize_transcript_line(
        _assistant_line(
            {"type": "tool_use", "id": "c", "name": "Bash", "input": {}},
            {"type": "text", "text": PROSE},
        )
    )
    assert text_and_call["payload"]["type"] == "message"
    assert text_and_call["payload"]["content"] == [{"type": "text", "text": PROSE}]

    call_and_image = CLAUDE_DRIVER.normalize_transcript_line(
        _assistant_line(
            {"type": "tool_use", "id": "c", "name": "Bash", "input": {}},
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": PNG_DATA,
                },
            },
        )
    )
    assert call_and_image["payload"]["type"] == "function_call"
    assert call_and_image["payload"]["name"] == "Bash"


def test_canonical_dict_joins_text_blocks_and_marks_the_final_answer() -> None:
    event = CLAUDE_DRIVER.normalize_transcript_line(
        _assistant_line(
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"},
            stop_reason="end_turn",
        )
    )
    assert event["payload"]["content"] == [{"type": "text", "text": "first\n\nsecond"}]
    assert event["payload"]["phase"] == "final_answer"
    assert event["timestamp"] == STAMP_TIMESTAMP


def test_canonical_dict_falls_through_to_reasoning_when_only_thinking() -> None:
    event = CLAUDE_DRIVER.normalize_transcript_line(
        _assistant_line({"type": "thinking", "thinking": THINKING})
    )
    assert event["payload"]["type"] == "reasoning"
    assert event["payload"]["summary"] == [{"type": "summary_text", "text": THINKING}]


def test_canonical_dict_renders_an_image_only_line_as_a_data_url() -> None:
    raw = _assistant_line(
        {
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": PNG_DATA,
            },
        }
    )
    decoded = claude_line_events(raw)
    assert [type(item) for item in decoded] == [Image]
    image = decoded[0]
    assert isinstance(image, Image)
    assert image.tool_output_type is None

    event = CLAUDE_DRIVER.normalize_transcript_line(raw)
    assert event["payload"]["content"] == [
        {"type": "image", "image_url": {"url": PNG_URL}}
    ]


def test_tool_result_line_decodes_to_output_plus_its_images() -> None:
    raw = {
        "type": "user",
        "timestamp": STAMP_TIMESTAMP,
        "message": {
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "call-1",
                    "is_error": True,
                    "content": [
                        {
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": "image/png",
                                "data": PNG_DATA,
                            },
                        }
                    ],
                }
            ]
        },
    }
    events = claude_line_events(raw, source=SOURCE, line=FIRST_LINE)
    assert [type(event) for event in events] == [ToolOutput, Image]
    output, image = events
    assert isinstance(output, ToolOutput)
    assert isinstance(image, Image)
    assert output.call_id == "call-1"
    assert output.failed is True
    assert output.tool_output_type == "function_call_output"
    assert image.url == PNG_URL
    assert image.tool_output_type == "function_call_output"

    canonical = CLAUDE_DRIVER.normalize_transcript_line(raw)
    assert canonical["payload"]["type"] == "function_call_output"
    assert canonical["payload"]["output"] == [
        {"type": "image", "image_url": {"url": PNG_URL}}
    ]


def test_compaction_activity_and_boundary_stay_distinguishable() -> None:
    running = claude_line_events(
        {"type": "system", "subtype": "status", "status": "compacting"}
    )
    settled = claude_line_events(
        {"type": "system", "subtype": "status", "compact_result": {"ok": True}}
    )
    boundary = claude_line_events({"type": "summary"})
    assert (running[0].active, running[0].boundary) == (True, False)
    assert (settled[0].active, settled[0].boundary) == (False, False)
    assert (boundary[0].active, boundary[0].boundary) == (False, True)
    assert running[0] != settled[0]

    assert CLAUDE_DRIVER.normalize_transcript_line({"type": "summary"})["type"] == (
        "compacted"
    )


def test_unrecognized_assistant_block_survives_as_an_unknown_event() -> None:
    events = claude_line_events(
        _assistant_line({"type": "server_tool_use", "name": "web_search"})
    )
    assert [type(event) for event in events] == [Unknown]
    assert events[0].raw_type == "server_tool_use"


def test_user_prompt_decodes_with_its_turn_boundary_id() -> None:
    events = claude_line_events(
        {
            "type": "user",
            "timestamp": STAMP_TIMESTAMP,
            "promptId": "prompt-xyz",
            "message": {"content": "operator prompt"},
        }
    )
    assert [type(event) for event in events] == [UserMessage]
    assert events[0].prompt_id == "prompt-xyz"
    assert events[0].text == "operator prompt"


def test_events_decoded_through_the_dict_seam_declare_an_unlocated_source() -> None:
    located = claude_line_events(
        _assistant_line({"type": "text", "text": PROSE}),
        source=SOURCE,
        line=SECOND_LINE,
    )
    unlocated = claude_line_events(_assistant_line({"type": "text", "text": PROSE}))
    assert located[0].at.source == SOURCE
    assert located[0].at.line == SECOND_LINE
    assert unlocated[0].at.source != located[0].at.source
