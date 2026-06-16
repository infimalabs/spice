"""Agent lifecycle, wrapper routing, and supervisor contracts."""

import argparse
import io
import json
import os
import subprocess
import sys
import time
from threading import Thread
from types import SimpleNamespace

import pytest

from spice.agent import driver as agent_driver
from spice.agent import lifecycle, sidechannel, wrap
from spice.agent.driver import (
    DRIVER,
    PLAYWRIGHT_MCP_ARGS,
    PLAYWRIGHT_MCP_COMMAND,
    PLAYWRIGHT_MCP_SERVER_NAME,
    playwright_mcp_args,
    write_playwright_mcp_config,
)
from spice.agent.gitshadow import (
    append_git_config_pairs,
    scrub_agent_git_shadow_environment,
)
from spice.errors import SpiceError
from spice.mail.inbox import compose_inbox_text, write_inbox_item
from spice.sessions.meter import ActiveContextSnapshot, ContextMeter

DIRECT_AGENT_PID = 2222
SUPERVISOR_PID = 3333
SUPERVISED_AGENT_PID = 4444
SHELL_TRACE_ENV = "SPICE_TEST_TRACE"  # env-policy: allow
SHELL_HOOK_FAILURE_EXIT_CODE = 127


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


def test_ensure_agent_dry_run_covers_start_resume_and_renew(tmp_path, monkeypatch):
    skill_path = (tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH).resolve()
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
    assert started.prompt == f"[$spice]({skill_path})"
    assert started.command[0] == "codex-test"
    assert 'model_reasoning_effort="high"' in _config_values(started.command)
    assert 'personality="friendly"' in _config_values(started.command)
    assert 'service_tier="fast"' in _config_values(started.command)
    assert resumed.action == "would-resume"
    assert resumed.command[-3:] == ["resume", "resume-thread", resumed.prompt]
    assert renewed.action == "would-renew"
    assert renewed.command[-1] == renewed.prompt


def test_start_agent_direct_path_writes_started_state_under_fakes(
    tmp_path, monkeypatch
):
    log_path = tmp_path / "agent.log"
    process = _FakeProcess(pid=DIRECT_AGENT_PID, returncode=None)
    spawned: list[tuple[list[str], object, object]] = []
    reaped: list[int] = []
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
        lambda _log_path, *, repo_root, fallback_thread_id: "started-thread",
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

    assert returned == log_path
    assert spawned == [(["codex", "exec", "prompt"], tmp_path, log_path)]
    assert state["pid"] == DIRECT_AGENT_PID
    assert state["mode"] == "start"
    assert state["model"] == "gpt-test"
    assert state["reasoning_effort"] == "medium"
    assert state["thread_id"] == "started-thread"
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
    log_path = tmp_path / "logs" / "supervised.log"
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
        lambda _log_path, *, repo_root, fallback_thread_id: "supervised-thread",
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
    assert state["thread_id"] == "supervised-thread"
    assert state["prompt_skill_path"] == str(skill_path)
    assert state["fast_mode"] is True
    assert thread.joined_timeouts == [1.0]


def test_require_started_process_distinguishes_credit_failure(tmp_path, monkeypatch):
    log_path = tmp_path / "agent.log"
    log_path.write_text("Error: Claude AI usage limit reached\n", encoding="utf-8")
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=1)

    class FakeDriver:
        def process_failure_kind(self, *, exit_code: int, output: str) -> str:
            assert exit_code == 1
            assert "usage limit reached" in output
            return lifecycle.AGENT_FAILURE_OUT_OF_CREDITS

    monkeypatch.setattr(lifecycle, "STARTUP_GRACE_SECONDS", 0)
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: FakeDriver())

    with pytest.raises(lifecycle.AgentOutOfCreditsError, match="usage limit reached"):
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


