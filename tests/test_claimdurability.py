"""Claim durability: what an actively-working agent is told when its claim moves.

The lease is a deadline, not a fence. Anything that ends or reassigns a claim
while the holder is still working has to reach the holder, because the holder
keeps editing files and eventually tries to land them against a row it no
longer owns.
"""

from __future__ import annotations

import shutil
from threading import Event, Thread, Timer

import pytest

from spice.agent import lifecycle, watchdog
from spice.agent.driver import DRIVER
from spice.tasks import alloc, claimstate, create, identity, ops, tw
from tests.test_tasks import ACTOR_A, PEER_ACTOR, task_repo

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

__all__ = ["task_repo"]

SHORT_LEASE_SECONDS = 2.0
SHORT_RENEWAL_SECONDS = SHORT_LEASE_SECONDS / 4
LONG_OPERATION_SECONDS = SHORT_LEASE_SECONDS * 1.5
LEASE_EXPIRY_GRACE_SECONDS = SHORT_RENEWAL_SECONDS
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


def _wait_for_claim_lease_to_expire(handle: str) -> None:
    """Let a real shortened lease elapse without polling or editing its deadline."""
    elapsed = Event()
    timer = Timer(SHORT_LEASE_SECONDS + LEASE_EXPIRY_GRACE_SECONDS, elapsed.set)
    timer.start()
    try:
        assert elapsed.wait(15.0)
    finally:
        timer.cancel()
    assert identity.resolve(handle)["claim_until"] < tw.now_iso()


def test_restarted_supervisor_names_peer_after_preclaim_quiet_heartbeat(
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
    log_path = task_repo / "supervisor.log"
    feedback: list[tuple[str, dict[str, object]]] = []
    loss_reported = Event()

    def capture_loss(_repo, _log, kind, **fields):
        if kind.startswith("claim.renewal-"):
            feedback.append((kind, fields))
            loss_reported.set()

    monkeypatch.setattr(watchdog, "publish_supervisor_feedback", capture_loss)
    reported: dict[str, str] = {}
    held: dict[str, str] = {}

    # A quiet beat happens before the agent claims. This fresh/restarted
    # supervisor has never held the row in memory when the host then starves
    # beyond the lease; only the durable witness can identify the lost row.
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, reported, {}, held)
    assert held == {}
    monkeypatch.setattr(claimstate.config, "CLAIM_TTL_SECONDS", SHORT_LEASE_SECONDS)
    ops.claim(handle)
    _wait_for_claim_lease_to_expire(handle)
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    taken = alloc.next_task()
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    signal = lifecycle.SupervisorLaneSignal()
    watcher = Thread(
        target=lifecycle._watch_supervised_lane,
        args=(task_repo, ACTOR_A, log_path, _AliveProcess(), signal),
        daemon=True,
    )
    watcher.start()
    try:
        assert loss_reported.wait(15.0)
    finally:
        signal.stop()
        watcher.join(timeout=2.0)

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

    monkeypatch.setattr(
        lifecycle, "SUPERVISOR_CLAIM_RENEWAL_SECONDS", SHORT_RENEWAL_SECONDS
    )
    monkeypatch.setattr(
        lifecycle, "SUPERVISOR_CLAIM_LEASE_SECONDS", SHORT_LEASE_SECONDS
    )
    first_renewal = Event()
    original_renew = lifecycle._renew_supervised_claim

    def observed_renewal(*args, **kwargs):
        original_renew(*args, **kwargs)
        first_renewal.set()

    monkeypatch.setattr(lifecycle, "_renew_supervised_claim", observed_renewal)
    signal = lifecycle.SupervisorLaneSignal()
    process = _AliveProcess()
    watcher = Thread(
        target=lifecycle._watch_supervised_lane,
        args=(task_repo, ACTOR_A, log_path, process, signal),
        daemon=True,
    )
    watcher.start()
    try:
        assert first_renewal.wait(15.0)

        # This is the long command: the agent blocks and issues no Spice command
        # while the independent supervisor timer renews more than once.
        operation = Event()
        operation.wait(LONG_OPERATION_SECONDS)
        monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
        peer_assignment = alloc.next_task()
        renewed = identity.resolve(handle)
    finally:
        signal.stop()
        watcher.join(timeout=2.0)

    assert renewed["claim_by"] == ACTOR_A
    assert renewed["claim_until"] > tw.now_iso()
    assert peer_assignment is None
    assert feedback == []
    assert LONG_OPERATION_SECONDS > SHORT_LEASE_SECONDS


def test_backend_failure_keeps_exact_witness_until_takeover_is_loud(
    task_repo, monkeypatch
):
    _route_peer_allocator(monkeypatch)
    handle = create.add(
        "Backend fails between claim and takeover",
        project="task.unit",
        origin="ack:1kG4pGxs",
        priority="medium",
        acceptance=["a retryable renewal failure cannot erase claim identity"],
    )
    log_path = task_repo / "supervisor.log"
    feedback = _capture_feedback(monkeypatch)
    reported: dict[str, str] = {}
    held: dict[str, str] = {}
    real_renew = claimstate.renew_claim
    monkeypatch.setattr(claimstate.config, "CLAIM_TTL_SECONDS", SHORT_LEASE_SECONDS)
    ops.claim(handle)
    monkeypatch.setattr(
        claimstate,
        "renew_claim",
        lambda *_args, **_kwargs: claimstate.ClaimRenewalResult(
            False, "backend_error", handle=handle, detail="backend unavailable"
        ),
    )

    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, reported, {}, held)
    witness_after_failure = claimstate.read_claim_witness(task_repo, ACTOR_A)
    monkeypatch.setattr(claimstate, "renew_claim", real_renew)
    _wait_for_claim_lease_to_expire(handle)
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    assert identity.render_handle(alloc.next_task() or {}) == handle
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, reported, {}, held)

    assert witness_after_failure is not None and witness_after_failure.active
    assert [kind for kind, _fields in feedback] == [
        "claim.renewal-failed",
        "claim.renewal-skipped",
    ]
    assert feedback[-1] == (
        "claim.renewal-skipped",
        {"reason": "claimed_by_other", "handle": handle, "detail": PEER_ACTOR},
    )
    retired = claimstate.read_claim_witness(task_repo, ACTOR_A)
    assert retired is not None and retired.active is False


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

    assert advanced == {
        "handle": handle,
        "uuid": identity.uuid_of(identity.resolve(handle)),
    }
    assert identity.resolve(handle)["phase"] == "review"
    assert feedback == []
    assert held == {}
    retired = claimstate.read_claim_witness(task_repo, ACTOR_A)
    assert retired is not None and retired.active is False


class _AliveProcess:
    def poll(self):
        return None


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
    retired = claimstate.read_claim_witness(task_repo, ACTOR_A)
    assert retired is not None and not retired.active


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
