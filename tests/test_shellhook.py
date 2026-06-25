"""Agent wrapper routing and shell steering contracts."""

import io
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


def test_wrapper_git_route_inherits_ambient_supervisor_environment(tmp_path):
    env = wrap.build_agent_run_environment(["git", "status"], repo_root=tmp_path)
    source = "ambient" if env is None else "explicit"

    assert source == "ambient"


def test_wrapper_spice_routes_use_plain_worktree_env(tmp_path, monkeypatch):
    # The supervisor exports the git shadow once; the wrapper just inherits the
    # worktree env for spice routes instead of re-injecting per command.
    monkeypatch.setattr(
        wrap,
        "agent_run_child_worktree_environment",
        lambda args, *, repo_root=None, base_env=None: {
            "route": "worktree",
            "repo": str(repo_root),
            "ZDOTDIR": "hook",
            "BASH_ENV": "hook",
        },
    )

    expected = {
        "route": "worktree",
        "repo": str(tmp_path),
        "ZDOTDIR": "hook",
        "BASH_ENV": "hook",
    }
    spice_env = wrap.build_agent_run_environment(
        ["spice", "task", "status"], repo_root=tmp_path
    )
    uv_spice_env = wrap.build_agent_run_environment(
        ["uv", "run", "spice", "task", "status"], repo_root=tmp_path
    )

    assert spice_env == expected
    assert uv_spice_env == expected


def test_wrapper_routes_worktree_spice_commands_through_python_module(
    tmp_path, monkeypatch
):
    _write_spice_product_shape(tmp_path)

    spice_command = wrap.build_agent_run_command(
        ["spice", "task", "status"], repo_root=tmp_path
    )
    uv_spice_command = wrap.build_agent_run_command(
        ["uv", "run", "spice", "task", "status"], repo_root=tmp_path
    )

    assert spice_command == [sys.executable, "-m", "spice", "task", "status"]
    assert uv_spice_command == [sys.executable, "-m", "spice", "task", "status"]


def test_wrapper_rewrites_stage_one_shell_command_before_stage_two(monkeypatch):
    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str) -> str | None:
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

    def fake_rewrite(*args: str) -> str | None:
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

    def fake_rewrite(*args: str) -> str | None:
        calls.append(args)
        return "rtk grep needle"

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    command = wrap.build_agent_run_command(["rg", "needle"], rewrite_rtk=True)

    assert command == ["rtk", "grep", "needle"]
    assert calls == [("rg", "needle")]


def test_wrapper_does_not_special_case_proxy_argv(monkeypatch):
    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str) -> str | None:
        calls.append(args)
        return None

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    command = wrap.build_agent_run_command(["proxy", "git", "status"], rewrite_rtk=True)

    assert command == ["proxy", "git", "status"]
    assert calls == [("proxy", "git", "status")]


def test_wrapper_routes_worktree_python_commands_through_active_interpreter(
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


def test_wrapper_routes_target_repo_python_commands_through_virtualenv_default(
    tmp_path, monkeypatch
):
    venv_python = tmp_path / "active-env" / "bin" / "python"
    venv_python.parent.mkdir(parents=True)
    venv_python.write_text("#!/bin/sh\n", encoding="utf-8")
    venv_python.chmod(0o755)
    monkeypatch.setenv("VIRTUAL_ENV", str(tmp_path / "active-env"))

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

    assert wrap.build_agent_run_command(
        ["python", "--version"], repo_root=tmp_path
    ) == [
        str(venv_python),
        "--version",
    ]


def test_wrapper_refuses_target_repo_python_without_repo_venv(tmp_path, monkeypatch):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)

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


def test_wrapper_does_not_install_shell_hook_environment_for_direct_shell_commands(
    tmp_path, monkeypatch
):
    monkeypatch.delenv("ZDOTDIR", raising=False)
    monkeypatch.delenv("BASH_ENV", raising=False)

    env = wrap.build_agent_run_environment(["zsh", "-c", "true"], repo_root=tmp_path)

    assert env is None


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
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", lambda *args: None)
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    base_env = dict(os.environ)
    base_env["PATH"] = str(bin_dir) + os.pathsep + os.environ.get("PATH", "")
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