def test_wrapper_proxy_marker_plain_exec_starts_side_channel_watch(
    tmp_path, monkeypatch
):
    monkeypatch.setenv("ZDOTDIR", "hook")
    monkeypatch.setenv("BASH_ENV", "hook")
    events: list[tuple[str, object, object | None]] = []
    stderr = io.StringIO()
    watch_thread = object()

    class FakeProcess:
        pid = 123

        def wait(self) -> int:
            events.append(("wait", None, None))
            return 7

    def fake_popen(command: list[str], env=None) -> FakeProcess:
        events.append(
            (
                "popen",
                command,
                None if env is None else (env.get("ZDOTDIR"), env.get("BASH_ENV")),
            )
        )
        return FakeProcess()

    def fake_watch(repo_root, *, parent_pid, stderr, initial_payload_already_rendered):
        events.append(
            (
                "watch",
                repo_root,
                (parent_pid, stderr, initial_payload_already_rendered),
            )
        )
        return watch_thread

    def fake_join(thread):
        events.append(("join", thread, None))

    monkeypatch.setattr(wrap, "start_agent_side_channel_watch", fake_watch)
    monkeypatch.setattr(wrap, "join_agent_side_channel_watch", fake_join)

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["proxy", "find", ".", "-maxdepth", "0", "-print"],
        popen_factory=fake_popen,
        stderr=stderr,
    )

    assert exit_code == 7
    assert events == [
        ("popen", ["find", ".", "-maxdepth", "0", "-print"], None),
        ("watch", tmp_path, (123, stderr, True)),
        ("wait", None, None),
        ("join", watch_thread, None),
    ]


def test_wrapper_drops_proxy_marker_and_leaves_plain_commands_native():
    assert wrap.build_agent_run_command(["find", ".", "-maxdepth", "0", "-print"]) == [
        "find",
        ".",
        "-maxdepth",
        "0",
        "-print",
    ]
    assert wrap.build_agent_run_command(
        ["find", ".", "(", "-name", "*.py", "-o", "-name", "*.md", ")", "-print"]
    ) == [
        "find",
        ".",
        "(",
        "-name",
        "*.py",
        "-o",
        "-name",
        "*.md",
        ")",
        "-print",
    ]
    assert wrap.build_agent_run_command(["find", ".", "-name", "*.py"]) == [
        "find",
        ".",
        "-name",
        "*.py",
    ]
    assert wrap.build_agent_run_command(
        ["proxy", "find", ".", "-maxdepth", "0", "-print"]
    ) == ["find", ".", "-maxdepth", "0", "-print"]
    assert wrap.build_agent_run_command(["rg", "needle"]) == ["rg", "needle"]


def test_wrapper_runs_plain_find_natively(tmp_path, monkeypatch):
    monkeypatch.setenv("ZDOTDIR", "hook")
    monkeypatch.setenv("BASH_ENV", "hook")
    events: list[tuple[str, object, object | None]] = []
    stderr = io.StringIO()
    watch_thread = object()

    class FakeProcess:
        pid = 321

        def wait(self) -> int:
            events.append(("wait", None, None))
            return 0

    def fake_popen(command: list[str], env=None) -> FakeProcess:
        events.append(
            (
                "popen",
                command,
                None if env is None else (env.get("ZDOTDIR"), env.get("BASH_ENV")),
            )
        )
        return FakeProcess()

    def fake_watch(repo_root, *, parent_pid, stderr, initial_payload_already_rendered):
        events.append(
            (
                "watch",
                repo_root,
                (parent_pid, stderr, initial_payload_already_rendered),
            )
        )
        return watch_thread

    def fake_join(thread):
        events.append(("join", thread, None))

    monkeypatch.setattr(wrap, "start_agent_side_channel_watch", fake_watch)
    monkeypatch.setattr(wrap, "join_agent_side_channel_watch", fake_join)

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["find", ".", "-name", "*.py"],
        popen_factory=fake_popen,
        stderr=stderr,
    )

    assert exit_code == 0
    assert events == [
        ("popen", ["find", ".", "-name", "*.py"], None),
        ("watch", tmp_path, (321, stderr, True)),
        ("wait", None, None),
        ("join", watch_thread, None),
    ]


def test_gitshadow_scrub_preserves_non_shadow_pairs():
    env = append_git_config_pairs(
        {
            "GIT_CONFIG_COUNT": "1",
            "GIT_CONFIG_KEY_0": "user.name",
            "GIT_CONFIG_VALUE_0": "Operator",
        },
        (("includeIf.gitdir:/repo/.git.path", "/repo/.git/agent.gitconfig"),),
    )

    scrubbed = scrub_agent_git_shadow_environment(env)

    assert scrubbed == {
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": "user.name",
        "GIT_CONFIG_VALUE_0": "Operator",
    }


