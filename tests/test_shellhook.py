"""Agent wrapper routing and shell steering contracts."""

import io
import json
import os
import shlex
import shutil
import subprocess
import sys
import time
import tomllib
from pathlib import Path

import pytest

from spice.agent import driver as agent_driver
from spice.agent import lifecycle, shellhook, wrap
from spice.agent.driver import CLAUDE_DRIVER, DRIVER
from spice.errors import SpiceError

SHELL_TRACE_ENV = "SPICE_TEST_TRACE"  # env-policy: allow
SHELL_HOOK_FAILURE_EXIT_CODE = 127
UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND = "spice agent " + "shell-hook"
UNSUPPORTED_AGENT_STEER_COMMAND = "spice agent " + "steer"
SCOPED_REWRITE_PROCESS_PID = 4242


def test_rtk_rewrite_protocol_accepts_current_result_pairs():
    responses = iter(
        [
            subprocess.CompletedProcess([], 3, stdout="rtk git status\n", stderr=""),
            subprocess.CompletedProcess([], 1, stdout="", stderr=""),
        ]
    )

    def run(*_args, **_kwargs):
        return next(responses)

    assert wrap.rtk_rewrite_command_text("git", "status", run=run) == "rtk git status"
    assert wrap.rtk_rewrite_command_text("true", run=run) is None


@pytest.mark.parametrize(
    "completed",
    [
        subprocess.CompletedProcess([], 0, stdout="rtk git status\n", stderr=""),
        subprocess.CompletedProcess([], 3, stdout="", stderr=""),
        subprocess.CompletedProcess([], 1, stdout="unexpected\n", stderr=""),
    ],
)
def test_rtk_rewrite_protocol_reports_invalid_result_with_install_path(completed):
    with pytest.raises(SpiceError) as exc_info:
        wrap.rtk_rewrite_command_text(
            "git", "status", run=lambda *_args, **_kwargs: completed
        )

    error = str(exc_info.value)
    assert "invalid RTK rewrite protocol result" in error
    assert wrap.RTK_UPSTREAM in error


def test_rtk_companion_validation_checks_version_and_rewrite_probe():
    calls: list[list[str]] = []

    def run(args, **_kwargs):
        calls.append(args)
        if args == ["rtk", "--version"]:
            return subprocess.CompletedProcess(
                args, 0, stdout="rtk 0.42.4\n", stderr=""
            )
        return subprocess.CompletedProcess(
            args, 3, stdout="rtk git status\n", stderr=""
        )

    assert wrap.validate_rtk_companion(run=run) == "0.42.4"
    assert calls == [
        ["rtk", "--version"],
        ["rtk", "rewrite", "--", "git", "status"],
    ]


@pytest.mark.parametrize(
    ("version_output", "expected"),
    [
        ("rtk 0.42.3\n", "is obsolete"),
        ("unknown\n", "could not validate RTK version"),
    ],
)
def test_rtk_companion_validation_reports_version_failure(version_output, expected):
    completed = subprocess.CompletedProcess(
        ["rtk", "--version"], 0, stdout=version_output, stderr=""
    )

    with pytest.raises(SpiceError) as exc_info:
        wrap.validate_rtk_companion(run=lambda *_args, **_kwargs: completed)

    error = str(exc_info.value)
    assert expected in error
    assert wrap.RTK_UPSTREAM in error


def test_rtk_companion_validation_reports_missing_binary():
    def missing(*_args, **_kwargs):
        raise FileNotFoundError("rtk")

    with pytest.raises(SpiceError) as exc_info:
        wrap.validate_rtk_companion(run=missing)

    assert "RTK unavailable" in str(exc_info.value)
    assert wrap.RTK_UPSTREAM in str(exc_info.value)


def test_wrapper_git_route_inherits_ambient_supervisor_environment(tmp_path):
    env = wrap.build_agent_run_environment(["git", "status"], repo_root=tmp_path)
    source = "ambient" if env is None else "explicit"

    assert source == "ambient"


def test_wrapper_spice_routes_inherit_ambient_env(tmp_path, monkeypatch):
    monkeypatch.setattr(
        wrap,
        "agent_run_child_worktree_environment",
        lambda *_args, **_kwargs: pytest.fail("spice route env should not be built"),
    )
    spice_env = wrap.build_agent_run_environment(
        ["spice", "task", "status"], repo_root=tmp_path
    )
    uv_spice_env = wrap.build_agent_run_environment(
        ["uv", "run", "spice", "task", "status"], repo_root=tmp_path
    )

    assert spice_env is None
    assert uv_spice_env is None


def test_wrapper_does_not_reroute_spice_commands_under_single_install(
    tmp_path,
):
    # Single-install model: spice is the installed tool, so the wrapper never
    # rewrites a spice invocation to a per-worktree `python -m spice`. Even a
    # worktree that contains spice's own source (product shape) must pass spice
    # commands through unchanged to the installed runtime on PATH.
    _write_spice_product_shape(tmp_path)

    spice_command = wrap.build_agent_run_command(
        ["spice", "task", "status"], repo_root=tmp_path
    )
    uv_spice_command = wrap.build_agent_run_command(
        ["uv", "run", "spice", "task", "status"], repo_root=tmp_path
    )

    assert spice_command == ["spice", "task", "status"]
    assert uv_spice_command == ["uv", "run", "spice", "task", "status"]


