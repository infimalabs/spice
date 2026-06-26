"""Completed-task sizing report built from structured task signals."""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

from spice.tasks import config, identity, ops, tw

MINUTE_SECONDS = 60
HOUR_SECONDS = 60 * MINUTE_SECONDS
ELAPSED_SMALL_SECONDS = 15 * MINUTE_SECONDS
ELAPSED_MEDIUM_SECONDS = HOUR_SECONDS
ELAPSED_LARGE_SECONDS = 3 * HOUR_SECONDS
COMMAND_MEDIUM_MAX = 3
DEPENDENCY_COMPLEXITY_MIN = 3
SCORE_SMALL_MAX = 1
SCORE_MEDIUM_MAX = 3
SCORE_LARGE_MAX = 5


@dataclass(frozen=True)
class TaskLifecycleEvent:
    kind: str
    ts: float


@dataclass(frozen=True)
class SizingComponent:
    name: str
    points: int
    detail: str


@dataclass(frozen=True)
class _CommitSignal:
    count: int
    source: str


@dataclass(frozen=True)
class TaskSizing:
    handle: str
    label: str
    score: int
    title: str
    project: str
    components: tuple[SizingComponent, ...]


def completed_task_sizing_report(
    *, limit: int | None = None, project: str | None = None
) -> str:
    rows = completed_task_sizing_rows(limit=limit, project=project)
    if not rows:
        return "no completed tasks"
    return "\n".join(render_task_sizing(row) for row in rows)


def completed_task_sizing_rows(
    *,
    limit: int | None = None,
    project: str | None = None,
    rows: list[dict[str, Any]] | None = None,
    events_by_task: dict[str, tuple[TaskLifecycleEvent, ...]] | None = None,
) -> list[TaskSizing]:
    selected_rows = tw.export(["status:completed"]) if rows is None else rows
    if project:
        selected_rows = [row for row in selected_rows if _project_matches(row, project)]
    selected_rows = sorted(selected_rows, key=_completed_sort_key, reverse=True)
    if limit is not None:
        selected_rows = selected_rows[:limit]
    events = events_by_task
    if events is None:
        events = _events_by_task_id([_uuid(row) for row in selected_rows])
    return [
        size_completed_task(row, events=events.get(_uuid(row), ()))
        for row in selected_rows
    ]


def size_completed_task(
    row: dict[str, Any],
    *,
    events: tuple[TaskLifecycleEvent, ...] = (),
) -> TaskSizing:
    components = (
        _elapsed_component(row, events),
        _command_component(row),
        _validation_component(row),
        _review_component(row),
        _blocked_component(row),
        _metadata_component(row),
    )
    score = sum(component.points for component in components)
    return TaskSizing(
        handle=identity.render_handle(row),
        label=_size_label(score),
        score=score,
        title=str(row.get("description") or ""),
        project=str(row.get("project") or ""),
        components=components,
    )


def render_task_sizing(report: TaskSizing) -> str:
    components = " ".join(
        f"{component.name}=+{component.points}({component.detail})"
        for component in report.components
    )
    return (
        f"{report.handle} size={report.label} size_score={report.score} "
        f"project={report.project or '-'} {components} title={report.title}"
    )


def _events_by_task_id(
    task_ids: list[str],
) -> dict[str, tuple[TaskLifecycleEvent, ...]]:
    ids = [task_id for task_id in task_ids if task_id]
    if not ids:
        return {}
    from spice.serve.team.store import ServeTeamStore

    placeholders = ", ".join("?" for _item in ids)
    with ServeTeamStore().connect() as connection:
        rows = connection.execute(
            "SELECT task_id, kind, ts FROM task_events "
            f"WHERE task_id IN ({placeholders}) ORDER BY ts, rowid",
            ids,
        ).fetchall()
    by_task: dict[str, list[TaskLifecycleEvent]] = {}
    for row in rows:
        by_task.setdefault(str(row["task_id"]), []).append(
            TaskLifecycleEvent(kind=str(row["kind"]), ts=float(row["ts"]))
        )
    return {task_id: tuple(events) for task_id, events in by_task.items()}


def _elapsed_component(
    row: dict[str, Any], events: tuple[TaskLifecycleEvent, ...]
) -> SizingComponent:
    event_seconds = _event_active_seconds(events)
    if event_seconds is not None:
        return SizingComponent(
            "elapsed",
            _elapsed_points(event_seconds),
            f"task_events:{int(event_seconds)}s",
        )
    field_seconds = _row_elapsed_seconds(row)
    if field_seconds is not None:
        return SizingComponent(
            "elapsed",
            _elapsed_points(field_seconds),
            f"task_fields_entry_end:{int(field_seconds)}s",
        )
    return SizingComponent("elapsed", 0, "no_structured_elapsed_signal")


def _event_active_seconds(events: tuple[TaskLifecycleEvent, ...]) -> float | None:
    total = 0.0
    active_start: float | None = None
    for event in events:
        if event.kind == "claim":
            active_start = event.ts
            continue
        if active_start is None:
            continue
        if event.kind in {"phaseAdvance", "review", "complete"}:
            total += max(0.0, event.ts - active_start)
            active_start = None
    return total if total > 0 else None


def _row_elapsed_seconds(row: dict[str, Any]) -> float | None:
    start = _parse_task_time(str(row.get("entry") or ""))
    end = _parse_task_time(str(row.get("end") or ""))
    if start is None or end is None:
        return None
    return max(0.0, (end - start).total_seconds())


