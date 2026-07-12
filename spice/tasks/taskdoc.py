"""Task-document input shared by ingest and the markdown dialect."""

from __future__ import annotations

import sys
from pathlib import Path


def read_document(path: str | Path) -> str:
    """Read one BOM-tolerant task document with canonical newlines."""
    if str(path) == "-":
        return _normalize_document_text(sys.stdin.read())
    with Path(path).open("r", encoding="utf-8-sig", newline=None) as stream:
        return _normalize_document_text(stream.read())


def _normalize_document_text(text: str) -> str:
    return text.removeprefix("\ufeff").replace("\r\n", "\n").replace("\r", "\n")
