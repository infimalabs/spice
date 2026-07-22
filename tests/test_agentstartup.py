"""Agent first-activity readiness and stalled-launch recovery contracts."""

import argparse
import json
import subprocess
from types import SimpleNamespace

import pytest

from spice.agent import lifecycle, lifecyclebinding, sidechannel
from spice.agent.driver import CLAUDE_DRIVER
from spice.agent.watchdog import AgentStartupSignal
from spice.process.groups import PROCESS_GROUP_TERMINATION_BOUND_SECONDS
from spice.tasks import claimstate

SUPERVISED_AGENT_PID = 4444
STARTUP_TERMINATED_EXIT_CODE = -15
STARTUP_TEST_GRACE_SECONDS = 0.01
THREAD_JOIN_TIMEOUT_SECONDS = 1.0
SLOW_CLEANUP_TEST_TIMEOUT_SECONDS = 5.0
# An already-spent first-activity window: whatever holds the wait open past it
# is the compaction phase alone, so these outcomes cannot pass by scheduling
# luck the way a merely-short window could.
EXPIRED_GRACE_SECONDS = 0.0


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


def _startup_outcome(signal: AgentStartupSignal, *, compacting_seconds: float) -> str:
    return signal.wait(EXPIRED_GRACE_SECONDS, compacting_seconds=compacting_seconds)


def test_compaction_window_governs_the_wait_until_it_settles():
    outcomes: list[str] = []

    # A resume that is compacting waits on the compaction, so first activity
    # still arrives long after the first-activity window is spent.
    compacting = AgentStartupSignal()
    compacting.note_compaction_active(True)
    waiter = lifecycle.Thread(
        target=lambda: outcomes.append(
            _startup_outcome(compacting, compacting_seconds=THREAD_JOIN_TIMEOUT_SECONDS)
        )
    )
    waiter.start()
    compacting.note_activity()
    waiter.join(timeout=THREAD_JOIN_TIMEOUT_SECONDS)

    # A compaction that never settles is still reported, against its own window.
    wedged = AgentStartupSignal()
    wedged.note_compaction_active(True)
    outcomes.append(
        _startup_outcome(wedged, compacting_seconds=STARTUP_TEST_GRACE_SECONDS)
    )

    # Once the compaction settles the first-activity deadline governs again.
    settled = AgentStartupSignal()
    settled.note_compaction_active(True)
    settled.note_compaction_active(False)
    outcomes.append(
        _startup_outcome(settled, compacting_seconds=SLOW_CLEANUP_TEST_TIMEOUT_SECONDS)
    )

    assert outcomes == ["activity", "compacting-timeout", "timeout"]


