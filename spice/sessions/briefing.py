"""The briefing: rehydrate an agent (or its successor) from the transcript.

The `spice session briefing` output. It answers, mechanically, the questions
a freshly compacted or freshly renewed agent must not guess at: what was
asked, what was last delivered, what to keep doing, what the working set was,
and what steering is pending.
"""

from __future__ import annotations

import re
from collections import Counter
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Literal, TypeAlias

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
from spice.sessions.briefingpressure import dirty_path_count, git_posture_lines
from spice.sessions.briefingrender import (
    active_filter_lines,
    apply_output_budget,
    drop_human_ask_duplicates,
    filter_compactions,
    inbox_lines,
)
from spice.sessions.briefingtaskplane import collect_task_plane_candidates
from spice.sessions.meter import (
    ContextMeter,
    GuidanceState,
    collect_context_meter,
    context_meter_instruction,
    meter_pressure_level,
)
from spice.sessions.slices import select_compaction_windows_from_files
from spice.sessions.records import (
    CommitRecord,
    CompactionRecord,
    TurnRecord,
    collect_commit_records,
    collect_compactions,
    collect_turns,
)

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
ASK_RANK_NAME = "ask_disposition_then_recency"
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
    user_after_text: str = ""
    intent_text: str = ""


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


@dataclass(frozen=True)
class BriefingPayload:
    files: tuple[Path, ...]
    filters: BriefingFilters
    horizon: ResolvedHorizon
    turns: tuple[TurnRecord, ...]
    compactions: tuple[CompactionRecord, ...]
    meter: ContextMeter
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


def ask_rank_key(timestamp: str, disposition: str) -> RankKey:
    return (ASK_DISPOSITION_RANK.get(disposition.lower(), 0), timestamp)


def recency_rank_key(timestamp: str) -> RankKey:
    return (timestamp,)


def file_touch_rank_key(timestamp: str, touch_count: int) -> RankKey:
    return (timestamp, touch_count)


def command_rank_key(timestamp: str, error_count: int) -> RankKey:
    return (error_count, timestamp)


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
    turns: list[TurnRecord] | None = None,
    start: str | None = None,
    end: str | None = None,
    contains: str | None = None,
    subject_thread_ids: frozenset[str] = frozenset(),
) -> list[RehydrationCandidate]:
    repo_root = repo_root_from_cwd()
    candidates = _human_ask_candidates(turns or [])
    if repo_root is not None:
        candidates.extend(
            _pending_ask_candidate(item) for item in collect_inbox_items(str(repo_root))
        )
        candidates.extend(
            _ack_state_ask_candidate(record)
            for record in ack_state_records(repo_root)
            if _ack_state_record_matches_subject(record, subject_thread_ids)
        )
    candidates = drop_human_ask_duplicates(candidates)
    return [
        candidate
        for candidate in candidates
        if _ask_candidate_matches_filters(
            candidate, start=start, end=end, contains=contains
        )
    ]


def _human_ask_candidates(turns: list[TurnRecord]) -> list[RehydrationCandidate]:
    return [
        ask_candidate(turn.start_ts, message.text, disposition="human")
        for turn in turns
        for message in turn.user_messages
        if message.shape is records.MessageShape.HUMAN
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
        _ack_state_ask_text(record),
        disposition=record.disposition,
        key=record.key,
    )


