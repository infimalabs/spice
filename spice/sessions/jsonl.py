"""Line readers for plain and compressed session JSONL transcripts."""

from __future__ import annotations

import gzip
from collections.abc import Iterator
from pathlib import Path

REVERSE_READ_BLOCK_BYTES = 64 * 1024


def iter_jsonl_lines(path: Path) -> Iterator[str]:
    if _is_gzip_path(path):
        with gzip.open(path, "rt", encoding="utf-8", errors="replace") as handle:
            yield from handle
        return
    with path.open(encoding="utf-8", errors="replace") as handle:
        yield from handle


def iter_jsonl_lines_reverse(path: Path) -> Iterator[str]:
    if _is_gzip_path(path):
        lines = [line for line in iter_jsonl_lines(path) if line.strip()]
        yield from reversed(lines)
        return
    with path.open("rb") as handle:
        handle.seek(0, 2)
        position = handle.tell()
        buffer = b""
        while position > 0:
            read_size = min(REVERSE_READ_BLOCK_BYTES, position)
            position -= read_size
            handle.seek(position)
            chunk = handle.read(read_size) + buffer
            lines = chunk.split(b"\n")
            buffer = lines[0]
            for raw_line in reversed(lines[1:]):
                if raw_line.strip():
                    yield raw_line.decode("utf-8", errors="replace")
        if buffer.strip():
            yield buffer.decode("utf-8", errors="replace")


def _is_gzip_path(path: Path) -> bool:
    return path.name.lower().endswith(".gz")
