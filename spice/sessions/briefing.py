"""The briefing: rehydrate an agent (or its successor) from the transcript.

The `spice session briefing` output. It answers, mechanically, the questions
a freshly compacted or freshly renewed agent must not guess at: what was
asked, what was last delivered, what to keep doing, what the working set was,
and what steering is pending.
"""

from __future__ import annotations

import subprocess
import tempfile
import time
from collections import Counter
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any, Callable, Literal, TypeAlias, TypedDict

from spice.errors import SpiceError
from spice.mail.ackstate import AckStateRecord, ack_state_records
from spice.mail.inbox import (
    INBOX_RESPONSE_ROW,
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    collect_refused_inbox_items,
    format_relative_seconds,
    inbox_ack_state_context_rows,
    inbox_deadletter_context_rows,
    inbox_item_key,
    parse_inbox_payload,
    relative_time_for_path,
)
from spice.paths import repo_root_from_cwd
from spice.policy import (
    MAGIC_BASELINE_REF,
)
from spice.policyconfig import ComplexityPolicy, resolve_policy
from spice.sessions import learnings as session_learnings
from spice.sessions import records
from spice.sessions.meter import (
    ContextMeter,
    GuidanceState,
    collect_context_meter,
    context_meter_instruction,
    meter_pressure_level,
)
from spice.sessions.util import parse_iso_ts
from spice.sessions.records import (
    CommitRecord,
    CompactionRecord,
    TurnRecord,
    collect_commit_records,
    collect_compactions,
    collect_turns,
)
from spice.studies import complexity, fileloc, magicnums, repodocs, shape
from spice.studies.walk import is_excluded_path

DEFAULT_RECENT_ASKS = 6
DEFAULT_RECENT_FINALS = 3
PREVIEW_CHARS = 200
RECENT_COMMITS_LIMIT = 5
COMMIT_PREVIEW_CHARS = 120
SWEEP_WINDOW_ASKS = 3
WORKING_SET_LIMIT = 10
DEFAULT_BRIEFING_MAX_LINES = 120
DEFAULT_BRIEFING_MAX_BYTES = 20_000
DIRTY_PRESSURE_PREVIEW_LIMIT = 6
TASK_PLANE_ROW_LIMIT = 8
TASK_PLANE_PREVIEW_CHARS = 180
DEFAULT_HORIZON_COMPACTIONS = 3
MAX_HORIZON_COMPACTIONS = 5
DEFAULT_HORIZON_MIN_SECONDS = 4 * 60 * 60
HORIZON_END_SENTINEL = "￿"

RehydrationCandidateKind: TypeAlias = Literal[
    "ask",
    "final",
    "commit",
    "command",
    "file",
    "compaction_intent",
    "task_plane",
]
RankKey: TypeAlias = tuple[int | float | str, ...]

ASK_DISPOSITION_RANK: dict[str, int] = {
    "pending": 30,
    "open": 30,
    "refused": 20,
    "nack": 20,
    "acked": 10,
    "acknowledged": 10,
    "responded": 10,
    "human": 10,
    "": 0,
}
ASK_RANK_NAME = "ask_disposition_then_recency"
FILE_RANK_NAME = "file_last_touch_then_hotspot"
COMMAND_RANK_NAME = "command_failures_then_recency"
RECENCY_RANK_NAME = "recency"
TASK_PLANE_RANK_NAME = "task_plane_state_then_urgency_recency"
TASK_PLANE_WEIGHTS = {
    "claim": 60,
    "posture": 55,
    "ready": 50,
    "review": 45,
    "completed": 30,
    "oops": 20,
}


@dataclass(frozen=True)
class RehydrationCandidate:
    kind: RehydrationCandidateKind
    timestamp: str
    text: str
    rank_name: str
    rank_key: RankKey
    label: str = ""
    count: int = 0
    key: str = ""


@dataclass(frozen=True)
class DirtyComplexityRegression:
    path: str
    function_name: str
    metric: str
    value: int
    active_threshold: int
    baseline_value: int | None


@dataclass(frozen=True)
class ResolvedHorizon:
    start: str | None
    basis: str
    requested_compactions: int
    selected_boundaries: tuple[str, ...]

    @property
    def selected_compactions(self) -> int:
        return len(self.selected_boundaries)


class DirtyWorktreePressure(TypedDict, total=False):
    available: bool
    dirtyPathCount: int
    scannedPathCount: int
    fileCountWithPressure: int
    totalFindings: int
    fileLocFindingCount: int
    complexityRegressionCount: int
    magicRegressionCount: int
    severity: str
    summary: list[str]
    summaryOverflow: int
    errors: list[str]
    oldestDirtyAgeSeconds: int
    oldestDirtyPath: str
    newestDirtyAgeSeconds: int
    newestDirtyPath: str


def clip(text: str | None, limit: int = PREVIEW_CHARS) -> str:
    if not text:
        return "-"
    flat = " ".join(text.split())
    if len(flat) <= limit:
        return flat
    return flat[: limit - 1].rstrip() + "…"


def sort_rehydration_candidates(
    candidates: list[RehydrationCandidate],
) -> list[RehydrationCandidate]:
    return sorted(candidates, key=lambda candidate: candidate.rank_key, reverse=True)


def ask_rank_key(timestamp: str, disposition: str) -> RankKey:
    return (ASK_DISPOSITION_RANK.get(disposition.lower(), 0), timestamp)


def recency_rank_key(timestamp: str) -> RankKey:
    return (timestamp,)


def file_touch_rank_key(timestamp: str, touch_count: int) -> RankKey:
    return (timestamp, touch_count)


def command_rank_key(timestamp: str, error_count: int) -> RankKey:
    return (error_count, timestamp)


def task_plane_rank_key(
    kind: str, urgency: float = 0.0, timestamp: str = ""
) -> RankKey:
    return (TASK_PLANE_WEIGHTS[kind], urgency, timestamp)


def ask_candidate(
    timestamp: str, text: str, *, disposition: str = "human", key: str = ""
) -> RehydrationCandidate:
    return RehydrationCandidate(
        kind="ask",
        timestamp=timestamp,
        text=text,
        rank_name=ASK_RANK_NAME,
        rank_key=ask_rank_key(timestamp, disposition),
        label=disposition,
        key=key,
    )


def collect_ask_candidates(
    *,
    start: str | None = None,
    end: str | None = None,
    contains: str | None = None,
) -> list[RehydrationCandidate]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return []
    candidates = [
        *(_pending_ask_candidate(item) for item in collect_inbox_items(str(repo_root))),
        *(_ack_state_ask_candidate(record) for record in ack_state_records(repo_root)),
    ]
    return [
        candidate
        for candidate in candidates
        if _ask_candidate_matches_filters(
            candidate, start=start, end=end, contains=contains
        )
    ]


