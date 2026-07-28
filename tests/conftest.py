"""Suite-wide opt-in audit: every SQLite connection closes deterministically.

Set SPICE_SQLITE_LIFECYCLE_AUDIT to a writable file path to count every
in-process sqlite3 connection the suite opens against explicit closes. Each
pytest process (xdist workers included) forces a garbage-collection pass at
session finish and appends one `opened=N closed=N` line to that file. A
resource-clean run shows equal counts on every line: implicit destructor
cleanup bypasses the counted close, so a connection left to garbage
collection surfaces as an opened/closed imbalance.

Also home to ``task_plane``: a real TaskChampion operations log a test can
write task mutations into at instants of its choosing. Serve reads lifecycle
facts by folding that log, so a test that wants a claim or a completion has to
put one where the task plane keeps it rather than beside it.
"""

from __future__ import annotations

import gc
import json
import os
import sqlite3
import subprocess
import threading
from collections.abc import Sequence
from datetime import UTC, datetime
from pathlib import Path

import pytest

from spice.serve import launch
from spice.serve.team.ids import thread_id_for_actor
from spice.tasks import config as task_config
from spice.tasks import opslog

SQLITE_LIFECYCLE_AUDIT_ENV = "SPICE_SQLITE_LIFECYCLE_AUDIT"  # env-policy: allow

_counts_lock = threading.Lock()
_counts = {"opened": 0, "closed": 0}
_real_connect = sqlite3.connect


@pytest.fixture
def git_worktree_tmp_path(tmp_path):
    """Make the standard temporary directory a real empty Git worktree."""
    subprocess.run(
        ["git", "init", "-q", "-b", "main"],
        cwd=tmp_path,
        check=True,
        capture_output=True,
        text=True,
    )
    return tmp_path


class _AuditedConnection(sqlite3.Connection):
    def __init__(self, *args, **kwargs) -> None:
        super().__init__(*args, **kwargs)
        self._audit_close_recorded = False
        with _counts_lock:
            _counts["opened"] += 1

    def close(self) -> None:
        if self._audit_close_recorded:
            super().close()
            return
        self._audit_close_recorded = True
        with _counts_lock:
            _counts["closed"] += 1
        super().close()


def _audited_connect(*args, **kwargs):
    kwargs.setdefault("factory", _AuditedConnection)
    return _real_connect(*args, **kwargs)


def _join_test_owned_available_work_watches(
    watches: list[launch.AvailableWorkWatch],
) -> list[str]:
    leaked: list[tuple[launch.AvailableWorkWatch, threading.Thread]] = []
    for watch in watches:
        thread = watch._thread
        if (
            thread is not None
            and thread.name == launch.AVAILABLE_WORK_WATCH_THREAD_NAME
            and thread.is_alive()
        ):
            leaked.append((watch, thread))
            watch._stop.set()
    for _watch, thread in leaked:
        thread.join(timeout=launch.AVAILABLE_WORK_WATCH_JOIN_SECONDS)
    still_alive = [thread.name for _watch, thread in leaked if thread.is_alive()]
    if still_alive:
        raise AssertionError(
            "AvailableWorkWatch teardown could not join: " + ", ".join(still_alive)
        )
    return [thread.name for _watch, thread in leaked]


@pytest.fixture(autouse=True)
def _available_work_watch_leak_guard(monkeypatch):
    watches: list[launch.AvailableWorkWatch] = []
    original_start = launch.AvailableWorkWatch.start

    def tracked_start(watch: launch.AvailableWorkWatch) -> None:
        watches.append(watch)
        original_start(watch)

    monkeypatch.setattr(launch.AvailableWorkWatch, "start", tracked_start)

    def join_leaks() -> list[str]:
        return _join_test_owned_available_work_watches(watches)

    yield join_leaks

    leaked = join_leaks()
    if leaked:
        pytest.fail(
            "test leaked AvailableWorkWatch background thread(s): " + ", ".join(leaked)
        )


