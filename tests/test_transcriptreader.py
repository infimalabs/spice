"""Shared transcript file-access engine contracts."""

from __future__ import annotations

import gzip
import json
from dataclasses import replace
from pathlib import Path

import pytest

from spice.agent.driver import CLAUDE_DRIVER, CODEX_DRIVER, AgentDriver
from spice.transcript import reader
from spice.transcript.decode import decode_line
from spice.transcript.events import AssistantText, Reasoning, ToolCall, Unknown
from spice.transcript.reader import (
    TranscriptCursor,
    TranscriptEventReader,
    cursor_offset,
    dispatch_records,
    locked_cursor,
    offset_after_line,
    read_bounded,
    read_forward,
    read_line,
    read_reverse_window,
    render_cursor,
    transcript_size,
)

TIMESTAMP = "2026-07-26T01:15:00.000Z"
UPDATED_CURSOR_OFFSET = 11
SOURCE_ACTOR = "thread:reader-contract"
SESSION_FIXTURES = Path(__file__).parent / "fixtures" / "session"


def _raw(payload: dict) -> bytes:
    return (
        json.dumps(payload, separators=(",", ":"), ensure_ascii=False) + "\n"
    ).encode()


def _fixture_lines() -> list[bytes]:
    return [
        _raw({"timestamp": TIMESTAMP, "value": "first ☃"}),
        b"{malformed\n",
        b'["not", "an", "object"]\n',
        _raw({"timestamp": TIMESTAMP, "value": "last"}),
    ]


def _offsets(lines: list[bytes]) -> list[int]:
    positions: list[int] = []
    offset = 0
    for line in lines:
        positions.append(offset)
        offset += len(line)
    return positions


def test_forward_bounded_reverse_and_cursor_modes_share_byte_offsets(
    tmp_path,
) -> None:
    lines = _fixture_lines()
    offsets = _offsets(lines)
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_bytes(b"".join(lines))

    forward = read_forward(transcript)
    assert [record.offset for record in forward.records] == offsets
    assert [record.parsed is not None for record in forward.records] == [
        True,
        False,
        False,
        True,
    ]
    assert forward.end_offset == len(b"".join(lines))
    assert forward.file_size == transcript_size(transcript)

    bounded = read_bounded(
        transcript,
        start_offset=offsets[1],
        end_offset=offsets[3],
    )
    assert [record.offset for record in bounded.records] == offsets[1:3]

    before_last = read_reverse_window(
        transcript,
        end_offset=offsets[3],
        max_bytes=forward.file_size,
    )
    assert [record.offset for record in before_last.records] == offsets[:3]

    key = render_cursor(TIMESTAMP, offsets[3])
    assert key == f"{TIMESTAMP}#{offsets[3]}"
    assert cursor_offset(key) == offsets[3]
    assert cursor_offset("not-a-cursor") is None
    assert cursor_offset("-1") is None

    last = read_line(transcript, offsets[3])
    assert last is not None
    assert last.parsed == {"timestamp": TIMESTAMP, "value": "last"}
    assert offset_after_line(transcript, offsets[0]) == offsets[1]


def test_partial_bounded_start_advances_to_the_next_complete_record(tmp_path) -> None:
    lines = _fixture_lines()
    offsets = _offsets(lines)
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_bytes(b"".join(lines))

    read = read_bounded(
        transcript,
        start_offset=offsets[0] + 5,
        end_offset=offsets[3],
        align_partial_start=True,
    )

    assert read.start_offset == offsets[1]
    assert [record.offset for record in read.records] == offsets[1:3]


def test_gzip_uses_the_same_uncompressed_cursor_coordinates(tmp_path) -> None:
    lines = _fixture_lines()
    offsets = _offsets(lines)
    transcript = tmp_path / "rollout.jsonl.gz"
    with gzip.open(transcript, "wb") as handle:
        handle.write(b"".join(lines))

    forward = read_forward(transcript)
    assert transcript_size(transcript) == len(b"".join(lines))
    assert [record.offset for record in forward.records] == offsets
    assert offset_after_line(transcript, offsets[0]) == offsets[1]
    assert read_line(transcript, offsets[3]) == forward.records[3]

    reverse = read_reverse_window(
        transcript,
        end_offset=offsets[3],
        max_bytes=forward.file_size,
    )
    assert reverse.records == forward.records[:3]


def test_one_read_parses_once_before_dispatching_to_multiple_consumers(
    tmp_path, monkeypatch
) -> None:
    lines = _fixture_lines()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_bytes(b"".join(lines))
    original = reader._parse_json_object
    parsed: list[str] = []

    def count_parse(raw: str):
        parsed.append(raw)
        return original(raw)

    monkeypatch.setattr(reader, "_parse_json_object", count_parse)
    read = read_forward(transcript)
    first_consumer = []
    second_consumer = []

    dispatch_records(
        read.records,
        first_consumer.append,
        second_consumer.append,
    )

    assert len(parsed) == len(lines)
    assert first_consumer == list(read.records)
    assert second_consumer == list(read.records)
    assert [
        id(first.parsed) for first in first_consumer if first.parsed is not None
    ] == [id(second.parsed) for second in second_consumer if second.parsed is not None]


def test_cursor_lock_is_reentrant_for_one_incremental_reader() -> None:
    cursor = TranscriptCursor(offset=7, last_key="stamp#7")
    with locked_cursor(cursor) as outer:
        with locked_cursor(cursor) as inner:
            inner.offset = UPDATED_CURSOR_OFFSET
    assert outer is cursor
    assert cursor.offset == UPDATED_CURSOR_OFFSET


