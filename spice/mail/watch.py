"""Extract assistant transcript text and operator-visible prose."""

from __future__ import annotations

import json
import re
from typing import Any

from spice.agent.driver import AgentDriver
from spice.sessions.util import first_text


def _line_might_carry_assistant_message(line: str) -> bool:
    return '"message"' in line and (
        '"role":"assistant"' in line or '"type":"assistant"' in line
    )


def _safe_loads(line: str) -> dict[str, Any] | None:
    try:
        obj = json.loads(line)
    except json.JSONDecodeError:
        return None
    return obj if isinstance(obj, dict) else None


def extract_assistant_text(line: str, driver: AgentDriver) -> str | None:
    """Return the assistant prose carried by a transcript JSONL `line`, or None.

    Cheap substring prefilter first: an overwhelming majority of transcript
    lines are tool calls / function results that we can reject without a JSON
    parse. Only the lines that COULD be an assistant message reach
    `json.loads` — then we validate shape and pull the first text frame.
    """
    if not _line_might_carry_assistant_message(line):
        return None
    obj = _safe_loads(line)
    if obj is None:
        return None
    event = driver.normalize_transcript_line(obj)
    if event is None:
        return None
    payload = event.get("payload") or {}
    if event.get("type") != "response_item":
        return None
    if payload.get("type") != "message" or payload.get("role") != "assistant":
        return None
    text = first_text(payload.get("content"))
    return text or None


_APP_DIRECTIVE_LINE_RE = re.compile(r"^\s*::[a-z][a-z0-9-]*\{.*\}\s*$")


def strip_app_directive_lines(text: str) -> str:
    """Remove app control directives from assistant-facing prose.

    Directives such as `::git-stage{...}` and `::git-commit{...}` are meant
    for the host app, not for the steering transcript or audible speech.
    """
    lines = [line for line in text.splitlines() if not _is_app_directive_line(line)]
    return "\n".join(lines).rstrip()


def _is_app_directive_line(line: str) -> bool:
    return _APP_DIRECTIVE_LINE_RE.match(line) is not None