def _pending_ask_candidate(item) -> RehydrationCandidate:
    key = inbox_item_key(item.name)
    return ask_candidate(
        _ask_timestamp_from_key(key),
        parse_inbox_payload(item.text).body,
        disposition="pending",
        key=key,
    )


def _ack_state_ask_candidate(record: AckStateRecord) -> RehydrationCandidate:
    return ask_candidate(
        _ask_timestamp_from_key(record.key),
        parse_inbox_payload(record.text).body,
        disposition=record.disposition,
        key=record.key,
    )


def _ask_candidate_matches_filters(
    candidate: RehydrationCandidate,
    *,
    start: str | None,
    end: str | None,
    contains: str | None,
) -> bool:
    if start and candidate.timestamp < start:
        return False
    if end and candidate.timestamp > end:
        return False
    needle = (contains or "").lower()
    haystack = "\n".join([candidate.text, candidate.key]).lower()
    return not needle or needle in haystack


def _ask_timestamp_from_key(key: str) -> str:
    raw = key[:-1] if key.endswith("Z") else key
    try:
        parsed = datetime.strptime(raw, "%Y%m%dT%H%M%S%f").replace(tzinfo=UTC)
    except ValueError as exc:
        raise SpiceError(f"invalid ACK steering key timestamp: {key or '-'}") from exc
    return parsed.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def collect_final_candidates(turns: list[TurnRecord]) -> list[RehydrationCandidate]:
    return [
        RehydrationCandidate(
            kind="final",
            timestamp=turn.start_ts,
            text=text,
            rank_name=RECENCY_RANK_NAME,
            rank_key=recency_rank_key(turn.start_ts),
        )
        for turn in turns
        for text in turn.final_answers
    ]


def collect_commit_candidates(
    commits: list[CommitRecord],
) -> list[RehydrationCandidate]:
    return [
        RehydrationCandidate(
            kind="commit",
            timestamp=record.start_ts,
            text=record.line,
            rank_name=RECENCY_RANK_NAME,
            rank_key=recency_rank_key(record.start_ts),
            label=record.sha,
        )
        for record in commits
    ]


def collect_command_candidates(turns: list[TurnRecord]) -> list[RehydrationCandidate]:
    return [
        RehydrationCandidate(
            kind="command",
            timestamp=turn.start_ts,
            text=f"commands={turn.command_count} errors={turn.error_count}",
            rank_name=COMMAND_RANK_NAME,
            rank_key=command_rank_key(turn.start_ts, turn.error_count),
            label=turn.turn_id or turn.source_file,
            count=turn.command_count,
        )
        for turn in turns
        if turn.command_count
    ]


def collect_file_touch_candidates(
    turns: list[TurnRecord],
) -> list[RehydrationCandidate]:
    counts: Counter[str] = Counter()
    last_touch: dict[str, str] = {}
    for turn in turns:
        timestamp = turn.last_activity_ts or turn.end_ts or turn.start_ts
        for path, count in turn.touched_files.items():
            counts[path] += count
            last_touch[path] = max(last_touch.get(path, ""), timestamp)
    return [
        RehydrationCandidate(
            kind="file",
            timestamp=last_touch[path],
            text=path,
            rank_name=FILE_RANK_NAME,
            rank_key=file_touch_rank_key(last_touch[path], counts[path]),
            label=path,
            count=counts[path],
        )
        for path in counts
    ]


def collect_compaction_intent_candidates(
    compactions: list[CompactionRecord],
) -> list[RehydrationCandidate]:
    return [
        RehydrationCandidate(
            kind="compaction_intent",
            timestamp=record.ts,
            text=record.first_user_after_text
            or record.summary_after_text
            or record.last_assistant_before_text
            or "",
            rank_name=RECENCY_RANK_NAME,
            rank_key=recency_rank_key(record.ts),
            label=clip(record.last_assistant_before_text),
        )
        for record in compactions
    ]


def collect_task_plane_candidates() -> list[RehydrationCandidate]:
    if repo_root_from_cwd() is None:
        return []
    try:
        from spice.tasks import alloc, identity, tw

        actor = tw.current_actor()
        active = alloc.visible_active_rows(actor)
        ready = [
            row
            for row in alloc.visible_ready_rows(actor)
            if _task_field(row, "phase") != "review"
        ]
        review = [
            row
            for row in alloc.visible_rows(actor, ["status:pending", "phase:review"])
            if not alloc.is_hidden(row) and not str(row.get("claim_by") or "")
        ]
        blocked = [
            row
            for row in alloc.visible_rows(actor, ["status:pending", "+BLOCKED"])
            if not alloc.is_hidden(row)
        ]
        completed = [
            row
            for row in alloc.visible_rows(actor, ["status:completed"])
            if not alloc.is_hidden(row)
        ]
        oops = alloc.oops_rows()
    except (OSError, RuntimeError, SpiceError, SystemExit):
        return []

    candidates: list[RehydrationCandidate] = []
    own_active = [row for row in active if str(row.get("claim_by") or "") == actor]
    if own_active:
        claimed = max(own_active, key=_task_row_timestamp)
        candidates.append(
            _task_claim_candidate(claimed, identity.render_handle(claimed))
        )
    if active or ready or review or blocked or oops:
        candidates.append(
            _task_posture_candidate(
                active=len(active),
                ready=len(ready),
                review=len(review),
                blocked=len(blocked),
                oops=len(oops),
            )
        )
    candidates.extend(
        _task_queue_candidate("ready", row, identity.render_handle(row))
        for row in ready
    )
    candidates.extend(
        _task_queue_candidate("review", row, identity.render_handle(row))
        for row in review
    )
    candidates.extend(
        _task_completed_candidate(row, identity.render_handle(row)) for row in completed
    )
    if oops:
        top = max(oops, key=_task_urgency)
        candidates.append(
            _task_oops_candidate(top, identity.render_handle(top), len(oops))
        )
    return candidates


def _task_claim_candidate(row: dict[str, object], handle: str) -> RehydrationCandidate:
    timestamp = _task_row_timestamp(row)
    return RehydrationCandidate(
        kind="task_plane",
        timestamp=timestamp,
        text=(
            f"claim {handle} phase={_task_field(row, 'phase') or '-'} "
            f"project={_task_field(row, 'project') or '-'} "
            f"acceptance={clip(_task_field(row, 'acceptance'), TASK_PLANE_PREVIEW_CHARS)}"
        ),
        rank_name=TASK_PLANE_RANK_NAME,
        rank_key=task_plane_rank_key("claim", _task_urgency(row), timestamp),
        label=handle,
    )


