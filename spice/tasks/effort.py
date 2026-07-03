"""Task phase effort window construction from lifecycle events."""

from __future__ import annotations

import sqlite3
from contextlib import AbstractContextManager
from dataclasses import dataclass
from typing import Any, Iterable, Mapping, Protocol

from spice.serve.team.ids import thread_id_for_actor
from spice.serve.team.store import ServeTeamStore
from spice.tasks import identity, ops

PARTIAL_MISSING_START = "missing_start"
PARTIAL_MISSING_END = "missing_end"
PARTIAL_HANDOFF = "handoff"


@dataclass(frozen=True, slots=True)
class PhaseEffortWindow:
    task_id: str
    handle: str
    title: str
    phase: str
    phase_index: int
    actor_id: str
    thread_id: str
    team_id: str
    driver: str
    model: str
    effort: str
    started_at: float | None
    ended_at: float | None
    partial_markers: tuple[str, ...] = ()

    @property
    def partial(self) -> bool:
        return bool(self.partial_markers)

    @property
    def wall_seconds(self) -> float | None:
        if self.started_at is None or self.ended_at is None:
            return None
        return max(0.0, self.ended_at - self.started_at)


class _EffortWindowStore(Protocol):
    def connect(self) -> AbstractContextManager[sqlite3.Connection]: ...


@dataclass(frozen=True, slots=True)
class _TaskShape:
    task_id: str
    handle: str
    title: str
    phases: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class _TaskLifecycleEvent:
    rowid: int
    ts: float
    kind: str
    task_id: str
    agent_id: str
    team_id: str


@dataclass(frozen=True, slots=True)
class _AgentEffortTags:
    actor_id: str
    thread_id: str
    driver: str
    model: str
    effort: str


@dataclass(frozen=True, slots=True)
class _OpenWindow:
    shape: _TaskShape
    phase_index: int
    event: _TaskLifecycleEvent


def phase_effort_windows_for_tasks(
    task_rows: Iterable[Mapping[str, Any]],
    *,
    store: _EffortWindowStore | None = None,
) -> tuple[PhaseEffortWindow, ...]:
    """Return deterministic per-task phase windows from stored lifecycle facts.

    ``task_rows`` supplies the durable Taskwarrior task shape: uuid, handle,
    title, and phase flow. Lifecycle timing and actor/team attribution come
    from ``ServeTeamStore.task_events``.
    """

    shapes = _task_shapes(task_rows)
    if not shapes:
        return ()
    resolved_store = store or ServeTeamStore()
    with resolved_store.connect() as connection:
        events = _task_lifecycle_events_locked(connection, tuple(shapes))
        tags = _agent_effort_tags_locked(
            connection, tuple(dict.fromkeys(event.agent_id for event in events))
        )
    windows: list[PhaseEffortWindow] = []
    for task_id in shapes:
        windows.extend(
            _windows_for_task(
                shapes[task_id],
                [event for event in events if event.task_id == task_id],
                tags,
            )
        )
    return tuple(
        sorted(
            windows,
            key=lambda window: (
                _sort_time(window),
                window.task_id,
                window.phase_index,
                window.actor_id,
                window.phase,
            ),
        )
    )


def _task_shapes(task_rows: Iterable[Mapping[str, Any]]) -> dict[str, _TaskShape]:
    shapes: dict[str, _TaskShape] = {}
    for raw in task_rows:
        row = dict(raw)
        task_id = str(row.get("uuid") or "").strip()
        if not task_id:
            continue
        phases = tuple(ops.phases_of(row))
        if not phases and str(row.get("phase") or "").strip():
            phases = (str(row.get("phase") or "").strip(),)
        shapes[task_id] = _TaskShape(
            task_id=task_id,
            handle=identity.render_handle(row),
            title=str(row.get("description") or ""),
            phases=phases,
        )
    return shapes


def _task_lifecycle_events_locked(
    connection: sqlite3.Connection, task_ids: tuple[str, ...]
) -> tuple[_TaskLifecycleEvent, ...]:
    if not task_ids:
        return ()
    rows = connection.execute(
        "SELECT rowid, ts, kind, task_id, agent_id, team_id FROM task_events "
        f"WHERE task_id IN ({_placeholders(task_ids)}) "
        "ORDER BY task_id, ts, rowid",
        task_ids,
    ).fetchall()
    return tuple(
        _TaskLifecycleEvent(
            rowid=int(row["rowid"]),
            ts=float(row["ts"] or 0.0),
            kind=str(row["kind"] or ""),
            task_id=str(row["task_id"] or ""),
            agent_id=str(row["agent_id"] or ""),
            team_id=str(row["team_id"] or ""),
        )
        for row in rows
    )


