"""The context meter: active-context state read from agent transcripts.

A `token_count` payload in the transcript carries the latest turn's usage and
the model context window. The meter folds those snapshots together with
`compacted` events. The harness uses the meter to decide when to repeat a
simple keep-working instruction; user-facing output does not expose the
underlying thresholds.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from statistics import median
from typing import Any, Iterator

from spice.agent.driver import driver_for_transcript
from spice.sessions.util import (
    normalize_timestamp,
    safe_percent,
)

REVERSE_READ_BLOCK_BYTES = 64 * 1024
RED_PRESSURE_PERCENT = 90.0
ORANGE_PRESSURE_PERCENT = 85.0
YELLOW_PRESSURE_PERCENT = 75.0


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


@dataclass(slots=True, frozen=True)
class ContextMeter:
    source_files: tuple[str, ...]
    latest_snapshot: ActiveContextSnapshot | None
    snapshot_count: int
    compaction_count: int
    latest_compaction_ts: str | None
    snapshots_since_compaction: int
    pre_compaction_min_tokens: int | None
    pre_compaction_median_tokens: float | None
    pre_compaction_max_tokens: int | None
    pre_compaction_min_percent: float | None
    pre_compaction_median_percent: float | None
    pre_compaction_max_percent: float | None


def collect_context_meter(files: list[Path]) -> ContextMeter:
    events: list[tuple[str, int, str, ActiveContextSnapshot | None]] = []
    snapshots: list[ActiveContextSnapshot] = []
    order = 0
    for path in files:
        for obj in _iter_jsonl_objects(path):
            order += 1
            snapshot = active_context_snapshot_from_object(path, obj)
            if snapshot is not None:
                snapshots.append(snapshot)
                events.append((snapshot.ts, order, "snapshot", snapshot))
                continue
            compaction_ts = compaction_ts_from_object(path, obj)
            if compaction_ts is not None:
                events.append((compaction_ts, order, "compaction", None))
    return _build_context_meter(files, sorted(events), snapshots)


def collect_latest_context_meter(files: list[Path]) -> ContextMeter:
    """Cheap latest-only meter: read each file backwards to the newest snapshot."""
    snapshots = [
        snapshot
        for path in files
        if (snapshot := latest_active_context_snapshot_for_file(path)) is not None
    ]
    snapshots.sort(key=lambda snapshot: (snapshot.ts, snapshot.source_file))
    latest_snapshot = snapshots[-1] if snapshots else None
    return ContextMeter(
        source_files=tuple(str(path) for path in files),
        latest_snapshot=latest_snapshot,
        snapshot_count=len(snapshots),
        compaction_count=0,
        latest_compaction_ts=None,
        snapshots_since_compaction=len(snapshots),
        pre_compaction_min_tokens=None,
        pre_compaction_median_tokens=None,
        pre_compaction_max_tokens=None,
        pre_compaction_min_percent=None,
        pre_compaction_median_percent=None,
        pre_compaction_max_percent=None,
    )


def latest_active_context_snapshot_for_file(path: Path) -> ActiveContextSnapshot | None:
    for line in _iter_jsonl_lines_reverse(path):
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict):
            snapshot = active_context_snapshot_from_object(path, obj)
            if snapshot is not None:
                return snapshot
    return None


def _iter_jsonl_objects(path: Path) -> Iterator[dict[str, Any]]:
    with path.open(encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(obj, dict):
                yield obj


def _iter_jsonl_lines_reverse(path: Path) -> Iterator[str]:
    with path.open("rb") as handle:
        handle.seek(0, 2)
        position = handle.tell()
        buffer = b""
        while position > 0:
            read_size = min(REVERSE_READ_BLOCK_BYTES, position)
            position -= read_size
            handle.seek(position)
            chunk = handle.read(read_size) + buffer
            lines = chunk.split(b"\n")
            buffer = lines[0]
            for raw_line in reversed(lines[1:]):
                if raw_line.strip():
                    yield raw_line.decode("utf-8", errors="replace")
        if buffer.strip():
            yield buffer.decode("utf-8", errors="replace")


def active_context_snapshot_from_object(
    path: Path, obj: dict[str, Any]
) -> ActiveContextSnapshot | None:
    fields = driver_for_transcript(path).context_snapshot_fields(obj)
    if fields is None:
        return None
    ts = normalize_timestamp(obj.get("timestamp"))
    if not isinstance(ts, str):
        return None
    return ActiveContextSnapshot(source_file=str(path), ts=ts, **fields)


def compaction_ts_from_object(path: Path, obj: dict[str, Any]) -> str | None:
    event = driver_for_transcript(path).normalize_transcript_line(obj)
    if event is None or event.get("type") != "compacted":
        return None
    ts = normalize_timestamp(obj.get("timestamp"))
    return ts if isinstance(ts, str) else None


def _build_context_meter(
    files: list[Path],
    events: list[tuple[str, int, str, ActiveContextSnapshot | None]],
    snapshots: list[ActiveContextSnapshot],
) -> ContextMeter:
    latest_snapshot: ActiveContextSnapshot | None = None
    latest_compaction_ts: str | None = None
    compaction_count = 0
    previous_snapshot: ActiveContextSnapshot | None = None
    pre_compaction_snapshots: list[ActiveContextSnapshot] = []
    for ts, _order, kind, snapshot in events:
        if kind == "snapshot":
            previous_snapshot = snapshot
            latest_snapshot = snapshot
            continue
        latest_compaction_ts = ts
        compaction_count += 1
        if previous_snapshot is not None:
            pre_compaction_snapshots.append(previous_snapshot)
    return ContextMeter(
        source_files=tuple(str(path) for path in files),
        latest_snapshot=latest_snapshot,
        snapshot_count=len(snapshots),
        compaction_count=compaction_count,
        latest_compaction_ts=latest_compaction_ts,
        snapshots_since_compaction=_count_snapshots_since_compaction(
            snapshots, latest_compaction_ts
        ),
        **_pre_compaction_stats(pre_compaction_snapshots),
    )


def _count_snapshots_since_compaction(
    snapshots: list[ActiveContextSnapshot], latest_compaction_ts: str | None
) -> int:
    if latest_compaction_ts is None:
        return len(snapshots)
    return sum(1 for snapshot in snapshots if snapshot.ts >= latest_compaction_ts)


def _pre_compaction_stats(
    snapshots: list[ActiveContextSnapshot],
) -> dict[str, Any]:
    token_values = [snapshot.total_tokens for snapshot in snapshots]
    percent_values = [
        percent
        for snapshot in snapshots
        if (percent := active_context_percent(snapshot)) is not None
    ]
    return {
        "pre_compaction_min_tokens": min(token_values) if token_values else None,
        "pre_compaction_median_tokens": median(token_values) if token_values else None,
        "pre_compaction_max_tokens": max(token_values) if token_values else None,
        "pre_compaction_min_percent": min(percent_values) if percent_values else None,
        "pre_compaction_median_percent": median(percent_values)
        if percent_values
        else None,
        "pre_compaction_max_percent": max(percent_values) if percent_values else None,
    }


def active_context_percent(snapshot: ActiveContextSnapshot | None) -> float | None:
    if snapshot is None or snapshot.model_context_window is None:
        return None
    return safe_percent(snapshot.total_tokens, snapshot.model_context_window)


def context_pressure_level(percent: float | None) -> str:
    if percent is None:
        return "unknown"
    if percent >= RED_PRESSURE_PERCENT:
        return "red"
    if percent >= ORANGE_PRESSURE_PERCENT:
        return "orange"
    if percent >= YELLOW_PRESSURE_PERCENT:
        return "yellow"
    return "green"


def context_pressure_should_warn(level: str) -> bool:
    return level in {"yellow", "orange", "red"}


def context_meter_cache_payload(meter: ContextMeter) -> dict[str, Any]:
    snapshot = meter.latest_snapshot
    return {
        "sourceFiles": list(meter.source_files),
        "latestSnapshot": active_context_snapshot_cache_payload(snapshot)
        if snapshot
        else None,
    }


def active_context_snapshot_cache_payload(
    snapshot: ActiveContextSnapshot,
) -> dict[str, Any]:
    return {
        "sourceFile": snapshot.source_file,
        "ts": snapshot.ts,
        "inputTokens": snapshot.input_tokens,
        "cachedInputTokens": snapshot.cached_input_tokens,
        "outputTokens": snapshot.output_tokens,
        "reasoningOutputTokens": snapshot.reasoning_output_tokens,
        "totalTokens": snapshot.total_tokens,
        "modelContextWindow": snapshot.model_context_window,
        "cumulativeTotalTokens": snapshot.cumulative_total_tokens,
    }


def context_meter_from_cache_payload(payload: Any) -> ContextMeter | None:
    if not isinstance(payload, dict):
        return None
    snapshot = active_context_snapshot_from_cache_payload(payload.get("latestSnapshot"))
    if snapshot is None:
        return None
    source_files = payload.get("sourceFiles")
    return ContextMeter(
        source_files=tuple(str(item) for item in source_files)
        if isinstance(source_files, list)
        else (),
        latest_snapshot=snapshot,
        snapshot_count=1,
        compaction_count=0,
        latest_compaction_ts=None,
        snapshots_since_compaction=1,
        pre_compaction_min_tokens=None,
        pre_compaction_median_tokens=None,
        pre_compaction_max_tokens=None,
        pre_compaction_min_percent=None,
        pre_compaction_median_percent=None,
        pre_compaction_max_percent=None,
    )


def active_context_snapshot_from_cache_payload(
    payload: Any,
) -> ActiveContextSnapshot | None:
    if not isinstance(payload, dict):
        return None
    ts = payload.get("ts")
    if not isinstance(ts, str):
        return None
    return ActiveContextSnapshot(
        source_file=str(payload.get("sourceFile") or ""),
        ts=ts,
        input_tokens=_int_payload_value(payload.get("inputTokens")),
        cached_input_tokens=_int_payload_value(payload.get("cachedInputTokens")),
        output_tokens=_int_payload_value(payload.get("outputTokens")),
        reasoning_output_tokens=_int_payload_value(
            payload.get("reasoningOutputTokens")
        ),
        total_tokens=_int_payload_value(payload.get("totalTokens")),
        model_context_window=_optional_int_payload_value(
            payload.get("modelContextWindow")
        ),
        cumulative_total_tokens=_int_payload_value(
            payload.get("cumulativeTotalTokens")
        ),
    )


def _int_payload_value(value: Any) -> int:
    return int(value) if isinstance(value, int) else 0


def _optional_int_payload_value(value: Any) -> int | None:
    return value if isinstance(value, int) else None


def context_meter_instruction(level: str) -> str:
    return "Keep working. Continue the claimed task with normal validation."
