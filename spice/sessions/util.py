"""Small shared helpers for transcript text and rendering."""

from __future__ import annotations

from pathlib import Path
from typing import Iterable


def format_int(value: int | None) -> str:
    if value is None:
        return "-"
    return f"{value:,}"


def format_float(value: float | None) -> str:
    if value is None:
        return "-"
    return f"{value:.1f}"


def dedupe_paths(paths: Iterable[Path]) -> list[Path]:
    seen: set[str] = set()
    unique: list[Path] = []
    for path in paths:
        normalized = str(path)
        if normalized in seen:
            continue
        seen.add(normalized)
        unique.append(path)
    return unique
