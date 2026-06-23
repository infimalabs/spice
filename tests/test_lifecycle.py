"""Agent lifecycle, wrapper routing, and supervisor contracts."""

import argparse
import json
import os
import subprocess
from types import SimpleNamespace

import pytest

from spice.agent import driver as agent_driver
from spice.agent import lifecycle, renewal, sidechannel, sidechannelnotify, wrap
from spice.agent.driver import (
    CLAUDE_DRIVER,
    CODEX_DRIVER,
    DRIVER,
    PLAYWRIGHT_MCP_ARGS,
    PLAYWRIGHT_MCP_COMMAND,
    PLAYWRIGHT_MCP_SERVER_NAME,
    operator_color_scheme,
    playwright_mcp_args,
    write_playwright_mcp_config,
)
from spice.errors import SpiceError

DIRECT_AGENT_PID = 2222
SUPERVISOR_PID = 3333
SUPERVISED_AGENT_PID = 4444
SHELL_TRACE_ENV = "SPICE_TEST_TRACE"  # env-policy: allow
SHELL_HOOK_FAILURE_EXIT_CODE = 127


@pytest.fixture(autouse=True)
def _git_worktree_tmp_path(request, tmp_path):
    if "tmp_path" in request.fixturenames:
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)


def test_shipped_agent_defaults_are_current_high_effort():
    assert CODEX_DRIVER.default_model == "gpt-5.5"
    assert CODEX_DRIVER.default_reasoning_effort == "xhigh"
    assert CODEX_DRIVER.default_service_tier == "fast"
    assert CLAUDE_DRIVER.default_reasoning_effort == "high"


def test_codex_driver_command_pins_fast_service_tier_and_playwright_mcp(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(agent_driver, "operator_color_scheme", lambda: "dark")
    prompt = "[$spice](/tmp/skill.md)"
    command = DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt=prompt,
        thread_id="thread-1",
        model="gpt-test",
        reasoning_effort="xhigh",
        personality="pragmatic",
        service_tier="fast",
        binary="codex-test",
        fast_mode=True,
    )
    configs = _config_values(command)

    assert command[:5] == ["codex-test", "exec", "--cd", str(tmp_path), "--model"]
    assert command[5] == "gpt-test"
    assert 'model_reasoning_effort="xhigh"' in configs
    assert (
        f'mcp_servers.{PLAYWRIGHT_MCP_SERVER_NAME}.command="{PLAYWRIGHT_MCP_COMMAND}"'
    ) in configs
    assert (
        f"mcp_servers.{PLAYWRIGHT_MCP_SERVER_NAME}.args="
        f'["--yes","@playwright/mcp@latest","--headless","--config",'
        f'"{tmp_path / ".spice" / "agent" / "playwright-mcp.json"}"]'
    ) in configs
    assert list(PLAYWRIGHT_MCP_ARGS) == [
        "--yes",
        "@playwright/mcp@latest",
        "--headless",
    ]
    assert json.loads(
        (tmp_path / ".spice" / "agent" / "playwright-mcp.json").read_text(
            encoding="utf-8"
        )
    ) == {"browser": {"contextOptions": {"colorScheme": "dark"}}}
    assert 'personality="pragmatic"' in configs
    assert 'service_tier="fast"' in configs
    assert command[command.index("--enable") + 1] == "fast_mode"
    assert command[-3:] == ["resume", "thread-1", prompt]


def test_ensure_agent_uses_shipped_codex_defaults_without_config(tmp_path, monkeypatch):
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: _status(),
    )

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.command[result.command.index("--model") + 1] == "gpt-5.5"
    assert 'model_reasoning_effort="xhigh"' in _config_values(result.command)


