"""The briefing: rehydrate an agent (or its successor) from the transcript.

The `spice session briefing` output. It answers, mechanically, the questions
a freshly compacted or freshly renewed agent must not guess at: what was
asked, what was last delivered, what the working set was, and what steering is
pending.
"""

from __future__ import annotations

import re
from collections import Counter
from dataclasses import dataclass, replace
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Literal, TypeAlias

from spice.agent.identity import uuid_thread_id
from spice.errors import SpiceError
from spice.mail.ackstate import AckStateRecord, ack_state_records
from spice.mail.inbox import (
    collect_inbox_items,
    inbox_item_key,
    parse_inbox_payload,
)
from spice.paths import repo_root_from_cwd
from spice.sessions import learnings as session_learnings
from spice.sessions import records
from spice.sessions.briefingpressure import git_posture_lines
from spice.sessions.briefingrender import (
    active_filter_lines,
    apply_output_budget,
    filter_compactions,
    inbox_lines,
)
from spice.sessions.briefingtaskplane import collect_task_plane_candidates
from spice.sessions.slices import select_compaction_windows_from_files
from spice.sessions.util import parse_iso_ts
from spice.sessions.records import (
    CommitRecord,
    CompactionRecord,
    TurnRecord,
    collect_commit_records,
    collect_compactions,
    collect_turns,
)

STEERING_ROW_LIMIT = 6
STEERING_TEXT_PREVIEW_CHARS = 200
STEERING_RESPONSE_PREVIEW_CHARS = 120
FINAL_ROW_LIMIT = 4
PREVIEW_CHARS = 200
RECENT_COMMITS_LIMIT = 5
COMMIT_PREVIEW_CHARS = 120
SWEEP_WINDOW_ASKS = 3
WORKING_SET_LIMIT = 10
DEFAULT_BRIEFING_MAX_LINES = 120
DEFAULT_BRIEFING_MAX_BYTES = 20_000
DEFAULT_RECENCY_MAX_SECONDS = 4 * 60 * 60
DIRTY_PRESSURE_PREVIEW_LIMIT = 6
TASK_PLANE_ROW_LIMIT = 8
TASK_PLANE_PREVIEW_CHARS = 180
DEFAULT_HORIZON_COMPACTIONS = 3
MAX_HORIZON_COMPACTIONS = 5
HORIZON_END_SENTINEL = "￿"
THREAD_ID_TOKEN_RE = re.compile(
    r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
    r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}|[0-9a-fA-F]{32}"
)

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
    "human": 0,
    "": 0,
}
ASK_RANK_NAME = "ask_recency_then_disposition"
FILE_RANK_NAME = "file_last_touch_then_hotspot"
COMMAND_RANK_NAME = "command_failures_then_recency"
RECENCY_RANK_NAME = "recency"


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
    response_text: str = ""
    user_after_text: str = ""
    intent_text: str = ""
    project: str = ""


@dataclass(frozen=True)
class BriefingFilters:
    start: str | None
    end: str | None
    contains: str | None
    turn_ids: tuple[str, ...]
    tools: tuple[str, ...]


@dataclass(frozen=True)
class SweepWindowPayload:
    index: int
    label: str
    turns: tuple[TurnRecord, ...]
    asks: tuple[RehydrationCandidate, ...]
    finals: tuple[RehydrationCandidate, ...]
    commits: tuple[RehydrationCandidate, ...]


@dataclass(frozen=True)
class BriefingPayload:
    files: tuple[Path, ...]
    filters: BriefingFilters
    horizon: ResolvedHorizon
    turns: tuple[TurnRecord, ...]
    compactions: tuple[CompactionRecord, ...]
    commits: tuple[CommitRecord, ...]
    asks: tuple[RehydrationCandidate, ...]
    recovery_asks: tuple[RehydrationCandidate, ...]
    finals: tuple[RehydrationCandidate, ...]
    commit_candidates: tuple[RehydrationCandidate, ...]
    compaction_intents: tuple[RehydrationCandidate, ...]
    command_candidates: tuple[RehydrationCandidate, ...]
    file_candidates: tuple[RehydrationCandidate, ...]
    task_plane: tuple[RehydrationCandidate, ...]
    recovery_first: bool
    sweep_windows: tuple[SweepWindowPayload, ...]


