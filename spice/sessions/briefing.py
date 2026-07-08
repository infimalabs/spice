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
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any, Literal, TypeAlias

from spice.agent.identity import uuid_thread_id
from spice.errors import SpiceError
from spice.mail.ackstate import AckStateRecord, ack_state_records
from spice.mail.inbox import (
    INBOX_RESPONSE_ROW,
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    collect_refused_inbox_items,
    inbox_ack_state_context_rows,
    inbox_deadletter_context_rows,
    inbox_item_key,
    parse_inbox_payload,
    relative_time_for_path,
)
from spice.paths import repo_root_from_cwd
from spice.sessions import learnings as session_learnings
from spice.sessions import records
from spice.sessions.briefingpressure import dirty_path_count, git_posture_lines
from spice.sessions.briefingtaskplane import collect_task_plane_candidates
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
    "human": 10,
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
    start: str | None = None,
    end: str | None = None,
    contains: str | None = None,
    subject_thread_ids: frozenset[str] = frozenset(),
) -> list[RehydrationCandidate]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return []
    candidates = [
        *(_pending_ask_candidate(item) for item in collect_inbox_items(str(repo_root))),
        *(
            _ack_state_ask_candidate(record)
            for record in ack_state_records(repo_root)
            if _ack_state_record_matches_subject(record, subject_thread_ids)
        ),
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
        )
        for record in compactions
    ]


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
    asks = collect_ask_candidates(
        start=effective_start,
        end=end,
        contains=contains,
        subject_thread_ids=_subject_thread_ids(files),
    )
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
    lines.extend(git_posture_lines())
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


def _recovery_lines(compactions: list[RehydrationCandidate]) -> list[str]:
    ranked = sort_rehydration_candidates(compactions)
    if not ranked:
        return []
    latest = ranked[0]
    return [
        "Recovery",
        f"  latest_compaction={latest.timestamp}",
        f"  assistant_before={latest.label}",
        f"  user_after={clip(latest.user_after_text)}",
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
                    record.intent_text or "",
                    record.summary_after_text or "",
                    record.first_user_after_text or "",
                ]
            ).lower()
            if needle not in haystack:
                continue
        kept.append(record)
    return kept


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
        start=effective_start,
        end=end,
        contains=contains,
        subject_thread_ids=_subject_thread_ids(files),
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
