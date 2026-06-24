"""Structured supervisor feedback lines for agent stderr."""

from __future__ import annotations

import shlex
from collections.abc import Iterable
from dataclasses import dataclass
from typing import Any

SUPERVISOR_FEEDBACK_FIELD = "feedback"


@dataclass(frozen=True)
class SupervisorFeedback:
    kind: str
    fields: dict[str, Any]


def supervisor_feedback_line(kind: str, **fields: Any) -> str:
    """Return one normalized supervisor feedback notice line."""
    clean_kind = kind.strip()
    if not clean_kind:
        raise ValueError("supervisor feedback kind is required")
    parts = [SUPERVISOR_FEEDBACK_FIELD, clean_kind]
    for key in sorted(fields):
        parts.append(f"{key}={_feedback_field_value(fields[key])}")
    return " ".join(shlex.quote(part) for part in parts)


def parse_supervisor_feedback_line(line: str) -> SupervisorFeedback | None:
    stripped = line.strip()
    try:
        parts = shlex.split(stripped)
    except ValueError:
        return None
    if len(parts) < 2 or parts[0] != SUPERVISOR_FEEDBACK_FIELD:
        return None
    kind = parts[1].strip()
    if not kind:
        return None
    fields: dict[str, Any] = {}
    for token in parts[2:]:
        if "=" not in token:
            return None
        key, value = token.split("=", 1)
        clean_key = key.strip()
        if not clean_key:
            return None
        fields[clean_key] = value
    return SupervisorFeedback(kind=kind, fields=fields)


def _feedback_field_value(value: object) -> str:
    if isinstance(value, str):
        return _one_line_value(value)
    if isinstance(value, Iterable):
        return ",".join(_one_line_value(str(item)) for item in value)
    return _one_line_value(str(value))


def _one_line_value(value: str) -> str:
    return " ".join(value.split())