@dataclass(frozen=True)
class ResolvedHorizon:
    start: str | None
    basis: str
    requested_compactions: int
    selected_boundaries: tuple[str, ...]

    @property
    def selected_compactions(self) -> int:
        return len(self.selected_boundaries)


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


def dedupe_rehydration_candidates(
    candidates: list[RehydrationCandidate],
) -> list[RehydrationCandidate]:
    groups: dict[str, list[RehydrationCandidate]] = {}
    unique: list[RehydrationCandidate] = []
    for candidate in candidates:
        key = _candidate_dedupe_key(candidate)
        if not key:
            unique.append(candidate)
            continue
        groups.setdefault(key, []).append(candidate)
    for group in groups.values():
        kept = max(
            group, key=lambda candidate: (candidate.timestamp, candidate.rank_key)
        )
        unique.append(replace(kept, count=len(group)) if len(group) > 1 else kept)
    return unique


def _candidate_dedupe_key(candidate: RehydrationCandidate) -> str:
    return " ".join(candidate.text.split()).casefold()


def ask_rank_key(timestamp: str, disposition: str) -> RankKey:
    return (timestamp, ASK_DISPOSITION_RANK.get(disposition.lower(), 0))


def recency_rank_key(timestamp: str) -> RankKey:
    return (timestamp,)


def file_touch_rank_key(timestamp: str, touch_count: int) -> RankKey:
    return (timestamp, touch_count)


def command_rank_key(timestamp: str, error_count: int) -> RankKey:
    return (error_count, timestamp)


def ask_candidate(
    timestamp: str,
    text: str,
    *,
    disposition: str = "human",
    key: str = "",
    response_text: str = "",
) -> RehydrationCandidate:
    return RehydrationCandidate(
        kind="ask",
        timestamp=timestamp,
        text=text,
        rank_name=ASK_RANK_NAME,
        rank_key=ask_rank_key(timestamp, disposition),
        label=disposition,
        key=key,
        response_text=response_text,
    )


def collect_ask_candidates(
    *,
    turns: list[TurnRecord] | None = None,
    start: str | None = None,
    end: str | None = None,
    contains: str | None = None,
    subject_thread_ids: frozenset[str] = frozenset(),
) -> list[RehydrationCandidate]:
    repo_root = repo_root_from_cwd()
    candidates: list[RehydrationCandidate] = []
    if repo_root is not None:
        candidates.extend(
            _pending_ask_candidate(item) for item in collect_inbox_items(str(repo_root))
        )
        candidates.extend(
            _ack_state_ask_candidate(record)
            for record in ack_state_records(repo_root)
            if _ack_state_record_matches_subject(record, subject_thread_ids)
        )
    return dedupe_rehydration_candidates(
        [
            candidate
            for candidate in candidates
            if _ask_candidate_matches_filters(
                candidate, start=start, end=end, contains=contains
            )
        ]
    )


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
        _ack_state_ask_request(record),
        disposition=record.disposition,
        key=record.key,
        response_text=record.ack_content.strip(),
    )


def _ack_state_ask_request(record: AckStateRecord) -> str:
    return parse_inbox_payload(record.text).body


def _ack_state_record_matches_subject(
    record: AckStateRecord, subject_thread_ids: frozenset[str]
) -> bool:
    if not subject_thread_ids:
        return True
    return (
        uuid_thread_id(str(record.lineage.get("thread_id") or "")) in subject_thread_ids
    )


def _subject_thread_ids(files: list[Path]) -> frozenset[str]:
    thread_ids: set[str] = set()
    for path in files:
        for match in THREAD_ID_TOKEN_RE.finditer(path.name):
            thread_id = uuid_thread_id(match.group(0))
            if thread_id:
                thread_ids.add(thread_id)
    return frozenset(thread_ids)


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
    haystack = "\n".join(
        [candidate.text, candidate.response_text, candidate.key]
    ).lower()
    return not needle or needle in haystack


def _ask_timestamp_from_key(key: str) -> str:
    raw = key.split("-", 1)[0]
    raw = raw[:-1] if raw.endswith("Z") else raw
    try:
        parsed = datetime.strptime(raw, "%Y%m%dT%H%M%S%f").replace(tzinfo=UTC)
    except ValueError as exc:
        raise SpiceError(f"invalid ACK steering key timestamp: {key or '-'}") from exc
    return parsed.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def collect_final_candidates(turns: list[TurnRecord]) -> list[RehydrationCandidate]:
    return dedupe_rehydration_candidates(
        [
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
    )


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
            text=record.intent_text
            or record.first_user_after_text
            or record.last_assistant_before_text
            or "",
            rank_name=RECENCY_RANK_NAME,
            rank_key=recency_rank_key(record.ts),
            label=clip(record.last_assistant_before_text),
            user_after_text=record.first_user_after_text or "",
            intent_text=record.intent_text or "",
        )
        for record in compactions
    ]