def _task_posture_candidate(
    *, active: int, ready: int, review: int, blocked: int, oops: int
) -> RehydrationCandidate:
    return RehydrationCandidate(
        kind="task_plane",
        timestamp=tw_nowish_rank_timestamp(),
        text=(
            f"posture active={active} ready={ready} review={review} "
            f"blocked={blocked} oops={oops}"
        ),
        rank_name=TASK_PLANE_RANK_NAME,
        rank_key=task_plane_rank_key("posture"),
        label="posture",
    )


def _task_queue_candidate(
    state: Literal["ready", "review"], row: dict[str, object], handle: str
) -> RehydrationCandidate:
    timestamp = _task_row_timestamp(row)
    urgency = _task_urgency(row)
    return RehydrationCandidate(
        kind="task_plane",
        timestamp=timestamp,
        text=(
            f"{state} {handle} urgency={urgency:.2f} "
            f"{clip(_task_field(row, 'description'), TASK_PLANE_PREVIEW_CHARS)}"
        ),
        rank_name=TASK_PLANE_RANK_NAME,
        rank_key=task_plane_rank_key(state, urgency, timestamp),
        label=handle,
    )


def _task_completed_candidate(
    row: dict[str, object], handle: str
) -> RehydrationCandidate:
    timestamp = _task_row_timestamp(row)
    return RehydrationCandidate(
        kind="task_plane",
        timestamp=timestamp,
        text=(
            f"completed {handle} validation="
            f"{clip(_task_field(row, 'validation'), TASK_PLANE_PREVIEW_CHARS)}"
        ),
        rank_name=TASK_PLANE_RANK_NAME,
        rank_key=task_plane_rank_key("completed", timestamp=timestamp),
        label=handle,
    )


def _task_oops_candidate(
    row: dict[str, object], handle: str, total: int
) -> RehydrationCandidate:
    timestamp = _task_row_timestamp(row)
    overflow = f" total={total}" if total > 1 else ""
    return RehydrationCandidate(
        kind="task_plane",
        timestamp=timestamp,
        text=(
            f"oops {handle}{overflow} "
            f"{clip(_task_field(row, 'description'), TASK_PLANE_PREVIEW_CHARS)}"
        ),
        rank_name=TASK_PLANE_RANK_NAME,
        rank_key=task_plane_rank_key("oops", _task_urgency(row), timestamp),
        label=handle,
    )


def _task_field(row: dict[str, object], key: str) -> str:
    value = row.get(key)
    return "" if value is None else str(value)


def _task_row_timestamp(row: dict[str, object]) -> str:
    for key in ("claim_at", "end", "modified", "entry", "incepted"):
        value = _task_field(row, key)
        if value:
            return value
    return ""


def tw_nowish_rank_timestamp() -> str:
    try:
        from spice.tasks import tw

        return tw.now_iso()
    except (OSError, RuntimeError, SpiceError, SystemExit):
        return ""


def _task_urgency(row: dict[str, object]) -> float:
    value = row.get("urgency")
    if not isinstance(value, int | float | str):
        return 0.0
    try:
        return float(value or 0.0)
    except ValueError:
        return 0.0


def render_briefing(
    files: list[Path],
    *,
    start: str | None = None,
    end: str | None = None,
    contains: str | None = None,
    turn_ids: list[str] | None = None,
    tools: list[str] | None = None,
    max_lines: int | None = DEFAULT_BRIEFING_MAX_LINES,
    max_bytes: int | None = DEFAULT_BRIEFING_MAX_BYTES,
    explain_pruning: bool = False,
) -> str:
    all_turns = collect_turns(files)
    all_compactions = collect_compactions(files)
    horizon = _resolve_horizon(
        all_turns,
        all_compactions,
        count=DEFAULT_HORIZON_COMPACTIONS,
        end=end,
    )
    effective_start = _effective_start(start, horizon.start)
    turns = records.filter_turns(
        all_turns,
        start=effective_start,
        end=end,
        contains=contains,
        turn_ids=turn_ids,
        tools=tools,
    )
    compactions = _filter_compactions(
        all_compactions, start=effective_start, end=end, contains=contains
    )
    meter = collect_context_meter(files)
    commits = collect_commit_records(turns)
    asks = collect_ask_candidates(start=effective_start, end=end, contains=contains)
    finals = collect_final_candidates(turns)
    commit_candidates = collect_commit_candidates(commits)
    compaction_intents = collect_compaction_intent_candidates(compactions)
    command_candidates = collect_command_candidates(turns)
    file_candidates = collect_file_touch_candidates(turns)
    task_plane = collect_task_plane_candidates()
    lines: list[str] = []
    lines.extend(_briefing_header_lines(files, turns))
    lines.extend(_horizon_lines(horizon))
    filter_lines = _active_filter_lines(
        start=start, end=end, contains=contains, turn_ids=turn_ids, tools=tools
    )
    if filter_lines:
        lines.append("Filters")
        lines.extend(filter_lines)
    lines.extend(_guidance_lines(meter))
    lines.extend(_learning_lines())
    lines.extend(_task_plane_lines(task_plane))
    lines.extend(_asks_lines(asks))
    lines.extend(_finals_lines(finals))
    lines.extend(_recovery_lines(compaction_intents))
    lines.extend(
        _activity_lines(turns, command_candidates, file_candidates, commit_candidates)
    )
    lines.extend(_git_posture_lines())
    lines.extend(_inbox_lines())
    return apply_output_budget(
        "\n".join(lines),
        max_lines=max_lines,
        max_bytes=max_bytes,
        explain=explain_pruning,
    )


def _briefing_header_lines(files: list[Path], turns: list[TurnRecord]) -> list[str]:
    window_start = turns[0].start_ts if turns else "-"
    window_end = (
        (turns[-1].end_ts or turns[-1].last_activity_ts or turns[-1].start_ts)
        if turns
        else "-"
    )
    return [
        "Briefing",
        f"  files={', '.join(Path(f).name for f in files)} turns={len(turns)} "
        f"window={window_start} -> {window_end}",
    ]


def _horizon_lines(horizon: ResolvedHorizon) -> list[str]:
    start = horizon.start or "session start"
    return [
        "Horizon",
        f"  horizon_basis={horizon.basis} start={start} "
        f"compactions={horizon.selected_compactions}/{horizon.requested_compactions}",
    ]


