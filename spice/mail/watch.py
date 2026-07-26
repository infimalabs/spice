"""Extract assistant transcript text and operator-visible prose."""

from __future__ import annotations

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
