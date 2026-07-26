"""Shared transcript file-access engine contracts."""

from __future__ import annotations

import gzip
import json
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
from pathlib import Path
from threading import Event

import pytest

from spice.agent.driver import CLAUDE_DRIVER, CODEX_DRIVER, AgentDriver
from spice.transcript import reader
from spice.transcript.decode import decode_line
from spice.transcript.events import (
    AssistantText,
    ContextUsage,
    Reasoning,
    ToolCall,
    Unknown,
)
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
CLAUDE_INPUT_TOKENS = 100
CLAUDE_CACHE_READ_TOKENS = 200
CLAUDE_CACHE_CREATE_TOKENS = 30
CLAUDE_OUTPUT_TOKENS = 20
CLAUDE_CACHED_INPUT_TOKENS = CLAUDE_CACHE_READ_TOKENS + CLAUDE_CACHE_CREATE_TOKENS
CLAUDE_TOTAL_TOKENS = (
    CLAUDE_INPUT_TOKENS + CLAUDE_CACHED_INPUT_TOKENS + CLAUDE_OUTPUT_TOKENS
)


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


def _transcript_path(tmp_path: Path, *, compressed: bool) -> Path:
    suffix = ".jsonl.gz" if compressed else ".jsonl"
    return tmp_path / f"rollout{suffix}"


def _write_transcript(path: Path, payload: bytes, *, compressed: bool) -> None:
    if compressed:
        with gzip.open(path, "wb") as handle:
            handle.write(payload)
        return
    path.write_bytes(payload)


def _append_transcript(path: Path, payload: bytes, *, compressed: bool) -> None:
    if compressed:
        with gzip.open(path, "ab") as handle:
            handle.write(payload)
        return
    with path.open("ab") as handle:
        handle.write(payload)


def test_forward_bounded_reverse_and_cursor_modes_share_byte_offsets(
    tmp_path,
) -> None:
    lines = _fixture_lines()
    offsets = _offsets(lines)
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_bytes(b"".join(lines))

    forward = read_forward(transcript, cursor=TranscriptCursor())
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

    forward = read_forward(transcript, cursor=TranscriptCursor())
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
    read = read_forward(transcript, cursor=TranscriptCursor())
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


def test_public_reader_attaches_claude_context_usage_after_message_blocks(
    tmp_path,
) -> None:
    path = tmp_path / "claude.jsonl"
    path.write_bytes(
        _raw(
            {
                "type": "assistant",
                "timestamp": TIMESTAMP,
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "measured"}],
                    "usage": {
                        "input_tokens": CLAUDE_INPUT_TOKENS,
                        "cache_read_input_tokens": CLAUDE_CACHE_READ_TOKENS,
                        "cache_creation_input_tokens": CLAUDE_CACHE_CREATE_TOKENS,
                        "output_tokens": CLAUDE_OUTPUT_TOKENS,
                    },
                },
            }
        )
    )

    read = TranscriptEventReader(path, CLAUDE_DRIVER, SOURCE_ACTOR).read("forward")

    assert [type(event) for event in read.events] == [AssistantText, ContextUsage]
    usage = read.events[1]
    assert isinstance(usage, ContextUsage)
    assert usage.last.input_tokens == CLAUDE_INPUT_TOKENS
    assert usage.last.cached_input_tokens == CLAUDE_CACHED_INPUT_TOKENS
    assert usage.last.output_tokens == CLAUDE_OUTPUT_TOKENS
    assert usage.last.total_tokens == CLAUDE_TOTAL_TOKENS
    assert usage.cumulative == usage.last
    assert usage.model_context_window == CLAUDE_DRIVER.default_context_window
    assert usage.at.source == str(path)
    assert usage.at.offset == 0
    assert usage.at.source_actor == SOURCE_ACTOR
    assert usage.at.ordinal == 1


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
    resumed = event_reader.read(
        "forward",
        cursor=TranscriptCursor(offset=first.end_offset),
    )
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