def _resolve_horizon(
    turns: list[TurnRecord],
    compactions: list[CompactionRecord],
    *,
    count: int,
    end: str | None,
    min_seconds: int = DEFAULT_HORIZON_MIN_SECONDS,
) -> ResolvedHorizon:
    requested = max(0, int(count))
    capped = min(requested, MAX_HORIZON_COMPACTIONS)
    eligible = [record.ts for record in compactions if not end or record.ts <= end]
    cap_excludes_boundaries = (
        requested > MAX_HORIZON_COMPACTIONS and len(eligible) > MAX_HORIZON_COMPACTIONS
    )
    if not eligible or capped == 0:
        basis = "hard_cap" if cap_excludes_boundaries else "compaction_count"
        return ResolvedHorizon(
            start=None,
            basis=basis,
            requested_compactions=requested,
            selected_boundaries=(),
        )

    selected_count = min(capped, len(eligible))
    count_selected = selected_count
    basis = "hard_cap" if cap_excludes_boundaries else "compaction_count"
    floor = _horizon_floor(
        end or _latest_horizon_ts(turns=turns, compactions=compactions),
        min_seconds=min_seconds,
    )
    if floor:
        max_selectable = min(MAX_HORIZON_COMPACTIONS, len(eligible))
        while selected_count < max_selectable and eligible[-selected_count] > floor:
            selected_count += 1
        if basis != "hard_cap":
            start = eligible[-selected_count]
            if (
                start > floor
                and selected_count == len(eligible)
                and selected_count < MAX_HORIZON_COMPACTIONS
            ):
                selected_boundaries = tuple(eligible[-selected_count:])
                return ResolvedHorizon(
                    start=None,
                    basis="wall_clock_floor",
                    requested_compactions=requested,
                    selected_boundaries=selected_boundaries,
                )
            if start > floor and selected_count == MAX_HORIZON_COMPACTIONS:
                basis = "hard_cap"
            elif selected_count > count_selected:
                basis = "wall_clock_floor"

    selected_boundaries = tuple(eligible[-selected_count:])
    return ResolvedHorizon(
        start=selected_boundaries[0] if selected_boundaries else None,
        basis=basis,
        requested_compactions=requested,
        selected_boundaries=selected_boundaries,
    )


def _latest_horizon_ts(
    *, turns: list[TurnRecord], compactions: list[CompactionRecord]
) -> str | None:
    values = [
        value
        for value in [
            *(_turn_activity_ts(turn) for turn in turns),
            *(record.ts for record in compactions),
        ]
        if value
    ]
    return max(values) if values else None


def _turn_activity_ts(turn: TurnRecord) -> str:
    return turn.end_ts or turn.last_activity_ts or turn.start_ts


def _horizon_floor(end: str | None, *, min_seconds: int) -> str | None:
    end_dt = parse_iso_ts(end)
    if end_dt is None:
        return None
    floor_dt = end_dt - timedelta(seconds=max(0, min_seconds))
    return floor_dt.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def _effective_start(user_start: str | None, horizon_start: str | None) -> str | None:
    return user_start or horizon_start


def _guidance_lines(meter: ContextMeter) -> list[str]:
    handle, phase = _active_claim_handle_phase()
    state = GuidanceState(
        level=meter_pressure_level(meter),
        claim_known=True,
        claim_handle=handle,
        claim_phase=phase,
        dirty_path_count=_dirty_path_count(),
    )
    instruction = context_meter_instruction(state)
    if not instruction:
        return []
    return ["Guidance", f"  keep_working={instruction}"]


def _dirty_path_count() -> int:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return 0
    pressure = _build_dirty_worktree_pressure(repo_root=repo_root)
    return int(pressure.get("dirtyPathCount") or 0)


def _learning_lines() -> list[str]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return []
    stem = _active_task_project_stem()
    if stem is None:
        return []
    try:
        records = session_learnings.top_learning_records(repo_root, stem)
    except (OSError, RuntimeError, SpiceError, SystemExit):
        return []
    if not records:
        return []
    lines = ["Learnings", f"  stem={stem}"]
    lines.extend(_learning_record_line(record) for record in records)
    return lines


def _active_claim_row() -> dict[str, Any] | None:
    try:
        from spice.tasks import alloc, tw

        actor = tw.current_actor()
        active = [
            row
            for row in alloc.visible_active_rows(actor)
            if str(row.get("claim_by") or "") == actor
        ]
        return active[0] if active else None
    except (OSError, RuntimeError, SpiceError, SystemExit):
        return None


def _active_task_project_stem() -> str | None:
    row = _active_claim_row()
    if row is None:
        return None
    from spice.tasks import config

    return config.project_stem(str(row.get("project") or "").strip())


def _active_claim_handle_phase() -> tuple[str | None, str | None]:
    row = _active_claim_row()
    if row is None:
        return None, None
    handle = str(row.get("id") or "").strip() or None
    phase = str(row.get("phase") or "").strip() or None
    return handle, phase


def _learning_record_line(record: session_learnings.LearningRecord) -> str:
    source = record.source_task or "-"
    return (
        f"  - {clip(record.statement, COMMIT_PREVIEW_CHARS)} "
        f"(confirmed={record.confirmation_count}, source={source})"
    )


def _task_plane_lines(candidates: list[RehydrationCandidate]) -> list[str]:
    ranked = sort_rehydration_candidates(candidates)
    if not ranked:
        return []
    shown = ranked[:TASK_PLANE_ROW_LIMIT]
    overflow = len(ranked) - len(shown)
    lines = ["Task Plane"]
    lines.extend(
        f"  {clip(candidate.text, TASK_PLANE_PREVIEW_CHARS)}" for candidate in shown
    )
    if overflow:
        lines.append(f"  +{overflow} more task-plane rows")
    return lines


def _asks_lines(asks: list[RehydrationCandidate]) -> list[str]:
    ranked = sort_rehydration_candidates(asks)
    lines = ["Latest Ask", _ask_line(ranked[0]) if ranked else "  -"]
    if len(ranked) > 1:
        lines.append("Recent Asks")
        for candidate in ranked[1 : DEFAULT_RECENT_ASKS + 1]:
            lines.append(_ask_line(candidate))
    return lines


def _ask_line(candidate: RehydrationCandidate) -> str:
    key = f" key={candidate.key}" if candidate.key else ""
    return f"  {candidate.label} {candidate.timestamp}{key} {clip(candidate.text)}"


def _finals_lines(finals: list[RehydrationCandidate]) -> list[str]:
    ranked = sort_rehydration_candidates(finals)
    lines = ["Latest Final", f"  {clip(ranked[0].text) if ranked else '-'}"]
    if len(ranked) > 1:
        lines.append("Recent Finals")
        for candidate in ranked[1 : DEFAULT_RECENT_FINALS + 1]:
            lines.append(f"  {candidate.timestamp} {clip(candidate.text)}")
    return lines


