"""Launch claims: a launch that never starts hands its reserved task back.

Serve reserves one READY row before the agent process exists, so nothing can
take it out from under a lane that is still starting. That reservation is
renewed only while the launch's own child lives, so a launch that dies before
first activity would otherwise hold an allocatable task for a whole lease with
nobody left to work it. The supervisor sees that ending and returns the row.
"""

from __future__ import annotations

import argparse
import shutil
from pathlib import Path

import pytest

from spice.agent import lifecycle, sidechannel
from spice.agent.paths import agent_thread_state_dir
from spice.agent.watchdog import AgentStartupSignal
from spice.errors import SpiceError
from spice.tasks import alloc, claimstate, create, identity, tw
from tests.test_tasks import ACTOR_A, PEER_ACTOR, task_repo

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

__all__ = ["task_repo"]

SUPERVISED_AGENT_PID = 4444
SUCCESSOR_AGENT_PID = 5555
LAUNCH_EXIT_CODE = 1
# The launch's own thread id, deliberately unlike the reservation's owner:
# serve reserves as the stopped lane's actor, and a renewal launch starts a
# brand-new thread, so the handback can never key off the started thread id.
LAUNCH_THREAD_ID = "cccccccccccccccccccccccccccccccc"
OUT_OF_CREDITS_LINE = "Credit balance too low to start this session\n"
UNREADABLE_STATE_DETAIL = "agent state is unreadable"
UNREADABLE_STATE_OSERROR_DETAIL = "agent state path cannot be read"
CLAIM_WITNESS_OSERROR_DETAIL = "claim witness cannot be written"
LAUNCH_ORIGIN = "ack:1kG9Z9zf"
LAUNCH_PROJECT = "serve.launch"


class _FakeProcess:
    def __init__(self, *, pid: int, returncode: int | None) -> None:
        self.pid = pid
        self.returncode = returncode

    def poll(self):
        return self.returncode

    def wait(self):
        return self.returncode


class _FakeThread:
    def __init__(self) -> None:
        self.startup_signal = AgentStartupSignal()

    def join(self, timeout: float | None = None) -> None:
        del timeout


class _FakeSideChannel:
    def __init__(self, repo_root: Path) -> None:
        self.repo_root = repo_root

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, traceback) -> None:
        return None


def _reserved_task(title: str, actor: str) -> tuple[str, str]:
    """One READY row held exactly the way serve holds it across a launch."""
    handle = create.add(title, project=LAUNCH_PROJECT, origin=LAUNCH_ORIGIN)
    uuid = str(identity.resolve(handle)["uuid"])
    assert _hold(uuid, actor, guard_unclaimed=True) is True
    return handle, uuid


def _hold(uuid: str, actor: str, *, guard_unclaimed: bool) -> bool:
    return claimstate.do_claim(
        uuid,
        actor,
        site=claimstate.current_claim_site(),
        context_thread=actor,
        lease_seconds=lifecycle.SUPERVISOR_CLAIM_LEASE_SECONDS,
        guard_unclaimed=guard_unclaimed,
    )


def _publish_launch_state(
    repo_root: Path,
    process: _FakeProcess,
    log_path: Path,
    *,
    startup_status: str,
) -> None:
    """Bind the lane to this process the way a started supervisor binds it."""
    state = lifecycle.build_agent_state(
        process=process,
        action="start",
        command=["codex", "exec", "prompt"],
        driver="codex",
        model="gpt-test",
        reasoning_effort="high",
        service_tier="",
        thread_id=LAUNCH_THREAD_ID,
        prompt_skill_path=repo_root / "skill.md",
        log_path=log_path,
        fast_mode=False,
        startup_status=startup_status,
    )
    lifecycle.write_agent_state(repo_root, state)


def _allocatable_handles() -> set[str]:
    rows = tw.export(["status:pending", "+READY", "-ACTIVE"])
    return {
        identity.render_handle(row)
        for row in rows
        if not alloc.is_hidden(row) and not str(row.get("claim_by") or "")
    }


def _claim_owner(handle: str) -> str:
    return str(identity.resolve(handle).get("claim_by") or "")


