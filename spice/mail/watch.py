"""Extract assistant transcript text and operator-visible prose."""

from __future__ import annotations

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
