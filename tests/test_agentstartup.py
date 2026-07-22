"""Agent first-activity readiness and stalled-launch recovery contracts."""

import argparse
import subprocess

import pytest

from spice.agent import lifecycle, lifecyclebinding, sidechannel
from spice.agent.watchdog import AgentStartupSignal
from spice.process.groups import PROCESS_GROUP_TERMINATION_BOUND_SECONDS

SUPERVISED_AGENT_PID = 4444
STARTUP_TERMINATED_EXIT_CODE = -15
STARTUP_TEST_GRACE_SECONDS = 0.01
THREAD_JOIN_TIMEOUT_SECONDS = 1.0
SLOW_CLEANUP_TEST_TIMEOUT_SECONDS = 5.0


@pytest.fixture(autouse=True)
def _git_worktree_tmp_path(request, tmp_path):
    if "tmp_path" in request.fixturenames:
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)


class _FakeProcess:
    def __init__(self, *, returncode: int | None) -> None:
        self.pid = SUPERVISED_AGENT_PID
        self.returncode = returncode
        self.wait_calls = 0

    def poll(self):
        return self.returncode

    def wait(self):
        self.wait_calls += 1
        return self.returncode


class _SilentProcess(_FakeProcess):
    def __init__(self) -> None:
        super().__init__(returncode=None)
        self.finished = lifecycle.Event()

    def wait(self):
        self.wait_calls += 1
        self.finished.wait(timeout=THREAD_JOIN_TIMEOUT_SECONDS)
        return self.returncode


class _FakeThread:
    def __init__(self, startup_signal: AgentStartupSignal) -> None:
        self.startup_signal = startup_signal
        self.joined_timeouts: list[float | None] = []

    def join(self, timeout: float | None = None) -> None:
        self.joined_timeouts.append(timeout)


class _FakeSideChannel:
    def __init__(self, repo_root) -> None:
        self.repo_root = repo_root

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, traceback) -> None:
        return None


def test_first_activity_transitions_starting_agent_to_ready_with_hook_warning(
    tmp_path, monkeypatch
):
    log_path = tmp_path / "startup.log"
    hooks_warning = (
        "warning: failed to parse hooks config /tmp/hooks.json: "
        "EOF while parsing a value\n"
    )
    log_path.write_text(hooks_warning, encoding="utf-8")
    process = _FakeProcess(returncode=None)
    thread_id = "dddddddddddddddddddddddddddddddd"
    state = lifecycle.build_agent_state(
        process=process,
        action="resume",
        command=["codex", "exec", "prompt"],
        driver="codex",
        model="gpt-test",
        reasoning_effort="high",
        service_tier="",
        thread_id=thread_id,
        prompt_skill_path=tmp_path / "skill.md",
        log_path=log_path,
        fast_mode=False,
        startup_status=lifecycle.AGENT_STARTUP_STARTING,
    )
    lifecycle.write_agent_state(tmp_path, state)
    monkeypatch.setattr(lifecyclebinding, "process_id_is_running", lambda _pid: True)
    monkeypatch.setattr(
        lifecyclebinding, "process_group_is_running", lambda _pgid: True
    )
    signal = AgentStartupSignal()
    stalled = lifecycle.Event()

    assert lifecycle.agent_status(tmp_path).process_status == "starting"
    assert lifecycle.ensure_agent(tmp_path, dry_run=True).action == "already-running"
    watcher = lifecycle.Thread(
        target=lifecycle._watch_agent_startup,
        args=(tmp_path, process, log_path, signal, stalled),
        kwargs={"grace_seconds": THREAD_JOIN_TIMEOUT_SECONDS},
    )
    watcher.start()
    signal.note_activity()
    watcher.join(timeout=THREAD_JOIN_TIMEOUT_SECONDS)

    ready = lifecycle.agent_status(tmp_path)
    assert ready.process_status == "running"
    assert ready.ready is True
    assert ready.ready_at.endswith("Z")
    assert log_path.read_text(encoding="utf-8") == hooks_warning


def test_silent_supervised_agent_stalls_recovers_and_arms_restart_refusal(
    tmp_path, monkeypatch
):
    log_path = tmp_path / "silent.log"
    log_path.write_text(
        "warning: failed to parse hooks config /tmp/hooks.json\n",
        encoding="utf-8",
    )
    process = _SilentProcess()
    startup_signal = AgentStartupSignal()
    stdout_thread = _FakeThread(startup_signal)
    thread_id = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
    terminated: list[int] = []
    monkeypatch.setattr(
        lifecycle, "FIRST_ACTIVITY_GRACE_SECONDS", STARTUP_TEST_GRACE_SECONDS
    )
    monkeypatch.setattr(lifecycle, "agent_environment", lambda _repo: {"ENV": "1"})
    monkeypatch.setattr(
        lifecycle,
        "spawn_supervised_agent",
        lambda command, *, cwd, log_path, env: (process, stdout_thread),
    )
    monkeypatch.setattr(
        lifecycle,
        "require_started_process",
        lambda _process, _log_path, **_kwargs: None,
    )
    monkeypatch.setattr(
        lifecycle,
        "started_agent_thread_id",
        lambda _log_path, *, repo_root, fallback_thread_id: thread_id,
    )
    monkeypatch.setattr(
        sidechannel,
        "AgentSideChannelServer",
        lambda repo_root: _FakeSideChannel(repo_root),
    )

    def terminate_silent_agent(target) -> None:
        terminated.append(target.pid)
        target.returncode = STARTUP_TERMINATED_EXIT_CODE
        target.finished.set()

    monkeypatch.setattr(lifecycle, "terminate_process_group", terminate_silent_agent)
    args = argparse.Namespace(
        repo_root=str(tmp_path),
        action="resume",
        model="gpt-test",
        reasoning_effort="high",
        service_tier="",
        resume_thread_id=thread_id,
        log_path=str(log_path),
        fast_mode=False,
        command_json='["codex","exec","prompt"]',
    )

    assert lifecycle.run_agent_supervisor(args) == STARTUP_TERMINATED_EXIT_CODE
    status = lifecycle.agent_status(tmp_path)
    outcomes = lifecycle.read_launch_outcomes(tmp_path)
    assert terminated == [SUPERVISED_AGENT_PID]
    assert status.process_status == lifecycle.AGENT_STARTUP_STALLED
    assert "no driver-defined first activity" in status.startup_failure
    assert outcomes[0]["failure_kind"] == lifecycle.AGENT_FAILURE_STARTUP_STALLED
    assert lifecycle.ensure_agent(tmp_path, dry_run=True).action == "would-resume"

    for _ in range(lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD - 1):
        lifecycle.record_launch_outcome(
            tmp_path,
            {
                "lifetime_seconds": lifecycle.FIRST_ACTIVITY_GRACE_SECONDS,
                "failure_kind": lifecycle.AGENT_FAILURE_STARTUP_STALLED,
                "ended_at": lifecycle.utc_now(),
            },
        )
    refusal = lifecycle.launch_refusal(tmp_path)
    assert isinstance(refusal, dict)
    assert (
        refusal["consecutive_rapid_deaths"] == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD
    )