def test_playwright_mcp_args_write_light_scheme_config(tmp_path, monkeypatch):
    monkeypatch.setattr(agent_driver, "operator_color_scheme", lambda: "light")

    config_path = write_playwright_mcp_config(tmp_path)

    assert config_path == tmp_path / ".spice" / "agent" / "playwright-mcp.json"
    assert json.loads(config_path.read_text(encoding="utf-8")) == {
        "browser": {"contextOptions": {"colorScheme": "light"}}
    }
    assert playwright_mcp_args(tmp_path) == [
        "--yes",
        "@playwright/mcp@latest",
        "--headless",
        "--config",
        str(config_path),
    ]


def test_operator_color_scheme_defaults_to_explicit_light_off_macos(monkeypatch):
    monkeypatch.setattr(agent_driver.sys, "platform", "linux")

    assert operator_color_scheme() == "light"


def test_operator_color_scheme_defaults_to_explicit_light_when_unreadable(monkeypatch):
    monkeypatch.setattr(agent_driver.sys, "platform", "darwin")

    def raise_os_error(*_args, **_kwargs):
        raise OSError("defaults unavailable")

    monkeypatch.setattr(agent_driver.subprocess, "run", raise_os_error)

    assert operator_color_scheme() == "light"


def test_ensure_agent_dry_run_covers_start_resume_and_renew(tmp_path, monkeypatch):
    status_thread = [""]
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: _status(thread_id=status_thread[0]),
    )

    started = lifecycle.ensure_agent(
        tmp_path,
        dry_run=True,
        model="gpt-direct",
        reasoning_effort="high",
        personality="friendly",
        agent_bin="codex-test",
        fast_mode=True,
    )
    status_thread[0] = "resume-thread"
    resumed = lifecycle.ensure_agent(
        tmp_path,
        dry_run=True,
    )
    renewed = lifecycle.ensure_agent(
        tmp_path,
        dry_run=True,
        force_new=True,
    )

    assert started.action == "would-start"
    assert started.prompt == "[$spice](.agents/skills/spice/SKILL.md)"
    assert str(tmp_path) not in started.prompt
    assert started.command[0] == "codex-test"
    assert 'model_reasoning_effort="high"' in _config_values(started.command)
    assert 'personality="friendly"' in _config_values(started.command)
    assert 'service_tier="fast"' in _config_values(started.command)
    assert resumed.action == "would-resume"
    assert resumed.command[-3:] == ["resume", "resume-thread", resumed.prompt]
    assert renewed.action == "would-renew"
    assert renewed.command[-1] == renewed.prompt


def test_ensure_agent_dry_run_uses_relative_skill_prompt_for_claude(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: _status(),
    )
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.prompt == "[$spice](.agents/skills/spice/SKILL.md)"
    assert str(tmp_path) not in result.prompt
    assert result.command[-1] == result.prompt


