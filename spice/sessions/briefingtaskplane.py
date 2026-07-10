"""Task-plane candidate collection for session briefing."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Literal

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
    actor: str
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
        from spice.tasks import identity

        rows = _collect_task_plane_rows()
    except (OSError, RuntimeError, SpiceError, SystemExit):
        return []

    candidates: list[RehydrationCandidate] = []
    own_active = [
        row for row in rows.active if _task_field(row, "claim_by") == rows.actor
    ]
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


def _collect_task_plane_rows() -> TaskPlaneRows:
    from spice.tasks import alloc, config, lanes, tw

    actor = tw.current_actor()
    route = lanes.team_route_for_actor(actor)
    scope = alloc.effective_route_filter_args(actor, route)
    taskrc = config.bootstrap()
    inventory = tw.export(
        [
            "(",
            "(",
            *scope,
            ")",
            "or",
            f"project:{config.OOPS_PROJECT}",
            ")",
        ],
        taskrc=taskrc,
    )
    ready = tw.export(["status:pending", "+READY", "-ACTIVE", *scope], taskrc=taskrc)
    blocked = tw.export(["status:pending", "+BLOCKED", *scope], taskrc=taskrc)
    visible = [row for row in inventory if not alloc.is_hidden(row)]
    return TaskPlaneRows(
        actor=actor,
        active=tuple(row for row in visible if _is_active(row)),
        ready=tuple(row for row in ready if _is_ready(row)),
        review=tuple(row for row in visible if _is_review(row)),
        blocked=tuple(row for row in blocked if not alloc.is_hidden(row)),
        completed=tuple(row for row in visible if _is_completed(row)),
        oops=tuple(row for row in inventory if _is_open_oops(row)),
    )


def _is_active(row: TaskRow) -> bool:
    return _task_field(row, "status") == "pending" and bool(
        _task_field(row, "claim_by")
    )


def _is_ready(row: TaskRow) -> bool:
    from spice.tasks import alloc

    return (
        not alloc.is_hidden(row)
        and not _task_field(row, "claim_by")
        and _task_field(row, "phase") != "review"
    )


def _is_review(row: TaskRow) -> bool:
    return (
        _task_field(row, "status") == "pending"
        and _task_field(row, "phase") == "review"
        and not _task_field(row, "claim_by")
    )


def _is_completed(row: TaskRow) -> bool:
    return _task_field(row, "status") == "completed"


def _is_open_oops(row: TaskRow) -> bool:
    from spice.tasks import alloc

    return alloc.is_oops(row) and _task_field(row, "status") in (
        "pending",
        "waiting",
    )


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