def test_wrapper_rewrites_stage_one_shell_command_before_stage_two(monkeypatch):
    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str, **_kwargs) -> str | None:
        calls.append(args)
        return "rtk git status --short"

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    command = wrap.build_agent_run_command(
        ["zsh", "-c", "git status --short"], rewrite_rtk=True
    )

    assert command == ["zsh", "-c", "rtk git status --short"]
    assert calls == [("git status --short",)]


def test_wrapper_rewrites_codex_snapshot_trailing_shell_exec(monkeypatch):
    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str, **_kwargs) -> str | None:
        calls.append(args)
        if args == ("git status --short",):
            return "rtk git status --short"
        return None

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    snapshot = (
        '__CODEX_SNAPSHOT_OVERRIDE_SET_0="${CODEX_THREAD_ID+x}"\n'
        "if . '.codex/shell_snapshots/thread.sh' >/dev/null 2>&1; "
        "then :; fi\n"
        "exec '/bin/zsh' -c 'git status --short'"
    )

    command = wrap.build_agent_run_command(
        ["/bin/zsh", "-c", snapshot], rewrite_rtk=True
    )

    assert command == [
        "/bin/zsh",
        "-c",
        (
            '__CODEX_SNAPSHOT_OVERRIDE_SET_0="${CODEX_THREAD_ID+x}"\n'
            "if . '.codex/shell_snapshots/thread.sh' >/dev/null 2>&1; "
            "then :; fi\n"
            "exec /bin/zsh -c 'rtk git status --short'"
        ),
    ]
    assert calls == [(snapshot,), ("git status --short",)]


def test_wrapper_rewrites_direct_agent_command_with_rtk_source_of_truth(monkeypatch):
    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str, **_kwargs) -> str | None:
        calls.append(args)
        return "rtk grep needle"

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    command = wrap.build_agent_run_command(["rg", "needle"], rewrite_rtk=True)

    assert command == ["rtk", "grep", "needle"]
    assert calls == [("rg", "needle")]


@pytest.mark.parametrize("identity_kind", ["basename", "absolute"])
def test_configured_rtk_routes_canonical_shell_and_direct_rewrites(
    tmp_path, monkeypatch, identity_kind
):
    executable = (
        "alternate-rtk"
        if identity_kind == "basename"
        else str(tmp_path / "Spice Tools" / "rtk companion")
    )
    _write_rtk_config(tmp_path, executable)
    calls: list[tuple[tuple[str, ...], str]] = []

    def fake_rewrite(*args: str, **kwargs) -> str:
        calls.append((args, kwargs["rtk_executable"]))
        if args == ("rg", "needle"):
            return "rtk grep needle"
        return "rtk git status"

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    direct = wrap.build_agent_run_command(
        ["rg", "needle"], repo_root=tmp_path, rewrite_rtk=True
    )
    shell = wrap.build_agent_run_command(
        ["zsh", "-c", "git status"], repo_root=tmp_path, rewrite_rtk=True
    )

    assert direct == [executable, "grep", "needle"]
    assert shell == ["zsh", "-c", f"{shlex.quote(executable)} git status"]
    assert calls == [
        (("rg", "needle"), executable),
        (("git status",), executable),
    ]


def test_canonical_and_resolved_rtk_direct_inputs_preserve_their_identity(
    tmp_path, monkeypatch
):
    executable = str(tmp_path / "Spice Tools" / "rtk companion")
    _write_rtk_config(tmp_path, executable)
    monkeypatch.setattr(
        wrap,
        "rtk_rewrite_command_text",
        lambda *_args, **_kwargs: "rtk unexpected-rewrite",
    )
    inputs = [
        ["rtk", "grep", "needle"],
        [executable, "grep", "needle"],
    ]

    outputs = [
        wrap.build_agent_run_command(
            command,
            repo_root=tmp_path,
            rewrite_rtk=True,
        )
        for command in inputs
    ]

    assert outputs == inputs


def test_wrapper_rejects_malformed_matched_direct_rewrite_before_execution(
    monkeypatch,
):
    executed: list[list[str]] = []
    monkeypatch.setattr(
        wrap,
        "rtk_rewrite_command_text",
        lambda *_args, **_kwargs: "rtk 'unterminated",
    )

    with pytest.raises(SpiceError) as exc_info:
        wrap.run_agent_command(
            None,
            ["rg", "needle"],
            popen_factory=lambda command, **_kwargs: executed.append(command),
            stderr=io.StringIO(),
        )

    assert executed == []
    assert "invalid RTK direct rewrite argv" in str(exc_info.value)
    assert "must be shell-parseable" in str(exc_info.value)


