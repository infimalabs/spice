"""Operations-log identity and incremental task-transition cache contracts."""

from __future__ import annotations

import json
import os
import sqlite3
from contextlib import closing
from pathlib import Path

import pytest

from spice.tasks import config, opslog, transitions
from spice.tasks.transitions import TaskTransitionKind, task_transitions

TASK_ID = "11111111-2222-4333-8444-555555555555"
OTHER_TASK_ID = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ACTOR_B = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"


@pytest.mark.parametrize("tail_delta", [-1, 0, 1], ids=["lower", "equal", "greater"])
def test_cache_rejects_same_path_database_replacement(task_plane, tmp_path, tail_delta):
    task_plane.record("claim", task_id=TASK_ID, agent_id=ACTOR_A, ts=1)
    _append_undo_points(task_plane.database, 2)
    original = task_transitions()
    original_tail = _database_tail(task_plane.database)
    replacement = tmp_path / f"replacement-{tail_delta}.sqlite3"
    _copy_database(task_plane.database, replacement)
    _rewrite_claim_actor(replacement, ACTOR_B)
    _move_database_tail(replacement, tail_delta)

    assert _database_tail(replacement) == original_tail + tail_delta
    os.replace(replacement, task_plane.database)
    replaced = task_transitions()
    claims = [
        item
        for item in replaced
        if item.task_id == TASK_ID and item.kind is TaskTransitionKind.CLAIM
    ]
    assert [item.actor for item in claims] == [ACTOR_B]
    assert replaced != original


def test_cache_keeps_distinct_backends_in_one_process(task_plane, tmp_path):
    backend_a = config.backend_root()
    task_plane.record("claim", task_id=TASK_ID, agent_id=ACTOR_A, ts=1)
    first = task_transitions()

    config.set_backend(str(tmp_path / "backend-b"))
    other_plane = type(task_plane)(opslog.operations_db_path())
    other_plane.record("claim", task_id=OTHER_TASK_ID, agent_id=ACTOR_B, ts=2)
    second = task_transitions()

    config.set_backend(str(backend_a))
    assert task_transitions() == first
    assert {item.task_id for item in first}.isdisjoint(item.task_id for item in second)
    assert {item.actor for item in second} == {ACTOR_B}


def test_cache_reuses_unchanged_log_then_continues_after_append(
    task_plane, monkeypatch
):
    task_plane.record("claim", task_id=TASK_ID, agent_id=ACTOR_A, ts=1)
    before = task_transitions()
    previous_tail = _database_tail(task_plane.database)
    real_fold = transitions._fold_log

    def unexpected_fold(*_args, **_kwargs):
        pytest.fail("unchanged operations log should reuse its cached fold")

    monkeypatch.setattr(transitions, "_fold_log", unexpected_fold)
    assert task_transitions() == before
    task_plane.record("phaseAdvance", task_id=TASK_ID, agent_id=ACTOR_A, ts=2)
    resumed: list[tuple[int, int]] = []

    def tracked_fold(log, task_ids, resume, *, tail_id=0):
        resumed.append((resume.tail_id, tail_id))
        return real_fold(log, task_ids, resume, tail_id=tail_id)

    monkeypatch.setattr(transitions, "_fold_log", tracked_fold)
    after = task_transitions()
    assert resumed == [(previous_tail, _database_tail(task_plane.database))]
    assert after[: len(before)] == before
    assert after[-1].kind is TaskTransitionKind.PHASE_ADVANCE


def _append_undo_points(database: Path, count: int) -> None:
    with closing(sqlite3.connect(database)) as connection:
        connection.executemany(
            "INSERT INTO operations (data) VALUES (?)",
            [(json.dumps("UndoPoint"),)] * count,
        )
        connection.commit()


def _copy_database(source: Path, target: Path) -> None:
    with (
        closing(sqlite3.connect(source)) as source_connection,
        closing(sqlite3.connect(target)) as target_connection,
    ):
        source_connection.backup(target_connection)


def _rewrite_claim_actor(database: Path, actor: str) -> None:
    with closing(sqlite3.connect(database)) as connection:
        cursor = connection.execute(
            "UPDATE operations SET data = json_set(data, '$.Update.value', ?) "
            "WHERE uuid = ? "
            "AND json_extract(data, '$.Update.property') = 'claim_by'",
            (actor, TASK_ID),
        )
        connection.commit()
    assert cursor.rowcount == 1


def _move_database_tail(database: Path, delta: int) -> None:
    with closing(sqlite3.connect(database)) as connection:
        if delta < 0:
            connection.execute(
                "DELETE FROM operations WHERE id = (SELECT MAX(id) FROM operations)"
            )
        elif delta > 0:
            connection.execute(
                "INSERT INTO operations (data) VALUES (?)",
                (json.dumps("UndoPoint"),),
            )
        connection.commit()


def _database_tail(database: Path) -> int:
    with closing(sqlite3.connect(database)) as connection:
        row = connection.execute("SELECT MAX(id) FROM operations").fetchone()
    return int(row[0])
