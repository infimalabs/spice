"""Shared file-access engine for authoritative transcript JSONL.

The transcript remains the sole stored truth.  This module only owns access to
that truth: opening plain or gzip files, byte-offset seeks, bounded and reverse
windows, cursor offsets, malformed-line handling, and one parsed line-record
handoff.  Dialect decoding and consumer-specific projection stay above it.
"""

from __future__ import annotations

import gzip
import json
import os
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path
from threading import RLock
from typing import Any, BinaryIO

REVERSE_WINDOW_BYTES = 8 * 1024 * 1024
BinaryTranscript = BinaryIO | gzip.GzipFile


@dataclass(frozen=True, slots=True)
class TranscriptLine:
    """One source line, its byte locus, and its parsed JSON object if valid."""

    raw: str
    offset: int
    parsed: dict[str, Any] | None


LineConsumer = Callable[[TranscriptLine], None]


@dataclass(frozen=True, slots=True)
class TranscriptFileIdentity:
    """Filesystem identity of the opened source behind one read."""

    device: int
    inode: int


@dataclass(frozen=True, slots=True)
class TranscriptRead:
    """One bounded access pass whose records were each parsed exactly once."""

    records: tuple[TranscriptLine, ...]
    access_start_offset: int
    start_offset: int
    end_offset: int
    file_size: int
    file_identity: TranscriptFileIdentity | None
    error: str | None = None


@dataclass
class TranscriptCursor:
    """Shared byte-cursor state and the lock guarding one incremental reader."""

    offset: int = 0
    last_key: str | None = None
    file_identity: TranscriptFileIdentity | None = None
    lock: RLock = field(default_factory=RLock, repr=False)


def dispatch_records(
    records: tuple[TranscriptLine, ...], *consumers: LineConsumer
) -> None:
    """Hand every parsed record to each consumer once, in source order."""
    for record in records:
        for consumer in consumers:
            consumer(record)


@contextmanager
def locked_cursor(cursor: TranscriptCursor) -> Iterator[TranscriptCursor]:
    """Serialize one incremental reader's cursor and window mutation."""
    with cursor.lock:
        yield cursor


def render_cursor(timestamp: str, offset: int) -> str:
    """Render the stable wire cursor used by transcript consumers."""
    return f"{timestamp}#{offset}" if timestamp else str(offset)


def cursor_offset(cursor: str) -> int | None:
    """Extract a non-negative byte offset from a timestamp#offset cursor."""
    raw = cursor.rsplit("#", 1)[-1]
    try:
        offset = int(raw)
    except ValueError:
        return None
    return offset if offset >= 0 else None


def transcript_size(path: Path) -> int | None:
    """Return the seek-coordinate size, uncompressed for gzip transcripts."""
    try:
        with _open_binary(path) as handle:
            handle.seek(0, 2)
            return handle.tell()
    except OSError:
        return None


def transcript_file_identity(path: Path) -> TranscriptFileIdentity | None:
    """Return the current filesystem identity for a transcript path."""
    try:
        stat = path.stat()
    except OSError:
        return None
    return TranscriptFileIdentity(device=stat.st_dev, inode=stat.st_ino)


def read_forward(
    path: Path,
    *,
    cursor: TranscriptCursor,
) -> TranscriptRead:
    """Read from the cursor's exact resume offset to EOF.

    A truncated source resets the resume offset to zero, matching the existing
    append-only contract.  Filesystem identity detects a replaced source
    independently of size.  Successful reads advance both pieces of state.
    """
    with locked_cursor(cursor):
        try:
            with _open_binary(path) as handle:
                file_identity = _handle_identity(handle)
                file_size = _handle_size(handle)
                start = max(cursor.offset, 0)
                source_replaced = (
                    cursor.file_identity is not None
                    and file_identity != cursor.file_identity
                )
                if source_replaced or start > file_size:
                    start = 0
                read = _read_open_range(
                    handle,
                    start=start,
                    end=file_size,
                    file_size=file_size,
                    file_identity=file_identity,
                    align_partial_start=False,
                )
        except OSError as exc:
            read = _failed_read(exc)
        else:
            cursor.offset = read.end_offset
            cursor.file_identity = read.file_identity
        return read