def test_wrapper_does_not_special_case_proxy_argv(monkeypatch):
    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str, **_kwargs) -> str | None:
        calls.append(args)
        return None

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    command = wrap.build_agent_run_command(["proxy", "git", "status"], rewrite_rtk=True)

    assert command == ["proxy", "git", "status"]
    assert calls == [("proxy", "git", "status")]


def test_wrapper_routes_python_commands_through_deployment_interpreter(
    tmp_path, monkeypatch
):
    _write_spice_product_shape(tmp_path)

    python_command = wrap.build_agent_run_command(
        ["python", "-m", "pip", "--version"], repo_root=tmp_path
    )
    python3_command = wrap.build_agent_run_command(
        ["python3", "-m", "pip", "--version"], repo_root=tmp_path
    )

    assert python_command == [sys.executable, "-m", "pip", "--version"]
    assert python3_command == [sys.executable, "-m", "pip", "--version"]


def test_wrapper_does_not_python_route_proxy_argv(tmp_path):
    _write_spice_product_shape(tmp_path)

    assert wrap.build_agent_run_command(
        ["python", "-m", "pip", "--version"], repo_root=tmp_path
    ) == [sys.executable, "-m", "pip", "--version"]
    assert wrap.build_agent_run_command(
        ["proxy", "python", "-m", "pip", "--version"], repo_root=tmp_path
    ) == ["proxy", "python", "-m", "pip", "--version"]


def test_wrapper_ignores_active_virtualenv_for_python_route(tmp_path, monkeypatch):
    venv_python = tmp_path / "active-env" / "bin" / "python"
    venv_python.parent.mkdir(parents=True)
    venv_python.write_text("#!/bin/sh\n", encoding="utf-8")
    venv_python.chmod(0o755)
    monkeypatch.setenv("VIRTUAL_ENV", str(tmp_path / "active-env"))

    assert wrap.build_agent_run_command(
        ["python", "--version"], repo_root=tmp_path
    ) == [
        sys.executable,
        "--version",
    ]


def test_wrapper_ignores_repo_venv_for_python_route(tmp_path, monkeypatch):
    venv_python = tmp_path / ".venv" / "bin" / "python"
    venv_python.parent.mkdir(parents=True)
    venv_python.write_text("#!/bin/sh\n", encoding="utf-8")
    venv_python.chmod(0o755)
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)

    assert wrap.build_agent_run_command(
        ["python", "--version"], repo_root=tmp_path
    ) == [
        sys.executable,
        "--version",
    ]


def test_wrapper_routes_python_without_repo_venv_to_deployment_interpreter(
    tmp_path, monkeypatch
):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)

    command = wrap.build_agent_run_command(["python", "--version"], repo_root=tmp_path)

    assert command == [sys.executable, "--version"]


def test_wrapper_plain_commands_do_not_inject_worktree_spice_pythonpath(
    tmp_path, monkeypatch
):
    _write_spice_product_shape(tmp_path)
    monkeypatch.delenv("PYTHONPATH", raising=False)

    env = wrap.build_agent_run_environment(["pytest"], repo_root=tmp_path)

    assert env is None


def test_static_shell_hook_paths_count_as_generated():
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()

    assert shellhook.is_generated_shell_hook_path(str(static_hook_dir))
    assert shellhook.is_generated_shell_hook_path(
        str(static_hook_dir / shellhook.BASH_HOOK_NAME)
    )


def test_wrapper_non_shell_commands_inherit_ambient_shell_hook_environment(
    tmp_path, monkeypatch
):
    monkeypatch.setenv("ZDOTDIR", "hook")
    monkeypatch.setenv("BASH_ENV", "hook")
    monkeypatch.setenv(SHELL_TRACE_ENV, "preserved")

    env = wrap.build_agent_run_environment(["true"], repo_root=tmp_path)

    assert env is None


def test_wrapper_exports_agent_scoped_rtk_db_for_ambient_agent_commands(
    tmp_path, monkeypatch
):
    _init_git_repo(tmp_path)
    monkeypatch.setenv(DRIVER.thread_id_env, "thread-a")
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)

    env = wrap.build_agent_run_environment(["true"], repo_root=tmp_path)

    assert env is not None
    assert Path(env[wrap.RTK_DB_PATH_ENV]) == (
        tmp_path / ".git" / "spice" / "agents" / "thread-a" / "rtk" / "history.db"
    )
    assert Path(env[wrap.RTK_DB_PATH_ENV]).parent.is_dir()