def test_agent_state_uses_gitdirs_and_actual_thread_ids_for_linked_worktrees(
    tmp_path, monkeypatch
):
    repo = tmp_path / "repo"
    linked = tmp_path / "linked"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    subprocess.run(
        ["git", "config", "user.email", "spice@example.test"],
        cwd=repo,
        check=True,
    )
    subprocess.run(["git", "config", "user.name", "Spice Tests"], cwd=repo, check=True)
    (repo / "README.md").write_text("initial\n", encoding="utf-8")
    subprocess.run(["git", "add", "README.md"], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "initial"], cwd=repo, check=True)
    subprocess.run(
        ["git", "worktree", "add", "-q", "--detach", str(linked), "HEAD"],
        cwd=repo,
        check=True,
    )
    common_agent_root = (
        repo / ".git" / "spice" / "agents" / DRIVER.state_dirname
    ).resolve()
    primary_worktree_dir = (
        repo / ".git" / "spice" / "agents" / DRIVER.state_dirname
    ).resolve()
    linked_git_dir = repo / ".git" / "worktrees" / linked.name / "spice" / "agents"
    linked_worktree_dir = (linked_git_dir / DRIVER.state_dirname).resolve()
    thread_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    linked_thread_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    primary_thread_dir = common_agent_root / thread_id
    linked_thread_dir = linked_worktree_dir / linked_thread_id

    assert lifecycle.agent_state_path(repo).parent == primary_worktree_dir
    assert sidechannelnotify.side_channel_marker_path(repo) == (
        primary_worktree_dir / "stderr.sock"
    )
    assert sidechannelnotify.side_channel_marker_path(linked) == (
        linked_worktree_dir / "stderr.sock"
    )
    with lifecycle.agent_ensure_lock(repo):
        assert (primary_worktree_dir / "ensure.lock").exists()

    log_path = primary_worktree_dir / "startup.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.write_text("starting\n", encoding="utf-8")
    final_log = lifecycle.settle_agent_log_path(repo, log_path, thread_id)
    lifecycle.write_agent_state(
        repo,
        {
            "mode": "start",
            "started_at": "2026-01-02T03:04:05Z",
            "prompt_skill_path": str(repo / lifecycle.WORKTREE_SKILL_RELATIVE_PATH),
            "thread_id": thread_id,
            "log_path": str(final_log),
        },
    )

    assert not log_path.exists()
    assert final_log == primary_thread_dir / "logs" / "startup.log"
    assert final_log.read_text(encoding="utf-8") == "starting\n"
    assert lifecycle.agent_state_path(repo) == primary_thread_dir / "state.json"
    assert (primary_worktree_dir / "thread-id").read_text(encoding="utf-8") == (
        f"{thread_id}\n"
    )
    assert wrap.context_meter_cache_path(repo) == (
        primary_thread_dir / "context-meter.json"
    )
    assert wrap.context_warning_state_path(repo) == (
        primary_thread_dir / "context-warning.json"
    )
    assert renewal.renewal_request_path(repo) == primary_thread_dir / "renew.json"
    assert repo / ".spice" not in primary_thread_dir.parents
    assert linked / ".spice" not in linked_worktree_dir.parents

    monkeypatch.setattr(lifecycle, "utc_now", lambda: "2026-01-02T03:04:05Z")
    linked_log = lifecycle.next_agent_log_path(linked)
    assert linked_log == linked_worktree_dir / "20260102T030405Z.log"
    linked_log.parent.mkdir(parents=True, exist_ok=True)
    linked_log.write_text("linked\n", encoding="utf-8")
    final_linked_log = lifecycle.settle_agent_log_path(
        linked, linked_log, linked_thread_id
    )
    lifecycle.write_agent_state(
        linked,
        {
            "mode": "start",
            "started_at": "2026-01-02T03:04:05Z",
            "prompt_skill_path": str(linked / lifecycle.WORKTREE_SKILL_RELATIVE_PATH),
            "thread_id": linked_thread_id,
            "log_path": str(final_linked_log),
        },
    )
    assert final_linked_log == linked_thread_dir / "logs" / "20260102T030405Z.log"
    assert lifecycle.agent_state_path(linked) == linked_thread_dir / "state.json"
    assert (linked_worktree_dir / "thread-id").read_text(encoding="utf-8") == (
        f"{linked_thread_id}\n"
    )
    assert not (common_agent_root / linked_thread_id).exists()


