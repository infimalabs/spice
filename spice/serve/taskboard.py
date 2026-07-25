"""Revision-coherent observation of the current task board."""

from __future__ import annotations

import threading
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from types import MappingProxyType
from typing import Any

from spice.errors import SpiceError
from spice.tasks import config as task_config
from spice.tasks import tw


@dataclass(frozen=True, slots=True, weakref_slot=True)
class TaskBoardObservation:
    """One stable task-backend revision and its normalized rows."""

    backend_identity: str
    revision: str
    rows: tuple[Mapping[str, Any], ...]
    error: str | None = None


_task_board_condition = threading.Condition()
_task_board_observations: dict[str, TaskBoardObservation] = {}
_task_board_builds: set[str] = set()


def _backend_identity(root: Path) -> str:
    return str(root.expanduser().resolve())


def _normalize_task_row(row: Mapping[str, Any]) -> Mapping[str, Any]:
    return MappingProxyType(dict(row))


def _read_task_board(root: Path) -> list[dict[str, Any]]:
    taskrc = task_config.materialize_task_backend(root)
    return tw.export(["status.any:"], taskrc=taskrc)


def _release_task_board_build(backend_identity: str) -> None:
    with _task_board_condition:
        _task_board_builds.discard(backend_identity)
        _task_board_condition.notify_all()


def current_task_board_observation(
    *, backend_root: Path | None = None
) -> TaskBoardObservation:
    """Return the current coherent board, coalescing concurrent cache misses.

    A backend failure degrades only the current call. Its empty observation is
    deliberately not cached, so a peer or later request can recover without a
    task revision change.
    """

    selected_root = (backend_root or task_config.backend_root()).expanduser().resolve()
    backend_identity = _backend_identity(selected_root)

    while True:
        revision = task_config.task_event_revision(selected_root)
        with _task_board_condition:
            cached = _task_board_observations.get(backend_identity)
            if cached is not None and cached.revision == revision:
                return cached
            if backend_identity in _task_board_builds:
                _task_board_condition.wait()
                continue
            _task_board_builds.add(backend_identity)
            break

    try:
        while True:
            revision = task_config.task_event_revision(selected_root)
            rows = _read_task_board(selected_root)
            normalized = tuple(_normalize_task_row(row) for row in rows)
            if task_config.task_event_revision(selected_root) == revision:
                break
    except SpiceError as exc:
        _release_task_board_build(backend_identity)
        return TaskBoardObservation(
            backend_identity=backend_identity,
            revision=revision,
            rows=(),
            error=str(exc),
        )
    except BaseException:
        _release_task_board_build(backend_identity)
        raise

    candidate = TaskBoardObservation(
        backend_identity=backend_identity,
        revision=revision,
        rows=normalized,
    )
    with _task_board_condition:
        current = _task_board_observations.get(backend_identity)
        if current is None or current.revision != revision:
            _task_board_observations[backend_identity] = candidate
            current = candidate
        _task_board_builds.discard(backend_identity)
        _task_board_condition.notify_all()
        return current