@pytest.mark.parametrize("identity_kind", ["builtin", "basename", "absolute"])
def test_rtk_selectors_and_children_share_distinct_thread_scoped_history(
    tmp_path, monkeypatch, identity_kind
):
    _init_git_repo(tmp_path)
    executable = {
        "builtin": "rtk",
        "basename": "alternate-rtk",
        "absolute": str(tmp_path / "Spice Tools" / "rtk companion"),
    }[identity_kind]
    _write_rtk_config(tmp_path, executable)
    monkeypatch.setenv(wrap.RTK_DB_PATH_ENV, "/ambient/global-history.db")
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.setattr(wrap, "emit_initial_side_channel_payload", lambda *_a, **_k: ())
    monkeypatch.setattr(wrap, "start_agent_side_channel_watch", lambda *_a, **_k: None)
    monkeypatch.setattr(wrap, "join_agent_side_channel_watch", lambda *_a, **_k: None)
    selector_environments: list[dict[str, str]] = []
    child_environments: list[dict[str, str]] = []
    selector_commands: list[list[str]] = []
    child_commands: list[list[str]] = []
    native_run = subprocess.run

    def rewrite_run(args, **kwargs):
        if args[:2] != [executable, "rewrite"]:
            return native_run(args, **kwargs)
        selector_commands.append(args)
        selector_environments.append(kwargs["env"])
        return subprocess.CompletedProcess(
            args,
            wrap.RTK_REWRITE_MATCH_EXIT_CODE,
            stdout="rtk git status",
            stderr="",
        )

    class Process:
        pid = SCOPED_REWRITE_PROCESS_PID

        def wait(self):
            return 0

    def popen(command, *, env):
        child_commands.append(command)
        child_environments.append(env)
        return Process()

    monkeypatch.setattr(wrap.subprocess, "run", rewrite_run)
    for thread_id, command in (
        ("thread-a", ["git", "status"]),
        ("thread-b", ["zsh", "-c", "git status"]),
    ):
        monkeypatch.setenv(DRIVER.thread_id_env, thread_id)
        assert (
            wrap.run_agent_command(
                tmp_path,
                command,
                popen_factory=popen,
                stderr=io.StringIO(),
            )
            == 0
        )

    expected_paths = [
        tmp_path / ".git" / "spice" / "agents" / thread / "rtk" / "history.db"
        for thread in ("thread-a", "thread-b")
    ]
    assert [Path(env[wrap.RTK_DB_PATH_ENV]) for env in selector_environments] == (
        expected_paths
    )
    assert [Path(env[wrap.RTK_DB_PATH_ENV]) for env in child_environments] == (
        expected_paths
    )
    assert selector_commands == [
        [executable, "rewrite", "--", "git", "status"],
        [executable, "rewrite", "--", "git status"],
    ]
    assert child_commands == [
        [executable, "git", "status"],
        ["zsh", "-c", f"{shlex.quote(executable)} git status"],
    ]


def test_wrapper_route_environment_uses_static_hook_stage_for_shell_execution(
    tmp_path, monkeypatch
):
    _write_spice_product_shape(tmp_path)
    monkeypatch.setenv("ZDOTDIR", "hook")
    monkeypatch.setenv("BASH_ENV", "hook")

    env = wrap.build_agent_run_environment(
        ["zsh", "-c", "true"],
        repo_root=tmp_path,
    )

    assert env is not None
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    assert env["ZDOTDIR"] == str(static_hook_dir)
    assert env["BASH_ENV"] == str(static_hook_dir / shellhook.BASH_HOOK_NAME)


def test_wrapper_installs_shell_hook_environment_for_direct_shell_commands(
    tmp_path, monkeypatch
):
    monkeypatch.delenv("ZDOTDIR", raising=False)
    monkeypatch.delenv("BASH_ENV", raising=False)

    env = wrap.build_agent_run_environment(["zsh", "-c", "true"], repo_root=tmp_path)

    assert env is not None
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    assert env["ZDOTDIR"] == str(static_hook_dir)
    assert env["BASH_ENV"] == str(static_hook_dir / shellhook.BASH_HOOK_NAME)


def test_agent_environment_redirects_zsh_compdump_outside_shellhooks_dir(
    tmp_path, monkeypatch
):
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv("ZDOTDIR", raising=False)
    monkeypatch.delenv("BASH_ENV", raising=False)
    monkeypatch.delenv("ZSH_COMPDUMP", raising=False)
    monkeypatch.setenv("HOME", str(tmp_path))

    env = lifecycle.agent_environment(tmp_path)

    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    assert env["ZSH_COMPDUMP"] == str(tmp_path / ".zcompdump")
    assert not env["ZSH_COMPDUMP"].startswith(str(hook_dir))


def test_agent_environment_redirects_zsh_compdump_to_original_zdotdir_when_set(
    tmp_path, monkeypatch
):
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)
    zdotdir = tmp_path / "zdotdir"
    monkeypatch.setenv("ZDOTDIR", str(zdotdir))
    monkeypatch.delenv("BASH_ENV", raising=False)
    monkeypatch.delenv("ZSH_COMPDUMP", raising=False)

    env = lifecycle.agent_environment(tmp_path)

    assert env["ZSH_COMPDUMP"] == str(zdotdir / ".zcompdump")