def test_inbox_injector_repeats_pending_steering_after_interval(tmp_path):
    write_inbox_item(
        tmp_path,
        "20260101T000000000001Z.txt",
        compose_inbox_text(body="operator steering", priority=None, stop=False),
    )
    now = [0.0]
    stderr = io.StringIO()
    injector = wrap.AgentInboxInjector(
        tmp_path,
        stderr=stderr,
        repeat_interval_seconds=15.0,
        time_factory=lambda: now[0],
    )

    injector.inject(force=False)
    now[0] = 10.0
    injector.inject(force=False)
    now[0] = 16.0
    injector.inject(force=False)

    output = stderr.getvalue()
    # Full readout at t=0 and again after the 15s repeat interval (t=16); the
    # suppressed inject at t=10 surfaces only a one-line pending count so the
    # command never looks empty while steering waits.
    assert output.count("operator steering") == 2
    assert output.count("recently shown") == 1
    assert "Task offload: decide now whether this steering needs a task" in output


def test_inbox_injector_suppresses_task_offload_for_maxim_guidance(tmp_path):
    write_inbox_item(
        tmp_path,
        "20260101T000000000002Z.txt",
        compose_inbox_text(
            body="No separate task is needed for the maxim itself.",
            priority="maxim",
            stop=False,
        ),
    )
    stderr = io.StringIO()
    injector = wrap.AgentInboxInjector(
        tmp_path,
        stderr=stderr,
        repeat_interval_seconds=15.0,
        time_factory=lambda: 0.0,
    )

    injector.inject(force=False)

    output = stderr.getvalue()
    assert "priority=maxim" in output
    assert "No separate task is needed for the maxim itself." in output
    assert "Task offload: decide now whether this steering needs a task" not in output


def test_context_meter_injector_repeats_warning_after_interval(tmp_path):
    now = [100.0]
    stderr = io.StringIO()
    meter = _context_meter(total_tokens=80_000, window=100_000)
    injector = wrap.AgentContextMeterInjector(
        tmp_path,
        stderr=stderr,
        repeat_interval_seconds=15.0,
        time_factory=lambda: now[0],
        meter_factory=lambda _repo: meter,
    )

    injector.inject(force=False)
    now[0] = 110.0
    injector.inject(force=False)
    now[0] = 116.0
    injector.inject(force=False)

    output = stderr.getvalue()
    assert output.count("Context Pressure") == 2
    assert "level=yellow" in output


def test_side_channel_watch_streams_later_inbox_to_stderr(tmp_path, monkeypatch):
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)

    with sidechannel.AgentSideChannelServer(tmp_path):
        thread = Thread(
            target=wrap.watch_agent_side_channel,
            kwargs={
                "repo_root": tmp_path,
                "parent_pid": os.getpid(),
                "stderr": stderr,
            },
        )
        thread.start()
        write_inbox_item(
            tmp_path,
            "20260101T000000000003Z.txt",
            compose_inbox_text(body="late steering", priority=None, stop=False),
        )
        output = _eventually(lambda: stderr.getvalue(), contains="late steering")

    thread.join(timeout=1.0)
    assert "Inbox Steering" in output
    # The late item's full readout streams exactly once; any later suppressed
    # inject surfaces only the one-line pending count, not a second full readout.
    assert output.count("late steering") == 1
    assert not thread.is_alive()


def test_side_channel_streams_to_each_connection_without_cross_suppression(
    tmp_path, monkeypatch
):
    # Two overlapping connections share the same 15s window; with per-connection
    # injectors each still gets the full readout (a shared injector would
    # suppress the second). Short commands must never be cross-suppressed.
    monkeypatch.chdir(tmp_path)
    write_inbox_item(
        tmp_path,
        "20260101T000000000004Z.txt",
        compose_inbox_text(body="multi connection steering", priority=None, stop=False),
    )
    stderr_a = io.StringIO()
    stderr_b = io.StringIO()

    with sidechannel.AgentSideChannelServer(tmp_path):
        threads = [
            Thread(
                target=wrap.watch_agent_side_channel,
                kwargs={
                    "repo_root": tmp_path,
                    "parent_pid": os.getpid(),
                    "stderr": buf,
                },
            )
            for buf in (stderr_a, stderr_b)
        ]
        for thread in threads:
            thread.start()
        out_a = _eventually(
            lambda: stderr_a.getvalue(), contains="multi connection steering"
        )
        out_b = _eventually(
            lambda: stderr_b.getvalue(), contains="multi connection steering"
        )

    for thread in threads:
        thread.join(timeout=1.0)
    assert "multi connection steering" in out_a
    assert "multi connection steering" in out_b