def test_typed_since_mode_pages_to_timestamp_with_source_line_context(
    tmp_path,
) -> None:
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
        for index in range(5)
    ]
    path = tmp_path / "codex.jsonl"
    path.write_bytes(b"".join(lines))

    read = TranscriptEventReader(path, CODEX_DRIVER).read(
        "since",
        start_timestamp="2026-07-26T01:15:03Z",
        context_lines_before_start=1,
        max_bytes=64,
    )

    assert [
        event.text for event in read.events if isinstance(event, AssistantText)
    ] == ["line 2", "line 3", "line 4"]


def test_public_typed_reader_reads_gzip_through_the_same_entry_point(tmp_path) -> None:
    line = _raw(
        {
            "timestamp": TIMESTAMP,
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "compressed fact"}],
            },
        }
    )
    path = tmp_path / "codex.jsonl.gz"
    _write_transcript(path, line, compressed=True)

    read = TranscriptEventReader(path, CODEX_DRIVER).read("forward")

    assert len(read.events) == 1
    assert isinstance(read.events[0], AssistantText)
    assert read.events[0].text == "compressed fact"
    assert read.events[0].at.offset == 0
    assert read.end_offset == len(line)


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


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
def test_garbled_mid_file_and_truncated_final_record_are_counted_skips(
    tmp_path: Path, *, compressed: bool
) -> None:
    before = _raw({"timestamp": TIMESTAMP, "value": "before"})
    garbled = b"\xff\xfe not json\n"
    after = _raw({"timestamp": TIMESTAMP, "value": "after"})
    truncated = b'{"timestamp":"unterminated"'
    payload = before + garbled + after + truncated
    transcript = _transcript_path(tmp_path, compressed=compressed)
    _write_transcript(transcript, payload, compressed=compressed)

    cursor = TranscriptCursor()
    read = read_forward(transcript, cursor=cursor)

    assert [record.parsed and record.parsed["value"] for record in read.records] == [
        "before",
        None,
        "after",
        None,
    ]
    assert sum(record.parsed is None for record in read.records) == 2
    assert "\ufffd" in read.records[1].raw
    assert read.records[-1].raw == truncated.decode()
    assert read.end_offset == read.file_size == len(payload)
    assert read_forward(transcript, cursor=cursor).records == ()


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
def test_empty_file_has_a_stable_zero_cursor(
    tmp_path: Path, *, compressed: bool
) -> None:
    transcript = _transcript_path(tmp_path, compressed=compressed)
    _write_transcript(transcript, b"", compressed=compressed)

    forward = read_forward(transcript, cursor=TranscriptCursor())
    reverse = read_reverse_window(transcript)

    assert forward.records == reverse.records == ()
    assert (
        forward.access_start_offset,
        forward.start_offset,
        forward.end_offset,
        forward.file_size,
    ) == (0, 0, 0, 0)
    assert reverse.end_offset == reverse.file_size == 0
    assert read_line(transcript, 0) is None
    assert offset_after_line(transcript, 0) == 0


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
def test_oversized_single_line_is_delivered_and_followed_by_a_stable_cursor(
    tmp_path: Path, *, compressed: bool
) -> None:
    oversized = _raw(
        {
            "timestamp": TIMESTAMP,
            "value": "x" * (reader.REVERSE_WINDOW_BYTES + 1),
        }
    )
    following = _raw({"timestamp": TIMESTAMP, "value": "following"})
    transcript = _transcript_path(tmp_path, compressed=compressed)
    _write_transcript(transcript, oversized + following, compressed=compressed)

    cursor = TranscriptCursor()
    forward = read_forward(transcript, cursor=cursor)
    reverse = read_reverse_window(
        transcript,
        max_bytes=reader.REVERSE_WINDOW_BYTES,
    )

    assert [record.offset for record in forward.records] == [0, len(oversized)]
    assert forward.records[0].parsed is not None
    assert len(forward.records[0].parsed["value"]) == reader.REVERSE_WINDOW_BYTES + 1
    assert [record.parsed and record.parsed["value"] for record in reverse.records] == [
        "following"
    ]
    assert reverse.start_offset == len(oversized)
    assert forward.end_offset == reverse.end_offset == len(oversized + following)
    assert read_forward(transcript, cursor=cursor).records == ()


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
def test_reverse_page_boundary_excludes_an_independent_later_append(
    tmp_path: Path, *, compressed: bool
) -> None:
    original_lines = [
        _raw({"timestamp": TIMESTAMP, "value": "before-0"}),
        _raw({"timestamp": TIMESTAMP, "value": "before-1"}),
    ]
    appended_lines = [
        _raw({"timestamp": TIMESTAMP, "value": "after-0"}),
        _raw({"timestamp": TIMESTAMP, "value": "after-1"}),
    ]
    transcript = _transcript_path(tmp_path, compressed=compressed)
    _write_transcript(transcript, b"".join(original_lines), compressed=compressed)
    cursor = TranscriptCursor()
    initial = read_forward(transcript, cursor=cursor)
    page_end = cursor.offset
    boundary_fixed = Event()

    def append_after_boundary() -> None:
        boundary_fixed.wait()
        _append_transcript(
            transcript,
            b"".join(appended_lines),
            compressed=compressed,
        )

    with ThreadPoolExecutor(max_workers=1) as executor:
        append = executor.submit(append_after_boundary)
        # The writer cannot mutate before page_end is fixed.  Waiting for it
        # here, then asserting page.file_size grew, witnesses that the reader
        # opened the larger source while still honoring the older boundary.
        boundary_fixed.set()
        append.result()
        page = read_reverse_window(
            transcript,
            end_offset=page_end,
            max_bytes=page_end,
        )

    resumed = read_forward(transcript, cursor=cursor)
    values = [
        record.parsed["value"]
        for record in (*page.records, *resumed.records)
        if record.parsed is not None
    ]

    assert initial.end_offset == page_end
    assert page.file_size > page_end
    assert page.end_offset == page_end
    assert [record.parsed["value"] for record in page.records] == [
        "before-0",
        "before-1",
    ]
    assert [record.parsed["value"] for record in resumed.records] == [
        "after-0",
        "after-1",
    ]
    assert values == ["before-0", "before-1", "after-0", "after-1"]
    assert len(values) == len(set(values))
    assert cursor.offset == resumed.end_offset == transcript_size(transcript)


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
@pytest.mark.parametrize("rotate", [False, True], ids=["shrink", "rotation"])
def test_cursor_resume_after_shrink_or_rotation_loses_and_duplicates_no_records(
    tmp_path: Path, *, compressed: bool, rotate: bool
) -> None:
    old_lines = [
        _raw({"timestamp": TIMESTAMP, "value": "old-0-long-record"}),
        _raw({"timestamp": TIMESTAMP, "value": "old-1-long-record"}),
    ]
    new_values = (
        ["new-0-long-replacement", "new-1-long-replacement", "new-2-long-replacement"]
        if rotate
        else ["new-0", "new-1"]
    )
    new_lines = [_raw({"timestamp": TIMESTAMP, "value": value}) for value in new_values]
    transcript = _transcript_path(tmp_path, compressed=compressed)
    _write_transcript(transcript, b"".join(old_lines), compressed=compressed)
    cursor = TranscriptCursor()
    first = read_forward(transcript, cursor=cursor)

    if rotate:
        transcript.rename(tmp_path / f"rotated-{transcript.name}")
    _write_transcript(transcript, b"".join(new_lines), compressed=compressed)
    if rotate:
        assert transcript_size(transcript) >= first.end_offset
    else:
        assert transcript_size(transcript) < first.end_offset

    resumed = read_forward(transcript, cursor=cursor)
    first_values = [
        record.parsed["value"] for record in first.records if record.parsed is not None
    ]
    resumed_values = [
        record.parsed["value"]
        for record in resumed.records
        if record.parsed is not None
    ]

    assert resumed.access_start_offset == resumed.start_offset == 0
    assert resumed_values == new_values
    assert len(first_values + resumed_values) == len(set(first_values + resumed_values))
    if rotate:
        assert first.file_identity != resumed.file_identity
    else:
        assert first.file_identity == resumed.file_identity
    assert cursor.file_identity == resumed.file_identity
    assert cursor.offset == resumed.end_offset == resumed.file_size
    assert resumed.file_size == transcript_size(transcript)