def test_agent_environment_preserves_caller_zsh_compdump_when_already_set(
    tmp_path, monkeypatch
):
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)
    custom_dump = str(tmp_path / "custom" / ".zcompdump")
    monkeypatch.delenv("ZDOTDIR", raising=False)
    monkeypatch.delenv("BASH_ENV", raising=False)
    monkeypatch.setenv("ZSH_COMPDUMP", custom_dump)

    env = lifecycle.agent_environment(tmp_path)

    assert env["ZSH_COMPDUMP"] == custom_dump


def test_agent_run_shell_command_loads_wrappers_from_ambient_hook_env(
    tmp_path, monkeypatch
):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    _write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={"common": {"wrap": ["grep"]}},
    )
    trace = tmp_path / "trace.log"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    wrap_bin = bin_dir / "wrap"
    wrap_bin.write_text(
        f'#!/bin/sh\nprintf \'wrap:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
        encoding="utf-8",
    )
    wrap_bin.chmod(0o755)
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", lambda *args, **_kwargs: None)
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    base_env = dict(os.environ)  # env-policy: allow
    base_env["PATH"] = (
        str(bin_dir) + os.pathsep + os.environ.get("PATH", "")
    )  # env-policy: allow
    base_env[SHELL_TRACE_ENV] = str(trace)
    base_env.pop(shellhook.ZDOTDIR_ENV, None)
    base_env.pop(shellhook.BASH_ENV_ENV, None)
    ambient_env = shellhook.apply_shell_steering_environment(
        tmp_path,
        base_env=base_env,
    )
    ambient_env[shellhook.SHELL_HOOK_PYTHON_ENV] = str(fake_python)
    for name, value in ambient_env.items():
        monkeypatch.setenv(name, value)

    exit_code = wrap.run_agent_command(
        tmp_path,
        [zsh, "-c", "grep needle /dev/null"],
        stderr=io.StringIO(),
    )

    assert exit_code == 0
    lines = _trace_lines(trace, expected_prefix="wrap:")
    assert "wrap:grep needle /dev/null" in lines


def test_agent_environment_does_not_inject_worktree_spice_pythonpath(
    tmp_path, monkeypatch
):
    _write_spice_product_shape(tmp_path)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv("PYTHONPATH", raising=False)

    env = lifecycle.agent_environment(tmp_path)

    assert "PYTHONPATH" not in env


def test_agent_environment_installs_shell_steering_hooks_for_default_driver(
    tmp_path, monkeypatch
):
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(shellhook.ZDOTDIR_ENV, raising=False)
    monkeypatch.delenv(shellhook.BASH_ENV_ENV, raising=False)

    env = lifecycle.agent_environment(tmp_path)

    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    assert env[shellhook.ZDOTDIR_ENV] == str(hook_dir)
    assert env[shellhook.BASH_ENV_ENV] == str(hook_dir / shellhook.BASH_HOOK_NAME)
    assert env[shellhook.SHELL_HOOK_PYTHON_ENV] == sys.executable
    assert env[shellhook.SHELL_HOOK_REPO_ROOT_ENV] == str(tmp_path.resolve())
    assert shellhook.SHELL_HOOK_WRAPPERS_ENV.startswith(
        "SPICE_SHELL_HOOK_"  # env-policy: allow
    )
    assert env[shellhook.SHELL_HOOK_WRAPPERS_ENV] == "\n".join(
        shellhook.render_agent_wrapper_lines(tmp_path)
    )
    assert env[shellhook.SHELL_HOOK_WRAPPERS_ENV] == "\n".join(
        _builtin_rtk_wrapper_lines()
    )
    assert env[shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV] == ""
    assert env[shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV] == ""
    zshenv = (hook_dir / ".zshenv").read_text(encoding="utf-8")
    assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in zshenv
    assert "spice agent run --" in zshenv
    assert "--preserve-shell-hook-env" not in zshenv
    assert shellhook.SHELL_HOOK_WRAPPERS_ENV in zshenv
    assert UNSUPPORTED_AGENT_STEER_COMMAND not in zshenv
    assert "--watch --parent-pid" not in zshenv


def test_agent_environment_precomputes_configured_shell_wrapper_block(
    tmp_path, monkeypatch
):
    _write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={
            "common": {
                "wrap": ["grep", "git"],
                "pytest": {"argv": ["$SPICE_SHELL_HOOK_PYTHON", "-m", "pytest"]},
            }
        },
    )
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)

    env = lifecycle.agent_environment(tmp_path)

    assert env[shellhook.SHELL_HOOK_WRAPPERS_ENV] == "\n".join(
        shellhook.render_agent_wrapper_lines(tmp_path)
    )
    assert env[shellhook.SHELL_HOOK_WRAPPERS_ENV] == "\n".join(
        [
            *_expected_wrapper_lines("wrap", ["grep", "git"]),
            *_expected_active_python_module_wrapper_lines(["pytest"]),
        ]
    )


