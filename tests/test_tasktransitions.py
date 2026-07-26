"""Semantic lifecycle transitions derived from the canonical task plane."""

from __future__ import annotations

import json
import shutil
import sqlite3
import subprocess
import time
from collections import Counter
from datetime import datetime
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.tasks import claimstate, config, create, identity, ops, opslog, transitions
from spice.tasks.transitions import (
    DRAINING_KINDS,
    TaskTransitionKind,
    task_transitions,
)

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ACTOR_B = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
RENEWAL_TICKS = 3
RENEWAL_LEASE_SECONDS = 3600.0

# What the deleted Serve mirror recorded for the lifecycle below: one row per
# lifecycle call site, stamped with the actor of whichever process made the
# call. Kept here as data, because parity with it is what licensed deleting it.
MIRROR_EVENTS = (
    ("claim", ACTOR_A),
    ("phaseAdvance", ACTOR_A),
    ("claim", ACTOR_B),
    ("review", ACTOR_B),
    ("complete", ACTOR_B),
    ("drain", ACTOR_B),
)


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-a")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def test_real_lifecycle_commands_each_derive_one_transition(task_repo, monkeypatch):
    opened = time.time()
    handle = _reviewable_task()
    ops.claim(handle)
    ops.done(handle, validation=["todo complete"])
    # A thread may not review its own work, so the review phase is a second
    # actor's, and every movement in it is credited to that actor.
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_B)
    ops.claim(handle)
    ops.review(handle, finding="clean")
    closed = time.time()

    task_id = identity.uuid_of(identity.resolve(handle))
    derived = task_transitions([task_id])

    assert [
        (
            transition.kind,
            transition.actor,
            transition.old_state.label,
            transition.new_state.label,
        )
        for transition in derived
    ] == [
        (
            TaskTransitionKind.CLAIM,
            ACTOR_A,
            "pending/todo[0] claim=- review=-",
            f"pending/todo[0] claim={ACTOR_A} review=-",
        ),
        (
            TaskTransitionKind.PHASE_ADVANCE,
            ACTOR_A,
            f"pending/todo[0] claim={ACTOR_A} review=-",
            "pending/review[1] claim=- review=-",
        ),
        (
            TaskTransitionKind.CLAIM,
            ACTOR_B,
            "pending/review[1] claim=- review=-",
            f"pending/review[1] claim={ACTOR_B} review=-",
        ),
        (
            TaskTransitionKind.REVIEW,
            ACTOR_B,
            f"pending/review[1] claim={ACTOR_B} review=-",
            f"pending/review[1] claim={ACTOR_B} review=clean",
        ),
        (
            TaskTransitionKind.COMPLETE,
            ACTOR_B,
            f"pending/review[1] claim={ACTOR_B} review=clean",
            f"completed/review[1] claim={ACTOR_B} review=clean",
        ),
    ]
    assert [transition.task_id for transition in derived] == [task_id] * len(derived)
    identities = [transition.id for transition in derived]
    assert identities == sorted(set(identities))
    stamps = [transition.at for transition in derived]
    assert stamps == sorted(stamps)
    assert opened <= stamps[0] and stamps[-1] <= closed


def test_mirror_parity_holds_where_the_counts_meant_the_same_thing(
    task_repo, monkeypatch
):
    handle = _reviewable_task()
    ops.claim(handle)
    ops.done(handle, validation=["todo complete"])
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_B)
    ops.claim(handle)
    ops.review(handle, finding="clean")

    task_id = identity.uuid_of(identity.resolve(handle))
    derived = task_transitions([task_id])
    mirrored = Counter(kind for kind, _actor in MIRROR_EVENTS)
    counted = Counter(transition.kind for transition in derived)

    # Every movement the mirror recorded, the reader derives -- with one
    # difference, and it is the reason the mirror needed six rows to say what
    # five transitions say. Completing a task is the movement that takes it off
    # the open board, so the mirror wrote a second `drain` row beside every
    # `complete` to make its drained count come out right. The reader counts a
    # completion as a draining movement instead of restating it.
    assert counted[TaskTransitionKind.COMPLETE] == mirrored["complete"]
    assert sum(counted[kind] for kind in DRAINING_KINDS) == mirrored["drain"]
    assert counted[TaskTransitionKind.CLAIM] == mirrored["claim"]
    assert counted[TaskTransitionKind.PHASE_ADVANCE] == mirrored["phaseAdvance"]
    assert counted[TaskTransitionKind.REVIEW] == mirrored["review"]
    assert len(derived) + 1 == len(MIRROR_EVENTS)

    # The mirror named the actor of the process that made the call; the reader
    # names the actor the plane recorded as holding the task. For every movement
    # a holder made, those are the same actor.
    assert [transition.actor for transition in derived] == [
        actor for kind, actor in MIRROR_EVENTS if kind != "drain"
    ]


def test_claim_renewal_moves_no_lifecycle_state(task_repo):
    handle = _reviewable_task()
    ops.claim(handle)
    task_id = identity.uuid_of(identity.resolve(handle))
    claimed = task_transitions([task_id])

    version = opslog.task_version(task_id)

    # Each tick asks for a strictly longer lease, so the recorded deadline
    # really moves every time. Renewal writes are otherwise invisible when a
    # whole cadence lands inside one second: the plane records an operation
    # only where a value changed, and a lease is stored to the second.
    for tick in range(RENEWAL_TICKS):
        claimstate.renew_claim(handle, lease_seconds=RENEWAL_LEASE_SECONDS + tick)

    # The log grew by every one of those writes; lifecycle state did not move.
    assert opslog.task_version(task_id) > version
    assert task_transitions([task_id]) == claimed