def test_agent_environment_inherits_worktree_spice_pythonpath(tmp_path, monkeypatch):
    _write_spice_product_shape(tmp_path)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)

    env = lifecycle.agent_environment(tmp_path)

    assert env["PYTHONPATH"].split(os.pathsep)[0] == str(tmp_path.resolve())


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
    assert env[shellhook.SHELL_HOOK_WRAPPERS_ENV] == ""
    assert env[shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV] == ""
    assert env[shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV] == ""
    zshenv = (hook_dir / ".zshenv").read_text(encoding="utf-8")
    assert "spice agent shell-hook" not in zshenv
    assert "spice agent run --" in zshenv
    assert "--preserve-shell-hook-env" not in zshenv
    assert shellhook.SHELL_HOOK_WRAPPERS_ENV in zshenv
    assert "spice agent steer" not in zshenv
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
    assert "spice agent shell-hook" not in zshenv
    assert "spice agent shell-hook" not in bashenv
    assert "spice agent run --" in zshenv
    assert "spice agent run --" in bashenv
    assert "--preserve-shell-hook-env" not in zshenv
    assert "--preserve-shell-hook-env" not in bashenv
    assert "spice agent steer" not in zshenv
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
    assert "spice agent shell-hook" not in first_zshenv
    assert "spice agent shell-hook" not in first_bashenv
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
        assert "spice agent shell-hook" not in text
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
        assert "spice agent shell-hook" not in static_text
        assert "spice agent run --" not in static_text
        assert shellhook.SHELL_HOOK_WRAPPERS_ENV in static_text
        assert shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV in static_text
        assert shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV in static_text

    package_data = tomllib.loads(Path("pyproject.toml").read_text(encoding="utf-8"))[
        "tool"
    ]["setuptools"]["package-data"]["spice.agent"]
    assert "shellhooks/.zshrc" in package_data
    assert "staticshellhooks/.zshrc" in package_data