def test_start_agent_direct_path_writes_started_state_under_fakes(
    tmp_path, monkeypatch
):
    log_path = tmp_path / "agent.log"
    process = _FakeProcess(pid=DIRECT_AGENT_PID, returncode=None)
    spawned: list[tuple[list[str], object, object]] = []
    reaped: list[int] = []
    thread_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    monkeypatch.setattr(lifecycle, "next_agent_log_path", lambda _repo: log_path)
    monkeypatch.setattr(
        lifecycle,
        "spawn_agent",
        lambda command, *, cwd, log_path: (
            spawned.append((command, cwd, log_path)) or process
        ),
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
        lifecycle, "reap_process_when_done", lambda proc: reaped.append(proc.pid)
    )

    returned = lifecycle.start_agent(
        tmp_path,
        action="start",
        command=["codex", "exec", "prompt"],
        model="gpt-test",
        reasoning_effort="medium",
        service_tier="",
        resume_thread_id="",
        prompt_skill_path=tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH,
        fast_mode=False,
        supervise_stdout=False,
    )
    state = lifecycle.read_agent_state(tmp_path)
    final_log_path = (
        tmp_path
        / ".git"
        / "spice"
        / "agents"
        / DRIVER.state_dirname
        / thread_id
        / "logs"
        / log_path.name
    ).resolve()

    assert returned == final_log_path
    assert spawned == [(["codex", "exec", "prompt"], tmp_path, log_path)]
    assert state["pid"] == DIRECT_AGENT_PID
    assert state["mode"] == "start"
    assert state["model"] == "gpt-test"
    assert state["reasoning_effort"] == "medium"
    assert state["thread_id"] == thread_id
    assert state["log_path"] == str(final_log_path)
    assert reaped == [DIRECT_AGENT_PID]


def test_start_agent_supervised_path_uses_supervisor_and_reaper(tmp_path, monkeypatch):
    log_path = tmp_path / "supervised.log"
    process = _FakeProcess(pid=SUPERVISOR_PID, returncode=None)
    spawned: list[dict[str, object]] = []
    required: list[tuple[int, object, object]] = []
    reaped: list[int] = []
    monkeypatch.setattr(lifecycle, "next_agent_log_path", lambda _repo: log_path)

    def spawn_supervisor(repo_root, **kwargs):
        spawned.append({"repo_root": repo_root, **kwargs})
        return process

    monkeypatch.setattr(lifecycle, "spawn_agent_supervisor", spawn_supervisor)
    monkeypatch.setattr(
        lifecycle,
        "require_supervisor_started",
        lambda proc, *, repo_root, log_path: required.append(
            (proc.pid, repo_root, log_path)
        ),
    )
    monkeypatch.setattr(
        lifecycle, "reap_process_when_done", lambda proc: reaped.append(proc.pid)
    )

    returned = lifecycle.start_agent(
        tmp_path,
        action="resume",
        command=["codex", "exec", "resume", "thread", "prompt"],
        model="gpt-test",
        reasoning_effort="high",
        service_tier="fast",
        resume_thread_id="thread",
        prompt_skill_path=tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH,
        fast_mode=True,
        supervise_stdout=True,
    )

    assert returned == log_path
    assert spawned[0]["repo_root"] == tmp_path
    assert spawned[0]["action"] == "resume"
    assert spawned[0]["service_tier"] == "fast"
    assert spawned[0]["fast_mode"] is True
    assert "prompt_skill_path" not in spawned[0]
    assert required == [(SUPERVISOR_PID, tmp_path, log_path)]
    assert reaped == [SUPERVISOR_PID]


def test_spawn_agent_supervisor_omits_prompt_skill_path_arg(tmp_path, monkeypatch):
    log_path = tmp_path / "supervised.log"
    spawned: list[dict[str, object]] = []

    class FakePopen(_FakeProcess):
        def __init__(self, command, **kwargs) -> None:
            super().__init__(pid=SUPERVISOR_PID, returncode=None)
            spawned.append({"command": command, **kwargs})

        @classmethod
        def __class_getitem__(cls, _item):
            return cls

    monkeypatch.setattr(lifecycle.subprocess, "Popen", FakePopen)
    monkeypatch.setattr(
        lifecycle, "agent_supervisor_environment", lambda repo_root: {"ENV": "1"}
    )

    returned = lifecycle.spawn_agent_supervisor(
        tmp_path,
        action="start",
        command=["codex", "exec", "prompt"],
        model="gpt-test",
        reasoning_effort="medium",
        service_tier="",
        resume_thread_id="",
        log_path=log_path,
        fast_mode=False,
    )

    command = spawned[0]["command"]
    assert isinstance(command, list)
    assert returned.pid == SUPERVISOR_PID
    assert command[command.index("--repo-root") + 1] == str(tmp_path)
    assert "--prompt-skill-path" not in command
    assert command[command.index("--command-json") + 1] == '["codex","exec","prompt"]'