def _recovery_lines(compactions: list[RehydrationCandidate]) -> list[str]:
    ranked = sort_rehydration_candidates(compactions)
    if not ranked:
        return []
    latest = ranked[0]
    return [
        "Recovery",
        f"  latest_compaction={latest.timestamp}",
        f"  assistant_before={latest.label}",
        f"  user_after={clip(latest.text)}",
    ]


def _activity_lines(
    turns: list[TurnRecord],
    command_candidates: list[RehydrationCandidate],
    file_candidates: list[RehydrationCandidate],
    commit_candidates: list[RehydrationCandidate],
) -> list[str]:
    lines = [
        "Activity",
        "  commands={c} patches={p} errors={e} web_searches={w}".format(
            c=sum(candidate.count for candidate in command_candidates),
            p=sum(turn.patch_count for turn in turns),
            e=sum(turn.error_count for turn in turns),
            w=sum(turn.web_search_count for turn in turns),
        ),
    ]
    working_set = sort_rehydration_candidates(file_candidates)
    if working_set:
        lines.append("Working Set")
        for candidate in working_set[:WORKING_SET_LIMIT]:
            lines.append(f"  {candidate.label} touches={candidate.count}")
    ranked_commits = sort_rehydration_candidates(commit_candidates)
    if ranked_commits:
        lines.append("Recent Commits")
        for candidate in ranked_commits[:RECENT_COMMITS_LIMIT]:
            lines.append(
                f"  {candidate.timestamp} {candidate.label} "
                f"{clip(candidate.text, COMMIT_PREVIEW_CHARS)}"
            )
    return lines


def active_file_order(turns: list[TurnRecord]) -> list[tuple[str, int]]:
    """The current working set: most-recently-touched first, count attached.

    Recency outranks raw frequency — the file an agent touched last is the
    file it was working on, however many times an older file was edited.
    """
    return [
        (candidate.label, candidate.count)
        for candidate in sort_rehydration_candidates(
            collect_file_touch_candidates(turns)
        )
    ]


def _active_filter_lines(
    *,
    start: str | None,
    end: str | None,
    contains: str | None,
    turn_ids: list[str] | None,
    tools: list[str] | None,
) -> list[str]:
    rows: list[str] = []
    if start:
        rows.append(f"  start={start}")
    if end:
        rows.append(f"  end={end}")
    if contains:
        rows.append(f"  contains={contains}")
    if turn_ids:
        rows.append(f"  turn_ids={', '.join(turn_ids)}")
    if tools:
        rows.append(f"  tools={', '.join(tools)}")
    return rows


def _filter_compactions(
    compactions: list[records.CompactionRecord],
    *,
    start: str | None,
    end: str | None,
    contains: str | None,
) -> list[records.CompactionRecord]:
    needle = (contains or "").lower()
    kept: list[records.CompactionRecord] = []
    for record in compactions:
        if start and record.ts < start:
            continue
        if end and record.ts > end:
            continue
        if needle:
            haystack = "\n".join(
                [
                    record.last_assistant_before_text or "",
                    record.summary_after_text or "",
                    record.first_user_after_text or "",
                ]
            ).lower()
            if needle not in haystack:
                continue
        kept.append(record)
    return kept


def _git_posture_lines() -> list[str]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return ["Git", "  repo=-"]
    branch = _git_read(repo_root, "branch", "--show-current") or "-"
    upstream = _git_read(
        repo_root, "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"
    )
    ahead = behind = "0"
    if upstream:
        delta = _git_read(
            repo_root, "rev-list", "--left-right", "--count", "HEAD...@{u}"
        )
        parts = delta.split()
        if len(parts) == 2:
            ahead, behind = parts
    else:
        upstream = "-"
        ahead = behind = "-"
    dirty_pressure = _build_dirty_worktree_pressure(repo_root=repo_root)
    dirty_count = int(dirty_pressure.get("dirtyPathCount") or 0)
    dirty_text = "clean" if dirty_count == 0 else f"{dirty_count} path(s)"
    lines = [
        "Git",
        f"  branch={branch} upstream={upstream} ahead={ahead} behind={behind}",
        f"  dirty={dirty_text}",
    ]
    if dirty_count:
        lines.extend(_dirty_pressure_lines(dirty_pressure))
    return lines


def _empty_dirty_worktree_pressure() -> DirtyWorktreePressure:
    return {
        "available": True,
        "dirtyPathCount": 0,
        "scannedPathCount": 0,
        "fileCountWithPressure": 0,
        "totalFindings": 0,
        "fileLocFindingCount": 0,
        "complexityRegressionCount": 0,
        "magicRegressionCount": 0,
        "severity": "none",
        "summary": [],
        "summaryOverflow": 0,
        "errors": [],
    }


def _build_dirty_worktree_pressure(*, repo_root: Path) -> DirtyWorktreePressure:
    dirty = _dirty_paths(repo_root)
    if not dirty:
        return _empty_dirty_worktree_pressure()
    relevant_paths = [
        path
        for path in dirty
        if not is_excluded_path(path, repo_root=repo_root)
        and (repo_root / path).exists()
    ]
    file_loc_findings, complexity_regressions, magic_regressions, errors = (
        _collect_dirty_pressure_findings(relevant_paths, repo_root=repo_root)
    )
    per_file_rules, ordered_summary = _dirty_pressure_summary(
        file_loc_findings,
        complexity_regressions,
        magic_regressions,
    )
    total_findings = (
        len(file_loc_findings) + len(complexity_regressions) + len(magic_regressions)
    )
    return {
        "available": True,
        "dirtyPathCount": len(dirty),
        "scannedPathCount": len(relevant_paths),
        "fileCountWithPressure": len(per_file_rules),
        "totalFindings": total_findings,
        "fileLocFindingCount": len(file_loc_findings),
        "complexityRegressionCount": len(complexity_regressions),
        "magicRegressionCount": len(magic_regressions),
        "severity": _dirty_pressure_severity(
            file_loc_findings=file_loc_findings,
            complexity_regressions=complexity_regressions,
            magic_regressions=magic_regressions,
            errors=errors,
        ),
        "summary": ordered_summary[:DIRTY_PRESSURE_PREVIEW_LIMIT],
        "summaryOverflow": max(0, len(ordered_summary) - DIRTY_PRESSURE_PREVIEW_LIMIT),
        "errors": errors,
        **_dirty_path_ages(dirty, repo_root=repo_root),
    }


