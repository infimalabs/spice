"""Deferred task creation and deferral-preserving lifecycle coverage."""

from __future__ import annotations

import shutil
import subprocess
from datetime import UTC, datetime
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.tasks import alloc, claimstate, config, create, identity, ops, render, tw

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

SCHEDULING_FIELDS = ("wait", "scheduled", "due", "until")

# Slack for comparing two SLA clocks started moments apart within one test.
SLA_TOLERANCE_SECONDS = 120.0

# scheduled sits in the past so +READY (which excludes future-scheduled rows)
# turns on wait alone once the task wakes.
DEFERRAL = {
    "wait": "2099-01-02T03:04:05Z",
    "scheduled": "2001-02-03T04:05:06Z",
    "due": "2099-03-04T05:06:07Z",
    "until": "2099-04-05T06:07:08Z",
}


def _scheduling_snapshot(handle: str) -> dict[str, str]:
    row = identity.resolve(handle)
    return {field: str(row.get(field) or "") for field in SCHEDULING_FIELDS}


def _parse_tw_datetime(value: str) -> datetime:
    return datetime.strptime(value, "%Y%m%dT%H%M%SZ").replace(tzinfo=UTC)


def _deferred_task(title: str, *, flow: list[str] | None = None) -> str:
    return create.add(
        title,
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["deferral survives the lifecycle"],
        flow=flow,
        **DEFERRAL,
    )


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    if shutil.which("task") is None:
        pytest.skip("Taskwarrior binary is required")
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-taskdeferred")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def test_deferred_creation_is_hidden_from_allocator_until_woken(task_repo):
    handle = create.add(
        "Deferred allocator task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        deferred=True,
    )
    row = identity.resolve(handle)

    assert str(row.get("wait") or "").startswith("2099")
    assert handle not in _ready_handles()

    output = ops.wake([handle])
    woken = identity.resolve(handle)

    assert f"woke {handle}: wait:" in output
    assert not str(woken.get("wait") or "")
    assert handle in _ready_handles()


def test_claim_preserves_scheduling_and_stays_visible_as_active(task_repo):
    handle = _deferred_task("Claim keeps the deferral envelope")
    before = _scheduling_snapshot(handle)

    ops.claim(handle)

    after = _scheduling_snapshot(handle)
    assert after == before
    row = identity.resolve(handle)
    assert str(row.get("claim_by") or "") == ACTOR
    assert str(row.get("start") or "") != ""
    # Allocator visibility: the claimed deferred task is the actor's own
    # active claim, resumable through `task next` and the claim resolver.
    resumed = alloc.next_task()
    assert resumed is not None
    assert identity.render_handle(resumed) == handle
    active = claimstate.active_claim(ACTOR)
    assert active is not None
    assert identity.render_handle(active) == handle
    status = render.render_status()
    assert "active 1" in status
    assert "waiting 0" in status


def test_reclaim_and_renewal_preserve_scheduling(task_repo):
    handle = _deferred_task("Reclaim keeps the deferral envelope")
    ops.claim(handle)
    before = _scheduling_snapshot(handle)

    ops.claim(handle)
    renewal = claimstate.renew_claim(handle)

    assert renewal.renewed is True
    assert _scheduling_snapshot(handle) == before


def test_unclaim_preserves_scheduling(task_repo):
    handle = _deferred_task("Unclaim keeps the deferral envelope")
    before = _scheduling_snapshot(handle)
    ops.claim(handle)

    ops.unclaim(handle)

    assert _scheduling_snapshot(handle) == before
    row = identity.resolve(handle)
    assert str(row.get("claim_by") or "") == ""
    assert str(row.get("start") or "") == ""


def test_deferred_plan_to_todo_advancement_preserves_scheduling(task_repo):
    handle = _deferred_task(
        "Deferred plan advancement keeps the envelope", flow=["plan", "todo"]
    )
    before = _scheduling_snapshot(handle)
    ops.claim(handle)

    output = ops.done(handle, validation=["plan phase validated"])

    assert f"advanced {handle} -> todo" in output
    assert _scheduling_snapshot(handle) == before
    row = identity.resolve(handle)
    assert str(row.get("phase") or "") == "todo"
    assert str(row.get("claim_by") or "") == ""
    # Still deferred: the todo phase stays off the allocator until woken.
    assert alloc.next_task() is None


def test_deferred_todo_to_review_advancement_preserves_scheduling(task_repo):
    handle = _deferred_task(
        "Deferred review advancement keeps the envelope", flow=["todo", "review"]
    )
    before = _scheduling_snapshot(handle)
    ops.claim(handle)

    output = ops.done(handle, validation=["todo phase validated"])

    assert f"advanced {handle} -> review" in output
    assert _scheduling_snapshot(handle) == before
    row = identity.resolve(handle)
    assert str(row.get("phase") or "") == "review"
    assert str(row.get("review_author") or "") == ACTOR
    assert alloc.next_task() is None


