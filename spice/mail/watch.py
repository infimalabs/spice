"""Extract assistant transcript text and operator-visible prose."""

from __future__ import annotations

import re

from spice.agent.driver import AgentDriver
from spice.transcript.decode import decode_assistant_text


def extract_assistant_text(line: str, driver: AgentDriver) -> str | None:
    """Return the assistant prose carried by a transcript JSONL `line`, or None.

    The dialect knowledge lives in the driver hooks the substrate consumes: the
    cheap prefilter that rejects the overwhelming majority of lines without a
    JSON parse, and the decode of what survives. The first text frame is the one
    this consumer wants, matching the single frame the dict seam used to carry.
    """
    texts = decode_assistant_text(line, driver)
    return next((text.text for text in texts if text.text), None)


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
