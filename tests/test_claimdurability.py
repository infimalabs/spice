"""Claim durability: what an actively-working agent is told when its claim moves.

The lease is a deadline, not a fence. Anything that ends or reassigns a claim
while the holder is still working has to reach the holder, because the holder
keeps editing files and eventually tries to land them against a row it no
longer owns.
"""

from __future__ import annotations

import shutil

import pytest

from spice.agent import lifecycle, watchdog
from spice.agent.driver import DRIVER
from spice.tasks import alloc, claimstate, create, identity, ops, tw
from tests.test_tasks import ACTOR_A, PEER_ACTOR, task_repo

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

__all__ = ["task_repo"]

LAPSED_DEADLINE = "2020-01-01T00:00:00.000000Z"
SHORT_LEASE_SECONDS = 2.0
UNIT_ROUTE = {"filter": ["project:task.unit"], "lifetime": "Drive"}


def _capture_feedback(monkeypatch) -> list[tuple[str, dict[str, object]]]:
    feedback: list[tuple[str, dict[str, object]]] = []
    monkeypatch.setattr(
        watchdog,
        "publish_supervisor_feedback",
        lambda _repo, _log, kind, **fields: feedback.append((kind, fields)),
    )
    return feedback


def _route_peer_allocator(monkeypatch) -> None:
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: dict(UNIT_ROUTE),
    )


def _lapse_lease(handle: str) -> str:
    """Starve the heartbeat the way a loaded host does, leaving the row stale."""
    uuid = identity.uuid_of(identity.resolve(handle))
    tw.run([uuid, "modify", f"claim_until:{LAPSED_DEADLINE}"])
    return uuid


def test_supervisor_heartbeat_names_the_peer_that_took_a_working_claim(
    task_repo, monkeypatch
):
    """The probe: a claim leaves the lane mid-work and the holder must hear it."""
    _route_peer_allocator(monkeypatch)
    handle = create.add(
        "Work still in flight when the claim moves",
        project="task.unit",
        origin="ack:1kG4pGxs",
        priority="medium",
        acceptance=["the holder hears that its claim left the lane"],
    )
    ops.claim(handle)
    log_path = task_repo / "supervisor.log"
    feedback = _capture_feedback(monkeypatch)
    reported: dict[str, str] = {}
    held: dict[str, str] = {}

    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, reported, {}, held)
    _lapse_lease(handle)
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    taken = alloc.next_task()
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, reported, {}, held)

    assert identity.render_handle(taken or {}) == handle
    assert identity.resolve(handle)["claim_by"] == PEER_ACTOR
    assert feedback == [
        (
            "claim.renewal-skipped",
            {"reason": "claimed_by_other", "handle": handle, "detail": PEER_ACTOR},
        )
    ]
    assert log_path.read_text(encoding="utf-8") == (
        "spice claim renewal skipped: "
        f"reason=claimed_by_other handle={handle} detail={PEER_ACTOR}\n"
    )
    assert held == {}


def test_heartbeat_holds_a_claim_across_an_operation_longer_than_its_lease(
    task_repo, monkeypatch
):
    """A single long command outruns the lease; the timer, not the agent, renews."""
    _route_peer_allocator(monkeypatch)
    handle = create.add(
        "One command longer than the lease",
        project="task.unit",
        origin="ack:1kG4pGxs",
        priority="medium",
        acceptance=["a long operation keeps its claim without running a command"],
    )
    row = identity.resolve(handle)
    claimstate.do_claim(
        identity.uuid_of(row),
        ACTOR_A,
        site=claimstate.current_claim_site(),
        context_thread=ACTOR_A,
        lease_seconds=SHORT_LEASE_SECONDS,
    )
    log_path = task_repo / "supervisor.log"
    feedback = _capture_feedback(monkeypatch)
    held: dict[str, str] = {}

    # The operation is still running: the agent issues nothing, and its lease
    # has already elapsed. Only the supervisor's own beat can save the row.
    lapsed = _lapse_lease(handle)
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, {}, held)
    renewed = identity.resolve(handle)
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    peer_assignment = alloc.next_task()

    assert identity.uuid_of(renewed) == lapsed
    assert renewed["claim_by"] == ACTOR_A
    assert renewed["claim_until"] > tw.now_iso()
    assert peer_assignment is None
    assert feedback == []
    assert held == {"handle": handle}


def test_heartbeat_stays_quiet_when_the_agent_advances_its_own_phase(
    task_repo, monkeypatch
):
    """`task done` hands the claim back on purpose and already says so."""
    handle = create.add(
        "Advance the phase and release on purpose",
        project="task.unit",
        origin="ack:1kG4pGxs",
        priority="medium",
        acceptance=["a deliberate advance draws no loss alarm"],
    )
    ops.claim(handle)
    log_path = task_repo / "supervisor.log"
    feedback = _capture_feedback(monkeypatch)
    held: dict[str, str] = {}

    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, {}, held)
    advanced = dict(held)
    ops.done(handle, validation=["phase advanced by its own holder"])
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, {}, held)

    assert advanced == {"handle": handle}
    assert identity.resolve(handle)["phase"] == "review"
    assert feedback == []
    assert held == {}


def test_heartbeat_reports_a_claim_that_moved_to_another_worktree(
    task_repo, monkeypatch
):
    """Two trees contending one handle is a loss the losing tree must hear."""
    handle = create.add(
        "Same actor, two worktrees, one handle",
        project="task.unit",
        origin="ack:1kG4pGxs",
        priority="medium",
        acceptance=["the tree that no longer owns the row is told"],
    )
    ops.claim(handle)
    log_path = task_repo / "supervisor.log"
    feedback = _capture_feedback(monkeypatch)
    held: dict[str, str] = {}

    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, {}, held)
    uuid = identity.uuid_of(identity.resolve(handle))
    tw.run([uuid, "modify", f"claim_worktree:{task_repo.parent / 'worktree-z'}"])
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, {}, held)

    assert feedback == [
        (
            "claim.renewal-skipped",
            {"reason": "different_worktree", "handle": handle, "detail": ""},
        )
    ]
    assert held == {}


def test_heartbeat_stays_quiet_for_a_lane_that_never_held_a_claim(
    task_repo, monkeypatch
):
    """Silence is correct only where there is no claim to lose."""
    create.add(
        "Unclaimed work on a quiet lane",
        project="task.unit",
        origin="ack:1kG4pGxs",
        priority="medium",
        acceptance=["an idle lane draws no claim-loss noise"],
    )
    log_path = task_repo / "supervisor.log"
    feedback = _capture_feedback(monkeypatch)
    reported: dict[str, str] = {}

    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, reported, {}, {})
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, reported, {}, {})

    assert feedback == []
    assert claimstate.active_claim(ACTOR_A) is None
    assert reported == {}
