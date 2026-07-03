"""Task phase effort window construction from lifecycle events."""

from __future__ import annotations

import json
import sqlite3
from contextlib import AbstractContextManager
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Mapping, Protocol

from spice.serve.team.ids import thread_id_for_actor
from spice.serve.team.store import ServeTeamStore
from spice.sessions import records
from spice.sessions.meter import (
    ActiveContextSnapshot,
    active_context_snapshot_from_object,
)
from spice.sessions.slices import turn_activity_ts
from spice.sessions.util import parse_iso_ts
from spice.tasks import identity, ops

PARTIAL_MISSING_START = "missing_start"
PARTIAL_MISSING_END = "missing_end"
PARTIAL_HANDOFF = "handoff"
PARTIAL_MISSING_TRANSCRIPT = "missing_transcript"


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


@dataclass(frozen=True, slots=True)
class PhaseEffortUsage:
    window: PhaseEffortWindow
    source_files: tuple[str, ...]
    input_tokens: int = 0
    cached_input_tokens: int = 0
    output_tokens: int = 0
    reasoning_output_tokens: int = 0
    total_tokens: int = 0
    turn_count: int = 0
    message_count: int = 0
    renewal_count: int = 0
    partial_markers: tuple[str, ...] = ()

    @property
    def task_id(self) -> str:
        return self.window.task_id

    @property
    def handle(self) -> str:
        return self.window.handle

    @property
    def phase(self) -> str:
        return self.window.phase

    @property
    def phase_index(self) -> int:
        return self.window.phase_index

    @property
    def actor_id(self) -> str:
        return self.window.actor_id

    @property
    def thread_id(self) -> str:
        return self.window.thread_id

    @property
    def driver(self) -> str:
        return self.window.driver

    @property
    def model(self) -> str:
        return self.window.model

    @property
    def effort(self) -> str:
        return self.window.effort

    @property
    def wall_seconds(self) -> float | None:
        return self.window.wall_seconds

    @property
    def partial(self) -> bool:
        return bool(self.partial_markers)


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


def phase_effort_usage_for_tasks(
    task_rows: Iterable[Mapping[str, Any]],
    transcript_files_by_thread: Mapping[str, Iterable[str | Path]],
    *,
    store: _EffortWindowStore | None = None,
) -> tuple[PhaseEffortUsage, ...]:
    return phase_effort_usage_for_windows(
        phase_effort_windows_for_tasks(task_rows, store=store),
        transcript_files_by_thread,
    )


def phase_effort_usage_for_windows(
    windows: Iterable[PhaseEffortWindow],
    transcript_files_by_thread: Mapping[str, Iterable[str | Path]],
) -> tuple[PhaseEffortUsage, ...]:
    transcript_cache: dict[str, _ThreadTranscriptUsage] = {}
    rows: list[PhaseEffortUsage] = []
    for window in windows:
        usage = transcript_cache.get(window.thread_id)
        if usage is None:
            usage = _thread_transcript_usage(
                transcript_files_by_thread.get(window.thread_id, ())
            )
            transcript_cache[window.thread_id] = usage
        rows.append(_phase_effort_usage(window, usage))
    return tuple(rows)


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


@dataclass(frozen=True, slots=True)
class _ThreadTranscriptUsage:
    source_files: tuple[str, ...]
    snapshots: tuple[ActiveContextSnapshot, ...]
    turns: tuple[records.TurnRecord, ...]
    renewals: tuple[str, ...]


def _thread_transcript_usage(files: Iterable[str | Path]) -> _ThreadTranscriptUsage:
    paths = tuple(Path(path) for path in files)
    existing = tuple(path for path in paths if path.is_file())
    if not existing:
        return _ThreadTranscriptUsage((), (), (), ())
    return _ThreadTranscriptUsage(
        source_files=tuple(str(path) for path in existing),
        snapshots=_active_context_snapshots(existing),
        turns=tuple(records.collect_turns(list(existing))),
        renewals=tuple(
            record.ts for record in records.collect_compactions(list(existing))
        ),
    )


def _phase_effort_usage(
    window: PhaseEffortWindow, transcript_usage: _ThreadTranscriptUsage
) -> PhaseEffortUsage:
    markers = _usage_partial_markers(window, transcript_usage)
    snapshots = [
        snapshot
        for snapshot in transcript_usage.snapshots
        if _timestamp_in_window(snapshot.ts, window)
    ]
    turns = [
        turn
        for turn in transcript_usage.turns
        if _timestamp_in_window(turn_activity_ts(turn), window)
    ]
    renewals = [
        renewal_ts
        for renewal_ts in transcript_usage.renewals
        if _timestamp_in_window(renewal_ts, window)
    ]
    return PhaseEffortUsage(
        window=window,
        source_files=transcript_usage.source_files,
        input_tokens=sum(snapshot.input_tokens for snapshot in snapshots),
        cached_input_tokens=sum(snapshot.cached_input_tokens for snapshot in snapshots),
        output_tokens=sum(snapshot.output_tokens for snapshot in snapshots),
        reasoning_output_tokens=sum(
            snapshot.reasoning_output_tokens for snapshot in snapshots
        ),
        total_tokens=sum(snapshot.total_tokens for snapshot in snapshots),
        turn_count=len(turns),
        message_count=sum(len(turn.ordered_messages) for turn in turns),
        renewal_count=len(renewals),
        partial_markers=markers,
    )


def _usage_partial_markers(
    window: PhaseEffortWindow, transcript_usage: _ThreadTranscriptUsage
) -> tuple[str, ...]:
    markers = list(window.partial_markers)
    if not transcript_usage.source_files:
        markers.append(PARTIAL_MISSING_TRANSCRIPT)
    return tuple(dict.fromkeys(markers))


def _active_context_snapshots(
    paths: tuple[Path, ...],
) -> tuple[ActiveContextSnapshot, ...]:
    snapshots: list[ActiveContextSnapshot] = []
    for path in paths:
        with path.open(encoding="utf-8", errors="replace") as handle:
            for line in handle:
                try:
                    raw = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if not isinstance(raw, dict):
                    continue
                snapshot = active_context_snapshot_from_object(path, raw)
                if snapshot is not None:
                    snapshots.append(snapshot)
    return tuple(sorted(snapshots, key=lambda item: (item.ts, item.source_file)))


def _timestamp_in_window(ts: str | None, window: PhaseEffortWindow) -> bool:
    parsed = parse_iso_ts(ts)
    if parsed is None:
        return False
    value = parsed.timestamp()
    if window.started_at is not None and value < window.started_at:
        return False
    if window.ended_at is not None and value >= window.ended_at:
        return False
    return True


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