def _dirty_paths(repo_root: Path) -> list[Path]:
    raw_paths: set[Path] = set()
    command_specs = (
        ("diff", "--name-only", "-z", "--diff-filter=ACMRD"),
        ("diff", "--cached", "--name-only", "-z", "--diff-filter=ACMRD"),
        ("ls-files", "--others", "--exclude-standard", "-z"),
    )
    for args in command_specs:
        for raw_path in _git_read_z(repo_root, *args):
            candidate = Path(raw_path)
            if candidate.parts:
                raw_paths.add(candidate)
    return sorted(raw_paths)


def _collect_dirty_pressure_findings(
    relevant_paths: list[Path], *, repo_root: Path
) -> tuple[
    list[fileloc.LocFinding],
    list[DirtyComplexityRegression],
    list[magicnums.MagicFinding],
    list[str],
]:
    errors: list[str] = []
    file_loc_findings: list[fileloc.LocFinding] = []
    complexity_regressions: list[DirtyComplexityRegression] = []
    magic_regressions: list[magicnums.MagicFinding] = []
    resolved = resolve_policy(repo_root)
    file_shape = resolved.file_shape
    complexity_bounds = resolved.complexity
    generated_patterns = (
        *resolved.file_shape_paths.generated_patterns,
        *shape.generated_path_patterns(repo_root),
    )

    try:
        file_loc_findings = fileloc.scan_loc_violations(
            relevant_paths,
            limit=file_shape.line_limit,
            flex_limit_value=file_shape.line_flex_limit,
            byte_limit=file_shape.byte_limit,
            byte_flex_limit_value=file_shape.byte_flex_limit,
            root=repo_root,
            source_suffixes=resolved.file_shape_paths.source_suffixes,
            generated_patterns=generated_patterns,
            repo_doc_paths=set(
                repodocs.repo_truth_doc_candidate_paths(repo_root, resolved)
            ),
            lockfile_suffixes=resolved.lockfiles.suffixes,
            lockfile_names=resolved.lockfiles.names,
            bounds_for_path=resolved.jittered_file_shape_for_path,
        )
    except (OSError, SpiceError) as exc:
        errors.append(_dirty_pressure_error("file-loc", exc))

    try:
        complexity_regressions = _scan_dirty_complexity_pressure(
            relevant_paths,
            repo_root=repo_root,
            suffixes=resolved.languages.complexity,
            ccn_threshold=complexity_bounds.ccn_flex_limit,
            length_threshold=complexity_bounds.length_flex_limit,
            bounds_for_path=resolved.jittered_complexity_for_path,
        )
    except (OSError, SpiceError) as exc:
        errors.append(_dirty_pressure_error("complexity", exc))

    try:
        magic_regressions = magicnums.detect_magic_regressions(
            relevant_paths,
            root=repo_root,
            baseline_ref=resolved.magic.baseline_ref,
            examine_threshold=resolved.magic.examine_threshold,
            examine_threshold_for_path=resolved.magic_examine_threshold_for_path,
            suffixes=resolved.languages.magic,
            c_grammar_suffixes=resolved.languages.c_grammar,
        )
    except (OSError, SpiceError) as exc:
        errors.append(_dirty_pressure_error("magic-numbers", exc))

    return file_loc_findings, complexity_regressions, magic_regressions, errors


def _scan_dirty_complexity_pressure(
    paths: list[Path],
    *,
    repo_root: Path,
    suffixes: tuple[str, ...],
    ccn_threshold: int,
    length_threshold: int,
    bounds_for_path: Callable[[Path], ComplexityPolicy] | None = None,
) -> list[DirtyComplexityRegression]:
    current_paths = [path for path in paths if (repo_root / path).exists()]
    if not current_paths:
        return []
    current_records = complexity.collect_complexity_records(
        current_paths, root=repo_root, suffixes=suffixes
    )
    with tempfile.TemporaryDirectory(prefix="spice-complexity-baseline-") as temp_dir:
        temp_root = Path(temp_dir)
        baseline_paths = _materialize_complexity_baseline_paths(
            current_paths,
            repo_root=repo_root,
            temp_root=temp_root,
        )
        baseline_records = complexity.collect_complexity_records(
            baseline_paths,
            root=temp_root,
            suffixes=suffixes,
        )
    return _detect_dirty_complexity_regressions(
        current_records,
        baseline_records,
        ccn_threshold=ccn_threshold,
        length_threshold=length_threshold,
        bounds_for_path=bounds_for_path,
    )


def _materialize_complexity_baseline_paths(
    paths: list[Path], *, repo_root: Path, temp_root: Path
) -> list[Path]:
    materialized: list[Path] = []
    for path in paths:
        result = subprocess.run(
            [
                "git",
                "-C",
                str(repo_root),
                "show",
                f"{MAGIC_BASELINE_REF}:{path.as_posix()}",
            ],
            capture_output=True,
            check=False,
        )
        if result.returncode != 0:
            continue
        target = temp_root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(result.stdout)
        materialized.append(path)
    return materialized


def _detect_dirty_complexity_regressions(
    current_records: list[complexity.ComplexityRecord],
    baseline_records: list[complexity.ComplexityRecord],
    *,
    ccn_threshold: int,
    length_threshold: int,
    bounds_for_path: Callable[[Path], ComplexityPolicy] | None = None,
) -> list[DirtyComplexityRegression]:
    baseline_index = _complexity_record_index(baseline_records)
    regressions: list[DirtyComplexityRegression] = []
    for record in sorted(
        current_records,
        key=lambda item: (item.path, item.function_name),
    ):
        baseline = baseline_index.get(record.key)
        bounds = bounds_for_path(Path(record.path)) if bounds_for_path else None
        active_ccn_threshold = (
            bounds.ccn_flex_limit if bounds is not None else ccn_threshold
        )
        active_length_threshold = (
            bounds.length_flex_limit if bounds is not None else length_threshold
        )
        ccn_unlimited = bounds.ccn_unlimited if bounds is not None else False
        length_unlimited = bounds.length_unlimited if bounds is not None else False
        if (
            not ccn_unlimited
            and record.ccn > active_ccn_threshold
            and (baseline is None or record.ccn > baseline.ccn)
        ):
            regressions.append(
                DirtyComplexityRegression(
                    path=record.path,
                    function_name=record.function_name,
                    metric="ccn",
                    value=record.ccn,
                    active_threshold=active_ccn_threshold,
                    baseline_value=baseline.ccn if baseline is not None else None,
                )
            )
        if not length_unlimited and record.length > active_length_threshold:
            regressions.append(
                DirtyComplexityRegression(
                    path=record.path,
                    function_name=record.function_name,
                    metric="length",
                    value=record.length,
                    active_threshold=active_length_threshold,
                    baseline_value=baseline.length if baseline is not None else None,
                )
            )
    return regressions