def test_layered_wrapper_false_disables_inherited_entry_and_group(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["common", "disabled", "active"],
        groups={
            "common": {
                "rtk": False,
                "wrap": ["grep"],
            },
            "disabled": False,
            "active": {"pytest": {"argv": ["python", "-m", "pytest"]}},
        },
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == [
        *_expected_wrapper_lines("wrap", ["grep"]),
        *_expected_python_module_wrapper_lines(["pytest"]),
    ]


def test_configured_agent_environment_installs_driver_shell_steering_hooks(
    tmp_path, monkeypatch
):
    from spice.config import WORKTREE_SOURCE, set_scope_section

    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    set_scope_section(tmp_path, WORKTREE_SOURCE, "agent", {"driver": "claude"})
    real_zdotdir = tmp_path / "real-zdotdir"
    real_zdotdir.mkdir()
    real_bash_env = tmp_path / "real-bash-env"
    real_bash_env.write_text("# real bash env\n", encoding="utf-8")
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(shellhook.HISTFILE_ENV, raising=False)
    monkeypatch.delenv(shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV, raising=False)
    monkeypatch.setenv(shellhook.ZDOTDIR_ENV, str(real_zdotdir))
    monkeypatch.setenv(shellhook.BASH_ENV_ENV, str(real_bash_env))

    env = lifecycle.agent_environment(tmp_path)

    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    assert env[shellhook.ZDOTDIR_ENV] == str(hook_dir)
    assert env[shellhook.BASH_ENV_ENV] == str(hook_dir / shellhook.BASH_HOOK_NAME)
    assert env[shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV] == str(real_zdotdir)
    assert env[shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV] == str(real_bash_env)
    assert env[shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV] == str(
        real_zdotdir / ".zsh_history"
    )
    zshenv = (hook_dir / ".zshenv").read_text(encoding="utf-8")
    bashenv = (hook_dir / shellhook.BASH_HOOK_NAME).read_text(encoding="utf-8")
    assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in zshenv
    assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in bashenv
    assert "spice agent run --" in zshenv
    assert "spice agent run --" in bashenv
    assert "--preserve-shell-hook-env" not in zshenv
    assert "--preserve-shell-hook-env" not in bashenv
    assert UNSUPPORTED_AGENT_STEER_COMMAND not in zshenv
    assert "--watch --parent-pid" not in zshenv


def test_shell_steering_runtime_environment_ignores_generated_hook_as_original():
    hook_dir = shellhook.packaged_shell_steering_hook_dir()

    env = shellhook.shell_steering_runtime_environment(
        base_env={
            shellhook.ZDOTDIR_ENV: str(hook_dir),
            shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
            shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV: str(hook_dir),
            shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV: str(
                hook_dir / shellhook.BASH_HOOK_NAME
            ),
        },
        python_command=["agent-python"],
    )

    assert env[shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV] == ""
    assert env[shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV] == ""


def test_shell_steering_runtime_environment_keeps_real_original_before_hook():
    hook_dir = shellhook.packaged_shell_steering_hook_dir()

    env = shellhook.shell_steering_runtime_environment(
        base_env={
            shellhook.ZDOTDIR_ENV: str(hook_dir),
            shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
            shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV: "/real-zdotdir",
            shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV: "/real-bash-env",
        },
        python_command=["agent-python"],
    )

    assert env[shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV] == "/real-zdotdir"
    assert env[shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV] == "/real-bash-env"


def test_shell_steering_runtime_environment_maps_zsh_history_to_home(tmp_path):
    env = shellhook.shell_steering_runtime_environment(
        base_env={"HOME": str(tmp_path)},
        python_command=["agent-python"],
    )

    assert env[shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV] == str(
        tmp_path / ".zsh_history"
    )


def test_shell_steering_runtime_environment_maps_zsh_history_to_original_zdotdir(
    tmp_path,
):
    real_zdotdir = tmp_path / "real-zdotdir"
    env = shellhook.shell_steering_runtime_environment(
        base_env={shellhook.ZDOTDIR_ENV: str(real_zdotdir)},
        python_command=["agent-python"],
    )

    assert env[shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV] == str(
        real_zdotdir / ".zsh_history"
    )


def test_shell_steering_runtime_environment_preserves_explicit_zsh_history(tmp_path):
    history = tmp_path / "custom-history"
    env = shellhook.shell_steering_runtime_environment(
        base_env={shellhook.HISTFILE_ENV: str(history)},
        python_command=["agent-python"],
    )

    assert env[shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV] == str(history)


def test_shell_steering_runtime_environment_ignores_generated_hook_zsh_history(
    tmp_path,
):
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = shellhook.shell_steering_runtime_environment(
        base_env={
            "HOME": str(tmp_path),
            shellhook.HISTFILE_ENV: str(hook_dir / ".zsh_history"),
        },
        python_command=["agent-python"],
    )

    assert env[shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV] == str(
        tmp_path / ".zsh_history"
    )


def test_shell_steering_files_are_stable_across_original_env_changes():
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    first_zshenv = (hook_dir / ".zshenv").read_text(encoding="utf-8")
    first_bashenv = (hook_dir / shellhook.BASH_HOOK_NAME).read_text(encoding="utf-8")

    assert (hook_dir / ".zshenv").read_text(encoding="utf-8") == first_zshenv
    assert (hook_dir / shellhook.BASH_HOOK_NAME).read_text(
        encoding="utf-8"
    ) == first_bashenv
    assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in first_zshenv
    assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in first_bashenv
    assert "spice agent run --" in first_zshenv
    assert "spice agent run --" in first_bashenv
    assert "--preserve-shell-hook-env" not in first_zshenv
    assert "--preserve-shell-hook-env" not in first_bashenv
    assert shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV in first_bashenv
    assert shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV in first_bashenv


def test_packaged_shell_hooks_are_static_env_driven_and_packaged():
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    dynamic_surfaces = {
        ".zshenv",
        ".zprofile",
        ".zlogin",
        shellhook.BASH_HOOK_NAME,
    }

    for filename in (*shellhook.ZSH_HOOK_NAMES, shellhook.BASH_HOOK_NAME):
        text = (hook_dir / filename).read_text(encoding="utf-8")
        assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in text
        assert shellhook.SHELL_HOOK_WRAPPERS_ENV in text
        assert shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV in text
        assert shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV in text
        if filename in dynamic_surfaces:
            assert "spice agent run --" in text
        assert "staticshellhooks" in text
        assert "--preserve-shell-hook-env" not in text
        if filename == shellhook.BASH_HOOK_NAME:
            assert shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV not in text
        else:
            assert shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV in text

        static_text = (static_hook_dir / filename).read_text(encoding="utf-8")
        assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in static_text
        assert "spice agent run --" not in static_text
        assert shellhook.SHELL_HOOK_WRAPPERS_ENV in static_text
        assert shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV in static_text
        assert shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV in static_text

    package_data = tomllib.loads(Path("pyproject.toml").read_text(encoding="utf-8"))[
        "tool"
    ]["setuptools"]["package-data"]["spice.agent"]
    assert "shellhooks/.zshrc" in package_data
    assert "staticshellhooks/.zshrc" in package_data


def _write_spice_product_shape(repo: Path) -> None:
    for relative in (
        Path("spice") / "__main__.py",
        Path("spice") / "cli" / "entry.py",
        Path("spice") / "agent" / "wrap.py",
    ):
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("# test spice product shape\n", encoding="utf-8")


def _init_git_repo(repo: Path) -> None:
    subprocess.run(
        ["git", "init"],
        cwd=repo,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _write_rtk_config(repo: Path, executable: str) -> None:
    (repo / "spice.toml").write_text(
        f"[rtk]\nexecutable = {json.dumps(executable)}\n",
        encoding="utf-8",
    )


def _write_agent_wrapper_config(
    repo: Path,
    *,
    order: list[str] | None,
    groups: dict[str, dict[str, object] | bool],
) -> None:
    lines: list[str] = []
    if order is not None:
        wrappers_value = "[" + ", ".join(f'"{name}"' for name in order) + "]"
        lines.extend(
            [
                "[tool.spice.agent]",
                f"wrappers = {wrappers_value}",
            ]
        )
    disabled_groups = [name for name, entries in groups.items() if entries is False]
    if disabled_groups:
        lines.extend(["", "[tool.spice.wrappers]"])
        lines.extend(f"{_toml_key(name)} = false" for name in disabled_groups)
    for group_name, entries in groups.items():
        if entries is False:
            continue
        assert isinstance(entries, dict)
        lines.extend(["", f"[tool.spice.wrappers.{group_name}]"])
        for wrapper, value in entries.items():
            if value is False:
                lines.append(f"{_toml_key(wrapper)} = false")
                continue
            if isinstance(value, dict):
                command = value["argv"]
                lines.append(
                    f"{_toml_key(wrapper)} = {{ argv = ["
                    + ", ".join(f'"{word}"' for word in command)
                    + "] }"
                )
                continue
            lines.append(
                f"{_toml_key(wrapper)} = ["
                + ", ".join(f'"{selector}"' for selector in value)
                + "]"
            )
    (repo / "pyproject.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")


def _toml_key(value: str) -> str:
    if shellhook.CONFIG_NAME_RE.fullmatch(value):
        return value
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _expected_project_common_with_pytest_wrapper_lines() -> list[str]:
    return [
        *_expected_wrapper_lines("wrap", ["run", "grep", "find", "git"]),
        *_expected_active_python_module_wrapper_lines(["pytest"]),
    ]


def _builtin_rtk_wrapper_lines(rtk_executable: str = "rtk") -> list[str]:
    command_word = shellhook.shell_command_word(rtk_executable)
    return [
        "",
        "rtk() {",
        '  if [ "${1-}" = grep ]; then',
        '    for _spice_word in "$@"; do',
        '      case "$_spice_word" in',
        "        --files|--type|--type=*|--no-heading)",
        "          shift",
        '          command rg "$@"',
        "          return",
        "          ;;",
        "      esac",
        "    done",
        "  fi",
        '  if [ "${1-}" = find ]; then',
        '    for _spice_word in "$@"; do',
        '      case "$_spice_word" in',
        "        -print|-print0|-prune|-exec|-execdir|-delete|'('|')'|'!'|-o|-a)",
        "          shift",
        '          command find "$@"',
        "          return",
        "          ;;",
        "      esac",
        "    done",
        "  fi",
        '  if [ "${1-}" = git ]; then',
        '    for _spice_word in "$@"; do',
        '      case "$_spice_word" in',
        "        --first-parent|--check|--name-status|--name-only)",
        "          shift",
        '          command git "$@"',
        "          return",
        "          ;;",
        "      esac",
        "    done",
        "  fi",
        '  if [ "${1-}" = grep ]; then',
        "    shift",
        f'    command {command_word} grep -E "$@"',
        "    return",
        "  fi",
        f'  command {command_word} "$@"',
        "}",
    ]


def _expected_wrapper_lines(wrapper: str, selectors: list[str]) -> list[str]:
    lines: list[str] = []
    for selector in selectors:
        lines.extend(["", f"{selector}() {{", f'  {wrapper} {selector} "$@"', "}"])
    return lines


def _expected_python_module_wrapper_lines(selectors: list[str]) -> list[str]:
    lines: list[str] = []
    for selector in selectors:
        lines.extend(["", f"{selector}() {{", f'  python -m {selector} "$@"', "}"])
    return lines


def _expected_active_python_module_wrapper_lines(selectors: list[str]) -> list[str]:
    lines: list[str] = []
    for selector in selectors:
        lines.extend(
            [
                "",
                f"{selector}() {{",
                f'  "$SPICE_SHELL_HOOK_PYTHON" -m {selector} "$@"',
                "}",
            ]
        )
    return lines


def _fake_spice_python(tmp_path: Path, *, run_agent_commands: bool = False) -> Path:
    path = tmp_path / "fake-python"
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    agent_run_exec = (
        (
            'if [ "$1" = "-m" ] && [ "$2" = "spice" ] '
            '&& [ "$3" = "agent" ] && [ "$4" = "run" ] '
            '&& [ "$5" = "--" ]; then\n'
            "  shift 5\n"
            '  if [ "$2" = "-c" ] || [ "$2" = "-lc" ]; then\n'
            f"    export ZDOTDIR={shlex.quote(str(static_hook_dir))}\n"
            f"    export BASH_ENV={shlex.quote(str(static_hook_dir / shellhook.BASH_HOOK_NAME))}\n"
            "  fi\n"
            '  exec "$@"\n'
            "fi\n"
        )
        if run_agent_commands
        else ""
    )
    path.write_text(
        (
            "#!/bin/sh\n"
            "printf 'fake:%s:%s:%s\\n' "
            f'"${{{shellhook.ZDOTDIR_ENV}-unset}}" '
            f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
            '"$*" '
            f'>> "${{{SHELL_TRACE_ENV}}}"\n'
            f"{agent_run_exec}"
        ),
        encoding="utf-8",
    )
    path.chmod(0o755)
    return path


def _trace_lines(trace: Path, *, expected_prefix: str) -> list[str]:
    return _eventually(
        lambda: (
            trace.read_text(encoding="utf-8").splitlines() if trace.exists() else []
        ),
        contains=expected_prefix,
    )


def _completed_process_detail(
    completed: subprocess.CompletedProcess, trace: Path
) -> str:
    trace_text = trace.read_text(encoding="utf-8") if trace.exists() else "<missing>"
    return (
        f"returncode={completed.returncode}\n"
        f"stdout={completed.stdout!r}\n"
        f"stderr={completed.stderr!r}\n"
        f"trace={trace_text!r}"
    )


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


def test_runtime_environment_preserves_operator_path_with_worktree_venv(tmp_path):
    (tmp_path / ".venv" / "bin").mkdir(parents=True)
    base_env = {"PATH": "/operator/bin:/usr/bin"}
    runtime_env = shellhook.shell_steering_runtime_environment(
        base_env=base_env, repo_root=tmp_path
    )
    env = {**base_env, **runtime_env}
    assert env["PATH"] == "/operator/bin:/usr/bin"


def test_runtime_environment_leaves_path_untouched_without_a_venv(tmp_path):
    env = shellhook.shell_steering_runtime_environment(
        base_env={"PATH": "/usr/bin"}, repo_root=tmp_path
    )
    assert "PATH" not in env


def test_worktree_python_wrapper_targets_venv_interpreter(tmp_path):
    python = tmp_path / ".venv" / "bin" / "python"
    python.parent.mkdir(parents=True)
    python.write_text("#!/bin/sh\n", encoding="utf-8")

    assert shellhook.render_worktree_python_wrapper_lines(tmp_path) == [
        "",
        "python() {",
        f'  command {shlex.quote(str(python))} "$@"',
        "}",
        "",
        "python3() {",
        f'  command {shlex.quote(str(python))} "$@"',
        "}",
    ]