@pytest.mark.parametrize(
    ("driver", "fixture_name"),
    [
        (CODEX_DRIVER, "supervised_codex.jsonl"),
        (CLAUDE_DRIVER, "supervised_claude.jsonl"),
    ],
    ids=["codex", "claude"],
)
def test_public_reader_decodes_each_driver_fixture_into_located_typed_events(
    driver: AgentDriver,
    fixture_name: str,
) -> None:
    path = SESSION_FIXTURES / fixture_name
    raw_lines = path.read_bytes().splitlines(keepends=True)
    offsets = _offsets(raw_lines)
    expected = [
        replace(
            event,
            at=replace(
                event.at,
                offset=offset,
                source_actor=SOURCE_ACTOR,
            ),
        )
        for offset, raw in zip(offsets, raw_lines, strict=True)
        for event in decode_line(
            raw.decode(),
            driver,
            source=str(path),
            line=offset,
        )
    ]

    read = TranscriptEventReader(path, driver, SOURCE_ACTOR).read("forward")

    assert list(read.events) == expected
    assert read.end_offset == path.stat().st_size
    assert read.error is None
    for event in read.events:
        assert event.at.source == str(path)
        assert event.at.line == event.at.offset
        assert event.at.source_actor == SOURCE_ACTOR
        assert event.at.timestamp is not None


def test_public_reader_keeps_every_event_on_one_multiblock_claude_line(
    tmp_path,
) -> None:
    path = tmp_path / "claude.jsonl"
    line = _raw(
        {
            "timestamp": TIMESTAMP,
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "checking the join"},
                    {
                        "type": "tool_use",
                        "id": "call-1",
                        "name": "Bash",
                        "input": {"command": "pwd"},
                    },
                    {"type": "thinking", "thinking": "preserve all blocks"},
                ]
            },
        }
    )
    path.write_bytes(line)

    read = TranscriptEventReader(path, CLAUDE_DRIVER, SOURCE_ACTOR).read("forward")

    assert [type(event) for event in read.events] == [
        AssistantText,
        ToolCall,
        Reasoning,
    ]
    assert [event.at.ordinal for event in read.events] == [0, 1, 2]
    assert {event.at.offset for event in read.events} == {0}
    assert {event.at.line for event in read.events} == {0}
    assert {event.at.source_actor for event in read.events} == {SOURCE_ACTOR}
    assert {event.at.timestamp for event in read.events} == {TIMESTAMP}


def test_public_reader_preserves_malformed_lines_as_located_unknowns(tmp_path) -> None:
    path = tmp_path / "broken.jsonl"
    path.write_bytes(b"{not json\n")

    read = TranscriptEventReader(path, CODEX_DRIVER, SOURCE_ACTOR).read("forward")

    assert len(read.events) == 1
    unknown = read.events[0]
    assert isinstance(unknown, Unknown)
    assert unknown.at.source == str(path)
    assert unknown.at.line == 0
    assert unknown.at.offset == 0
    assert unknown.at.ordinal == 0
    assert unknown.at.source_actor == SOURCE_ACTOR
    assert unknown.at.timestamp is None


def test_typed_access_modes_preserve_overlaps_and_cursor_resume(tmp_path) -> None:
    lines = [
        _raw(
            {
                "timestamp": f"2026-07-26T01:15:0{index}.000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "text", "text": f"line {index}"}],
                },
            }
        )
        for index in range(4)
    ]
    offsets = _offsets(lines)
    path = tmp_path / "codex.jsonl"
    path.write_bytes(b"".join(lines))

    event_reader = TranscriptEventReader(path, CODEX_DRIVER, SOURCE_ACTOR)
    whole = event_reader.read("forward")
    first = event_reader.read(
        "bounded",
        start_offset=0,
        end_offset=offsets[2],
    )
    resumed = event_reader.read("forward", start_offset=first.end_offset)
    reverse = event_reader.read(
        "reverse",
        end_offset=offsets[3],
        max_bytes=offsets[3],
    )

    assert first.events + resumed.events == whole.events
    assert reverse.events == whole.events[:3]
    assert len({(event.at.offset, event.at.ordinal) for event in whole.events}) == len(
        whole.events
    )


def test_one_typed_read_parses_and_decodes_once_before_many_projections(
    tmp_path, monkeypatch
) -> None:
    lines = [
        _raw(
            {
                "timestamp": TIMESTAMP,
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "text", "text": str(index)}],
                },
            }
        )
        for index in range(3)
    ]
    path = tmp_path / "codex.jsonl"
    path.write_bytes(b"".join(lines))
    original_parse = reader._parse_json_object
    parse_calls: list[str] = []
    original_decode = CODEX_DRIVER.transcript_line_events
    decode_calls: list[int] = []

    def count_parse(raw: str):
        parse_calls.append(raw)
        return original_parse(raw)

    def count_decode(_driver: AgentDriver, raw: dict, *, source: str, line: int):
        decode_calls.append(line)
        return original_decode(raw, source=source, line=line)

    monkeypatch.setattr(reader, "_parse_json_object", count_parse)
    monkeypatch.setattr(type(CODEX_DRIVER), "transcript_line_events", count_decode)

    read = TranscriptEventReader(path, CODEX_DRIVER, SOURCE_ACTOR).read("forward")
    first_projection = []
    second_projection = []
    read.dispatch(
        first_projection.append,
        second_projection.append,
    )

    assert len(parse_calls) == len(lines)
    assert decode_calls == _offsets(lines)
    assert first_projection == list(read.events)
    assert second_projection == list(read.events)
    assert [id(event) for event in first_projection] == [
        id(event) for event in second_projection
    ]