def test_run_agent_supervisor_writes_state_under_fakes(tmp_path, monkeypatch):
    log_path = tmp_path / "supervisor.log"
    skill_path = (tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH).resolve()
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=5)
    thread = _FakeThread()
    side_events: list[tuple[str, object]] = []
    spawned: list[dict[str, object]] = []
    thread_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    monkeypatch.setattr(lifecycle, "agent_environment", lambda repo_root: {"ENV": "1"})
    monkeypatch.setattr(
        lifecycle,
        "spawn_supervised_agent",
        lambda command, *, cwd, log_path, env: (
            spawned.append(
                {"command": command, "cwd": cwd, "log_path": log_path, "env": env}
            )
            or (process, thread)
        ),
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
        lambda repo_root: _FakeSideChannel(repo_root, side_events),
    )
    args = argparse.Namespace(
        repo_root=str(tmp_path),
        action="resume",
        model="gpt-test",
        reasoning_effort="high",
        service_tier="fast",
        resume_thread_id="resume-thread",
        log_path=str(log_path),
        fast_mode=True,
        command_json='["codex","exec","prompt"]',
    )

    exit_code = lifecycle.run_agent_supervisor(args)
    state = lifecycle.read_agent_state(tmp_path)
    final_log_path = (
        tmp_path
        / ".git"
        / "spice"
        / "agents"
        / DRIVER.state_dirname
        / thread_id
        / "logs"
        / log_path.name
    ).resolve()

    assert exit_code == 5
    assert side_events == [("enter", tmp_path), ("exit", tmp_path)]
    assert spawned == [
        {
            "command": ["codex", "exec", "prompt"],
            "cwd": tmp_path,
            "log_path": log_path,
            "env": {"ENV": "1"},
        }
    ]
    assert state["pid"] == SUPERVISED_AGENT_PID
    assert state["supervisor_pid"] == os.getpid()
    assert state["thread_id"] == thread_id
    assert state["log_path"] == str(final_log_path)
    assert state["prompt_skill_path"] == str(skill_path)
    assert state["fast_mode"] is True
    assert thread.joined_timeouts == [1.0]


def test_require_supervisor_started_accepts_thread_settled_log_path(
    tmp_path, monkeypatch
):
    thread_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    log_path = lifecycle.next_agent_log_path(tmp_path)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.write_text("starting\n", encoding="utf-8")
    final_log_path = lifecycle.settle_agent_log_path(tmp_path, log_path, thread_id)
    lifecycle.write_agent_state(
        tmp_path,
        {
            "pid": SUPERVISED_AGENT_PID,
            "thread_id": thread_id,
            "log_path": str(final_log_path),
            "mode": "start",
            "started_at": "2026-01-02T03:04:05Z",
            "prompt_skill_path": str(tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH),
        },
    )
    monkeypatch.setattr(lifecycle, "SUPERVISOR_STARTUP_TIMEOUT_SECONDS", 0)
    monkeypatch.setattr(
        lifecycle,
        "process_id_is_running",
        lambda pid: pid == SUPERVISED_AGENT_PID,
    )
    process = _FakeProcess(pid=SUPERVISOR_PID, returncode=None)

    lifecycle.require_supervisor_started(process, repo_root=tmp_path, log_path=log_path)