def test_one_command_writing_many_properties_is_one_transition(task_repo):
    handle = _reviewable_task()
    ops.claim(handle)
    task_id = identity.uuid_of(identity.resolve(handle))
    before = task_transitions([task_id])

    ops.done(handle, validation=["todo complete"])

    advanced = task_transitions([task_id])[len(before) :]
    # A transition is identified by its transaction's first operation, so
    # reading from there covers that one transaction and nothing before it.
    written = _transaction_properties(task_id, after_id=advanced[0].id - 1)

    # One `task done` moves the phase and its index, releases the claim, and
    # clears the whole lease it was held under, all in that single transaction.
    # Three of those writes are lifecycle state, and together they are one
    # advance.
    assert set(written) > {"phase", "phase_i", "claim_by"}
    assert [transition.kind for transition in advanced] == [
        TaskTransitionKind.PHASE_ADVANCE
    ]


def test_duplicate_command_preserves_the_first_actor_and_time(task_repo):
    handle = _reviewable_task()
    ops.claim(handle)
    task_id = identity.uuid_of(identity.resolve(handle))
    claimed = task_transitions([task_id])

    # A command retried after its writes already landed appends the same
    # properties again, moving nothing: the state it commits against is the
    # state it wrote. The first attempt stays the transition of record.
    _append_transaction(
        task_id,
        [("claim_by", ACTOR_A), ("modified", "2031-01-01T00:00:00.000000Z")],
    )

    assert task_transitions([task_id]) == claimed


def test_interrupted_mutation_folds_the_writes_that_landed(task_repo):
    handle = _reviewable_task()
    ops.claim(handle)
    task_id = identity.uuid_of(identity.resolve(handle))
    stamp = "2031-02-03T04:05:06.000000Z"

    # A command killed mid-write leaves the properties it managed to append and
    # never stamps `modified`. The status write landed, so the task completed.
    _append_transaction(task_id, [("status", "completed")], timestamp=stamp)

    interrupted = task_transitions([task_id])[-1]

    assert interrupted.kind is TaskTransitionKind.COMPLETE
    assert interrupted.actor == ACTOR_A
    assert interrupted.at == _epoch(stamp)


def test_restarting_the_reader_derives_the_same_transitions(task_repo):
    handle = _reviewable_task()
    ops.claim(handle)
    ops.done(handle, validation=["todo complete"])
    task_id = identity.uuid_of(identity.resolve(handle))

    whole_log = [
        transition for transition in task_transitions() if transition.task_id == task_id
    ]
    _restart_reader()

    # A fold held across reads and a fold started from nothing describe the same
    # history, because the history they read is append-only and never rewritten.
    assert task_transitions() == task_transitions()
    assert task_transitions([task_id]) == tuple(whole_log)


def test_two_commands_read_as_one_transaction_fail_loudly(task_repo):
    handle = _reviewable_task()
    ops.claim(handle)
    task_id = identity.uuid_of(identity.resolve(handle))

    # Two `modified` stamps inside one contiguous run means two commands were
    # read as one, and every count derived from the run would be short.
    _append_transaction(
        task_id,
        [
            ("status", "completed"),
            ("modified", "2031-01-01T00:00:00.000000Z"),
            ("phase", "review"),
            ("modified", "2031-01-01T00:00:01.000000Z"),
        ],
    )

    with pytest.raises(SpiceError) as exc:
        task_transitions([task_id])

    message = str(exc.value)
    assert "carry 2 modified writes" in message
    assert "one transaction writes at most one" in message
    assert str(opslog.operations_db_path()) in message


def _reviewable_task() -> str:
    return create.add(
        "Move through every lifecycle phase",
        project="task.unit",
        origin="ack:1jN54zJJ",
        flow=["todo", "review"],
        acceptance=["every movement is derived once"],
    )


def _restart_reader() -> None:
    """Drop what a process held across reads, the way a new process starts."""
    with transitions._history_lock:
        transitions._histories.clear()


def _epoch(timestamp: str) -> float:
    return datetime.fromisoformat(timestamp).timestamp()


def _append_transaction(
    task_id: str, writes: list[tuple[str, str]], *, timestamp: str = ""
) -> None:
    """Append one transaction the way TaskChampion commits one command's writes."""
    stamp = timestamp or "2031-01-01T00:00:00.000000Z"
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
        for name, value in writes
    ]
    connection = sqlite3.connect(opslog.operations_db_path())
    try:
        connection.executemany("INSERT INTO operations (data) VALUES (?)", rows)
        connection.commit()
    finally:
        connection.close()
    _restart_reader()


def _transaction_properties(task_id: str, *, after_id: int) -> list[str]:
    connection = sqlite3.connect(opslog.operations_db_path())
    try:
        rows = connection.execute(
            "SELECT json_extract(data, '$.Update.property') FROM operations "
            "WHERE uuid = ? AND id > ? ORDER BY id",
            (task_id, after_id),
        ).fetchall()
    finally:
        connection.close()
    return [str(row[0]) for row in rows if row[0]]


def _init_repo(path: Path) -> Path:
    path.mkdir(parents=True, exist_ok=True)
    _run(path, "git", "init")
    _run(path, "git", "config", "user.email", "test@example.com")
    _run(path, "git", "config", "user.name", "Test")
    (path / "README.md").write_text("test\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "init")
    return path


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
