"""The one crossing from a raw transcript line of text to typed events.

Above this module a consumer holds a line and a driver and wants facts. Below it
each dialect adapter holds a parsed object and knows its own shape. This is where
the two meet, and it is the only place that decides what an unparseable line
means or when a line is cheap enough to skip.

Dialect knowledge reaches here only through `AgentDriver` hooks, never through a
substring a consumer wrote. That is the whole point of the seam: `mail/watch`
once carried both dialects' raw JSON shapes in one prefilter, so a third driver
would have silently produced no ACK text until someone remembered to widen it.
"""

from __future__ import annotations

import json
from typing import Any

from spice.agent.driver import AgentDriver
from spice.transcript.events import (
    UNLOCATED_SOURCE,
    AssistantText,
    LineStamper,
    TranscriptEvent,
    Unknown,
)


def decode_line(
    raw_line: str,
    driver: AgentDriver,
    *,
    source: str = UNLOCATED_SOURCE,
    line: int = 0,
) -> list[TranscriptEvent]:
    """Decode one raw JSONL line into every typed event it carries, in order.

    A line that is not a JSON object decodes to a single `Unknown` rather than
    to nothing, so an unreadable fragment stays a visible fact.
    """
    obj = _loads(raw_line)
    if obj is None:
        stamper = LineStamper(source=source, line=line, timestamp=None)
        return [
            Unknown(
                at=stamper.stamp(),
                reason="line is not a JSON object",
                raw_type=None,
            )
        ]
    return driver.transcript_line_events(obj, source=source, line=line)


def decode_assistant_text(
    raw_line: str,
    driver: AgentDriver,
    *,
    source: str = UNLOCATED_SOURCE,
    line: int = 0,
) -> list[AssistantText]:
    """The assistant prose on one line, skipping non-candidates unparsed.

    The driver's line hint runs first, so the overwhelming majority of lines cost
    a substring search instead of a JSON parse. The hint is permissive by
    contract, so a line it admits still has to survive the decoder.
    """
    if not driver.line_may_carry_assistant_text(raw_line):
        return []
    events = decode_line(raw_line, driver, source=source, line=line)
    return [event for event in events if isinstance(event, AssistantText)]


def _loads(raw_line: str) -> dict[str, Any] | None:
    try:
        obj = json.loads(raw_line)
    except (json.JSONDecodeError, ValueError):
        return None
    return obj if isinstance(obj, dict) else None
