"""Active-context snapshots normalized from agent transcript records."""

from __future__ import annotations

from dataclasses import dataclass

from spice.transcript.events import ContextUsage
from spice.transcript.timestamps import normalize_timestamp


@dataclass(slots=True, frozen=True)
class ActiveContextSnapshot:
    source_file: str
    ts: str
    input_tokens: int
    cached_input_tokens: int
    output_tokens: int
    reasoning_output_tokens: int
    total_tokens: int
    model_context_window: int | None
    cumulative_total_tokens: int


def active_context_snapshot_from_event(
    event: ContextUsage,
) -> ActiveContextSnapshot | None:
    ts = normalize_timestamp(event.at.timestamp)
    if not isinstance(ts, str):
        return None
    last = event.last
    return ActiveContextSnapshot(
        source_file=event.at.source,
        ts=ts,
        input_tokens=last.input_tokens,
        cached_input_tokens=last.cached_input_tokens,
        output_tokens=last.output_tokens,
        reasoning_output_tokens=last.reasoning_output_tokens,
        total_tokens=last.total_tokens,
        model_context_window=event.model_context_window,
        cumulative_total_tokens=(
            event.cumulative.total_tokens if event.cumulative is not None else 0
        ),
    )
