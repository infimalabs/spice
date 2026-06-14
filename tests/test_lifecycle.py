"""Agent lifecycle, wrapper routing, and supervisor contracts."""

import argparse
import io
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

from spice.agent import cli as agent_cli
from spice.agent import driver as agent_driver
from spice.agent import lifecycle, shellhook, sidechannel, wrap
from spice.agent.driver import (
    CLAUDE_DRIVER,
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
        lifecycle, "require_started_process", lambda _process, _log_path: None
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
        lifecycle, "require_started_process", lambda _process, _log_path: None
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


def test_agent_environment_refuses_ambient_thread(tmp_path, monkeypatch):
    monkeypatch.setenv(DRIVER.thread_id_env, "ambient-thread")

    with pytest.raises(SpiceError, match="refusing to spawn an agent"):
        lifecycle.agent_environment(tmp_path)


def test_wrapper_gitshadow_route_uses_shadow_and_spice_route_scrubs(
    tmp_path, monkeypatch
):
    shadow_calls: list[object] = []
    scrub_calls: list[object] = []
    monkeypatch.setattr(
        wrap,
        "agent_git_shadow_environment",
        lambda repo_root, *, base_env=None: (
            shadow_calls.append(repo_root) or {"route": "git", "repo": str(repo_root)}
        ),
    )
    monkeypatch.setattr(
        wrap,
        "scrub_agent_git_shadow_environment",
        lambda env: scrub_calls.append(env) or {"route": "spice"},
    )

    git_env = wrap.build_agent_run_environment(["git", "status"], repo_root=tmp_path)
    proxy_git_env = wrap.build_agent_run_environment(
        ["proxy", "git", "status"], repo_root=tmp_path
    )
    spice_env = wrap.build_agent_run_environment(
        ["spice", "task", "status"], repo_root=tmp_path
    )
    uv_spice_env = wrap.build_agent_run_environment(
        ["uv", "run", "spice", "task", "status"], repo_root=tmp_path
    )

    assert git_env == {"route": "git", "repo": str(tmp_path)}
    assert proxy_git_env == {"route": "git", "repo": str(tmp_path)}
    assert spice_env == {"route": "spice"}
    assert uv_spice_env == {"route": "spice"}
    assert shadow_calls == [tmp_path, tmp_path]
    assert len(scrub_calls) == 2


def test_wrapper_routes_worktree_spice_commands_through_python_module(
    tmp_path, monkeypatch
):
    _write_spice_product_shape(tmp_path)
    monkeypatch.setattr(wrap, "proxy_bin", lambda: "missing-spice-proxy")

    spice_command = wrap.build_agent_run_command(
        ["spice", "task", "status"], repo_root=tmp_path
    )
    uv_spice_command = wrap.build_agent_run_command(
        ["uv", "run", "spice", "task", "status"], repo_root=tmp_path
    )

    assert spice_command == [sys.executable, "-m", "spice", "task", "status"]
    assert uv_spice_command == [sys.executable, "-m", "spice", "task", "status"]


def test_wrapper_routes_worktree_python_commands_through_active_interpreter(
    tmp_path, monkeypatch
):
    _write_spice_product_shape(tmp_path)
    monkeypatch.setattr(wrap, "proxy_bin", lambda: "missing-spice-proxy")

    python_command = wrap.build_agent_run_command(
        ["python", "-m", "pip", "--version"], repo_root=tmp_path
    )
    python3_command = wrap.build_agent_run_command(
        ["python3", "-m", "pip", "--version"], repo_root=tmp_path
    )
    proxy_python_command = wrap.build_agent_run_command(
        ["proxy", "python", "-m", "pip", "--version"], repo_root=tmp_path
    )

    assert python_command == [sys.executable, "-m", "pip", "--version"]
    assert python3_command == [sys.executable, "-m", "pip", "--version"]
    assert proxy_python_command == [sys.executable, "-m", "pip", "--version"]


def test_wrapper_proxy_receives_worktree_python_interpreter(tmp_path, monkeypatch):
    _write_spice_product_shape(tmp_path)
    monkeypatch.setenv(wrap.PROXY_BIN_ENV, "rtk-test")
    monkeypatch.setattr(
        wrap.shutil,
        "which",
        lambda proxy: "/opt/bin/rtk-test" if proxy == "rtk-test" else None,
    )

    assert wrap.build_agent_run_command(
        ["python", "-m", "pip", "--version"], repo_root=tmp_path
    ) == ["/opt/bin/rtk-test", sys.executable, "-m", "pip", "--version"]
    assert wrap.build_agent_run_command(
        ["proxy", "python", "-m", "pip", "--version"], repo_root=tmp_path
    ) == ["/opt/bin/rtk-test", "proxy", sys.executable, "-m", "pip", "--version"]


def test_wrapper_routes_target_repo_python_commands_through_virtualenv_default(
    tmp_path, monkeypatch
):
    venv_python = tmp_path / "active-env" / "bin" / "python"
    venv_python.parent.mkdir(parents=True)
    venv_python.write_text("#!/bin/sh\n", encoding="utf-8")
    venv_python.chmod(0o755)
    monkeypatch.setenv("VIRTUAL_ENV", str(tmp_path / "active-env"))
    monkeypatch.setattr(wrap, "proxy_bin", lambda: "missing-spice-proxy")

    assert wrap.build_agent_run_command(
        ["python", "--version"], repo_root=tmp_path
    ) == [
        str(venv_python),
        "--version",
    ]


def test_wrapper_routes_target_repo_python_commands_through_repo_venv(
    tmp_path, monkeypatch
):
    venv_python = tmp_path / ".venv" / "bin" / "python"
    venv_python.parent.mkdir(parents=True)
    venv_python.write_text("#!/bin/sh\n", encoding="utf-8")
    venv_python.chmod(0o755)
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)
    monkeypatch.setattr(wrap, "proxy_bin", lambda: "missing-spice-proxy")

    assert wrap.build_agent_run_command(
        ["python", "--version"], repo_root=tmp_path
    ) == [
        str(venv_python),
        "--version",
    ]