def _ack_state_ask_text(record: AckStateRecord) -> str:
    request = parse_inbox_payload(record.text).body
    response = record.ack_content.strip()
    if not response:
        return request
    return f"{request} | response: {response}"


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
    filters = BriefingFilters(
        start=start,
        end=end,
        contains=contains,
        turn_ids=tuple(turn_ids or ()),
        tools=tuple(tools or ()),
    )
    turns = records.filter_turns(
        all_turns,
        start=effective_start,
        end=end,
        contains=contains,
        turn_ids=list(filters.turn_ids) or None,
        tools=list(filters.tools) or None,
    )
    compactions = filter_compactions(
        all_compactions, start=effective_start, end=end, contains=contains
    )
    turn_tuple = tuple(turns)
    compaction_tuple = tuple(compactions)
    commits = tuple(collect_commit_records(list(turn_tuple)))
    asks = tuple(
        collect_ask_candidates(
            turns=list(turn_tuple),
            start=effective_start,
            end=end,
            contains=contains,
            subject_thread_ids=_subject_thread_ids(list(file_tuple)),
        )
    )
    recovery_asks = tuple(
        collect_ask_candidates(
            turns=list(turn_tuple),
            start=start,
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
        meter=collect_context_meter(list(file_tuple), start=effective_start),
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
            if sweep_count is not None and not sweep_falls_back
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
    recovery = _recovery_lines(
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
    if payload.recovery_first:
        lines.extend(recovery)
    lines.extend(_guidance_lines(payload.meter))
    lines.extend(_learning_lines())
    lines.extend(_task_plane_lines(list(payload.task_plane)))
    lines.extend(_asks_lines(list(payload.asks)))
    lines.extend(_finals_lines(list(payload.finals)))
    if not payload.recovery_first:
        lines.extend(recovery)
    lines.extend(
        _activity_lines(
            list(payload.turns),
            list(payload.command_candidates),
            list(payload.file_candidates),
            list(payload.commit_candidates),
        )
    )
    lines.extend(git_posture_lines())
    lines.extend(inbox_lines())
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


def _guidance_lines(meter: ContextMeter) -> list[str]:
    handle, phase = _active_claim_handle_phase()
    state = GuidanceState(
        level=meter_pressure_level(meter),
        claim_known=True,
        claim_handle=handle,
        claim_phase=phase,
        dirty_path_count=dirty_path_count(),
    )
    instruction = context_meter_instruction(state)
    if not instruction:
        return []
    return ["Guidance", f"  keep_working={instruction}"]


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


def _recovery_lines(
    compactions: list[RehydrationCandidate],
    asks: list[RehydrationCandidate] | None = None,
) -> list[str]:
    ranked = sort_rehydration_candidates(compactions)
    if not ranked:
        return []
    latest = ranked[0]
    steering = _latest_steering_before(asks or [], latest.timestamp)
    lines = [
        "Recovery",
        f"  latest_compaction={latest.timestamp}",
        f"  assistant_before={latest.label}",
    ]
    if latest.intent_text:
        lines.append(f"  intent={clip(latest.intent_text)}")
    lines.append(f"  user_after={clip(latest.user_after_text)}")
    if steering is not None:
        key = f" key={steering.key}" if steering.key else ""
        lines.append(
            f"  steering={steering.label} {steering.timestamp}{key} "
            f"{clip(steering.text)}"
        )
    return lines


def _latest_steering_before(
    asks: list[RehydrationCandidate], timestamp: str
) -> RehydrationCandidate | None:
    before = [ask for ask in asks if ask.key and ask.timestamp <= timestamp]
    return max(before, key=lambda ask: (ask.timestamp, ask.key), default=None)


def _latest_event_is_compaction(
    turns: list[records.TurnRecord], compactions: list[records.CompactionRecord]
) -> bool:
    if not compactions:
        return False
    latest_compaction = max(record.ts for record in compactions)
    latest_turn = max((_turn_activity_ts(turn) for turn in turns), default="")
    return latest_compaction >= latest_turn


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
                [
                    ask
                    for ask in asks
                    if (not window_start or ask.timestamp >= window_start)
                    and ask.timestamp < window_end
                ]
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
                        collect_final_candidates(list(window_turns))
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
    if not payload.sweep_windows:
        return render_briefing_payload(payload)
    lines: list[str] = [
        "Sweep",
        f"  windows={len(payload.sweep_windows)} files={len(payload.files)}",
    ]
    lines.extend(_horizon_lines(payload.horizon))
    for window in payload.sweep_windows:
        lines.append(f"Window {window.index} (from {window.label})")
        for candidate in window.asks[:SWEEP_WINDOW_ASKS]:
            lines.append(f"  ask {_ask_line(candidate).strip()}")
        if window.finals:
            latest = window.finals[0]
            lines.append(f"  final {latest.timestamp} {clip(latest.text)}")
        if not window.asks and not window.finals:
            lines.append("  (no dialogue in this window)")
    return "\n".join(lines)