def build_briefing_payload(
    files: list[Path],
    *,
    start: str | None = None,
    end: str | None = None,
    contains: str | None = None,
    turn_ids: list[str] | None = None,
    tools: list[str] | None = None,
    sweep_count: int | None = None,
) -> BriefingPayload:
    file_tuple = tuple(files)
    horizon_count = (
        sweep_count if sweep_count is not None else DEFAULT_HORIZON_COMPACTIONS
    )
    horizon = _resolve_horizon(list(file_tuple), count=horizon_count, end=end)
    sweep_falls_back = sweep_count is not None and not start and horizon.start is None
    if sweep_falls_back:
        horizon = _resolve_horizon(
            list(file_tuple), count=DEFAULT_HORIZON_COMPACTIONS, end=end
        )
    effective_start = _effective_start(start, horizon.start)
    all_turns = collect_turns(list(file_tuple), start=effective_start)
    all_compactions = collect_compactions(list(file_tuple), start=effective_start)
    recency_floor = _recency_floor(
        end=end,
        turns=all_turns,
        compactions=all_compactions,
        max_seconds=DEFAULT_RECENCY_MAX_SECONDS,
    )
    candidate_start = _latest_start(effective_start, recency_floor)
    recovery_start = _latest_start(start, recency_floor)
    filters = BriefingFilters(
        start=start,
        end=end,
        contains=contains,
        turn_ids=tuple(turn_ids or ()),
        tools=tuple(tools or ()),
    )
    turns = records.filter_turns(
        all_turns,
        start=candidate_start,
        end=end,
        contains=contains,
        turn_ids=list(filters.turn_ids) or None,
        tools=list(filters.tools) or None,
    )
    compactions = filter_compactions(
        all_compactions, start=candidate_start, end=end, contains=contains
    )
    turn_tuple = tuple(turns)
    compaction_tuple = tuple(compactions)
    commits = tuple(collect_commit_records(list(turn_tuple)))
    asks = tuple(
        collect_ask_candidates(
            turns=list(turn_tuple),
            start=candidate_start,
            end=end,
            contains=contains,
            subject_thread_ids=_subject_thread_ids(list(file_tuple)),
        )
    )
    recovery_asks = tuple(
        collect_ask_candidates(
            turns=list(turn_tuple),
            start=recovery_start,
            end=end,
            contains=contains,
            subject_thread_ids=_subject_thread_ids(list(file_tuple)),
        )
    )
    finals = tuple(collect_final_candidates(list(turn_tuple)))
    commit_candidates = tuple(collect_commit_candidates(list(commits)))
    compaction_intents = tuple(
        collect_compaction_intent_candidates(list(compaction_tuple))
    )
    command_candidates = tuple(collect_command_candidates(list(turn_tuple)))
    file_candidates = tuple(collect_file_touch_candidates(list(turn_tuple)))
    task_plane = tuple(collect_task_plane_candidates())
    return BriefingPayload(
        files=file_tuple,
        filters=filters,
        horizon=horizon,
        turns=turn_tuple,
        compactions=compaction_tuple,
        commits=commits,
        asks=asks,
        recovery_asks=recovery_asks,
        finals=finals,
        commit_candidates=commit_candidates,
        compaction_intents=compaction_intents,
        command_candidates=command_candidates,
        file_candidates=file_candidates,
        task_plane=task_plane,
        recovery_first=_latest_event_is_compaction(
            list(turn_tuple), list(compaction_tuple)
        ),
        sweep_windows=(
            _build_sweep_windows(
                turns=turn_tuple,
                asks=asks,
                horizon=horizon,
                start=effective_start,
                end=end,
            )
            if not sweep_falls_back
            else ()
        ),
    )


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
    payload = build_briefing_payload(
        files,
        start=start,
        end=end,
        contains=contains,
        turn_ids=turn_ids,
        tools=tools,
    )
    return render_briefing_payload(
        payload,
        max_lines=max_lines,
        max_bytes=max_bytes,
        explain_pruning=explain_pruning,
    )


