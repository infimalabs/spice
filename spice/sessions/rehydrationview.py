"""Candidate-to-text rendering for session rehydration views."""

from __future__ import annotations

from spice.sessions.briefing import (
    COMMIT_PREVIEW_CHARS,
    FINAL_ROW_LIMIT,
    RECENT_COMMITS_LIMIT,
    STEERING_RESPONSE_PREVIEW_CHARS,
    STEERING_ROW_LIMIT,
    STEERING_TEXT_PREVIEW_CHARS,
    TASK_PLANE_PREVIEW_CHARS,
    TASK_PLANE_ROW_LIMIT,
    WORKING_SET_LIMIT,
    RehydrationCandidate,
    SweepWindowPayload,
    clip,
    sort_rehydration_candidates,
)
from spice.sessions.records import TurnRecord


def task_plane_lines(candidates: list[RehydrationCandidate]) -> list[str]:
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


def steering_lines(asks: list[RehydrationCandidate]) -> list[str]:
    ranked = sort_rehydration_candidates(
        [candidate for candidate in asks if _is_steering_candidate(candidate)]
    )
    if not ranked:
        return []
    shown = ranked[:STEERING_ROW_LIMIT]
    overflow = len(ranked) - len(shown)
    lines = ["Steering"]
    lines.extend(_steering_line(candidate) for candidate in shown)
    if overflow:
        lines.append(f"  +{overflow} more steering rows")
    return lines


def _is_steering_candidate(candidate: RehydrationCandidate) -> bool:
    return bool(candidate.key and candidate.text.strip())


def _steering_line(candidate: RehydrationCandidate) -> str:
    key = f" key={candidate.key}" if candidate.key else ""
    repeat = repeat_count_fragment(candidate).strip()
    repeat_fragment = f" {repeat}" if repeat else ""
    response = (
        f" response={clip(candidate.response_text, STEERING_RESPONSE_PREVIEW_CHARS)}"
        if candidate.response_text.strip()
        else ""
    )
    return (
        f"  {candidate.timestamp} disposition={candidate.label}{key}"
        f"{repeat_fragment} "
        f"text={clip(candidate.text, STEERING_TEXT_PREVIEW_CHARS)}{response}"
    )


def ask_line(candidate: RehydrationCandidate) -> str:
    key = f" key={candidate.key}" if candidate.key else ""
    repeat = repeat_count_fragment(candidate)
    return (
        f"  {candidate.label} {candidate.timestamp}{key}{repeat} {clip(candidate.text)}"
    )


def finals_lines(finals: list[RehydrationCandidate]) -> list[str]:
    ranked = sort_rehydration_candidates(finals)
    if not ranked:
        return []
    shown = ranked[:FINAL_ROW_LIMIT]
    overflow = len(ranked) - len(shown)
    lines = ["Latest Final", _final_line(shown[0], include_timestamp=False)]
    if len(shown) > 1:
        lines.append("Recent Finals")
        for candidate in shown[1:]:
            lines.append(_final_line(candidate, include_timestamp=True))
    if overflow:
        lines.append(f"  +{overflow} more final rows")
    return lines


def _final_line(candidate: RehydrationCandidate, *, include_timestamp: bool) -> str:
    repeat = repeat_count_fragment(candidate).strip()
    prefix = f"{candidate.timestamp} " if include_timestamp else ""
    if repeat:
        prefix = f"{prefix}{repeat} "
    return f"  {prefix}{clip(candidate.text)}"


def repeat_count_fragment(candidate: RehydrationCandidate) -> str:
    return f" repeat_count={candidate.count}" if candidate.count > 1 else ""


def recovery_lines(
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


def trajectory_lines(windows: list[SweepWindowPayload]) -> list[str]:
    summaries = [
        (window, summary)
        for window in windows
        if (summary := window_trajectory_summary(window))
    ]
    if not summaries:
        return []
    lines = ["Trajectory"]
    lines.extend(
        f"  window={window.index} from={window.label} {summary}"
        for window, summary in summaries
    )
    return lines


def window_trajectory_summary(window: SweepWindowPayload) -> str:
    parts: list[str] = []
    if window.finals:
        latest_final = window.finals[0]
        parts.append(f"final={clip(latest_final.text, COMMIT_PREVIEW_CHARS)}")
    if window.asks:
        latest_ask = window.asks[0]
        response = (
            f" response={clip(latest_ask.response_text, COMMIT_PREVIEW_CHARS)}"
            if latest_ask.response_text.strip()
            else ""
        )
        parts.append(
            f"steering={latest_ask.label} {latest_ask.timestamp} "
            f"{clip(latest_ask.text, COMMIT_PREVIEW_CHARS)}{response}"
        )
    if window.commits:
        latest_commit = window.commits[0]
        parts.append(
            f"commit={latest_commit.label} "
            f"{clip(latest_commit.text, COMMIT_PREVIEW_CHARS)}"
        )
    if parts:
        return "; ".join(parts)
    if _window_has_work(window):
        command_count = sum(turn.command_count for turn in window.turns)
        patch_count = sum(turn.patch_count for turn in window.turns)
        return (
            f"activity=turns={len(window.turns)} "
            f"commands={command_count} patches={patch_count}"
        )
    return ""


def _window_has_work(window: SweepWindowPayload) -> bool:
    return bool(window.turns or window.asks or window.finals or window.commits)


def activity_lines(
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
        shown_working_set = working_set[:WORKING_SET_LIMIT]
        overflow = len(working_set) - len(shown_working_set)
        lines.append("Working Set")
        for candidate in shown_working_set:
            lines.append(f"  {candidate.label} touches={candidate.count}")
        if overflow:
            lines.append(f"  +{overflow} more working-set rows")
    ranked_commits = sort_rehydration_candidates(commit_candidates)
    if ranked_commits:
        shown_commits = ranked_commits[:RECENT_COMMITS_LIMIT]
        overflow = len(ranked_commits) - len(shown_commits)
        lines.append("Recent Commits")
        for candidate in shown_commits:
            lines.append(
                f"  {candidate.timestamp} {candidate.label} "
                f"{clip(candidate.text, COMMIT_PREVIEW_CHARS)}"
            )
        if overflow:
            lines.append(f"  +{overflow} more commit rows")
    return lines