def test_run_agent_command_streams_later_side_channel_while_child_runs(
    tmp_path, monkeypatch
):
    monkeypatch.chdir(tmp_path)
    stderr = io.StringIO()
    ready = tmp_path / "ready"
    results: list[int] = []
    script = (
        "from pathlib import Path; "
        "import sys, time; "
        "Path(sys.argv[1]).write_text('ready', encoding='utf-8'); "
        "time.sleep(0.4)"
    )

    with sidechannel.AgentSideChannelServer(tmp_path):
        thread = Thread(
            target=lambda: results.append(
                wrap.run_agent_command(
                    tmp_path,
                    [sys.executable, "-c", script, str(ready)],
                    stderr=stderr,
                )
            )
        )
        thread.start()
        _eventually(lambda: "ready" if ready.exists() else "", contains="ready")
        write_inbox_item(
            tmp_path,
            "20260101T000000000004Z.txt",
            compose_inbox_text(body="runner steering", priority=None, stop=False),
        )
        output = _eventually(lambda: stderr.getvalue(), contains="runner steering")
        thread.join(timeout=2.0)

    assert results == [0]
    assert "Inbox Steering" in output
    assert output.count("Inbox Steering") == 1


def test_run_agent_command_does_not_duplicate_initial_side_channel_with_watch(
    tmp_path, monkeypatch
):
    monkeypatch.chdir(tmp_path)
    write_inbox_item(
        tmp_path,
        "20260101T000000000005Z.txt",
        compose_inbox_text(body="initial steering", priority=None, stop=False),
    )
    stderr = io.StringIO()

    with sidechannel.AgentSideChannelServer(tmp_path):
        exit_code = wrap.run_agent_command(
            tmp_path,
            [sys.executable, "-c", "import time; time.sleep(0.1)"],
            stderr=stderr,
        )

    output = stderr.getvalue()
    assert exit_code == 0
    assert "initial steering" in output
    assert output.count("Inbox Steering") == 1


def test_run_agent_command_dumps_initial_inbox_without_side_channel_server(
    tmp_path, monkeypatch
):
    monkeypatch.chdir(tmp_path)
    write_inbox_item(
        tmp_path,
        "20260101T000000000006Z.txt",
        compose_inbox_text(body="synchronous steering", priority=None, stop=False),
    )
    stderr = io.StringIO()

    exit_code = wrap.run_agent_command(
        tmp_path,
        [sys.executable, "-c", ""],
        stderr=stderr,
    )

    output = stderr.getvalue()
    assert exit_code == 0
    assert "synchronous steering" in output
    assert output.count("Inbox Steering") == 1


def test_side_channel_watch_exits_when_parent_pid_exits_without_shell_trap(
    tmp_path, monkeypatch
):
    watcher = wrap._parent_exit_watcher(os.getpid())
    if watcher is None:
        pytest.skip("platform does not expose process-exit watch handles")
    watcher.close()
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)
    parent = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(0.1)"])

    with sidechannel.AgentSideChannelServer(tmp_path):
        thread = Thread(
            target=wrap.watch_agent_side_channel,
            kwargs={
                "repo_root": tmp_path,
                "parent_pid": parent.pid,
                "stderr": stderr,
            },
        )
        thread.start()
        parent.wait(timeout=2.0)
        thread.join(timeout=2.0)
        alive = thread.is_alive()

    assert not alive


def _config_values(command: list[str]) -> list[str]:
    return [
        command[index + 1] for index, part in enumerate(command) if part == "--config"
    ]


def _eventually(factory, *, contains: str):
    deadline = time.monotonic() + 2.0
    latest = factory()
    while time.monotonic() < deadline:
        if _contains(latest, contains):
            return latest
        time.sleep(0.05)
        latest = factory()
    return latest


def _contains(value, needle: str) -> bool:
    if isinstance(value, str):
        return needle in value
    return any(needle in item for item in value)


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


def _context_meter(*, total_tokens: int, window: int) -> ContextMeter:
    snapshot = ActiveContextSnapshot(
        source_file="rollout.jsonl",
        ts="2026-01-01T00:00:00.000Z",
        input_tokens=total_tokens,
        cached_input_tokens=0,
        output_tokens=0,
        reasoning_output_tokens=0,
        total_tokens=total_tokens,
        model_context_window=window,
        cumulative_total_tokens=total_tokens,
    )
    return ContextMeter(
        source_files=("rollout.jsonl",),
        latest_snapshot=snapshot,
        snapshot_count=1,
        compaction_count=0,
        latest_compaction_ts=None,
        snapshots_since_compaction=1,
        pre_compaction_min_tokens=None,
        pre_compaction_median_tokens=None,
        pre_compaction_max_tokens=None,
        pre_compaction_min_percent=None,
        pre_compaction_median_percent=None,
        pre_compaction_max_percent=None,
    )