def _unreadable_agent_state(_repo_root: Path) -> dict[str, object]:
    raise SpiceError(UNREADABLE_STATE_DETAIL)


def _unreadable_agent_state_oserror(_repo_root: Path) -> dict[str, object]:
    raise OSError(UNREADABLE_STATE_OSERROR_DETAIL)


def test_launch_that_dies_out_of_credits_returns_its_task_to_the_board(
    task_repo, monkeypatch
):
    """The whole supervised launch, from reservation to an allocatable row."""
    repo_root = task_repo.resolve()
    handle, uuid = _reserved_task("Reserved for a launch that never starts", ACTOR_A)
    log_path = repo_root / "launch.log"
    log_path.write_text(OUT_OF_CREDITS_LINE, encoding="utf-8")
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=LAUNCH_EXIT_CODE)
    stdout_thread = _FakeThread()
    monkeypatch.setattr(lifecycle, "agent_environment", lambda _repo: {"ENV": "1"})
    monkeypatch.setattr(
        lifecycle,
        "spawn_supervised_agent",
        lambda command, *, cwd, log_path, env: (process, stdout_thread),
    )
    monkeypatch.setattr(
        lifecycle, "require_started_process", lambda _process, _log, **_kwargs: None
    )
    monkeypatch.setattr(
        lifecycle,
        "started_agent_thread_id",
        lambda _log, *, repo_root, fallback_thread_id: LAUNCH_THREAD_ID,
    )
    monkeypatch.setattr(
        lifecycle, "_watch_supervised_lane", lambda *_args, **_kwargs: None
    )
    monkeypatch.setattr(
        sidechannel,
        "AgentSideChannelServer",
        lambda repo_root, **_kwargs: _FakeSideChannel(repo_root),
    )
    args = argparse.Namespace(
        repo_root=str(repo_root),
        action="start",
        model="gpt-test",
        reasoning_effort="high",
        service_tier="",
        resume_thread_id=LAUNCH_THREAD_ID,
        log_path=str(log_path),
        fast_mode=False,
        command_json='["codex","exec","prompt"]',
        launch_claim_uuid=uuid,
        launch_claim_actor=ACTOR_A,
    )

    # Held across the launch: the reservation is what keeps a peer off the row
    # while this lane starts, so it is still owned when the launch dies.
    assert _claim_owner(handle) == ACTOR_A
    exit_code = lifecycle.run_agent_supervisor(args)

    outcome = lifecycle.read_launch_outcomes(repo_root)[-1]
    settled_log = (
        agent_thread_state_dir(repo_root, LAUNCH_THREAD_ID) / "logs" / log_path.name
    ).read_text(encoding="utf-8")
    assert exit_code == LAUNCH_EXIT_CODE
    # Immediately, on the launch's own ending — not when the lease runs out.
    assert _claim_owner(handle) == ""
    assert handle in _allocatable_handles()
    assert outcome["released_claim"] == uuid
    assert outcome["failure_kind"] == lifecycle.AGENT_FAILURE_OUT_OF_CREDITS
    # The provider's own refusal survives beside the handback it caused.
    assert OUT_OF_CREDITS_LINE in settled_log
    assert f"spice launch claim released: {uuid} reserved for {ACTOR_A}" in settled_log


