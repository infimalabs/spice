"""Deferred task creation, intake, and lifecycle scheduling coverage."""

from __future__ import annotations

import shutil
from datetime import UTC, datetime

import pytest

from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.tasks import alloc, claimstate, config, create, identity, ops, render, tw
from tests.test_reposcaffolding import init_committed_repo as _init_repo

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
SECOND_ACTOR = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

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


def test_deferred_plan_to_todo_advancement_starts_intake(task_repo, monkeypatch):
    handle = _deferred_task(
        "Deferred plan advancement starts intake", flow=["plan", "todo"]
    )
    before = _scheduling_snapshot(handle)
    ops.claim(handle)

    output = ops.done(handle, validation=["plan phase validated"])

    assert f"advanced {handle} -> todo" in output
    after = _scheduling_snapshot(handle)
    assert after["wait"] == ""
    assert {field: after[field] for field in ("scheduled", "due", "until")} == {
        field: before[field] for field in ("scheduled", "due", "until")
    }
    row = identity.resolve(handle)
    assert str(row.get("phase") or "") == "todo"
    assert str(row.get("claim_by") or "") == ""
    assert str(row.get(config.TASK_READY_AT_UDA) or "") != ""
    assert handle in _ready_handles()

    # Phase advance is intake. The claim it releases is immediately available
    # to another lane without an intervening task-wake mutation.
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )
    monkeypatch.setenv(DRIVER.thread_id_env, SECOND_ACTOR)
    offered = alloc.next_task()
    assert offered is not None
    assert identity.render_handle(offered) == handle
    assert str(offered.get("claim_by") or "") == SECOND_ACTOR


def test_deferred_todo_to_review_advancement_starts_intake(task_repo):
    handle = _deferred_task(
        "Deferred review advancement starts intake", flow=["todo", "review"]
    )
    before = _scheduling_snapshot(handle)
    ops.claim(handle)

    output = ops.done(handle, validation=["todo phase validated"])

    assert f"advanced {handle} -> review" in output
    after = _scheduling_snapshot(handle)
    assert after["wait"] == ""
    assert {field: after[field] for field in ("scheduled", "due", "until")} == {
        field: before[field] for field in ("scheduled", "due", "until")
    }
    row = identity.resolve(handle)
    assert str(row.get("phase") or "") == "review"
    assert str(row.get("review_author") or "") == ACTOR
    assert str(row.get(config.TASK_READY_AT_UDA) or "") != ""
    assert handle in _ready_handles()


def test_deferred_review_completion_preserves_intake_scheduling(task_repo, monkeypatch):
    handle = _deferred_task(
        "Deferred review completion keeps intake scheduling", flow=["todo", "review"]
    )
    ops.claim(handle)
    ops.done(handle, validation=["todo phase validated"])
    before = _scheduling_snapshot(handle)
    assert before["wait"] == ""
    monkeypatch.setenv(DRIVER.thread_id_env, SECOND_ACTOR)

    ops.claim(handle)
    assert _scheduling_snapshot(handle) == before
    output = ops.review(handle, finding="clean", note="intake scheduling intact")

    assert f"completed {handle}" in output
    assert _scheduling_snapshot(handle) == before
    row = identity.resolve(handle)
    assert str(row.get("review_by") or "") == SECOND_ACTOR


def test_phase_advance_and_wake_start_the_same_suspended_sla(task_repo, monkeypatch):
    due = "20300102T030405Z"
    calls: list[tuple[str | None, str]] = []

    def fixed_sla_due_args(
        explicit: str | None, priority: str, *, auto_due: bool = True
    ) -> list[str]:
        assert auto_due is True
        calls.append((explicit, priority))
        return [f"due:{due}"]

    advanced = create.add(
        "Phase intake starts its suspended SLA",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["phase intake starts the SLA"],
        flow=["plan", "todo"],
        deferred=True,
    )
    woken = create.add(
        "Wake starts the comparison SLA",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["wake starts the SLA"],
        deferred=True,
    )
    monkeypatch.setattr(create, "sla_due_args", fixed_sla_due_args)

    ops.claim(advanced)
    ops.done(advanced, validation=["plan accepted"])
    ops.wake([woken])

    assert _scheduling_snapshot(advanced)["due"] == due
    assert _scheduling_snapshot(woken)["due"] == due
    assert calls == [(None, "M"), (None, "M")]


def test_hidden_oops_advance_clears_wait_without_entering_public_queue(
    task_repo, monkeypatch
):
    handle = create.add_one(
        title="Hidden triage advances without promotion",
        project=config.OOPS_PROJECT,
        priority=config.SEVERITY_PRIORITY["medium"],
        flow=["plan", "todo"],
        tags=[],
        after=[],
        acceptance=["triage plan is actionable"],
        wait=None,
        claim=False,
        origin="ack:1jN54zJJ",
        system_project=True,
    )
    assert _scheduling_snapshot(handle)["wait"] != ""
    ops.claim(handle)

    ops.done(handle, validation=["triage planned"])

    row = identity.resolve(handle)
    assert str(row.get("phase") or "") == "todo"
    assert str(row.get("wait") or "") == ""
    assert alloc.is_hidden(row)
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {
            "filter": [f"project:{config.OOPS_PROJECT}"],
            "lifetime": "Drive",
        },
    )
    monkeypatch.setenv(DRIVER.thread_id_env, SECOND_ACTOR)
    assert alloc.next_task() is None


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


def test_wake_bare_oops_leads_with_claiming_it_in_place(task_repo):
    created = ops.oops(
        "Bare wake of an oops has a repair",
        description="triage stays in plan mode",
        origin="ack:1jN54zJJ",
    )
    handle = created.split()[1]

    with pytest.raises(SpiceError) as exc_info:
        ops.wake([handle])

    assert str(exc_info.value) == (
        "claim the deferred oops triage task in place with "
        f"`spice task claim {handle}` because it is already in plan mode, then "
        f"create and connect public child tasks; cannot wake it: {handle}"
    )


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
    assert f"promoted {handle} -> {fresh}: wait: due:" in output
    assert "project:task.unit" in output
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


def _ready_handles() -> set[str]:
    rows = tw.export(["status:pending", "+READY", "-ACTIVE"])
    return {
        identity.render_handle(row)
        for row in rows
        if not alloc.is_hidden(row) and not str(row.get("claim_by") or "")
    }