def test_compacting_resume_reaches_first_activity_instead_of_being_killed(
    tmp_path, monkeypatch
):
    log_path = tmp_path / "compacting.log"
    log_path.write_text("", encoding="utf-8")
    process = _FakeProcess(returncode=None)
    state = lifecycle.build_agent_state(
        process=process,
        action="resume",
        command=["claude", "--resume", "thread"],
        driver="claude",
        model="claude-test",
        reasoning_effort="high",
        service_tier="",
        thread_id="cccccccccccccccccccccccccccccccc",
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
    signal.note_compaction_active(True)
    stalled = lifecycle.Event()

    watcher = lifecycle.Thread(
        target=lifecycle._watch_agent_startup,
        args=(tmp_path, process, log_path, signal, stalled),
        kwargs={
            "grace_seconds": EXPIRED_GRACE_SECONDS,
            "compacting_seconds": SLOW_CLEANUP_TEST_TIMEOUT_SECONDS,
        },
    )
    watcher.start()
    # A compacting lane is alive but has produced nothing, so it stays
    # `starting` until real activity promotes it.
    assert lifecycle.agent_status(tmp_path).process_status == "starting"
    signal.note_activity()
    watcher.join(timeout=SLOW_CLEANUP_TEST_TIMEOUT_SECONDS)

    ready = lifecycle.agent_status(tmp_path)
    assert ready.process_status == "running"
    assert ready.ready is True
    assert ready.ready_at.endswith("Z")


def test_wedged_compaction_stalls_with_a_compaction_specific_failure(
    tmp_path, monkeypatch
):
    log_path = tmp_path / "wedged.log"
    log_path.write_text("", encoding="utf-8")
    process = _FakeProcess(returncode=None)
    terminated: list[int] = []
    state = lifecycle.build_agent_state(
        process=process,
        action="resume",
        command=["claude", "--resume", "thread"],
        driver="claude",
        model="claude-test",
        reasoning_effort="high",
        service_tier="",
        thread_id="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        prompt_skill_path=tmp_path / "skill.md",
        log_path=log_path,
        fast_mode=False,
        startup_status=lifecycle.AGENT_STARTUP_STARTING,
    )
    lifecycle.write_agent_state(tmp_path, state)
    monkeypatch.setattr(
        lifecycle,
        "terminate_process_group",
        lambda target: terminated.append(target.pid),
    )
    signal = AgentStartupSignal()
    signal.note_compaction_active(True)
    stalled = lifecycle.Event()

    lifecycle._watch_agent_startup(
        tmp_path,
        process,
        log_path,
        signal,
        stalled,
        grace_seconds=EXPIRED_GRACE_SECONDS,
        compacting_seconds=STARTUP_TEST_GRACE_SECONDS,
    )

    status = lifecycle.agent_status(tmp_path)
    assert terminated == [SUPERVISED_AGENT_PID]
    assert stalled.is_set() is True
    assert "compaction never settled" in status.startup_failure
    assert f"{STARTUP_TEST_GRACE_SECONDS:g}s" in status.startup_failure
    assert f"{STARTUP_TEST_GRACE_SECONDS:g}s" in log_path.read_text(encoding="utf-8")


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
        lambda repo_root, **_kwargs: _FakeSideChannel(repo_root),
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
        launch_claim_uuid="",
        launch_claim_actor="",
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
        lambda repo_root, **_kwargs: _FakeSideChannel(repo_root),
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
        launch_claim_uuid="",
        launch_claim_actor="",
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


def test_ensure_agent_starts_fresh_when_the_bound_thread_has_no_local_conversation(
    tmp_path, monkeypatch
):
    # Reproduce the spice-e brick: the worktree pointer names a Claude thread
    # with no local conversation, so a `--resume` exits within a second and,
    # while it stays bound, loops every retry into the same dead start.
    config_dir = tmp_path / "claude"
    (config_dir / "projects").mkdir(parents=True)
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(config_dir))
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)
    monkeypatch.setattr(claimstate, "active_claim_phase", lambda _actor: "")

    dead_thread = "019f880685c07312b89f6bfc6cdd0bb5"
    unknown_thread = "3c1d7e045a2b4f6c8d9e0a1b2c3d4e5f"
    unknown_dashed = "3c1d7e04-5a2b-4f6c-8d9e-0a1b2c3d4e5f"
    live_thread = "768bcba1a66f4d229ce7bcf65b5d16aa"
    live_dashed = "768bcba1-a66f-4d22-9ce7-bcf65b5d16aa"
    # A partial/legacy transcript exists globally but cannot establish which
    # cwd can resume it. It must be just as safe as the entirely absent one.
    unknown_project = config_dir / "projects" / "-unknown"
    unknown_project.mkdir(parents=True)
    (unknown_project / f"{unknown_dashed}.jsonl").write_text(
        json.dumps({"type": "queue-operation"}) + "\n",
        encoding="utf-8",
    )
    # A genuinely resumable session for this same worktree: its transcript
    # records this worktree's own cwd, so `--resume` can reach it.
    project = config_dir / "projects" / "-live"
    project.mkdir(parents=True)
    (project / f"{live_dashed}.jsonl").write_text(
        json.dumps({"type": "user", "cwd": str(tmp_path.resolve()), "message": {}})
        + "\n",
        encoding="utf-8",
    )

    bound_thread = [dead_thread]
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: SimpleNamespace(
            running=False,
            thread_id=bound_thread[0],
            log_path=None,
            process_status="idle",
        ),
    )

    stale = lifecycle.ensure_agent(tmp_path, dry_run=True)
    bound_thread[0] = unknown_thread
    unknown = lifecycle.ensure_agent(tmp_path, dry_run=True)
    bound_thread[0] = live_thread
    resumable = lifecycle.ensure_agent(tmp_path, dry_run=True)

    # Same worktree, three bound threads: missing and cwd-unknown conversations
    # self-heal to fresh starts; the proven-local one still resumes.
    assert (stale.action, unknown.action, "--resume" in unknown.command) == (
        "would-start",
        "would-start",
        False,
    )
    assert resumable.action == "would-resume"
    assert resumable.command[resumable.command.index("--resume") + 1] == live_dashed
