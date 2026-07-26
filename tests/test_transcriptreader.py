"""Shared transcript file-access engine contracts."""

from __future__ import annotations

import gzip
import json
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Barrier

import pytest

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

    read = read_forward(transcript)

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
    assert read_forward(transcript, start_offset=read.end_offset).records == ()


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
def test_empty_file_has_a_stable_zero_cursor(
    tmp_path: Path, *, compressed: bool
) -> None:
    transcript = _transcript_path(tmp_path, compressed=compressed)
    _write_transcript(transcript, b"", compressed=compressed)

    forward = read_forward(transcript)
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

    forward = read_forward(transcript)
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
    assert read_forward(transcript, start_offset=forward.end_offset).records == ()


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
def test_reverse_page_snapshot_does_not_duplicate_a_concurrent_append(
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
    page_end = sum(map(len, original_lines))
    append_gate = Barrier(2)

    def append_during_page() -> None:
        append_gate.wait()
        _append_transcript(
            transcript,
            b"".join(appended_lines),
            compressed=compressed,
        )

    with ThreadPoolExecutor(max_workers=1) as executor:
        append = executor.submit(append_during_page)
        append_gate.wait()
        page = read_reverse_window(
            transcript,
            end_offset=page_end,
            max_bytes=page_end,
        )
        append.result()

    resumed = read_forward(transcript, start_offset=page.end_offset)
    values = [
        record.parsed["value"]
        for record in (*page.records, *resumed.records)
        if record.parsed is not None
    ]

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
    assert resumed.end_offset == transcript_size(transcript)


@pytest.mark.parametrize("compressed", [False, True], ids=["plain", "gzip"])
@pytest.mark.parametrize("rotate", [False, True], ids=["shrink", "rotation"])
def test_cursor_resume_after_shrink_or_rotation_loses_and_duplicates_no_records(
    tmp_path: Path, *, compressed: bool, rotate: bool
) -> None:
    old_lines = [
        _raw({"timestamp": TIMESTAMP, "value": "old-0-long-record"}),
        _raw({"timestamp": TIMESTAMP, "value": "old-1-long-record"}),
    ]
    new_lines = [
        _raw({"timestamp": TIMESTAMP, "value": "new-0"}),
        _raw({"timestamp": TIMESTAMP, "value": "new-1"}),
    ]
    transcript = _transcript_path(tmp_path, compressed=compressed)
    _write_transcript(transcript, b"".join(old_lines), compressed=compressed)
    first = read_forward(transcript)

    if rotate:
        transcript.rename(tmp_path / f"rotated-{transcript.name}")
    _write_transcript(transcript, b"".join(new_lines), compressed=compressed)
    assert transcript_size(transcript) < first.end_offset

    resumed = read_forward(transcript, start_offset=first.end_offset)
    first_values = [
        record.parsed["value"] for record in first.records if record.parsed is not None
    ]
    resumed_values = [
        record.parsed["value"]
        for record in resumed.records
        if record.parsed is not None
    ]

    assert resumed.access_start_offset == resumed.start_offset == 0
    assert resumed_values == ["new-0", "new-1"]
    assert len(first_values + resumed_values) == len(set(first_values + resumed_values))
    assert resumed.end_offset == resumed.file_size == transcript_size(transcript)