def _complexity_record_index(
    records: list[complexity.ComplexityRecord],
) -> dict[tuple[str, str], complexity.ComplexityRecord]:
    index: dict[tuple[str, str], complexity.ComplexityRecord] = {}
    for record in records:
        incumbent = index.get(record.key)
        if incumbent is None or (record.ccn, record.length) > (
            incumbent.ccn,
            incumbent.length,
        ):
            index[record.key] = record
    return index


def _dirty_pressure_summary(
    file_loc_findings: list[fileloc.LocFinding],
    complexity_regressions: list[DirtyComplexityRegression],
    magic_regressions: list[magicnums.MagicFinding],
) -> tuple[dict[str, set[str]], list[str]]:
    per_file_rules, file_loc_index, complexity_index, magic_index = (
        _index_dirty_pressure_rules(
            file_loc_findings,
            complexity_regressions,
            magic_regressions,
        )
    )
    ordered_summary = [
        f"{path} [{' ,'.join(sorted(labels))}]".replace(" ,", ",")
        for path, labels in sorted(
            per_file_rules.items(),
            key=lambda item: _dirty_pressure_severity_key(
                item[0],
                item[1],
                file_loc_index=file_loc_index,
                complexity_index=complexity_index,
                magic_index=magic_index,
            ),
        )
    ]
    return per_file_rules, ordered_summary


def _index_dirty_pressure_rules(
    file_loc_findings: list[fileloc.LocFinding],
    complexity_regressions: list[DirtyComplexityRegression],
    magic_regressions: list[magicnums.MagicFinding],
) -> tuple[
    dict[str, set[str]],
    dict[str, list[fileloc.LocFinding]],
    dict[str, list[DirtyComplexityRegression]],
    dict[str, list[magicnums.MagicFinding]],
]:
    per_file_rules: dict[str, set[str]] = {}
    file_loc_index: dict[str, list[fileloc.LocFinding]] = {}
    complexity_index: dict[str, list[DirtyComplexityRegression]] = {}
    magic_index: dict[str, list[magicnums.MagicFinding]] = {}

    def mark(path: str, label: str) -> None:
        per_file_rules.setdefault(path, set()).add(label)

    for finding in file_loc_findings:
        file_loc_index.setdefault(finding.path, []).append(finding)
        mark(finding.path, "file-loc")
    for regression in complexity_regressions:
        complexity_index.setdefault(regression.path, []).append(regression)
        mark(regression.path, f"complexity-{regression.metric}")
    for finding in magic_regressions:
        magic_index.setdefault(finding.path, []).append(finding)
        mark(finding.path, "magic")
    return per_file_rules, file_loc_index, complexity_index, magic_index


def _dirty_pressure_severity_key(
    path: str,
    labels: set[str],
    *,
    file_loc_index: dict[str, list[fileloc.LocFinding]],
    complexity_index: dict[str, list[DirtyComplexityRegression]],
    magic_index: dict[str, list[magicnums.MagicFinding]],
) -> tuple[object, ...]:
    loc_findings = file_loc_index.get(path, [])
    complexity_findings = complexity_index.get(path, [])
    magic_findings = magic_index.get(path, [])
    max_line_over = max(
        (
            max(0, finding.line_count - finding.line_limit)
            for finding in loc_findings
            if finding.over_line_limit
        ),
        default=0,
    )
    max_byte_over = max(
        (
            max(0, finding.byte_count - finding.byte_limit)
            for finding in loc_findings
            if finding.over_byte_limit
        ),
        default=0,
    )
    max_complexity_over = max(
        (
            max(0, regression.value - regression.active_threshold)
            for regression in complexity_findings
        ),
        default=0,
    )
    max_magic_value = max(
        (_magic_literal_abs(finding.literal) for finding in magic_findings),
        default=0.0,
    )
    return (
        -len(loc_findings),
        -len(labels),
        -max_line_over,
        -len(complexity_findings),
        -max_complexity_over,
        -len(magic_findings),
        -max_magic_value,
        -max_byte_over,
        path,
    )


def _dirty_pressure_severity(
    *,
    file_loc_findings: list[fileloc.LocFinding],
    complexity_regressions: list[DirtyComplexityRegression],
    magic_regressions: list[magicnums.MagicFinding],
    errors: list[str],
) -> str:
    if errors:
        return "unknown"
    if file_loc_findings or complexity_regressions:
        return "high"
    if magic_regressions:
        return "medium"
    return "none"


def _dirty_path_ages(paths: list[Path], *, repo_root: Path) -> DirtyWorktreePressure:
    now = time.time()
    rows: list[tuple[float, str]] = []
    for path in paths:
        try:
            mtime = (repo_root / path).stat().st_mtime
        except OSError:
            continue
        rows.append((max(0.0, now - mtime), path.as_posix()))
    if not rows:
        return {}
    oldest_age, oldest_path = max(rows, key=lambda row: (row[0], row[1]))
    newest_age, newest_path = min(rows, key=lambda row: (row[0], row[1]))
    return {
        "oldestDirtyAgeSeconds": int(oldest_age),
        "oldestDirtyPath": oldest_path,
        "newestDirtyAgeSeconds": int(newest_age),
        "newestDirtyPath": newest_path,
    }


def _dirty_pressure_lines(pressure: DirtyWorktreePressure) -> list[str]:
    if not pressure.get("available"):
        return ["  pressure=unavailable"]
    dirty_paths = int(pressure.get("dirtyPathCount") or 0)
    scanned_paths = int(pressure.get("scannedPathCount") or 0)
    lines = [
        "  pressure "
        f"severity={pressure.get('severity') or 'unknown'} "
        f"findings={int(pressure.get('totalFindings') or 0)} "
        f"files={int(pressure.get('fileCountWithPressure') or 0)} "
        f"scanned={scanned_paths}/{dirty_paths} "
        f"file-loc={int(pressure.get('fileLocFindingCount') or 0)} "
        f"complexity={int(pressure.get('complexityRegressionCount') or 0)} "
        f"magic-numbers={int(pressure.get('magicRegressionCount') or 0)}"
    ]
    age_line = _dirty_pressure_age_line(pressure)
    if age_line:
        lines.append(f"  {age_line}")
    errors = pressure.get("errors") or []
    lines.extend(f"  pressure_error={error}" for error in errors if error)
    summary_rows = pressure.get("summary") or []
    lines.extend(
        f"  pressure_file={summary}"
        for summary in summary_rows[:3]
        if isinstance(summary, str)
    )
    overflow = int(pressure.get("summaryOverflow") or 0)
    if overflow:
        lines.append(
            f"  pressure_more={overflow} additional dirty files carry findings"
        )
    return lines