def test_wrapper_refuses_target_repo_python_without_repo_venv(tmp_path, monkeypatch):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)
    monkeypatch.setattr(wrap, "proxy_bin", lambda: "missing-spice-proxy")

    command = wrap.build_agent_run_command(["python", "--version"], repo_root=tmp_path)

    assert command[:2] == [sys.executable, "-c"]
    assert "refusing to run python from global PATH" in command[2]
    assert str(tmp_path / ".venv" / "bin" / "python") in command[2]
    assert command[3:] == ["--version"]


def test_wrapper_plain_commands_inherit_worktree_spice_pythonpath(tmp_path):
    _write_spice_product_shape(tmp_path)

    env = wrap.build_agent_run_environment(["pytest"], repo_root=tmp_path)

    assert env is not None
    assert env["PYTHONPATH"].split(os.pathsep)[0] == str(tmp_path.resolve())


def test_agent_environment_inherits_worktree_spice_pythonpath(tmp_path, monkeypatch):
    _write_spice_product_shape(tmp_path)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)

    env = lifecycle.agent_environment(tmp_path)

    assert env["PYTHONPATH"].split(os.pathsep)[0] == str(tmp_path.resolve())


def test_agent_environment_installs_shell_steering_hooks_for_default_driver(
    tmp_path, monkeypatch
):
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)

    env = lifecycle.agent_environment(tmp_path)

    hook_dir = tmp_path / ".spice" / "agents" / "codex" / "shell-hook"
    assert env[shellhook.ZDOTDIR_ENV] == str(hook_dir)
    assert env[shellhook.BASH_ENV_ENV] == str(hook_dir / shellhook.BASH_HOOK_NAME)
    zshenv = (hook_dir / ".zshenv").read_text(encoding="utf-8")
    assert "spice agent steer --repo-root" in zshenv
    assert (
        str(tmp_path / ".spice" / "agents" / "codex" / "side-channel" / "socket")
        in zshenv
    )