def test_supervisor_records_third_rapid_death_when_claim_handback_raises_oserror(
    task_repo, monkeypatch
):
    """A failed terminal handback cannot suppress restart-storm memory."""
    repo_root = task_repo.resolve()
    handle, uuid = _reserved_task("Held when terminal handback cannot finish", ACTOR_A)
    log_path = repo_root / "supervisor-handback.log"
    log_path.write_text("", encoding="utf-8")
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=LAUNCH_EXIT_CODE)
    stdout_thread = _FakeThread()
    monkeypatch.setattr(lifecycle, "agent_environment", lambda _repo: {"ENV": "1"})
    monkeypatch.setattr(
        lifecycle,
        "spawn_supervised_agent",
        lambda command, *, cwd, log_path, env: (process, stdout_thread),
    )
    monkeypatch.setattr(
        lifecycle, "require_started_process", lambda _process, _log, **_kwargs: None
    )
    monkeypatch.setattr(
        lifecycle,
        "started_agent_thread_id",
        lambda _log, *, repo_root, fallback_thread_id: LAUNCH_THREAD_ID,
    )
    monkeypatch.setattr(
        lifecycle, "_watch_supervised_lane", lambda *_args, **_kwargs: None
    )
    monkeypatch.setattr(
        sidechannel,
        "AgentSideChannelServer",
        lambda repo_root, **_kwargs: _FakeSideChannel(repo_root),
    )

    def fail_claim_handback(*_args, **_kwargs):
        raise OSError(CLAIM_WITNESS_OSERROR_DETAIL)

    monkeypatch.setattr(claimstate, "release_claim", fail_claim_handback)
    for _index in range(lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD - 1):
        lifecycle.record_launch_outcome(
            repo_root,
            {
                "lifetime_seconds": 0.5,
                "exit_code": LAUNCH_EXIT_CODE,
                "ended_at": lifecycle.utc_now(),
            },
        )
    args = argparse.Namespace(
        repo_root=str(repo_root),
        action="start",
        model="gpt-test",
        reasoning_effort="high",
        service_tier="",
        resume_thread_id=LAUNCH_THREAD_ID,
        log_path=str(log_path),
        fast_mode=False,
        command_json='["codex","exec","prompt"]',
        launch_claim_uuid=uuid,
        launch_claim_actor=ACTOR_A,
    )

    exit_code = lifecycle.run_agent_supervisor(args)
    outcomes = lifecycle.read_launch_outcomes(repo_root)
    refusal = lifecycle.launch_refusal(repo_root)
    settled_log = (
        agent_thread_state_dir(repo_root, LAUNCH_THREAD_ID) / "logs" / log_path.name
    ).read_text(encoding="utf-8")

    assert exit_code == LAUNCH_EXIT_CODE
    assert _claim_owner(handle) == ACTOR_A
    assert len(outcomes) == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD
    assert outcomes[-1]["thread_id"] == LAUNCH_THREAD_ID
    assert isinstance(refusal, dict)
    assert refusal["consecutive_rapid_deaths"] == (
        lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD
    )
    assert f"spice launch claim kept: {CLAIM_WITNESS_OSERROR_DETAIL}" in settled_log


def test_launch_that_reached_readiness_keeps_the_task_it_is_working(task_repo):
    repo_root = task_repo.resolve()
    handle, uuid = _reserved_task("Worked by a live agent", ACTOR_A)
    log_path = repo_root / "ready.log"
    log_path.write_text("", encoding="utf-8")
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=None)
    _publish_launch_state(
        repo_root, process, log_path, startup_status=lifecycle.AGENT_STARTUP_READY
    )

    released = lifecycle._release_unready_launch_claim(
        repo_root, process, lifecycle.LaunchClaim(uuid=uuid, actor=ACTOR_A), log_path
    )

    assert released == ""
    assert _claim_owner(handle) == ACTOR_A


def test_superseded_launch_leaves_the_current_lane_binding_alone(task_repo):
    """A newer launch already owns the lane; the loser speaks for nothing."""
    repo_root = task_repo.resolve()
    handle, uuid = _reserved_task("Reserved before a relaunch", ACTOR_A)
    log_path = repo_root / "superseded.log"
    log_path.write_text("", encoding="utf-8")
    loser = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=LAUNCH_EXIT_CODE)
    successor = _FakeProcess(pid=SUCCESSOR_AGENT_PID, returncode=None)
    _publish_launch_state(
        repo_root, successor, log_path, startup_status=lifecycle.AGENT_STARTUP_STARTING
    )

    released = lifecycle._release_unready_launch_claim(
        repo_root, loser, lifecycle.LaunchClaim(uuid=uuid, actor=ACTOR_A), log_path
    )

    assert released == ""
    assert _claim_owner(handle) == ACTOR_A


