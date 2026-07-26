"""Policy-free assembled-message reduction over typed transcript facts."""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import replace
from pathlib import Path
from typing import cast

import pytest

from spice.agent.driver import CLAUDE_DRIVER, CODEX_DRIVER, AgentDriver
from spice.transcript.assembly import (
    AssembledMessage,
    AssembledMessageReducer,
    DirectiveKind,
    SpanKind,
)
from spice.transcript.events import (
    AssistantText,
    CommandExecution,
    Compaction,
    ContextUsage,
    FailureSignal,
    Image,
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
from spice.transcript.reader import TranscriptEventReader

FIXTURES = Path(__file__).parent / "fixtures" / "transcript"
SOURCE_ACTOR = "thread:assembly-fixture"
ACK_KEY = "1jN54zgX"
NACK_KEY = "1jN552dN"
TIMESTAMP = "2026-07-26T02:20:00.000Z"


@pytest.mark.parametrize(
    ("driver", "fixture_name", "expected_message_count", "expected_kinds"),
    [
        (
            CLAUDE_DRIVER,
            "assembled_claude.jsonl",
            2,
            [
                SpanKind.PROSE,
                SpanKind.DIRECTIVE,
                SpanKind.ACK,
                SpanKind.DIRECTIVE,
                SpanKind.NACK,
                SpanKind.TOOL,
                SpanKind.REASONING,
                SpanKind.IMAGE,
                SpanKind.FINAL_ANSWER,
                SpanKind.COMPACTION,
            ],
        ),
        (
            CODEX_DRIVER,
            "assembled_codex.jsonl",
            4,
            [
                SpanKind.PROSE,
                SpanKind.DIRECTIVE,
                SpanKind.ACK,
                SpanKind.DIRECTIVE,
                SpanKind.NACK,
                SpanKind.IMAGE,
                SpanKind.FINAL_ANSWER,
                SpanKind.TOOL,
                SpanKind.REASONING,
                SpanKind.COMPACTION,
            ],
        ),
    ],
    ids=["claude", "codex"],
)
def test_driver_fixtures_reduce_identically_live_and_file_backed(
    driver: AgentDriver,
    fixture_name: str,
    expected_message_count: int,
    expected_kinds: list[SpanKind],
) -> None:
    path = FIXTURES / fixture_name
    read = TranscriptEventReader(path, driver, SOURCE_ACTOR).read("forward")

    reducer = AssembledMessageReducer()
    live_messages = []
    for event in read.events:
        live_messages.extend(reducer.push(event))
    live_messages.extend(reducer.finish())
    file_messages = _assemble(
        TranscriptEventReader(path, driver, SOURCE_ACTOR).read("forward").events
    )

    assert tuple(live_messages) == file_messages
    assert len(file_messages) == expected_message_count
    spans = [span for message in file_messages for span in message.spans]
    assert [span.kind for span in spans] == expected_kinds
    assert [span.keys for span in spans if span.kind is SpanKind.ACK] == [(ACK_KEY,)]
    assert [span.keys for span in spans if span.kind is SpanKind.NACK] == [(NACK_KEY,)]
    assert [
        span.directive_kind for span in spans if span.kind is SpanKind.DIRECTIVE
    ] == [DirectiveKind.TASK, DirectiveKind.APP]
    assert [span.text for span in spans if span.kind is SpanKind.PROSE] == [
        "visible preamble"
    ]
    assert all(message.at.offset is not None for message in file_messages)
    assert all(message.at.source_actor == SOURCE_ACTOR for message in file_messages)


def test_final_boundary_follows_every_fact_on_its_source_line() -> None:
    at = _at(7)
    first = AssistantText(at=at, text="first", final=True)
    second = AssistantText(
        at=replace(at, ordinal=1),
        text="second",
        final=True,
    )
    tool = ToolCall(
        at=replace(at, ordinal=2),
        call_id="call-1",
        name="exec_command",
        arguments="{}",
    )

    messages = _assemble([first, second, tool])

    assert len(messages) == 1
    assert [span.kind for span in messages[0].spans] == [
        SpanKind.PROSE,
        SpanKind.PROSE,
        SpanKind.TOOL,
        SpanKind.FINAL_ANSWER,
    ]
    assert messages[0].spans[-1].event is second


def test_closed_event_set_is_handled_without_dictionary_input() -> None:
    usage = TokenUsage(1, 0, 0, 1, 0, 2)
    events: list[TranscriptEvent] = [
        AssistantText(at=_at(1), text="visible", final=False),
        Reasoning(at=_at(2), summary="thought"),
        ToolCall(at=_at(3), call_id="call", name="tool", arguments="{}"),
        ToolOutput(
            at=_at(4),
            call_id="call",
            content="output",
            failed=False,
            tool_output_type="function_call_output",
        ),
        CommandExecution(
            at=_at(5),
            command="git status",
            cwd="/repo",
            exit_code=0,
            status="completed",
            turn_id="turn-1",
        ),
        Image(at=_at(6), url="data:image/png;base64,abc"),
        UserMessage(at=_at(7), text="prompt", prompt_id="prompt-1"),
        TurnBoundary(at=_at(8), kind="started", turn_id="turn-1"),
        Compaction(at=_at(9), active=False, boundary=True),
        WebSearch(at=_at(10), status="completed", action_type="search", query="q"),
        ContextUsage(
            at=_at(11),
            last=usage,
            cumulative=None,
            model_context_window=100,
        ),
        FailureSignal(
            at=_at(12),
            kind="out-of-credits",
            reset_epoch=1_784_280_000,
        ),
        Unknown(at=_at(13), reason="future", raw_type="future"),
    ]

    messages = _assemble(events)

    assert [span.kind for message in messages for span in message.spans] == [
        SpanKind.PROSE,
        SpanKind.REASONING,
        SpanKind.TOOL,
        SpanKind.TOOL,
        SpanKind.IMAGE,
        SpanKind.COMPACTION,
        SpanKind.TOOL,
        SpanKind.FAILURE,
    ]
    with pytest.raises(TypeError, match="typed TranscriptEvent"):
        AssembledMessageReducer().push(
            cast(TranscriptEvent, {"type": "message", "payload": {}})
        )


@pytest.mark.parametrize(
    ("header", "expected_kind"),
    (
        (f"ACK {ACK_KEY}:", SpanKind.ACK),
        (f"NACK {NACK_KEY}:", SpanKind.NACK),
    ),
)
def test_bodyless_keyed_response_keeps_its_reducer_classification(
    header: str,
    expected_kind: SpanKind,
) -> None:
    event = AssistantText(at=_at(14), text=header, final=False)

    (message,) = _assemble((event,))

    (span,) = message.spans
    assert span.kind is expected_kind
    assert span.keys == ((ACK_KEY,) if expected_kind is SpanKind.ACK else (NACK_KEY,))
    assert span.text == ""
    assert span.response_index == 0
    assert span.response_kind is expected_kind
    assert message.assistant_text_events == (event,)


def _at(line: int) -> Provenance:
    return Provenance(
        source="/transcripts/assembly.jsonl",
        line=line,
        ordinal=0,
        timestamp=TIMESTAMP,
        offset=line * 100,
        source_actor=SOURCE_ACTOR,
    )


def _assemble(
    events: Iterable[TranscriptEvent],
) -> tuple[AssembledMessage, ...]:
    reducer = AssembledMessageReducer()
    messages: list[AssembledMessage] = []
    for event in events:
        messages.extend(reducer.push(event))
    messages.extend(reducer.finish())
    return tuple(messages)
