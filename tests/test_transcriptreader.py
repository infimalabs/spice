"""Shared transcript file-access engine contracts."""

from __future__ import annotations

import gzip
import json

from spice.transcript import reader
from spice.transcript.reader import (
    TranscriptCursor,
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