class TaskPlane:
    """A TaskChampion operations log that records real lifecycle mutations.

    Each ``record`` call commits one transaction the way Taskwarrior does: an
    undo point, then the per-property Update operations that one command
    writes, stamped at the instant the caller names. Serve derives claims,
    advances, reviews and drains by folding exactly these properties, so a
    test states the mutation and the derivation is the thing under test.

    Only a claim names its actor outright; every later movement is credited to
    whoever was holding the task, exactly as the plane records it. A movement
    on a task the named agent does not hold is therefore refused here rather
    than written as a transaction no real command could have produced.

    Agents are named the way Serve names them, and written down the way the
    task plane names them: a Taskwarrior attribute may not hold Serve's
    ``thread:`` actor, so a real ``spice task`` command stores the bare thread
    id and Serve puts the prefix back when it reads. Recording the same
    translation here is what keeps the id a test states and the id a test
    asserts on the same id.
    """

    PHASE_FLOW = ("todo", "review")

    def __init__(self, database: Path) -> None:
        self.database = database
        self.database.parent.mkdir(parents=True, exist_ok=True)
        self._phase_index: dict[str, int] = {}
        self._holders: dict[str, str] = {}
        self._write(
            "CREATE TABLE IF NOT EXISTS operations ("
            "id INTEGER PRIMARY KEY AUTOINCREMENT, data STRING, "
            "uuid GENERATED ALWAYS AS (coalesce("
            'json_extract(data, "$.Update.uuid"), '
            'json_extract(data, "$.Create.uuid"), '
            'json_extract(data, "$.Delete.uuid"))) VIRTUAL, '
            "synced bool DEFAULT false)",
            (),
        )

    def record(self, kind: str, *, task_id: str, agent_id: str, ts: float) -> None:
        """Commit the one command that moves a task the way ``kind`` names."""
        writes: list[tuple[str, str]] = []
        if task_id not in self._phase_index:
            self._phase_index[task_id] = 0
            self._commit(task_id, ts, [("status", "pending"), ("phase", "todo")])
        if kind == "claim":
            self._holders[task_id] = agent_id
            writes = [("claim_by", _plane_actor(agent_id)), ("start", _stamp(ts))]
        elif kind == "phaseAdvance":
            self._require_holder(kind, task_id, agent_id)
            index = self._phase_index[task_id] + 1
            self._phase_index[task_id] = index
            self._holders[task_id] = ""
            writes = [
                ("claim_by", ""),
                ("phase", self._phase_name(index)),
                ("phase_i", str(index)),
                ("start", ""),
            ]
        elif kind == "review":
            self._require_holder(kind, task_id, agent_id)
            writes = [
                ("review_by", _plane_actor(agent_id)),
                ("review_finding", "clean"),
            ]
        elif kind == "complete":
            self._require_holder(kind, task_id, agent_id)
            writes = [("end", _stamp(ts)), ("status", "completed")]
        elif kind == "drain":
            self._require_holder(kind, task_id, agent_id)
            writes = [("status", "deleted")]
        else:
            raise AssertionError(f"unknown task lifecycle kind: {kind}")
        self._commit(task_id, ts, writes)

    def _require_holder(self, kind: str, task_id: str, agent_id: str) -> None:
        holder = self._holders.get(task_id, "")
        if holder != agent_id:
            raise AssertionError(
                f"{kind} of {task_id} is credited to whoever holds it, and "
                f"{agent_id} does not: the holder is {holder or 'nobody'}"
            )

    def _phase_name(self, index: int) -> str:
        if index < len(self.PHASE_FLOW):
            return self.PHASE_FLOW[index]
        return f"phase{index}"

    def _commit(self, task_id: str, ts: float, writes: list[tuple[str, str]]) -> None:
        stamp = _stamp(ts)
        rows = [(json.dumps("UndoPoint"),)]
        rows += [
            (
                json.dumps(
                    {
                        "Update": {
                            "uuid": task_id,
                            "property": name,
                            "value": value,
                            "timestamp": stamp,
                        }
                    }
                ),
            )
            for name, value in [*writes, ("modified", stamp)]
        ]
        self._write("INSERT INTO operations (data) VALUES (?)", rows)

    def _write(self, statement: str, rows: Sequence[tuple]) -> None:
        connection = sqlite3.connect(self.database)
        try:
            if rows:
                connection.executemany(statement, rows)
            else:
                connection.execute(statement)
            connection.commit()
        finally:
            connection.close()


def _plane_actor(agent_id: str) -> str:
    """The task plane's own name for the agent Serve calls ``agent_id``."""
    return thread_id_for_actor(agent_id) if agent_id else ""


def _stamp(ts: float) -> str:
    return datetime.fromtimestamp(float(ts), tz=UTC).isoformat()


@pytest.fixture
def team_event():
    """Append one team-plane event at an instant the test chooses.

    Serve credits a task movement to the team its actor was in when the move
    happened, and it learns that from the team plane's own event log. A test
    that places its task facts at epochs of its own choosing has to say when
    the team formed and when actors moved between teams, or every movement
    falls outside every membership span and is credited to its actor's own
    lane. The store's own ``create_team`` stamps the wall clock, which no
    chosen epoch is ever inside.
    """

    def seed(store, kind: str, *, team_id: str, ts: float, **payload) -> None:
        with store.connect() as connection:
            connection.execute(
                "INSERT INTO events (ts, kind, team_id, payload) VALUES (?, ?, ?, ?)",
                (float(ts), kind, team_id, json.dumps(payload)),
            )

    return seed


@pytest.fixture
def task_plane(tmp_path):
    """The canonical task plane every lifecycle fact in a test is written to.

    A test that already stood a task backend up keeps it, so facts recorded
    here land in the very operations log its real ``spice task`` commands
    write to. A test without one gets a private backend, never the repo's own.
    """
    selected = task_config.backend_override()
    if selected is None:
        task_config.set_backend(str(tmp_path / "task-backend"))
    try:
        yield TaskPlane(opslog.operations_db_path())
    finally:
        if selected is None:
            task_config.set_backend(None)


def pytest_configure(config) -> None:
    if os.environ.get(SQLITE_LIFECYCLE_AUDIT_ENV):  # env-policy: allow
        sqlite3.connect = _audited_connect


def pytest_sessionfinish(session, exitstatus) -> None:
    audit_path = os.environ.get(SQLITE_LIFECYCLE_AUDIT_ENV)  # env-policy: allow
    if not audit_path:
        return
    gc.collect()
    with _counts_lock:
        line = f"opened={_counts['opened']} closed={_counts['closed']}\n"
    with open(audit_path, "a", encoding="utf-8") as handle:
        handle.write(line)