def test_deferred_review_completion_preserves_scheduling(task_repo, monkeypatch):
    handle = _deferred_task(
        "Deferred review completion keeps the envelope", flow=["todo", "review"]
    )
    ops.claim(handle)
    ops.done(handle, validation=["todo phase validated"])
    before = _scheduling_snapshot(handle)
    reviewer = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    monkeypatch.setenv(DRIVER.thread_id_env, reviewer)

    ops.claim(handle)
    assert _scheduling_snapshot(handle) == before
    output = ops.review(handle, finding="clean", note="deferral intact")

    assert f"completed {handle}" in output
    assert _scheduling_snapshot(handle) == before
    row = identity.resolve(handle)
    assert str(row.get("review_by") or "") == reviewer


def test_blocked_task_claim_preserves_scheduling(task_repo):
    blocker = create.add(
        "Blocker in front of the deferred follow-up",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["blocker exists"],
    )
    handle = create.add(
        "Blocked task keeps its scheduling envelope",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["blocked claim leaves scheduling untouched"],
        after=[blocker],
        due="2099-03-04T05:06:07Z",
    )
    before = _scheduling_snapshot(handle)
    assert before["due"] != ""

    ops.claim(handle)

    assert _scheduling_snapshot(handle) == before
    row = identity.resolve(handle)
    assert str(row.get("claim_by") or "") == ACTOR


def test_ready_task_claim_preserves_scheduling(task_repo):
    handle = create.add(
        "Ready task keeps its SLA due date",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["ready claim leaves scheduling untouched"],
    )
    before = _scheduling_snapshot(handle)
    assert before["due"] != ""

    ops.claim(handle)

    assert _scheduling_snapshot(handle) == before
    assert handle in {identity.render_handle(r) for r in tw.export(["+ACTIVE"])}


def test_deferred_creation_suspends_sla_and_wake_starts_it(task_repo):
    deferred = create.add(
        "Deferred task starts its SLA clock at wake",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["the SLA clock starts at wake"],
        deferred=True,
    )
    assert _scheduling_snapshot(deferred)["due"] == ""

    output = ops.wake([deferred])
    ordinary = create.add(
        "Ordinary task of the same priority",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["the ordinary SLA baseline"],
    )

    woken_due = _parse_tw_datetime(_scheduling_snapshot(deferred)["due"])
    ordinary_due = _parse_tw_datetime(_scheduling_snapshot(ordinary)["due"])
    assert f"woke {deferred}: wait: due:" in output
    # Same seam, same priority, moments apart: the woken clock matches the
    # ordinary creation clock to within test runtime.
    assert abs((woken_due - ordinary_due).total_seconds()) <= SLA_TOLERANCE_SECONDS
    assert deferred in _ready_handles()


def test_deferred_explicit_due_stays_exact_through_wake(task_repo):
    handle = create.add(
        "Deferred task keeps its explicit due",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["an explicit due survives deferral"],
        deferred=True,
        due="2099-03-04T05:06:07Z",
    )
    before = _scheduling_snapshot(handle)
    assert before["due"] == "20990304T050607Z"

    output = ops.wake([handle])

    after = _scheduling_snapshot(handle)
    assert f"woke {handle}: wait:" in output
    assert after["due"] == before["due"]
    assert after["wait"] == ""
    assert handle in _ready_handles()


def test_wake_into_promotion_starts_sla_clock(task_repo):
    created = ops.oops(
        "Promoted oops starts its SLA clock",
        description="promotion candidate",
        origin="ack:1jN54zJJ",
    )
    handle = created.split()[1]
    assert _scheduling_snapshot(handle)["due"] == ""
    started = datetime.now(UTC)

    output = ops.wake([handle], into="task.unit")

    row = identity.resolve(handle)
    fresh = identity.render_handle(row)
    priority = str(row.get("priority") or "")
    sla_seconds = config.SLA_DUE_SECONDS[priority]
    due_delta = (_parse_tw_datetime(str(row.get("due"))) - started).total_seconds()
    assert f"promoted {handle} -> {fresh}: wait: project:task.unit due:" in output
    assert sla_seconds - SLA_TOLERANCE_SECONDS <= due_delta
    assert due_delta <= sla_seconds + SLA_TOLERANCE_SECONDS
    assert fresh in _ready_handles()


def test_wake_clears_only_wait(task_repo):
    handle = _deferred_task("Wake clears wait and nothing else")
    before = _scheduling_snapshot(handle)

    ops.wake([handle])

    after = _scheduling_snapshot(handle)
    assert after["wait"] == ""
    assert after["wait"] != before["wait"]
    rest = ("scheduled", "due", "until")
    assert {field: after[field] for field in rest} == {
        field: before[field] for field in rest
    }
    assert handle in _ready_handles()


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-b", "main")
    _run(path, "git", "config", "user.email", "spice@example.test")
    _run(path, "git", "config", "user.name", "Spice Tests")
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "initial")
    return path


def _ready_handles() -> set[str]:
    rows = tw.export(["status:pending", "+READY", "-ACTIVE"])
    return {
        identity.render_handle(row)
        for row in rows
        if not alloc.is_hidden(row) and not str(row.get("claim_by") or "")
    }


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
