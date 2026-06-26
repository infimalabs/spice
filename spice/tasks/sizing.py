"""Completed-task sizing report."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

from spice.tasks import config, identity, ops, tw

_BLOCKED_TAGS = frozenset({"blocked", "stale", "oops"})
_BLOCKED_STATUSES = frozenset({"blocked", "stale", "waiting"})
ELAPSED_SHORT_MINUTES = 15
ELAPSED_MEDIUM_MINUTES = 60
ELAPSED_LONG_MINUTES = 180


@dataclass(frozen=True)
class SizingComponent:
    name: str
    points: int
    detail: str


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
    *, limit: int | None = None, project: str | None = None
) -> list[TaskSizing]:
    rows = tw.export(["status:completed"])
    if project:
        rows = [row for row in rows if _project_matches(row, project)]
    rows = sorted(rows, key=_completed_sort_key, reverse=True)
    if limit is not None:
        rows = rows[:limit]
    return [size_completed_task(row) for row in rows]


def size_completed_task(row: dict[str, Any]) -> TaskSizing:
    components = (
        _elapsed_component(row),
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


def _elapsed_component(row: dict[str, Any]) -> SizingComponent:
    start = _parse_task_time(str(row.get("claim_at") or row.get("start") or ""))
    done = _first_annotation_time(row, prefix="validation:") or _parse_task_time(
        str(row.get("end") or "")
    )
    entry = _parse_task_time(str(row.get("entry") or ""))
    if start is None or (done is not None and done < start):
        start = entry
    minutes = _elapsed_minutes(start, done)
    if minutes is None:
        return SizingComponent("elapsed", 0, "unknown")
    if minutes < ELAPSED_SHORT_MINUTES:
        points = 0
    elif minutes < ELAPSED_MEDIUM_MINUTES:
        points = 1
    elif minutes < ELAPSED_LONG_MINUTES:
        points = 2
    else:
        points = 3
    return SizingComponent("elapsed", points, f"{minutes}m")


def _validation_component(row: dict[str, Any]) -> SizingComponent:
    records = len(_validation_items(row))
    if records <= 1:
        points = 0
    elif records <= 3:
        points = 1
    else:
        points = 2
    return SizingComponent("validation", points, f"records={records}")


def _review_component(row: dict[str, Any]) -> SizingComponent:
    review_annotations = [
        annotation
        for annotation in row.get("annotations") or []
        if str(annotation.get("description") or "").startswith("review:")
    ]
    finding = str(row.get("review_finding") or "").strip().casefold()
    changed = finding not in ("", "clean")
    if not changed:
        points = 0
    elif len(review_annotations) <= 1:
        points = 2
    else:
        points = 3
    detail = f"finding={finding or '-'},cycles={max(1, len(review_annotations))}"
    return SizingComponent("review", points, detail)


def _blocked_component(row: dict[str, Any]) -> SizingComponent:
    signals: list[str] = []
    tags = {str(tag).casefold() for tag in row.get("tags") or []}
    signals.extend(f"tag:{tag}" for tag in sorted(tags & _BLOCKED_TAGS))

    status = str(row.get("status") or "").casefold()
    if status in _BLOCKED_STATUSES:
        signals.append(f"status:{status}")

    project = str(row.get("project") or "").casefold()
    if project == config.OOPS_PROJECT or project.startswith(f"{config.OOPS_PROJECT}."):
        signals.append(f"project:{config.OOPS_PROJECT}")

    phase = str(row.get("phase") or "").casefold()
    if phase == "oops":
        signals.append("phase:oops")

    if not signals:
        return SizingComponent("blocked", 0, "none")
    return SizingComponent("blocked", 2, ",".join(dict.fromkeys(signals)))


def _metadata_component(row: dict[str, Any]) -> SizingComponent:
    points = 0
    details: list[str] = []
    deps = len(row.get("depends") or [])
    if deps > 2:
        points += 1
    details.append(f"deps={deps}")
    phases = ops.phases_of(row)
    if "verify" in phases:
        points += 1
        details.append("verify")
    return SizingComponent("metadata", points, ",".join(details))


def _validation_items(row: dict[str, Any]) -> list[str]:
    parts = [
        part.strip()
        for part in str(row.get("validation") or "").split(" | ")
        if part.strip()
    ]
    parts.extend(
        str(annotation.get("description") or "").removeprefix("validation:").strip()
        for annotation in row.get("annotations") or []
        if str(annotation.get("description") or "").startswith("validation:")
    )
    return list(dict.fromkeys(part for part in parts if part))


def _first_annotation_time(row: dict[str, Any], *, prefix: str) -> datetime | None:
    for annotation in row.get("annotations") or []:
        if str(annotation.get("description") or "").startswith(prefix):
            parsed = _parse_task_time(str(annotation.get("entry") or ""))
            if parsed is not None:
                return parsed
    return None


def _elapsed_minutes(start: datetime | None, end: datetime | None) -> int | None:
    if start is None or end is None or end < start:
        return None
    return int((end - start).total_seconds() // 60)


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


def _size_label(score: int) -> str:
    if score <= 1:
        return "S"
    if score <= 3:
        return "M"
    if score <= 5:
        return "L"
    return "XL"


def _completed_sort_key(row: dict[str, Any]) -> str:
    return str(row.get("end") or row.get("modified") or row.get("entry") or "")


def _project_matches(row: dict[str, Any], project: str) -> bool:
    row_project = str(row.get("project") or "")
    return row_project == project or row_project.startswith(f"{project}.")
