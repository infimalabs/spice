"""The crossings from parsed transcript/stdout lines to typed events.

Above this module a consumer holds a driver and wants facts. Below it each
dialect adapter holds a parsed object and knows its own shape. These two entry
points are where the planes meet, and this is the only module that decides what
an unparseable JSONL line means.

Reading either surface belongs to the reader engine, which parses each line
exactly once and hands the result here, so nothing above these crossings holds
a raw JSONL line or parses one a second time.

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
    return _decode_with(
        obj,
        driver,
        stdout=False,
        source=source,
        line=line,
        offset=offset,
        source_actor=source_actor,
    )


def decode_stdout_parsed_line(
    obj: dict[str, Any] | None,
    driver: AgentDriver,
    *,
    source: str = UNLOCATED_SOURCE,
    line: int = 0,
    offset: int | None = None,
    source_actor: str | None = None,
) -> list[TranscriptEvent]:
    """Decode one parsed JSON stdout line through the driver's stdout seam."""
    return _decode_with(
        obj,
        driver,
        stdout=True,
        source=source,
        line=line,
        offset=offset,
        source_actor=source_actor,
    )


def _decode_with(
    obj: dict[str, Any] | None,
    driver: AgentDriver,
    *,
    stdout: bool,
    source: str,
    line: int,
    offset: int | None,
    source_actor: str | None,
) -> list[TranscriptEvent]:
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
        decoder = driver.stdout_line_events if stdout else driver.transcript_line_events
        events = decoder(obj, source=source, line=line)
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
