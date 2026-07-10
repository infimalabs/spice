"""Task-plane candidate collection for session briefing."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Callable, Literal

from spice.errors import SpiceError
from spice.paths import repo_root_from_cwd

if TYPE_CHECKING:
    from spice.sessions.briefing import (
        RankKey,
        RehydrationCandidate,
        RehydrationCandidateKind,
    )

TASK_PLANE_PREVIEW_CHARS = 180
TASK_PLANE_RANK_NAME = "task_plane_state_then_urgency_recency"
TASK_PLANE_WEIGHTS = {
    "claim": 60,
    "posture": 55,
    "ready": 50,
    "review": 45,
    "completed": 30,
    "oops": 20,
}
TaskRow = dict[str, object]


@dataclass(frozen=True)
class TaskPlaneRows:
    active: tuple[TaskRow, ...]
    ready: tuple[TaskRow, ...]
    review: tuple[TaskRow, ...]
    blocked: tuple[TaskRow, ...]
    completed: tuple[TaskRow, ...]
    oops: tuple[TaskRow, ...]


def task_plane_rank_key(
    kind: str, urgency: float = 0.0, timestamp: str = ""
) -> tuple[int | float | str, ...]:
    return (TASK_PLANE_WEIGHTS[kind], urgency, timestamp)


def _candidate(
    *,
    kind: "RehydrationCandidateKind",
    timestamp: str,
    text: str,
    rank_name: str,
    rank_key: "RankKey",
    label: str = "",
    count: int = 0,
    key: str = "",
    project: str = "",
) -> "RehydrationCandidate":
    from spice.sessions.briefing import RehydrationCandidate

    return RehydrationCandidate(
        kind=kind,
        timestamp=timestamp,
        text=text,
        rank_name=rank_name,
        rank_key=rank_key,
        label=label,
        count=count,
        key=key,
        project=project,
    )


def _clip(text: str | None, limit: int) -> str:
    from spice.sessions.briefing import clip

    return clip(text, limit)


def collect_task_plane_candidates() -> list["RehydrationCandidate"]:
    if repo_root_from_cwd() is None:
        return []
    try:
        from spice.tasks import alloc, identity, tw

        actor = tw.current_actor()
        snapshot = alloc.briefing_snapshot(actor)
        rows = classify_task_plane_rows(
            list(snapshot.rows),
            visible_uuids=snapshot.visible_uuids,
            is_hidden=alloc.is_hidden,
            is_oops=alloc.is_oops,
        )
    except (OSError, RuntimeError, SpiceError, SystemExit):
        return []

    candidates: list[RehydrationCandidate] = []
    own_active = [row for row in rows.active if str(row.get("claim_by") or "") == actor]
    if own_active:
        claimed = max(own_active, key=_task_row_timestamp)
        candidates.append(
            _task_claim_candidate(claimed, identity.render_handle(claimed))
        )
    if rows.active or rows.ready or rows.review or rows.blocked or rows.oops:
        candidates.append(
            _task_posture_candidate(
                active=len(rows.active),
                ready=len(rows.ready),
                review=len(rows.review),
                blocked=len(rows.blocked),
                oops=len(rows.oops),
            )
        )
    candidates.extend(
        _task_queue_candidate("ready", row, identity.render_handle(row))
        for row in rows.ready
    )
    candidates.extend(
        _task_queue_candidate("review", row, identity.render_handle(row))
        for row in rows.review
    )
    candidates.extend(
        _task_completed_candidate(row, identity.render_handle(row))
        for row in rows.completed
    )
    if rows.oops:
        top = max(rows.oops, key=_task_urgency)
        candidates.append(
            _task_oops_candidate(top, identity.render_handle(top), len(rows.oops))
        )
    return candidates


def classify_task_plane_rows(
    rows: list[TaskRow],
    *,
    visible_uuids: frozenset[str],
    is_hidden: Callable[[TaskRow], bool],
    is_oops: Callable[[TaskRow], bool],
) -> TaskPlaneRows:
    now = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    status_by_uuid = {
        _task_field(row, "uuid"): _task_field(row, "status") for row in rows
    }
    visible = [
        row
        for row in rows
        if _task_field(row, "uuid") in visible_uuids and not is_hidden(row)
    ]
    pending = [row for row in visible if _task_field(row, "status") == "pending"]
    active = [
        row
        for row in pending
        if _task_field(row, "start") and _task_field(row, "claim_by")
    ]
    ready = [
        row
        for row in pending
        if _task_field(row, "phase") != "review"
        and not _task_field(row, "claim_by")
        and _task_is_ready(row, now=now, status_by_uuid=status_by_uuid)
    ]
    review = [
        row
        for row in pending
        if _task_field(row, "phase") == "review" and not _task_field(row, "claim_by")
    ]
    blocked = [
        row for row in pending if _task_is_blocked(row, status_by_uuid=status_by_uuid)
    ]
    completed = [row for row in visible if _task_field(row, "status") == "completed"]
    oops = [
        row
        for row in rows
        if _task_field(row, "status") in ("pending", "waiting") and is_oops(row)
    ]
    return TaskPlaneRows(
        active=tuple(active),
        ready=tuple(ready),
        review=tuple(review),
        blocked=tuple(blocked),
        completed=tuple(completed),
        oops=tuple(oops),
    )


def _task_is_ready(row: TaskRow, *, now: str, status_by_uuid: dict[str, str]) -> bool:
    return bool(
        _task_field(row, "status") == "pending"
        and not _task_field(row, "start")
        and not _task_is_blocked(row, status_by_uuid=status_by_uuid)
        and _task_time_has_arrived(row, "wait", now=now)
        and _task_time_has_arrived(row, "scheduled", now=now)
    )


def _task_is_blocked(row: TaskRow, *, status_by_uuid: dict[str, str]) -> bool:
    depends = row.get("depends") or []
    dependency_uuids = depends if isinstance(depends, list) else [depends]
    return any(
        status_by_uuid.get(str(dependency_uuid)) not in ("completed", "deleted")
        for dependency_uuid in dependency_uuids
    )


def _task_time_has_arrived(row: TaskRow, key: str, *, now: str) -> bool:
    value = _task_field(row, key)
    return not value or value <= now


def _task_claim_candidate(
    row: dict[str, object], handle: str
) -> "RehydrationCandidate":
    timestamp = _task_row_timestamp(row)
    return _candidate(
        kind="task_plane",
        timestamp=timestamp,
        text=(
            f"claim {handle} phase={_task_field(row, 'phase') or '-'} "
            f"project={_task_field(row, 'project') or '-'} "
            f"acceptance={_clip(_task_field(row, 'acceptance'), TASK_PLANE_PREVIEW_CHARS)}"
        ),
        rank_name=TASK_PLANE_RANK_NAME,
        rank_key=task_plane_rank_key("claim", _task_urgency(row), timestamp),
        label=handle,
        project=_task_field(row, "project"),
    )


def _task_posture_candidate(
    *, active: int, ready: int, review: int, blocked: int, oops: int
) -> "RehydrationCandidate":
    return _candidate(
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
) -> "RehydrationCandidate":
    timestamp = _task_row_timestamp(row)
    urgency = _task_urgency(row)
    return _candidate(
        kind="task_plane",
        timestamp=timestamp,
        text=(
            f"{state} {handle} urgency={urgency:.2f} "
            f"{_clip(_task_field(row, 'description'), TASK_PLANE_PREVIEW_CHARS)}"
        ),
        rank_name=TASK_PLANE_RANK_NAME,
        rank_key=task_plane_rank_key(state, urgency, timestamp),
        label=handle,
    )


def _task_completed_candidate(
    row: dict[str, object], handle: str
) -> "RehydrationCandidate":
    timestamp = _task_row_timestamp(row)
    return _candidate(
        kind="task_plane",
        timestamp=timestamp,
        text=(
            f"completed {handle} validation="
            f"{_clip(_task_field(row, 'validation'), TASK_PLANE_PREVIEW_CHARS)}"
        ),
        rank_name=TASK_PLANE_RANK_NAME,
        rank_key=task_plane_rank_key("completed", timestamp=timestamp),
        label=handle,
    )


def _task_oops_candidate(
    row: dict[str, object], handle: str, total: int
) -> "RehydrationCandidate":
    timestamp = _task_row_timestamp(row)
    overflow = f" total={total}" if total > 1 else ""
    return _candidate(
        kind="task_plane",
        timestamp=timestamp,
        text=(
            f"oops {handle}{overflow} "
            f"{_clip(_task_field(row, 'description'), TASK_PLANE_PREVIEW_CHARS)}"
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