def render_briefing_payload(
    payload: BriefingPayload,
    *,
    max_lines: int | None = DEFAULT_BRIEFING_MAX_LINES,
    max_bytes: int | None = DEFAULT_BRIEFING_MAX_BYTES,
    explain_pruning: bool = False,
) -> str:
    from spice.sessions.rehydrationview import (
        activity_lines,
        finals_lines,
        recovery_lines,
        steering_lines,
        task_plane_lines,
        trajectory_lines,
    )

    recovery = recovery_lines(
        list(payload.compaction_intents), list(payload.recovery_asks)
    )
    lines: list[str] = []
    lines.extend(_briefing_header_lines(list(payload.files), list(payload.turns)))
    lines.extend(_horizon_lines(payload.horizon))
    filter_lines = active_filter_lines(
        start=payload.filters.start,
        end=payload.filters.end,
        contains=payload.filters.contains,
        turn_ids=payload.filters.turn_ids,
        tools=payload.filters.tools,
    )
    if filter_lines:
        lines.append("Filters")
        lines.extend(filter_lines)
    lines.extend(steering_lines(list(payload.asks)))
    lines.extend(task_plane_lines(list(payload.task_plane)))
    lines.extend(_learning_lines(list(payload.task_plane)))
    lines.extend(recovery)
    lines.extend(trajectory_lines(list(payload.sweep_windows)))
    lines.extend(finals_lines(list(payload.finals)))
    lines.extend(
        activity_lines(
            list(payload.turns),
            list(payload.command_candidates),
            list(payload.file_candidates),
            list(payload.commit_candidates),
        )
    )
    lines.extend(git_posture_lines())
    lines.extend(inbox_lines(max_consumed_age_seconds=DEFAULT_RECENCY_MAX_SECONDS))
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
    files: list[Path],
    *,
    count: int,
    end: str | None,
) -> ResolvedHorizon:
    selection = select_compaction_windows_from_files(
        files,
        count=count,
        end=end,
        hard_cap=MAX_HORIZON_COMPACTIONS,
    )
    return ResolvedHorizon(
        start=selection.start_ts,
        basis=selection.basis,
        requested_compactions=selection.requested_count,
        selected_boundaries=selection.selected_boundaries,
    )


def _turn_activity_ts(turn: TurnRecord) -> str:
    return turn.end_ts or turn.last_activity_ts or turn.start_ts


def _effective_start(user_start: str | None, horizon_start: str | None) -> str | None:
    return user_start or horizon_start


def _latest_start(*values: str | None) -> str | None:
    present = [value for value in values if value]
    return max(present) if present else None


def _recency_floor(
    *,
    end: str | None,
    turns: list[TurnRecord],
    compactions: list[CompactionRecord],
    max_seconds: int,
) -> str | None:
    reference = _recency_reference_ts(end=end, turns=turns, compactions=compactions)
    reference_dt = parse_iso_ts(reference)
    if reference_dt is None:
        reference_dt = datetime.now(UTC)
    floor = reference_dt.astimezone(UTC) - timedelta(seconds=max(0, max_seconds))
    return floor.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def _recency_reference_ts(
    *,
    end: str | None,
    turns: list[TurnRecord],
    compactions: list[CompactionRecord],
) -> str | None:
    if end:
        return end
    values = [
        value
        for value in [
            *(_turn_activity_ts(turn) for turn in turns),
            *(record.ts for record in compactions),
        ]
        if value
    ]
    return max(values) if values else None


def _learning_lines(task_plane: list[RehydrationCandidate]) -> list[str]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return []
    stem = _active_task_project_stem(task_plane)
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


def _active_task_project_stem(
    task_plane: list[RehydrationCandidate],
) -> str | None:
    project = next(
        (candidate.project for candidate in task_plane if candidate.project), ""
    )
    if not project:
        return None
    from spice.tasks import config

    return config.project_stem(project)


def _learning_record_line(record: session_learnings.LearningRecord) -> str:
    source = record.source_task or "-"
    return (
        f"  - {clip(record.statement, COMMIT_PREVIEW_CHARS)} "
        f"(confirmed={record.confirmation_count}, source={source})"
    )


