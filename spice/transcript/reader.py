"""Shared typed reader for authoritative transcript JSONL.

The transcript remains the sole stored truth.  This module only owns access to
that truth: opening plain or gzip files, byte-offset seeks, bounded and reverse
windows, cursor offsets, malformed-line handling, and one internal parsed
line-record handoff to the driver-backed decoder. Public reads expose only the
resulting typed event stream. Consumer-specific projection stays above it.
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
from typing import Any, BinaryIO, Literal

from spice.agent.driver import AgentDriver
from spice.transcript.decode import _decode_parsed_line
from spice.transcript.events import TranscriptEvent

REVERSE_WINDOW_BYTES = 8 * 1024 * 1024
BinaryTranscript = BinaryIO | gzip.GzipFile

__all__ = [
    "REVERSE_WINDOW_BYTES",
    "TranscriptCursor",
    "TranscriptEventRead",
    "TranscriptEventReader",
    "cursor_offset",
    "locked_cursor",
    "offset_after_line",
    "render_cursor",
    "transcript_size",
]


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
    """Internal access pass whose line records were each parsed exactly once."""

    records: tuple[TranscriptLine, ...]
    access_start_offset: int
    start_offset: int
    end_offset: int
    file_size: int
    file_identity: TranscriptFileIdentity | None
    error: str | None = None


@dataclass(frozen=True, slots=True)
class TranscriptEventRead:
    """One access pass decoded into ordered typed facts exactly once."""

    events: tuple[TranscriptEvent, ...]
    access_start_offset: int
    start_offset: int
    end_offset: int
    file_size: int
    error: str | None = None

    def dispatch(self, *consumers: EventConsumer) -> None:
        """Hand each already-decoded fact to every projection in source order."""
        for event in self.events:
            for consumer in consumers:
                consumer(event)


@dataclass(frozen=True, slots=True)
class TranscriptEventReader:
    """Driver-bound typed access to one authoritative transcript."""

    path: Path
    driver: AgentDriver
    source_actor: str | None

    def read(
        self,
        mode: Literal["forward", "bounded", "reverse"],
        *,
        start_offset: int = 0,
        end_offset: int | None = None,
        align_partial_start: bool = False,
        max_bytes: int = REVERSE_WINDOW_BYTES,
    ) -> TranscriptEventRead:
        """Decode one explicit access mode into ordered typed facts.

        ``forward`` resumes at ``start_offset`` and runs to EOF. ``bounded``
        covers ``start_offset`` through the required ``end_offset``. ``reverse``
        reads the byte window ending before ``end_offset`` (or EOF).
        """
        if mode == "forward":
            raw_read = read_forward(self.path, start_offset=start_offset)
        elif mode == "bounded":
            if end_offset is None:
                raise ValueError("bounded transcript reads require end_offset")
            raw_read = read_bounded(
                self.path,
                start_offset=start_offset,
                end_offset=end_offset,
                align_partial_start=align_partial_start,
            )
        elif mode == "reverse":
            raw_read = read_reverse_window(
                self.path,
                end_offset=end_offset,
                max_bytes=max_bytes,
            )
        else:
            raise ValueError(f"unsupported transcript read mode: {mode}")
        return self._decode(raw_read)

    def _decode(self, read: TranscriptRead) -> TranscriptEventRead:
        source = str(self.path)
        events: list[TranscriptEvent] = []
        for record in read.records:
            events.extend(
                _decode_parsed_line(
                    record.parsed,
                    self.driver,
                    source=source,
                    line=record.offset,
                    offset=record.offset,
                    source_actor=self.source_actor,
                )
            )
        return TranscriptEventRead(
            events=tuple(events),
            access_start_offset=read.access_start_offset,
            start_offset=read.start_offset,
            end_offset=read.end_offset,
            file_size=read.file_size,
            error=read.error,
        )


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


EventConsumer = Callable[[TranscriptEvent], None]


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
    start_offset: int = 0,
    cursor: TranscriptCursor | None = None,
) -> TranscriptRead:
    """Read from an exact resume offset to EOF.

    A truncated source resets the resume offset to zero, matching the existing
    append-only cursor contract.  Supplying ``cursor`` additionally detects a
    replaced source by filesystem identity, uses its offset instead of
    ``start_offset``, and advances both pieces of state after a successful read.
    """
    if cursor is not None:
        with locked_cursor(cursor):
            read = _read_forward(
                path,
                start_offset=cursor.offset,
                expected_identity=cursor.file_identity,
            )
            if read.error is None:
                cursor.offset = read.end_offset
                cursor.file_identity = read.file_identity
            return read
    return _read_forward(path, start_offset=start_offset)


def _read_forward(
    path: Path,
    *,
    start_offset: int,
    expected_identity: TranscriptFileIdentity | None = None,
) -> TranscriptRead:
    try:
        with _open_binary(path) as handle:
            file_identity = _handle_identity(handle)
            file_size = _handle_size(handle)
            start = max(start_offset, 0)
            source_replaced = (
                expected_identity is not None and file_identity != expected_identity
            )
            if source_replaced or start > file_size:
                start = 0
            return _read_open_range(
                handle,
                start=start,
                end=file_size,
                file_size=file_size,
                file_identity=file_identity,
                align_partial_start=False,
            )
    except OSError as exc:
        return _failed_read(exc)


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