def test_agent_wrapper_lines_adds_ordered_agent_wrapper_functions(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={"common": {"wrap": ["grep", "find", "git"]}},
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == _expected_wrapper_lines(
        "wrap", ["grep", "find", "git"]
    )


def test_agent_wrapper_lines_uses_builtin_common_default(tmp_path):
    assert shellhook.render_agent_wrapper_lines(tmp_path) == []


def test_agent_wrapper_lines_explicit_common_group_inherits_builtin_default(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={},
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == []


def test_agent_wrapper_lines_project_common_group_overrides_builtin_default(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=None,
        groups={"common": {"wrap": ["grep"]}},
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == _expected_wrapper_lines(
        "wrap", ["grep"]
    )


def test_agent_wrapper_lines_project_common_can_add_pytest_wrapper(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=None,
        groups={
            "common": {
                "wrap": ["run", "grep", "find", "git"],
                "pytest": {"argv": ["$SPICE_SHELL_HOOK_PYTHON", "-m", "pytest"]},
            }
        },
    )

    assert (
        shellhook.render_agent_wrapper_lines(tmp_path)
        == _expected_project_common_with_pytest_wrapper_lines()
    )


def test_agent_wrapper_lines_accepts_direct_argv_wrapper(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["tests"],
        groups={"tests": {"pytest": {"argv": ["python", "-m", "pytest"]}}},
    )

    assert shellhook.render_agent_wrapper_lines(
        tmp_path
    ) == _expected_python_module_wrapper_lines(["pytest"])


def test_spice_checkout_maps_bare_pre_commit_to_dev_gate():
    repo = Path(__file__).resolve().parents[1]
    lines = shellhook.render_agent_wrapper_lines(repo)

    wrapper_start = lines.index("pre-commit() {")
    assert lines[wrapper_start : wrapper_start + 3] == [
        "pre-commit() {",
        '  spice dev pre-commit "$@"',
        "}",
    ]


def test_agent_wrapper_lines_honors_empty_agent_wrapper_list(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=[],
        groups={"common": {"wrap": ["grep"]}},
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == []


def test_agent_wrapper_lines_fails_loudly_for_path_wrapper_selectors(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["shells"],
        groups={"shells": {"dash": ["/bin/sh", "sh"]}},
    )

    with pytest.raises(SpiceError, match="requires the redirector stage"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_fails_loudly_for_path_wrapper_commands(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["shells"],
        groups={"shells": {"pytest": {"argv": ["/bin/python", "-m", "pytest"]}}},
    )

    with pytest.raises(SpiceError, match="path wrapper command"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_fails_loudly_for_duplicate_wrapper_selectors(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["base", "shells"],
        groups={"base": {"wrap": ["sh"]}, "shells": {"dash": ["sh"]}},
    )

    with pytest.raises(SpiceError, match="configured by both"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_zshenv_hook_reexec_restores_for_nested_shells(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    home = tmp_path / "home"
    home.mkdir()
    trace = tmp_path / "trace.log"
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    (home / ".zshenv").write_text(
        (
            "print -r -- "
            f'"real-zshenv:${{{shellhook.ZDOTDIR_ENV}-unset}}" '
            f'>> "${{{SHELL_TRACE_ENV}}}"\n'
        ),
        encoding="utf-8",
    )
    base_env = {"HOME": str(home)}
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    command = (
        "sleep 0.1; "
        "printf 'after:%s:%s\\n' "
        f'"${{{shellhook.ZDOTDIR_ENV}-unset}}" '
        f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
        f'>> "${{{SHELL_TRACE_ENV}}}"; '
        f"{shutil.which('zsh') or zsh} -c 'true'"
    )
    env = {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", ""),
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(
            base_env=base_env, python_command=[str(fake_python)]
        ),
    }

    subprocess.run([zsh, "-c", command], check=True, env=env)

    lines = _trace_lines(trace, expected_prefix="after:")
    assert (
        f"after:{static_hook_dir}:{static_hook_dir / shellhook.BASH_HOOK_NAME}" in lines
    )
    assert lines.count("real-zshenv:unset") == 2


def test_zsh_login_hook_reexec_restores_across_startup_files(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    home = tmp_path / "home"
    home.mkdir()
    trace = tmp_path / "trace.log"
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    for name in shellhook.ZSH_HOOK_NAMES:
        (home / name).write_text(
            f"print -r -- 'real:{name}' >> \"${{{SHELL_TRACE_ENV}}}\"\n",
            encoding="utf-8",
        )
    base_env = {"HOME": str(home)}
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", ""),
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(
            base_env=base_env, python_command=[str(fake_python)]
        ),
    }

    subprocess.run([zsh, "-lc", "sleep 0.1"], check=True, env=env, timeout=2)

    lines = _trace_lines(trace, expected_prefix="real:")
    assert lines[0].startswith("fake:unset:unset:-m spice agent run --")
    assert lines[1:] == ["real:.zshenv", "real:.zprofile", "real:.zlogin"]


def test_zshrc_hook_sources_real_interactive_zshrc_and_loads_wrappers(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    _write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={"common": {"wrap": ["grep"]}},
    )
    home = tmp_path / "home"
    home.mkdir()
    trace = tmp_path / "trace.log"
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    wrap_bin = bin_dir / "wrap"
    wrap_bin.write_text(
        f'#!/bin/sh\nprintf \'wrap:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
        encoding="utf-8",
    )
    wrap_bin.chmod(0o755)
    (home / ".zshenv").write_text(
        f"print -r -- 'real:.zshenv' >> \"${{{SHELL_TRACE_ENV}}}\"\n",
        encoding="utf-8",
    )
    (home / ".zshrc").write_text(
        f"print -r -- 'real:.zshrc' >> \"${{{SHELL_TRACE_ENV}}}\"\n",
        encoding="utf-8",
    )
    base_env = {"HOME": str(home)}
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        "HOME": str(home),
        "PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", ""),
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        shellhook.SHELL_HOOK_WRAPPERS_ENV: "\n".join(
            shellhook.render_agent_wrapper_lines(tmp_path)
        ),
        **shellhook.shell_steering_runtime_environment(
            base_env=base_env,
            python_command=[str(fake_python)],
            repo_root=tmp_path,
        ),
    }

    completed = subprocess.run(
        [zsh, "-i"],
        input=(
            f'print -r -- "histfile:$HISTFILE" >> "${{{SHELL_TRACE_ENV}}}"\n'
            "grep needle /dev/null\n"
            "exit\n"
        ),
        text=True,
        capture_output=True,
        check=False,
        env=env,
        timeout=3,
    )

    assert completed.returncode == 0, _completed_process_detail(completed, trace)
    lines = _trace_lines(trace, expected_prefix="wrap:")
    assert lines.count("real:.zshenv") == 1
    assert lines.count("real:.zshrc") == 1
    assert f"histfile:{home / '.zsh_history'}" in lines
    assert "wrap:grep needle /dev/null" in lines
    assert not any(line.startswith("fake:") for line in lines)
    assert not (hook_dir / ".zsh_history").exists()


def test_zshrc_hook_interactive_shell_loads_bare_pre_commit_wrapper(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    _write_agent_wrapper_config(
        tmp_path,
        order=["repo-tools"],
        groups={"repo-tools": {"pre-commit": {"argv": ["spice", "dev", "pre-commit"]}}},
    )
    home = tmp_path / "home"
    home.mkdir()
    trace = tmp_path / "trace.log"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    spice = bin_dir / "spice"
    spice.write_text(
        f'#!/bin/sh\nprintf \'spice:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
        encoding="utf-8",
    )
    spice.chmod(0o755)
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        "HOME": str(home),
        "PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", ""),
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        shellhook.SHELL_HOOK_WRAPPERS_ENV: "\n".join(
            shellhook.render_agent_wrapper_lines(tmp_path)
        ),
        **shellhook.shell_steering_runtime_environment(
            base_env={"HOME": str(home)},
            repo_root=tmp_path,
        ),
    }

    completed = subprocess.run(
        [zsh, "-i"],
        input="pre-commit --all-files\nexit\n",
        text=True,
        capture_output=True,
        check=False,
        env=env,
        timeout=3,
    )

    assert completed.returncode == 0, _completed_process_detail(completed, trace)
    lines = _trace_lines(trace, expected_prefix="spice:")
    assert "spice:dev pre-commit --all-files" in lines


def test_zsh_login_hook_reexec_does_not_loop_when_active_zdotdir_is_hook(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    home = tmp_path / "home"
    home.mkdir()
    trace = tmp_path / "trace.log"
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", ""),
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(
            base_env={shellhook.ZDOTDIR_ENV: str(hook_dir)},
            python_command=[str(fake_python)],
        ),
    }

    subprocess.run(
        [zsh, "-lc", f"printf 'ran\\n' >> \"${{{SHELL_TRACE_ENV}}}\""],
        check=True,
        env=env,
        timeout=2,
    )

    lines = _trace_lines(trace, expected_prefix="ran")
    agent_run_lines = [line for line in lines if "-m spice agent run --" in line]
    assert len(agent_run_lines) == 1
    assert lines[-1] == "ran"


def test_bash_env_hook_reexec_restores_for_nested_shells(tmp_path):
    bash = shutil.which("bash")
    if bash is None:
        pytest.skip("bash is not installed")
    home = tmp_path / "home"
    home.mkdir()
    trace = tmp_path / "trace.log"
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    real_bash_env = tmp_path / "real-bash-env"
    real_bash_env.write_text(
        (
            "printf 'real-bash:%s\\n' "
            f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
            f'>> "${{{SHELL_TRACE_ENV}}}"\n'
        ),
        encoding="utf-8",
    )
    base_env = {"HOME": str(home), shellhook.BASH_ENV_ENV: str(real_bash_env)}
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    command = (
        "sleep 0.1; "
        "printf 'after:%s\\n' "
        f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
        f'>> "${{{SHELL_TRACE_ENV}}}"; '
        f"{bash} -c 'true'"
    )
    env = {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", ""),
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(
            base_env=base_env, python_command=[str(fake_python)]
        ),
    }

    subprocess.run([bash, "-c", command], check=True, env=env)

    lines = _trace_lines(trace, expected_prefix="after:")
    assert f"after:{static_hook_dir / shellhook.BASH_HOOK_NAME}" in lines
    assert lines.count(f"real-bash:{real_bash_env}") == 2


def test_zshenv_hook_execs_noninteractive_command_under_agent_run_once(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    trace = tmp_path / "trace.log"
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    base_env = {}
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    command = (
        "printf 'ran:%s:%s\\n' "
        f'"${{{shellhook.ZDOTDIR_ENV}-unset}}" '
        f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
        f'>> "${{{SHELL_TRACE_ENV}}}"; '
        "exit 7"
    )
    env = {
        "PATH": os.environ.get("PATH", ""),
        "SHELL": zsh,
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(
            base_env=base_env, python_command=[str(fake_python)]
        ),
    }

    completed = subprocess.run([zsh, "-c", command], check=False, env=env, timeout=2)

    assert completed.returncode == 7
    lines = _trace_lines(trace, expected_prefix="ran:")
    agent_run_lines = [line for line in lines if "-m spice agent run --" in line]
    assert len(agent_run_lines) == 1
    assert agent_run_lines[0].startswith("fake:unset:unset:")
    assert f" {zsh} -c " in agent_run_lines[0]
    assert (
        f"ran:{static_hook_dir}:{static_hook_dir / shellhook.BASH_HOOK_NAME}" in lines
    )


def test_agent_shell_environment_routes_reexeced_shell_to_static_stage(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    trace = tmp_path / "trace.log"
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    base_env = {
        "PATH": os.environ.get("PATH", ""),
        "SHELL": zsh,
        SHELL_TRACE_ENV: str(trace),
    }
    env = shellhook.apply_shell_steering_environment(
        tmp_path,
        base_env=base_env,
    )
    env[shellhook.SHELL_HOOK_PYTHON_ENV] = str(fake_python)
    command = (
        "printf 'ran:%s:%s\\n' "
        f'"${{{shellhook.ZDOTDIR_ENV}-unset}}" '
        f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
        f'>> "${{{SHELL_TRACE_ENV}}}"; '
        "exit 7"
    )

    completed = subprocess.run([zsh, "-c", command], check=False, env=env, timeout=2)

    assert completed.returncode == 7
    lines = _trace_lines(trace, expected_prefix="ran:")
    agent_run_lines = [line for line in lines if "-m spice agent run --" in line]
    assert len(agent_run_lines) == 1
    assert agent_run_lines[0].startswith("fake:unset:unset:")
    assert f" {zsh} -c " in agent_run_lines[0]
    assert (
        f"ran:{static_hook_dir}:{static_hook_dir / shellhook.BASH_HOOK_NAME}" in lines
    )


def test_zshenv_hook_loads_wrapper_functions_after_agent_run_reexec(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    _write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={"common": {"wrap": ["grep"]}},
    )
    trace = tmp_path / "trace.log"
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    wrap_bin = bin_dir / "wrap"
    wrap_bin.write_text(
        (f'#!/bin/sh\nprintf \'wrap:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n'),
        encoding="utf-8",
    )
    wrap_bin.chmod(0o755)
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        "PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", ""),
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        shellhook.SHELL_HOOK_WRAPPERS_ENV: "\n".join(
            shellhook.render_agent_wrapper_lines(tmp_path)
        ),
        **shellhook.shell_steering_runtime_environment(
            base_env={},
            python_command=[str(fake_python)],
            repo_root=tmp_path,
        ),
    }

    subprocess.run([zsh, "-c", "grep needle /dev/null"], check=True, env=env)

    lines = _trace_lines(trace, expected_prefix="wrap:")
    assert "wrap:grep needle /dev/null" in lines


def test_bash_env_hook_execs_noninteractive_command_under_agent_run_once(tmp_path):
    bash = shutil.which("bash")
    if bash is None:
        pytest.skip("bash is not installed")
    trace = tmp_path / "trace.log"
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    base_env = {}
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    command = (
        "printf 'ran:%s\\n' "
        f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
        f'>> "${{{SHELL_TRACE_ENV}}}"; '
        "exit 6"
    )
    env = {
        "PATH": os.environ.get("PATH", ""),
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(
            base_env=base_env, python_command=[str(fake_python)]
        ),
    }

    completed = subprocess.run([bash, "-c", command], check=False, env=env)

    assert completed.returncode == 6
    lines = _trace_lines(trace, expected_prefix="ran:")
    agent_run_lines = [line for line in lines if "-m spice agent run --" in line]
    assert len(agent_run_lines) == 1
    assert agent_run_lines[0].startswith("fake:unset:unset:")
    assert f" {bash} -c " in agent_run_lines[0]
    assert f"ran:{static_hook_dir / shellhook.BASH_HOOK_NAME}" in lines


def test_bash_env_hook_fails_noninteractive_shell_without_execution_string(tmp_path):
    bash = shutil.which("bash")
    if bash is None:
        pytest.skip("bash is not installed")
    base_env = {}
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    script = tmp_path / "script.sh"
    script.write_text("exit 0\n", encoding="utf-8")
    env = {
        "PATH": os.environ.get("PATH", ""),
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        **shellhook.shell_steering_runtime_environment(base_env=base_env),
    }

    completed = subprocess.run(
        [bash, str(script)], capture_output=True, check=False, env=env, text=True
    )

    assert completed.returncode == SHELL_HOOK_FAILURE_EXIT_CODE
    assert "cannot agent-run reexec noninteractive shell" in completed.stderr


def _write_spice_product_shape(repo: Path) -> None:
    for relative in (
        Path("spice") / "__main__.py",
        Path("spice") / "cli" / "entry.py",
        Path("spice") / "agent" / "wrap.py",
    ):
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("# test spice product shape\n", encoding="utf-8")


def _write_agent_wrapper_config(
    repo: Path, *, order: list[str] | None, groups: dict[str, dict[str, object]]
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
    for group_name, entries in groups.items():
        lines.extend(["", f"[tool.spice.wrappers.{group_name}]"])
        for wrapper, value in entries.items():
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