def _parse_task_time(raw: str) -> datetime | None:
    value = raw.strip()
    if not value:
        return None
    for fmt in (
        "%Y%m%dT%H%M%S%fZ",
        "%Y%m%dT%H%M%SZ",
        "%Y-%m-%dT%H:%M:%S.%fZ",
        "%Y-%m-%dT%H:%M:%SZ",
    ):
        try:
            return datetime.strptime(value, fmt).replace(tzinfo=UTC)
        except ValueError:
            pass
    return None


def _elapsed_points(seconds: float) -> int:
    if seconds < ELAPSED_SMALL_SECONDS:
        return 0
    if seconds < ELAPSED_MEDIUM_SECONDS:
        return 1
    if seconds < ELAPSED_LARGE_SECONDS:
        return 2
    return 3


def _command_component(row: dict[str, Any]) -> SizingComponent:
    signal = _commit_signal(row)
    if signal is None:
        return SizingComponent("commands", 0, "no_structured_command_signal")
    count = signal.count
    if count <= 1:
        points = 0
    elif count <= COMMAND_MEDIUM_MAX:
        points = 1
    else:
        points = 2
    return SizingComponent("commands", points, f"git_commits:{count}:{signal.source}")


def _commit_signal(row: dict[str, Any]) -> _CommitSignal | None:
    signal = _commit_signal_from(
        row,
        before_key="done_upstream_head",
        after_key="done_head",
        source="done_upstream_head..done_head",
    )
    if signal is None:
        return None
    if signal.count == 0 and _has_review_metadata(row):
        return None
    return signal


def _commit_signal_from(
    row: dict[str, Any],
    *,
    before_key: str,
    after_key: str,
    source: str,
) -> _CommitSignal | None:
    before = str(row.get(before_key) or "")
    after = str(row.get(after_key) or "")
    if not before or not after:
        return None
    count = _git_rev_count(before, after)
    if count is None:
        return None
    return _CommitSignal(count=count, source=source)


def _git_rev_count(before: str, after: str) -> int | None:
    result = subprocess.run(
        ["git", "rev-list", "--count", f"{before}..{after}"],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        return None
    try:
        return int(result.stdout.strip() or "0")
    except ValueError:
        return None


def _has_review_metadata(row: dict[str, Any]) -> bool:
    return any(
        str(row.get(key) or "").strip()
        for key in ("review_author", "review_by", "review_at", "review_finding")
    )


def _validation_component(row: dict[str, Any]) -> SizingComponent:
    tags = [str(tag) for tag in row.get("tags") or []]
    gate_tags = sorted(tag for tag in tags if tag.startswith("gate:"))
    if gate_tags:
        return SizingComponent("validation", 1, f"quality_gates:{','.join(gate_tags)}")
    return SizingComponent("validation", 0, "no_structured_validation_signal")


def _review_component(row: dict[str, Any]) -> SizingComponent:
    finding = str(row.get("review_finding") or "").strip().casefold()
    if finding and finding != "clean":
        return SizingComponent("review", 2, f"review_finding:{finding}")
    return SizingComponent("review", 0, f"review_finding:{finding or 'clean'}")


def _blocked_component(row: dict[str, Any]) -> SizingComponent:
    tags = {str(tag).casefold() for tag in row.get("tags") or []}
    signals: list[str] = []
    for tag in ("blocked", "stale", "oops"):
        if tag in tags:
            signals.append(f"tag:{tag}")
    status = str(row.get("status") or "").casefold()
    if status in {"blocked", "stale", "waiting"}:
        signals.append(f"status:{status}")
    project = str(row.get("project") or "").casefold()
    if project == config.OOPS_PROJECT or project.startswith(f"{config.OOPS_PROJECT}."):
        signals.append(f"project:{config.OOPS_PROJECT}")
    phase = str(row.get("phase") or "").casefold()
    if phase == "oops":
        signals.append("phase:oops")
    if not signals:
        return SizingComponent("blocked", 0, "no_structured_blocker_signal")
    return SizingComponent("blocked", 2, ",".join(dict.fromkeys(signals)))


def _metadata_component(row: dict[str, Any]) -> SizingComponent:
    points = 0
    details: list[str] = []
    depends = row.get("depends") or []
    if len(depends) >= DEPENDENCY_COMPLEXITY_MIN:
        points += 1
        details.append(f"depends:{len(depends)}")
    phases = ops.phases_of(row)
    if "verify" in phases:
        points += 1
        details.append("phase:verify")
    if not details:
        details.append("flow:default")
    return SizingComponent("metadata", points, ",".join(details))


def _size_label(score: int) -> str:
    if score <= SCORE_SMALL_MAX:
        return "S"
    if score <= SCORE_MEDIUM_MAX:
        return "M"
    if score <= SCORE_LARGE_MAX:
        return "L"
    return "XL"


def _completed_sort_key(row: dict[str, Any]) -> str:
    return str(row.get("end") or row.get("modified") or row.get("entry") or "")


def _project_matches(row: dict[str, Any], project: str) -> bool:
    row_project = str(row.get("project") or "")
    return row_project == project or row_project.startswith(f"{project}.")


def _uuid(row: dict[str, Any]) -> str:
    return str(row.get("uuid") or "")