def test_configured_agent_environment_installs_driver_shell_steering_hooks(
    tmp_path, monkeypatch
):
    from spice.config import update_section

    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    update_section(tmp_path, "agent", {"driver": "claude"})
    real_zdotdir = tmp_path / "real-zdotdir"
    real_zdotdir.mkdir()
    real_bash_env = tmp_path / "real-bash-env"
    real_bash_env.write_text("# real bash env\n", encoding="utf-8")
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.setenv(shellhook.ZDOTDIR_ENV, str(real_zdotdir))
    monkeypatch.setenv(shellhook.BASH_ENV_ENV, str(real_bash_env))

    env = lifecycle.agent_environment(tmp_path)

    hook_dir = tmp_path / ".spice" / "agents" / "claude" / "shell-hook"
    assert env[shellhook.ZDOTDIR_ENV] == str(hook_dir)
    assert env[shellhook.BASH_ENV_ENV] == str(hook_dir / shellhook.BASH_HOOK_NAME)
    zshenv = (hook_dir / ".zshenv").read_text(encoding="utf-8")
    bashenv = (hook_dir / shellhook.BASH_HOOK_NAME).read_text(encoding="utf-8")
    assert "spice agent steer --repo-root" in zshenv
    assert (
        str(tmp_path / ".spice" / "agents" / "claude" / "side-channel" / "socket")
        in zshenv
    )
    assert f"export {shellhook.SPICE_STEER_EMITTED_ENV}=1" in zshenv
    assert f"export {shellhook.ZDOTDIR_ENV}={real_zdotdir}" in zshenv
    assert f". {real_zdotdir / '.zshenv'}" in zshenv
    assert f"export {shellhook.BASH_ENV_ENV}={real_bash_env}" in bashenv
    assert f". {real_bash_env}" in bashenv


def test_zshenv_hook_emits_once_then_restores_for_nested_shells(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    home = tmp_path / "home"
    home.mkdir()
    trace = tmp_path / "trace.log"
    fake_python = _fake_steer_python(tmp_path)
    (home / ".zshenv").write_text(
        (
            "print -r -- "
            f'"real-zshenv:${{{shellhook.ZDOTDIR_ENV}-unset}}:'
            f'${{{shellhook.SPICE_STEER_EMITTED_ENV}:-}}" '
            f'>> "${{{SHELL_TRACE_ENV}}}"\n'
        ),
        encoding="utf-8",
    )
    marker = shellhook.shell_steering_marker_path(
        tmp_path, driver_state_dirname="claude"
    )
    marker.parent.mkdir(parents=True)
    marker.write_text("/tmp/spice-side.sock\n", encoding="utf-8")
    hook_dir = shellhook.write_shell_steering_files(
        tmp_path,
        driver_state_dirname="claude",
        base_env={"HOME": str(home)},
        python_command=[str(fake_python)],
    )
    command = (
        "printf 'after:%s:%s:%s\\n' "
        f'"${{{shellhook.ZDOTDIR_ENV}-unset}}" '
        f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
        f'"${{{shellhook.SPICE_STEER_EMITTED_ENV}:-}}" '
        f'>> "${{{SHELL_TRACE_ENV}}}"; '
        f"{shutil.which('zsh') or zsh} -c 'true'"
    )
    env = {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", ""),
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
    }

    subprocess.run([zsh, "-c", command], check=True, env=env)

    lines = trace.read_text(encoding="utf-8").splitlines()
    assert sum(line.startswith("fake:") for line in lines) == 1
    assert "after:unset:unset:1" in lines
    assert lines.count("real-zshenv:unset:1") == 2


def test_bash_env_hook_emits_once_then_restores_for_nested_shells(tmp_path):
    bash = shutil.which("bash")
    if bash is None:
        pytest.skip("bash is not installed")
    home = tmp_path / "home"
    home.mkdir()
    trace = tmp_path / "trace.log"
    fake_python = _fake_steer_python(tmp_path)
    real_bash_env = tmp_path / "real-bash-env"
    real_bash_env.write_text(
        (
            "printf 'real-bash:%s:%s\\n' "
            f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
            f'"${{{shellhook.SPICE_STEER_EMITTED_ENV}:-}}" '
            f'>> "${{{SHELL_TRACE_ENV}}}"\n'
        ),
        encoding="utf-8",
    )
    marker = shellhook.shell_steering_marker_path(
        tmp_path, driver_state_dirname="claude"
    )
    marker.parent.mkdir(parents=True)
    marker.write_text("/tmp/spice-side.sock\n", encoding="utf-8")
    hook_dir = shellhook.write_shell_steering_files(
        tmp_path,
        driver_state_dirname="claude",
        base_env={"HOME": str(home), shellhook.BASH_ENV_ENV: str(real_bash_env)},
        python_command=[str(fake_python)],
    )
    command = (
        "printf 'after:%s:%s\\n' "
        f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
        f'"${{{shellhook.SPICE_STEER_EMITTED_ENV}:-}}" '
        f'>> "${{{SHELL_TRACE_ENV}}}"; '
        f"{bash} -c 'true'"
    )
    env = {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", ""),
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        SHELL_TRACE_ENV: str(trace),
    }

    subprocess.run([bash, "-c", command], check=True, env=env)

    lines = trace.read_text(encoding="utf-8").splitlines()
    assert sum(line.startswith("fake:") for line in lines) == 1
    assert f"after:{real_bash_env}:1" in lines
    assert lines.count(f"real-bash:{real_bash_env}:1") == 2


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


def test_wrapper_missing_proxy_plain_exec_still_injects_steering(tmp_path, monkeypatch):
    monkeypatch.setenv(wrap.PROXY_BIN_ENV, "missing-rtk")
    monkeypatch.setattr(wrap.shutil, "which", lambda _proxy: None)
    events: list[tuple[str, object, object | None]] = []
    stderr = io.StringIO()

    class FakeProcess:
        def wait(self) -> int:
            events.append(("wait", None, None))
            return 7

    def fake_popen(command: list[str]) -> FakeProcess:
        events.append(("popen", command, None))
        return FakeProcess()

    def fake_side_channel(repo_root, *, stderr):
        events.append(("inject", repo_root, stderr))

    monkeypatch.setattr(wrap, "inject_agent_side_channel", fake_side_channel)

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["proxy", "find", ".", "-maxdepth", "0", "-print"],
        popen_factory=fake_popen,
        stderr=stderr,
    )

    assert exit_code == 7
    assert events == [
        ("inject", tmp_path, stderr),
        ("popen", ["find", ".", "-maxdepth", "0", "-print"], None),
        ("wait", None, None),
    ]