def test_require_started_process_distinguishes_codex_credit_failure(
    tmp_path, monkeypatch
):
    log_path = tmp_path / "agent.log"
    log_path.write_text(
        "ERROR: You've hit your usage limit. Visit "
        "https://chatgpt.com/codex/settings/usage to purchase more credits "
        "or try again at 4:36 PM.\n",
        encoding="utf-8",
    )
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=1)

    monkeypatch.setattr(lifecycle, "STARTUP_GRACE_SECONDS", 0)
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CODEX_DRIVER)

    with pytest.raises(lifecycle.AgentOutOfCreditsError, match="hit your usage limit"):
        lifecycle.require_started_process(process, log_path, repo_root=tmp_path)


def test_agent_environment_refuses_ambient_thread(tmp_path, monkeypatch):
    monkeypatch.setenv(DRIVER.thread_id_env, "ambient-thread")

    with pytest.raises(SpiceError, match="refusing to spawn an agent"):
        lifecycle.agent_environment(tmp_path)


def test_agent_binding_error_reports_stale_launch_cwd_and_ignores_prompt_skill(
    tmp_path,
):
    lane = tmp_path / "lane"
    other = tmp_path / "other"
    lane.mkdir()
    other.mkdir()
    lane_skill = lane / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    other_skill = other / lifecycle.WORKTREE_SKILL_RELATIVE_PATH

    cwd_error = lifecycle.agent_binding_error(
        lane,
        SimpleNamespace(
            repo_root=lane,
            command=["codex", "exec", "--cd", str(other)],
            prompt_skill_path=lane_skill,
        ),
    )
    skill_error = lifecycle.agent_binding_error(
        lane,
        SimpleNamespace(
            repo_root=lane,
            command=["codex", "exec", "--cd", str(lane)],
            prompt_skill_path=other_skill,
        ),
    )

    assert f"launch cwd {other.resolve()} != lane root {lane.resolve()}" in cwd_error
    assert skill_error == ""


def test_agent_binding_allows_launch_cwd_inside_worktree_for_side_channel(tmp_path):
    child = tmp_path / "subdir"
    child.mkdir()

    diagnostic = sidechannel.side_channel_binding_diagnostic(
        tmp_path,
        {
            "type": "hello",
            "repoRoot": str(tmp_path),
            "cwd": str(child),
        },
    )

    assert diagnostic == ""


def test_side_channel_binding_diagnostic_refuses_wrong_repo_root(tmp_path):
    other = tmp_path / "other"
    other.mkdir()

    diagnostic = sidechannel.side_channel_binding_diagnostic(
        tmp_path,
        {
            "type": "hello",
            "repoRoot": str(other),
            "cwd": str(other),
        },
    )

    assert "Agent Binding Mismatch" in diagnostic
    assert f"lane_repo_root={tmp_path.resolve()}" in diagnostic
    assert f"wrapper_repo_root={other.resolve()}" in diagnostic
    assert "steering_delivery=refused" in diagnostic


def _config_values(command: list[str]) -> list[str]:
    return [
        command[index + 1] for index, part in enumerate(command) if part == "--config"
    ]


def _status(*, thread_id: str = "", running: bool = False):
    return SimpleNamespace(
        running=running,
        thread_id=thread_id,
        log_path=None,
        process_status="running" if running else "idle",
    )


class _FakeProcess:
    def __init__(self, *, pid: int, returncode: int | None) -> None:
        self.pid = pid
        self.returncode = returncode
        self.wait_calls = 0

    def poll(self):
        return self.returncode

    def wait(self):
        self.wait_calls += 1
        return self.returncode


class _FakeThread:
    def __init__(self) -> None:
        self.joined_timeouts: list[float | None] = []

    def join(self, timeout: float | None = None) -> None:
        self.joined_timeouts.append(timeout)


class _FakeSideChannel:
    def __init__(self, repo_root, events) -> None:
        self.repo_root = repo_root
        self.events = events

    def __enter__(self):
        self.events.append(("enter", self.repo_root))
        return self

    def __exit__(self, *_exc):
        self.events.append(("exit", self.repo_root))