def test_startup_stall_waits_for_slow_group_cleanup_and_terminal_state(
    tmp_path, monkeypatch
):
    log_path = tmp_path / "slow-cleanup.log"
    log_path.write_text("silent startup\n", encoding="utf-8")
    process = _SilentProcess()
    stdout_thread = _FakeThread(AgentStartupSignal())
    thread_id = "ffffffffffffffffffffffffffffffff"
    cleanup_started = lifecycle.Event()
    release_cleanup = lifecycle.Event()
    ordered_events: list[str] = []
    supervisor_results: list[int] = []
    record_launch_outcome = lifecycle.record_launch_outcome
    monkeypatch.setattr(
        lifecycle, "FIRST_ACTIVITY_GRACE_SECONDS", STARTUP_TEST_GRACE_SECONDS
    )
    monkeypatch.setattr(lifecycle, "agent_environment", lambda _repo: {"ENV": "1"})
    monkeypatch.setattr(
        lifecycle,
        "spawn_supervised_agent",
        lambda command, *, cwd, log_path, env: (process, stdout_thread),
    )
    monkeypatch.setattr(
        lifecycle,
        "require_started_process",
        lambda _process, _log_path, **_kwargs: None,
    )
    monkeypatch.setattr(
        lifecycle,
        "started_agent_thread_id",
        lambda _log_path, *, repo_root, fallback_thread_id: thread_id,
    )
    monkeypatch.setattr(
        sidechannel,
        "AgentSideChannelServer",
        lambda repo_root: _FakeSideChannel(repo_root),
    )

    def terminate_after_slow_cleanup(target) -> None:
        ordered_events.append("cleanup-started")
        target.returncode = STARTUP_TERMINATED_EXIT_CODE
        target.finished.set()
        cleanup_started.set()
        release_cleanup.wait(timeout=SLOW_CLEANUP_TEST_TIMEOUT_SECONDS)
        ordered_events.append("cleanup-complete")

    def record_terminal_outcome(repo_root, outcome) -> None:
        state = lifecycle.read_agent_state(repo_root)
        ordered_events.append(f"outcome-after-{state['startup_status']}")
        record_launch_outcome(repo_root, outcome)
        ordered_events.append("outcome-recorded")

    monkeypatch.setattr(
        lifecycle, "terminate_process_group", terminate_after_slow_cleanup
    )
    monkeypatch.setattr(lifecycle, "record_launch_outcome", record_terminal_outcome)
    args = argparse.Namespace(
        repo_root=str(tmp_path),
        action="resume",
        model="gpt-test",
        reasoning_effort="high",
        service_tier="",
        resume_thread_id=thread_id,
        log_path=str(log_path),
        fast_mode=False,
        command_json='["codex","exec","prompt"]',
    )

    def run_supervisor() -> None:
        supervisor_results.append(lifecycle.run_agent_supervisor(args))
        ordered_events.append("supervisor-returned")

    assert lifecycle.STARTUP_WATCH_JOIN_SECONDS == (
        PROCESS_GROUP_TERMINATION_BOUND_SECONDS
        + lifecycle.STARTUP_STATE_PERSISTENCE_ALLOWANCE_SECONDS
    )
    supervisor = lifecycle.Thread(target=run_supervisor)
    supervisor.start()
    assert cleanup_started.wait(timeout=SLOW_CLEANUP_TEST_TIMEOUT_SECONDS) is True
    assert ordered_events == ["cleanup-started"]

    release_cleanup.set()
    supervisor.join(timeout=SLOW_CLEANUP_TEST_TIMEOUT_SECONDS)

    assert ordered_events == [
        "cleanup-started",
        "cleanup-complete",
        f"outcome-after-{lifecycle.AGENT_STARTUP_STALLED}",
        "outcome-recorded",
        "supervisor-returned",
    ]
    assert supervisor_results == [STARTUP_TERMINATED_EXIT_CODE]
    assert lifecycle.read_launch_outcomes(tmp_path)[0]["failure_kind"] == (
        lifecycle.AGENT_FAILURE_STARTUP_STALLED
    )