def test_wrapper_skips_side_channel_after_shell_hook_emitted(tmp_path, monkeypatch):
    monkeypatch.setenv(
        shellhook.SPICE_STEER_EMITTED_ENV, shellhook.SPICE_STEER_EMITTED_VALUE
    )
    monkeypatch.setenv(wrap.PROXY_BIN_ENV, "missing-rtk")
    monkeypatch.setattr(wrap.shutil, "which", lambda _proxy: None)
    events: list[tuple[str, object, object | None]] = []
    stderr = io.StringIO()

    class FakeProcess:
        def wait(self) -> int:
            events.append(("wait", None, None))
            return 0

    def fake_popen(command: list[str]) -> FakeProcess:
        events.append(("popen", command, None))
        return FakeProcess()

    def fake_side_channel(repo_root, *, stderr):
        events.append(("inject", repo_root, stderr))

    monkeypatch.setattr(wrap, "inject_agent_side_channel", fake_side_channel)

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["true"],
        popen_factory=fake_popen,
        stderr=stderr,
    )

    assert exit_code == 0
    assert events == [
        ("popen", ["true"], None),
        ("wait", None, None),
    ]


def test_agent_steer_cli_emits_side_channel_for_explicit_repo(tmp_path, monkeypatch):
    events: list[Path] = []
    monkeypatch.setattr(
        wrap, "emit_agent_side_channel", lambda repo_root: events.append(repo_root)
    )

    exit_code = agent_cli.handle_agent(
        argparse.Namespace(agent_action="steer", repo_root=str(tmp_path))
    )

    assert exit_code == 0
    assert events == [tmp_path.resolve()]


def test_wrapper_resolves_proxy_steps_aside_for_implicit_find_and_keeps_explicit_passthrough(
    monkeypatch,
):
    monkeypatch.setenv(wrap.PROXY_BIN_ENV, "rtk-test")
    monkeypatch.setattr(
        wrap.shutil,
        "which",
        lambda proxy: "/opt/bin/rtk-test" if proxy == "rtk-test" else None,
    )

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
    ) == ["/opt/bin/rtk-test", "proxy", "find", ".", "-maxdepth", "0", "-print"]
    assert wrap.build_agent_run_command(["rg", "needle"]) == [
        "/opt/bin/rtk-test",
        "rg",
        "needle",
    ]


