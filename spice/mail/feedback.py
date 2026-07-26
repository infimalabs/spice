"""Structured supervisor feedback lines for agent stderr."""

from __future__ import annotations

import shlex
from collections.abc import Iterable
from dataclasses import dataclass
from typing import Any

SUPERVISOR_FEEDBACK_FIELD = "feedback"
SUPERVISOR_FEEDBACK_HEADING = "Supervisor Feedback"


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


def supervisor_feedback_notices(text: str) -> list[SupervisorFeedback]:
    """Parse every notice the supervisor appended under its heading in `text`.

    The supervisor writes its notices as an indented block beneath a
    `Supervisor Feedback` heading inside otherwise arbitrary tool output, so a
    block ends at the first blank line, the first unindented line, or the next
    heading. Reading that framing belongs with the line grammar it frames, not
    with whichever consumer happens to render the notices.
    """
    notices: list[SupervisorFeedback] = []
    lines = text.replace("\r\n", "\n").replace("\r", "\n").split("\n")
    index = 0
    while index < len(lines):
        if lines[index].strip() != SUPERVISOR_FEEDBACK_HEADING:
            index += 1
            continue
        index += 1
        while index < len(lines):
            line = lines[index]
            stripped = line.strip()
            if stripped == SUPERVISOR_FEEDBACK_HEADING:
                break
            if not stripped:
                index += 1
                break
            if line == stripped:
                break
            feedback = parse_supervisor_feedback_line(stripped)
            if feedback is not None:
                notices.append(feedback)
            index += 1
    return notices


def _feedback_field_value(value: object) -> str:
    if isinstance(value, str):
        return _one_line_value(value)
    if isinstance(value, Iterable):
        return ",".join(_one_line_value(str(item)) for item in value)
    return _one_line_value(str(value))


def _one_line_value(value: str) -> str:
    return " ".join(value.split())