def test_unreadable_lane_state_reports_itself_rather_than_raising(
    task_repo, monkeypatch
):
    """Cleanup rides a failing launch's exit path and must not raise over it.

    Without a readable binding there is no way to tell whether this launch
    still speaks for the lane, so the reservation stays put and the lease --
    the slow path this whole handback exists to beat -- ends it instead.
    """
    repo_root = task_repo.resolve()
    handle, uuid = _reserved_task("Reserved while lane state is unreadable", ACTOR_A)
    log_path = repo_root / "unreadable.log"
    log_path.write_text("", encoding="utf-8")
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=LAUNCH_EXIT_CODE)
    monkeypatch.setattr(lifecycle, "read_agent_state", _unreadable_agent_state)

    released = lifecycle._release_unready_launch_claim(
        repo_root, process, lifecycle.LaunchClaim(uuid=uuid, actor=ACTOR_A), log_path
    )

    assert released == ""
    assert _claim_owner(handle) == ACTOR_A
    assert f"spice launch claim kept: {UNREADABLE_STATE_DETAIL}" in log_path.read_text(
        encoding="utf-8"
    )


def test_unreadable_lane_state_oserror_keeps_and_reports_the_reservation(
    task_repo, monkeypatch
):
    """Path.read_text failures stay inside terminal launch cleanup."""
    repo_root = task_repo.resolve()
    handle, uuid = _reserved_task("Reserved behind an unreadable state path", ACTOR_A)
    log_path = repo_root / "unreadable-oserror.log"
    log_path.write_text("", encoding="utf-8")
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=LAUNCH_EXIT_CODE)
    monkeypatch.setattr(lifecycle, "read_agent_state", _unreadable_agent_state_oserror)

    lifecycle._release_unready_launch_claim(
        repo_root, process, lifecycle.LaunchClaim(uuid=uuid, actor=ACTOR_A), log_path
    )

    assert _claim_owner(handle) == ACTOR_A
    assert (
        f"spice launch claim kept: {UNREADABLE_STATE_OSERROR_DETAIL}"
        in log_path.read_text(encoding="utf-8")
    )


def test_failed_claim_witness_write_reports_the_completed_row_handback(
    task_repo, monkeypatch
):
    """The witness write follows row release, but cannot escape cleanup."""
    repo_root = task_repo.resolve()
    handle, uuid = _reserved_task("Released before its witness write fails", ACTOR_A)
    log_path = repo_root / "witness-oserror.log"
    log_path.write_text("", encoding="utf-8")
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=LAUNCH_EXIT_CODE)
    _publish_launch_state(
        repo_root, process, log_path, startup_status=lifecycle.AGENT_STARTUP_STARTING
    )

    def fail_witness_write(*_args, **_kwargs):
        raise OSError(CLAIM_WITNESS_OSERROR_DETAIL)

    monkeypatch.setattr(claimstate, "_write_claim_witness", fail_witness_write)

    lifecycle._release_unready_launch_claim(
        repo_root, process, lifecycle.LaunchClaim(uuid=uuid, actor=ACTOR_A), log_path
    )

    assert handle in _allocatable_handles()
    assert (
        f"spice launch claim kept: {CLAIM_WITNESS_OSERROR_DETAIL}"
        in log_path.read_text(encoding="utf-8")
    )


def test_row_reassigned_mid_launch_stays_with_its_new_owner(task_repo):
    repo_root = task_repo.resolve()
    handle, uuid = _reserved_task("Reassigned while the launch died", ACTOR_A)
    log_path = repo_root / "reassigned.log"
    log_path.write_text("", encoding="utf-8")
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=LAUNCH_EXIT_CODE)
    _publish_launch_state(
        repo_root, process, log_path, startup_status=lifecycle.AGENT_STARTUP_STARTING
    )
    assert _hold(uuid, PEER_ACTOR, guard_unclaimed=False) is True

    released = lifecycle._release_unready_launch_claim(
        repo_root, process, lifecycle.LaunchClaim(uuid=uuid, actor=ACTOR_A), log_path
    )

    assert released == ""
    assert _claim_owner(handle) == PEER_ACTOR
    assert "spice launch claim kept: owned elsewhere now" in log_path.read_text(
        encoding="utf-8"
    )