def _dirty_pressure_age_line(pressure: DirtyWorktreePressure) -> str | None:
    oldest = pressure.get("oldestDirtyAgeSeconds")
    newest = pressure.get("newestDirtyAgeSeconds")
    if oldest is None or newest is None:
        return None
    return (
        "dirty_age="
        f"oldest={pressure.get('oldestDirtyPath') or '-'}:"
        f"{_format_dirty_age(oldest)} "
        f"newest={pressure.get('newestDirtyPath') or '-'}:"
        f"{_format_dirty_age(newest)}"
    )


def _format_dirty_age(raw_seconds: int | float) -> str:
    seconds = float(raw_seconds)
    return format_relative_seconds(seconds).removesuffix(" ago")


def _dirty_pressure_error(label: str, exc: BaseException) -> str:
    return f"{label}: {clip(str(exc), 120)}"


def _magic_literal_abs(literal: str) -> float:
    try:
        return abs(float(literal.replace("_", "")))
    except ValueError:
        return 0.0


def _git_read(repo_root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        check=False,
        text=True,
    )
    return result.stdout.strip() if result.returncode == 0 else ""


def _git_read_z(repo_root: Path, *args: str) -> list[str]:
    result = subprocess.run(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        return []
    raw = result.stdout.decode("utf-8", errors="surrogateescape")
    return [part for part in raw.split("\0") if part]


def apply_output_budget(
    text: str,
    *,
    max_lines: int | None,
    max_bytes: int | None,
    explain: bool,
) -> str:
    lines = text.splitlines()
    original_lines = len(lines)
    original_bytes = len(text.encode("utf-8"))
    pruned = False
    line_budget = max_lines if max_lines and max_lines > 0 else None
    byte_budget = max_bytes if max_bytes and max_bytes > 0 else None
    reserve = 1 if explain else 0
    if line_budget and len(lines) > line_budget:
        keep = max(1, line_budget - reserve)
        lines = lines[:keep]
        pruned = True

    def pruning_note(retained_lines: int, retained_bytes: int) -> str:
        return (
            "Pruning "
            f"original_lines={original_lines} original_bytes={original_bytes} "
            f"max_lines={line_budget or '-'} max_bytes={byte_budget or '-'} "
            f"retained_lines={retained_lines} retained_content_bytes={retained_bytes}"
        )

    def rendered(with_note: bool) -> str:
        out = list(lines)
        if with_note:
            text_without_note = "\n".join(out)
            retained_bytes = len(text_without_note.encode("utf-8"))
            out.append(pruning_note(len(lines) + 1, retained_bytes))
        return "\n".join(out)

    if byte_budget:
        while lines and len(rendered(explain and pruned).encode("utf-8")) > byte_budget:
            lines.pop()
            pruned = True
        if (
            not lines
            and len(rendered(explain and pruned).encode("utf-8")) > byte_budget
        ):
            return _truncate_to_bytes(rendered(explain and pruned), byte_budget)
    if pruned and explain:
        return rendered(True)
    return "\n".join(lines)


def _truncate_to_bytes(text: str, max_bytes: int) -> str:
    return text.encode("utf-8")[: max(0, max_bytes)].decode("utf-8", errors="ignore")


def _inbox_lines() -> list[str]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return []
    items = collect_inbox_items(str(repo_root))
    deadletters = collect_deadlettered_inbox_items(str(repo_root))
    refused = collect_refused_inbox_items(str(repo_root))
    lines = ["Inbox", f"  pending={len(items)}"]
    for item in items:
        lines.append(
            f"  key={inbox_item_key(item.name)} "
            f"age={relative_time_for_path(item.source_path)}"
        )
    if items:
        lines.append(f"  {INBOX_RESPONSE_ROW}")
    if deadletters:
        lines.append(f"  deadlettered={len(deadletters)}")
        lines.extend(f"  {line}" for line in inbox_deadletter_context_rows(deadletters))
    if refused:
        lines.append(f"  refused={len(refused)}")
        lines.extend(f"  {line}" for line in inbox_ack_state_context_rows(refused))
    return lines


def render_sweep(
    files: list[Path],
    *,
    count: int,
    start: str | None = None,
    end: str | None = None,
    contains: str | None = None,
    turn_ids: list[str] | None = None,
    tools: list[str] | None = None,
) -> str:
    """Briefings across the last `count` compaction windows, newest last.

    Each window is the span between two compactions: the asks that opened it
    and the final that closed it. A renewed agent reads these to recover not
    just the latest state but the trajectory.
    """
    all_turns = collect_turns(files)
    all_compactions = collect_compactions(files)
    horizon = _resolve_horizon(all_turns, all_compactions, count=count, end=end)
    effective_start = _effective_start(start, horizon.start)
    turns = records.filter_turns(
        all_turns,
        start=effective_start,
        end=end,
        contains=contains,
        turn_ids=turn_ids,
        tools=tools,
    )
    window_start = effective_start or horizon.start
    boundaries = [
        boundary
        for boundary in horizon.selected_boundaries
        if (not window_start or boundary > window_start)
        and (not end or boundary <= end)
    ]
    if not window_start:
        return render_briefing(
            files,
            start=start,
            end=end,
            contains=contains,
            turn_ids=turn_ids,
            tools=tools,
        )
    lines: list[str] = [
        "Sweep",
        f"  windows={len(boundaries) + 1} files={len(files)}",
    ]
    lines.extend(_horizon_lines(horizon))
    edges = [window_start, *boundaries, HORIZON_END_SENTINEL]
    sweep_asks = collect_ask_candidates(
        start=effective_start, end=end, contains=contains
    )
    for index in range(len(edges) - 1):
        window_start, window_end = edges[index], edges[index + 1]
        window_turns = [
            turn
            for turn in turns
            if (not window_start or turn.start_ts >= window_start)
            and turn.start_ts < window_end
        ]
        label = window_start or "session start"
        lines.append(f"Window {index} (from {label})")
        asks = [
            ask
            for ask in sweep_asks
            if (not window_start or ask.timestamp >= window_start)
            and ask.timestamp < window_end
        ]
        asks = sort_rehydration_candidates(asks)
        for candidate in asks[:SWEEP_WINDOW_ASKS]:
            lines.append(f"  ask {_ask_line(candidate).strip()}")
        finals = sort_rehydration_candidates(collect_final_candidates(window_turns))
        if finals:
            latest = finals[0]
            lines.append(f"  final {latest.timestamp} {clip(latest.text)}")
        if not asks and not finals:
            lines.append("  (no dialogue in this window)")
    return "\n".join(lines)
