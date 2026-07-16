"""Evidence-backed completed-task sizing report."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from spice.tasks import claimstate, effort, identity, tw

MINUTE_SECONDS = 60
HOUR_SECONDS = 60 * MINUTE_SECONDS
ELAPSED_SMALL_SECONDS = 15 * MINUTE_SECONDS
ELAPSED_MEDIUM_SECONDS = HOUR_SECONDS
ELAPSED_LARGE_SECONDS = 3 * HOUR_SECONDS
DEPENDENCY_COMPLEXITY_MIN = 3
SCORE_SMALL_MAX = 1
SCORE_MEDIUM_MAX = 3
SCORE_LARGE_MAX = 5


@dataclass(frozen=True)
class SizingComponent:
    name: str
    points: int | None
    detail: str


@dataclass(frozen=True)
class SizingEvidence:
    name: str
    status: str
    detail: str


@dataclass(frozen=True)
class TaskSizing:
    handle: str
    label: str | None
    score: int | None
    title: str
    project: str
    components: tuple[SizingComponent, ...]
    evidence: tuple[SizingEvidence, ...]


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
    effort_windows_by_task: dict[str, tuple[effort.PhaseEffortWindow, ...]]
    | None = None,
) -> list[TaskSizing]:
    selected_rows = tw.export(["status:completed"]) if rows is None else rows
    if project:
        selected_rows = [row for row in selected_rows if _project_matches(row, project)]
    selected_rows = sorted(selected_rows, key=_completed_sort_key, reverse=True)
    if limit is not None:
        selected_rows = selected_rows[:limit]
    windows = effort_windows_by_task
    if windows is None:
        windows = _effort_windows_by_task(selected_rows)
    return [
        size_completed_task(row, windows=windows.get(_uuid(row), ()))
        for row in selected_rows
    ]


def size_completed_task(
    row: dict[str, Any],
    *,
    windows: tuple[effort.PhaseEffortWindow, ...] = (),
) -> TaskSizing:
    components = (
        _elapsed_component(windows),
        _review_component(row),
        _metadata_component(row),
    )
    available = all(component.points is not None for component in components)
    score = (
        sum(
            component.points for component in components if component.points is not None
        )
        if available
        else None
    )
    return TaskSizing(
        handle=identity.render_handle(row),
        label=_size_label(score) if score is not None else None,
        score=score,
        title=str(row.get("description") or ""),
        project=str(row.get("project") or ""),
        components=components,
        evidence=(_validation_evidence(row),),
    )


def render_task_sizing(report: TaskSizing) -> str:
    components = " ".join(_render_component(item) for item in report.components)
    evidence = " ".join(_render_evidence(item) for item in report.evidence)
    label = report.label if report.label is not None else "unavailable"
    score = str(report.score) if report.score is not None else "unavailable"
    return (
        f"{report.handle} size={label} size_score={score} "
        f"project={report.project or '-'} {components} {evidence} title={report.title}"
    )


def _render_component(component: SizingComponent) -> str:
    if component.points is None:
        return f"{component.name}=unavailable({component.detail})"
    return f"{component.name}=+{component.points}({component.detail})"


def _render_evidence(item: SizingEvidence) -> str:
    return f"{item.name}={item.status}({item.detail})"


def _effort_windows_by_task(
    rows: list[dict[str, Any]],
) -> dict[str, tuple[effort.PhaseEffortWindow, ...]]:
    grouped: dict[str, list[effort.PhaseEffortWindow]] = {}
    for window in effort.phase_effort_windows_for_tasks(rows):
        grouped.setdefault(window.task_id, []).append(window)
    return {task_id: tuple(windows) for task_id, windows in grouped.items()}


def _elapsed_component(
    windows: tuple[effort.PhaseEffortWindow, ...],
) -> SizingComponent:
    if not windows:
        return SizingComponent("elapsed", None, "no_phase_effort_windows")
    seconds = 0.0
    for window in windows:
        wall_seconds = effort.phase_effort_wall_seconds(window)
        if wall_seconds is None:
            return SizingComponent("elapsed", None, "incomplete_phase_effort_window")
        seconds += wall_seconds
    return SizingComponent(
        "elapsed",
        _elapsed_points(seconds),
        f"phase_effort_windows:{int(seconds)}s",
    )


def _elapsed_points(seconds: float) -> int:
    if seconds < ELAPSED_SMALL_SECONDS:
        return 0
    if seconds < ELAPSED_MEDIUM_SECONDS:
        return 1
    if seconds < ELAPSED_LARGE_SECONDS:
        return 2
    return 3


def _review_component(row: dict[str, Any]) -> SizingComponent:
    if "review" not in claimstate.phases_of(row):
        return SizingComponent("review", 0, "phase:not_required")
    finding = str(row.get("review_finding") or "").strip().casefold()
    if not finding:
        return SizingComponent("review", None, "no_review_finding")
    if finding == "clean":
        return SizingComponent("review", 0, "review_finding:clean")
    return SizingComponent("review", 2, "review_finding:non_clean")


def _metadata_component(row: dict[str, Any]) -> SizingComponent:
    points = 0
    details: list[str] = []
    depends = row.get("depends") or []
    if len(depends) >= DEPENDENCY_COMPLEXITY_MIN:
        points += 1
        details.append(f"depends:{len(depends)}")
    phases = claimstate.phases_of(row)
    if "verify" in phases:
        points += 1
        details.append("phase:verify")
    if not details:
        details.append("flow:default")
    return SizingComponent("metadata", points, ",".join(details))


def _validation_evidence(row: dict[str, Any]) -> SizingEvidence:
    if str(row.get("validation") or "").strip():
        return SizingEvidence("validation", "recorded", "completion_validation")
    return SizingEvidence("validation", "unavailable", "no_completion_validation")


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
