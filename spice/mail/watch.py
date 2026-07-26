"""Extract assistant transcript text and operator-visible prose."""

from __future__ import annotations

import re
from collections.abc import Sequence

from spice.transcript.events import AssistantText


def extract_assistant_text(texts: Sequence[AssistantText]) -> str | None:
    """Return the assistant prose one transcript record carries, or None.

    Mail reads typed facts and nothing else. The dialect knowledge stays under
    the reader engine, which consults the driver's cheap line hint and decodes
    what survives; no line, no raw record, and no dialect literal reaches here.
    The first non-empty text frame is the one this consumer wants, matching the
    single frame the raw-line seam used to carry.
    """
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