def read_bounded(
    path: Path,
    *,
    start_offset: int,
    end_offset: int,
    align_partial_start: bool = False,
) -> TranscriptRead:
    """Read one byte-offset range in source order."""
    try:
        with _open_binary(path) as handle:
            file_identity = _handle_identity(handle)
            file_size = _handle_size(handle)
            start = min(max(start_offset, 0), file_size)
            end = min(max(end_offset, start), file_size)
            return _read_open_range(
                handle,
                start=start,
                end=end,
                file_size=file_size,
                file_identity=file_identity,
                align_partial_start=align_partial_start,
            )
    except OSError as exc:
        return _failed_read(exc)


def read_reverse_window(
    path: Path,
    *,
    end_offset: int | None = None,
    max_bytes: int = REVERSE_WINDOW_BYTES,
) -> TranscriptRead:
    """Read the bounded tail ending before ``end_offset``, chronologically."""
    try:
        with _open_binary(path) as handle:
            file_identity = _handle_identity(handle)
            file_size = _handle_size(handle)
            end = (
                file_size if end_offset is None else min(max(end_offset, 0), file_size)
            )
            start = max(0, end - max(max_bytes, 0))
            return _read_open_range(
                handle,
                start=start,
                end=end,
                file_size=file_size,
                file_identity=file_identity,
                align_partial_start=start > 0,
            )
    except OSError as exc:
        return _failed_read(exc)


def read_line(path: Path, offset: int) -> TranscriptLine | None:
    """Read and parse the line beginning at an exact cursor offset."""
    try:
        with _open_binary(path) as handle:
            file_size = _handle_size(handle)
            if offset < 0 or offset >= file_size:
                return None
            handle.seek(offset)
            raw = handle.readline()
            return _line_record(offset, raw) if raw else None
    except OSError:
        return None


def offset_after_line(path: Path, offset: int) -> int:
    """Return the byte position after the line at ``offset``."""
    try:
        with _open_binary(path) as handle:
            handle.seek(max(offset, 0))
            handle.readline()
            return handle.tell()
    except OSError:
        return offset


@contextmanager
def _open_binary(path: Path) -> Iterator[BinaryTranscript]:
    if path.name.lower().endswith(".gz"):
        with gzip.open(path, "rb") as handle:
            yield handle
        return
    with path.open("rb") as handle:
        yield handle


def _handle_size(handle: BinaryTranscript) -> int:
    handle.seek(0, 2)
    size = handle.tell()
    handle.seek(0)
    return size


def _handle_identity(handle: BinaryTranscript) -> TranscriptFileIdentity:
    stat = os.fstat(handle.fileno())
    return TranscriptFileIdentity(device=stat.st_dev, inode=stat.st_ino)


def _read_open_range(
    handle: BinaryTranscript,
    *,
    start: int,
    end: int,
    file_size: int,
    file_identity: TranscriptFileIdentity,
    align_partial_start: bool,
) -> TranscriptRead:
    handle.seek(start)
    if align_partial_start and not _is_line_boundary(handle, start):
        handle.readline()
    actual_start = handle.tell()
    records: list[TranscriptLine] = []
    while True:
        offset = handle.tell()
        if offset >= end:
            break
        raw = handle.readline()
        if not raw:
            break
        records.append(_line_record(offset, raw))
    return TranscriptRead(
        records=tuple(records),
        access_start_offset=start,
        start_offset=actual_start,
        end_offset=handle.tell(),
        file_size=file_size,
        file_identity=file_identity,
    )


def _is_line_boundary(handle: BinaryTranscript, offset: int) -> bool:
    if offset <= 0:
        return True
    handle.seek(offset - 1)
    boundary = handle.read(1) == b"\n"
    handle.seek(offset)
    return boundary


def _line_record(offset: int, raw: bytes) -> TranscriptLine:
    text = raw.decode("utf-8", errors="replace")
    return TranscriptLine(raw=text, offset=offset, parsed=_parse_json_object(text))


def _parse_json_object(raw: str) -> dict[str, Any] | None:
    try:
        loaded = json.loads(raw)
    except json.JSONDecodeError:
        return None
    return loaded if isinstance(loaded, dict) else None


def _failed_read(exc: OSError) -> TranscriptRead:
    return TranscriptRead(
        records=(),
        access_start_offset=0,
        start_offset=0,
        end_offset=0,
        file_size=0,
        file_identity=None,
        error=str(exc),
    )