def _latest_event_is_compaction(
    turns: list[records.TurnRecord], compactions: list[records.CompactionRecord]
) -> bool:
    if not compactions:
        return False
    latest_compaction = max(record.ts for record in compactions)
    latest_turn = max((_turn_activity_ts(turn) for turn in turns), default="")
    return latest_compaction >= latest_turn


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


def _build_sweep_windows(
    *,
    turns: tuple[TurnRecord, ...],
    asks: tuple[RehydrationCandidate, ...],
    horizon: ResolvedHorizon,
    start: str | None,
    end: str | None,
) -> tuple[SweepWindowPayload, ...]:
    window_start = start or horizon.start
    if not window_start:
        return ()
    boundaries = [
        boundary
        for boundary in horizon.selected_boundaries
        if (not window_start or boundary > window_start)
        and (not end or boundary <= end)
    ]
    windows: list[SweepWindowPayload] = []
    edges = [window_start, *boundaries, HORIZON_END_SENTINEL]
    for index in range(len(edges) - 1):
        window_start, window_end = edges[index], edges[index + 1]
        window_turns = tuple(
            turn
            for turn in turns
            if (not window_start or turn.start_ts >= window_start)
            and turn.start_ts < window_end
        )
        window_asks = tuple(
            sort_rehydration_candidates(
                dedupe_rehydration_candidates(
                    [
                        ask
                        for ask in asks
                        if (not window_start or ask.timestamp >= window_start)
                        and ask.timestamp < window_end
                    ]
                )
            )
        )
        windows.append(
            SweepWindowPayload(
                index=index,
                label=window_start or "session start",
                turns=window_turns,
                asks=window_asks,
                finals=tuple(
                    sort_rehydration_candidates(
                        dedupe_rehydration_candidates(
                            collect_final_candidates(list(window_turns))
                        )
                    )
                ),
                commits=tuple(
                    sort_rehydration_candidates(
                        collect_commit_candidates(
                            collect_commit_records(list(window_turns))
                        )
                    )
                ),
            )
        )
    return tuple(windows)


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
    payload = build_briefing_payload(
        files,
        start=start,
        end=end,
        contains=contains,
        turn_ids=turn_ids,
        tools=tools,
        sweep_count=count,
    )
    return render_sweep_payload(payload)


def render_sweep_payload(payload: BriefingPayload) -> str:
    from spice.sessions.rehydrationview import (
        ask_line,
        repeat_count_fragment,
        window_trajectory_summary,
    )

    if not payload.sweep_windows:
        return render_briefing_payload(payload)
    lines: list[str] = [
        "Sweep",
        f"  windows={len(payload.sweep_windows)} files={len(payload.files)}",
    ]
    lines.extend(_horizon_lines(payload.horizon))
    for window in payload.sweep_windows:
        lines.append(f"Window {window.index} (from {window.label})")
        entries = _diverse_sweep_entries(window)
        for kind, candidate in entries:
            if kind == "ask":
                lines.append(f"  ask {ask_line(candidate).strip()}")
            else:
                repeat = repeat_count_fragment(candidate)
                lines.append(
                    f"  final {candidate.timestamp}{repeat} {clip(candidate.text)}"
                )
        if not entries:
            summary = window_trajectory_summary(window)
            if summary:
                lines.append(f"  trajectory {summary}")
            else:
                lines.append("  (no dialogue in this window)")
    return "\n".join(lines)


def _diverse_sweep_entries(
    window: SweepWindowPayload,
) -> list[tuple[str, RehydrationCandidate]]:
    entries = [("ask", candidate) for candidate in window.asks[:SWEEP_WINDOW_ASKS]]
    entries.extend(("final", candidate) for candidate in window.finals[:1])
    return _avoid_consecutive_kinds(entries)


def _avoid_consecutive_kinds(
    entries: list[tuple[str, RehydrationCandidate]],
) -> list[tuple[str, RehydrationCandidate]]:
    remaining = list(entries)
    ordered: list[tuple[str, RehydrationCandidate]] = []
    while remaining:
        previous_kind = ordered[-1][0] if ordered else None
        index = 0
        if previous_kind is not None:
            alternate_index = next(
                (
                    candidate_index
                    for candidate_index, (kind, _candidate) in enumerate(remaining)
                    if kind != previous_kind
                ),
                None,
            )
            if alternate_index is not None:
                index = alternate_index
        ordered.append(remaining.pop(index))
    return ordered