def test_wrapper_runs_implicit_find_natively_even_when_proxy_is_installed(
    tmp_path, monkeypatch
):
    monkeypatch.setenv(wrap.PROXY_BIN_ENV, "rtk-test")
    monkeypatch.setattr(
        wrap.shutil,
        "which",
        lambda proxy: "/opt/bin/rtk-test" if proxy == "rtk-test" else None,
    )
    events: list[tuple[str, object, object | None]] = []
    stderr = io.StringIO()

    class FakeProcess:
        def wait(self) -> int:
            events.append(("wait", None, None))
            return 0

    def fake_popen(command: list[str]) -> FakeProcess:
        events.append(("popen", command, None))
        return FakeProcess()

    def fake_side_channel(repo_root, *, stderr):
        events.append(("inject", repo_root, stderr))

    monkeypatch.setattr(wrap, "inject_agent_side_channel", fake_side_channel)

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["find", ".", "-name", "*.py"],
        popen_factory=fake_popen,
        stderr=stderr,
    )

    assert exit_code == 0
    assert events == [
        ("inject", tmp_path, stderr),
        ("popen", ["find", ".", "-name", "*.py"], None),
        ("wait", None, None),
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
    assert output.count("Inbox Steering") == 2
    assert "20260101T000000000001Z" in output
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


def test_side_channel_server_payload_reaches_wrapper_stderr(tmp_path, monkeypatch):
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)

    with sidechannel.AgentSideChannelServer(
        tmp_path,
        payload_factory=lambda repo_root: f"side payload for {repo_root.name}\n",
    ):
        wrap.inject_agent_side_channel(tmp_path, stderr=stderr)

    assert stderr.getvalue() == f"side payload for {tmp_path.name}\n"


def _config_values(command: list[str]) -> list[str]:
    return [
        command[index + 1] for index, part in enumerate(command) if part == "--config"
    ]


def _write_spice_product_shape(repo: Path) -> None:
    for relative in (
        Path("spice") / "__main__.py",
        Path("spice") / "cli" / "entry.py",
        Path("spice") / "agent" / "wrap.py",
    ):
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("# test spice product shape\n", encoding="utf-8")


def _fake_steer_python(tmp_path: Path) -> Path:
    path = tmp_path / "fake-python"
    path.write_text(
        (
            "#!/bin/sh\n"
            "printf 'fake:%s:%s:%s:%s\\n' "
            f'"${{{shellhook.SPICE_STEER_EMITTED_ENV}:-}}" '
            f'"${{{shellhook.ZDOTDIR_ENV}-unset}}" '
            f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
            '"$*" '
            f'>> "${{{SHELL_TRACE_ENV}}}"\n'
        ),
        encoding="utf-8",
    )
    path.chmod(0o755)
    return path


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


def test_available_skill_path_materializes_into_the_worktree(tmp_path):
    located = lifecycle.available_skill_path(tmp_path, required=True)

    expected = tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    assert located == expected.resolve()
    assert located.read_text(
        encoding="utf-8"
    ) == lifecycle.packaged_skill_path().read_text(encoding="utf-8")


def test_available_skill_path_required_fails_without_worktree_skill(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(
        lifecycle, "packaged_skill_path", lambda: tmp_path / "missing-package.md"
    )

    with pytest.raises(SpiceError, match="missing spice skill at"):
        lifecycle.available_skill_path(tmp_path, required=True)


def test_available_skill_path_keeps_the_repo_owned_skill_verbatim(tmp_path):
    import subprocess

    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    owned = tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    owned.parent.mkdir(parents=True)
    content = "---\nname: spice\n---\nrepo-owned skill\n"
    owned.write_text(content, encoding="utf-8")
    subprocess.run(
        ["git", "-C", str(tmp_path), "add", "--", owned.as_posix()], check=True
    )

    located = lifecycle.available_skill_path(tmp_path, required=True)

    assert located == owned.resolve()
    assert owned.read_text(encoding="utf-8") == content


def test_materialize_worktree_skill_refreshes_stale_copies(tmp_path):
    target = tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    target.parent.mkdir(parents=True)
    target.write_text("stale content from an older install\n", encoding="utf-8")

    located = lifecycle.materialize_worktree_skill(tmp_path)

    assert located == target.resolve()
    assert target.read_text(
        encoding="utf-8"
    ) == lifecycle.packaged_skill_path().read_text(encoding="utf-8")
