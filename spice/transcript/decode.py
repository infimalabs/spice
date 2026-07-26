"""The one crossing from a parsed transcript line to typed events.

Above this module a consumer holds a driver and wants facts. Below it each
dialect adapter holds a parsed object and knows its own shape. This is where the
two meet, and it is the only place that decides what an unparseable line means.

Reading a transcript belongs to the reader engine, which parses each line
exactly once and hands the result here, so nothing above this crossing holds a
raw transcript line or parses one a second time.

Dialect knowledge reaches here only through `AgentDriver` hooks, never through a
substring a consumer wrote. That is the whole point of the seam: consumers once
carried both dialects' raw JSON shapes in their own prefilters, so a third
driver would have silently produced no prose until someone widened every copy.
"""

from __future__ import annotations

from dataclasses import replace
from typing import Any

from spice.agent.driver import AgentDriver
from spice.transcript.events import (
    UNLOCATED_SOURCE,
    LineStamper,
    TranscriptEvent,
    Unknown,
)


def first_text(content: Any) -> str | None:
    """Return the first text block from legacy transcript message content."""
    if not isinstance(content, list):
        return None
    for item in content:
        if isinstance(item, dict) and isinstance(item.get("text"), str):
            return item["text"]
    return None


def decode_parsed_line(
    obj: dict[str, Any] | None,
    driver: AgentDriver,
    *,
    source: str = UNLOCATED_SOURCE,
    line: int = 0,
    offset: int | None = None,
    source_actor: str | None = None,
) -> list[TranscriptEvent]:
    """Decode one parsed line into every typed event it carries, in order.

    `obj` is None for a line the reader could not read as a JSON object, which
    decodes to a single `Unknown` rather than to nothing, so an unreadable
    fragment stays a visible fact instead of a silent gap.
    """
    if obj is None:
        stamper = LineStamper(source=source, line=line, timestamp=None)
        events: list[TranscriptEvent] = [
            Unknown(
                at=stamper.stamp(),
                reason="line is not a JSON object",
                raw_type=None,
            )
        ]
    else:
        events = driver.transcript_line_events(obj, source=source, line=line)
    if offset is None and source_actor is None:
        return events
    return [
        replace(
            event,
            at=replace(
                event.at,
                offset=offset,
                source_actor=source_actor,
            ),
        )
        for event in events
    ]
