"""Active-context snapshots normalized from agent transcript records."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.agent.driver import AgentDriver
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


def active_context_snapshot_from_object(
    path: Path, obj: dict[str, Any], driver: AgentDriver
) -> ActiveContextSnapshot | None:
    fields = driver.context_snapshot_fields(obj)
    if fields is None:
        return None
    ts = normalize_timestamp(obj.get("timestamp"))
    if not isinstance(ts, str):
        return None
    return ActiveContextSnapshot(source_file=str(path), ts=ts, **fields)
