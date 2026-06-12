"""Compaction-bounded recovery slices for session transcripts."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Sequence

from spice.sessions.records import CompactionRecord, TurnRecord


@dataclass(slots=True)
class SliceRecord:
    slice_id: str
    source_file: str
    start_ts: str
    end_ts: str
    basis: str
    anchor_kind: str
    turn_ids: list[str] = field(default_factory=list)
    compaction_count: int = 0
    status: str = "closed"
    ordered_messages: list[tuple[str, str]] = field(default_factory=list)
    crossing_turn_files: list[str] = field(default_factory=list)
    patch_count: int = 0


def build_compaction_slices(
    turns: Sequence[TurnRecord], compactions: Sequence[CompactionRecord]
) -> list[SliceRecord]:
    """Return chronological slices bounded by compaction events."""
    if not compactions:
        exact = build_exact_slice(
            turns,
            compactions,
            start_ts=None,
            end_ts=None,
            basis="full_history_no_compactions",
        )
        return [exact] if exact else []
    records: list[SliceRecord] = []
    for index, compaction in enumerate(compactions):
        start_ts = (
            compactions[index - 1].ts
            if index
            else _first_activity_ts(turns, compactions)
        )
        if start_ts is None:
            continue
        records.append(
            make_slice_record(
                slice_id=f"compaction-{index + 1}",
                source_file=_source_file(turns, compactions),
                start_ts=start_ts,
                end_ts=compaction.ts,
                basis="compaction_interval",
                anchor_kind="compaction",
                turns=turns_overlapping(turns, start_ts, compaction.ts),
                compactions=[compaction],
            )
        )
    return records


def build_exact_slice(
    turns: Sequence[TurnRecord],
    compactions: Sequence[CompactionRecord],
    *,
    start_ts: str | None,
    end_ts: str | None,
    basis: str,
) -> SliceRecord | None:
    if not turns and not compactions:
        return None
    actual_start = start_ts or _first_activity_ts(turns, compactions)
    actual_end = end_ts or _last_activity_ts(turns, compactions)
    if actual_start is None or actual_end is None:
        return None
    return make_slice_record(
        slice_id=slice_id_for(actual_start, actual_end),
        source_file=_source_file(turns, compactions),
        start_ts=actual_start,
        end_ts=actual_end,
        basis=basis,
        anchor_kind="explicit_time" if start_ts or end_ts else "full_history",
        turns=turns_overlapping(turns, actual_start, actual_end),
        compactions=compactions_between(compactions, actual_start, actual_end),
    )


def make_slice_record(
    *,
    slice_id: str,
    source_file: str,
    start_ts: str,
    end_ts: str,
    basis: str,
    anchor_kind: str,
    turns: Sequence[TurnRecord],
    compactions: Sequence[CompactionRecord],
) -> SliceRecord:
    return SliceRecord(
        slice_id=slice_id,
        source_file=source_file,
        start_ts=start_ts,
        end_ts=end_ts,
        basis=basis,
        anchor_kind=anchor_kind,
        turn_ids=[turn.turn_id for turn in turns if turn.turn_id],
        compaction_count=len(compactions),
        status="open" if any(not turn.completed for turn in turns) else "closed",
        ordered_messages=_slice_ordered_messages(compactions),
        crossing_turn_files=_crossing_turn_files(turns),
        patch_count=sum(turn.patch_count for turn in turns),
    )


def turns_overlapping(
    turns: Sequence[TurnRecord], start_ts: str, end_ts: str
) -> list[TurnRecord]:
    return [
        turn
        for turn in turns
        if turn_activity_ts(turn) >= start_ts and turn.start_ts <= end_ts
    ]


def compactions_between(
    compactions: Sequence[CompactionRecord], start_ts: str, end_ts: str
) -> list[CompactionRecord]:
    return [record for record in compactions if start_ts <= record.ts <= end_ts]


def slice_id_for(start_ts: str, end_ts: str) -> str:
    return f"{_compact_ts(start_ts)}-{_compact_ts(end_ts)}"


def turn_activity_ts(turn: TurnRecord) -> str:
    return turn.end_ts or turn.last_activity_ts or turn.start_ts


def _slice_ordered_messages(
    compactions: Sequence[CompactionRecord],
) -> list[tuple[str, str]]:
    if not compactions:
        return []
    latest = compactions[-1]
    messages: list[tuple[str, str]] = []
    if latest.last_assistant_before_text:
        messages.append(("assistant_before", latest.last_assistant_before_text))
    if latest.first_user_after_text:
        messages.append(("user_after", latest.first_user_after_text))
    return messages


def _crossing_turn_files(turns: Sequence[TurnRecord]) -> list[str]:
    counts: dict[str, int] = {}
    for turn in turns:
        for path, count in turn.touched_files.items():
            counts[path] = counts.get(path, 0) + count
    ranked = sorted(counts, key=lambda path: (-counts[path], Path(path).name))
    return [Path(path).name for path in ranked[:8]]


def _first_activity_ts(
    turns: Sequence[TurnRecord], compactions: Sequence[CompactionRecord]
) -> str | None:
    candidates = [turn.start_ts for turn in turns] + [
        record.ts for record in compactions
    ]
    return min(candidates) if candidates else None


def _last_activity_ts(
    turns: Sequence[TurnRecord], compactions: Sequence[CompactionRecord]
) -> str | None:
    candidates = [turn_activity_ts(turn) for turn in turns] + [
        record.ts for record in compactions
    ]
    return max(candidates) if candidates else None


def _source_file(
    turns: Sequence[TurnRecord], compactions: Sequence[CompactionRecord]
) -> str:
    if turns:
        return turns[-1].source_file
    if compactions:
        return compactions[-1].source_file
    return "-"


def _compact_ts(value: str) -> str:
    return (
        value.replace("-", "")
        .replace(":", "")
        .replace(".", "")
        .replace("+", "")
        .replace("Z", "z")
    )