def _agent_effort_tags_locked(
    connection: sqlite3.Connection, agent_ids: tuple[str, ...]
) -> dict[str, _AgentEffortTags]:
    if not agent_ids:
        return {}
    rows = connection.execute(
        "SELECT actor_id, thread_id, actual_driver, actual_model, actual_effort, "
        "desired_driver, desired_model, desired_effort "
        "FROM agent_identities "
        f"WHERE actor_id IN ({_placeholders(agent_ids)})",
        agent_ids,
    ).fetchall()
    tags = {
        str(row["actor_id"]): _AgentEffortTags(
            actor_id=str(row["actor_id"]),
            thread_id=str(row["thread_id"] or "")
            or thread_id_for_actor(str(row["actor_id"])),
            driver=str(row["actual_driver"] or row["desired_driver"] or ""),
            model=str(row["actual_model"] or row["desired_model"] or ""),
            effort=str(row["actual_effort"] or row["desired_effort"] or ""),
        )
        for row in rows
    }
    for agent_id in agent_ids:
        tags.setdefault(
            agent_id,
            _AgentEffortTags(
                actor_id=agent_id,
                thread_id=thread_id_for_actor(agent_id),
                driver="",
                model="",
                effort="",
            ),
        )
    return tags


def _windows_for_task(
    shape: _TaskShape,
    events: list[_TaskLifecycleEvent],
    tags: dict[str, _AgentEffortTags],
) -> list[PhaseEffortWindow]:
    windows: list[PhaseEffortWindow] = []
    open_window: _OpenWindow | None = None
    phase_index = 0
    closed_phase_indexes: set[int] = set()
    for event in events:
        if event.kind == "claim":
            if open_window is not None:
                windows.append(
                    _close_window(
                        open_window,
                        event.ts,
                        tags,
                        markers=(PARTIAL_HANDOFF,),
                    )
                )
            open_window = _OpenWindow(shape, phase_index, event)
            closed_phase_indexes.discard(phase_index)
            continue
        if event.kind == "phaseAdvance":
            if open_window is not None:
                windows.append(_close_window(open_window, event.ts, tags))
                open_window = None
            else:
                windows.append(_missing_start_window(shape, phase_index, event, tags))
            closed_phase_indexes.add(phase_index)
            phase_index += 1
            continue
        if event.kind == "review":
            if open_window is not None:
                windows.append(_close_window(open_window, event.ts, tags))
                open_window = None
            else:
                windows.append(_missing_start_window(shape, phase_index, event, tags))
            closed_phase_indexes.add(phase_index)
            continue
        if event.kind == "complete":
            if open_window is not None:
                windows.append(_close_window(open_window, event.ts, tags))
                open_window = None
                closed_phase_indexes.add(phase_index)
            elif phase_index not in closed_phase_indexes:
                windows.append(_missing_start_window(shape, phase_index, event, tags))
                closed_phase_indexes.add(phase_index)
    if open_window is not None:
        windows.append(
            _close_window(
                open_window,
                None,
                tags,
                markers=(PARTIAL_MISSING_END,),
            )
        )
    return windows


def _close_window(
    window: _OpenWindow,
    ended_at: float | None,
    tags: dict[str, _AgentEffortTags],
    *,
    markers: tuple[str, ...] = (),
) -> PhaseEffortWindow:
    return _window(
        window.shape,
        window.phase_index,
        window.event,
        tags,
        started_at=window.event.ts,
        ended_at=ended_at,
        markers=markers,
    )


def _missing_start_window(
    shape: _TaskShape,
    phase_index: int,
    event: _TaskLifecycleEvent,
    tags: dict[str, _AgentEffortTags],
) -> PhaseEffortWindow:
    return _window(
        shape,
        phase_index,
        event,
        tags,
        started_at=None,
        ended_at=event.ts,
        markers=(PARTIAL_MISSING_START,),
    )


def _window(
    shape: _TaskShape,
    phase_index: int,
    event: _TaskLifecycleEvent,
    tags: dict[str, _AgentEffortTags],
    *,
    started_at: float | None,
    ended_at: float | None,
    markers: tuple[str, ...],
) -> PhaseEffortWindow:
    tag = tags[event.agent_id]
    return PhaseEffortWindow(
        task_id=shape.task_id,
        handle=shape.handle,
        title=shape.title,
        phase=_phase_name(shape, phase_index),
        phase_index=phase_index,
        actor_id=tag.actor_id,
        thread_id=tag.thread_id,
        team_id=event.team_id,
        driver=tag.driver,
        model=tag.model,
        effort=tag.effort,
        started_at=started_at,
        ended_at=ended_at,
        partial_markers=markers,
    )


def _phase_name(shape: _TaskShape, phase_index: int) -> str:
    if 0 <= phase_index < len(shape.phases):
        return shape.phases[phase_index]
    return ""


def _sort_time(window: PhaseEffortWindow) -> float:
    if window.started_at is not None:
        return window.started_at
    if window.ended_at is not None:
        return window.ended_at
    return 0.0


def _placeholders(values: tuple[str, ...]) -> str:
    return ",".join("?" for _value in values)
