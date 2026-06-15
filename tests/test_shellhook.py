"""Agent wrapper routing and shell steering contracts."""

import io
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

from spice.agent import cli as agent_cli
from spice.agent import driver as agent_driver
from spice.agent import lifecycle, shellhook, wrap
from spice.agent.driver import CLAUDE_DRIVER, DRIVER
from spice.errors import SpiceError

SHELL_TRACE_ENV = "SPICE_TEST_TRACE"  # env-policy: allow
SHELL_HOOK_FAILURE_EXIT_CODE = 127


def test_wrapper_gitshadow_route_uses_shadow_and_spice_route_scrubs(
    tmp_path, monkeypatch
):
    shadow_calls: list[object] = []
    scrub_calls: list[object] = []
    monkeypatch.setattr(
        wrap,
        "agent_git_shadow_environment",
        lambda repo_root, *, base_env=None: (
            shadow_calls.append(repo_root)
            or {
                "route": "git",
                "repo": str(repo_root),
                "ZDOTDIR": "hook",
                "BASH_ENV": "hook",
            }
        ),
    )
    monkeypatch.setattr(
        wrap,
        "scrub_agent_git_shadow_environment",
        lambda env: (
            scrub_calls.append(env)
            or {"route": "spice", "ZDOTDIR": "hook", "BASH_ENV": "hook"}
        ),
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


def test_wrapper_proxy_marker_drops_after_worktree_python_routing(tmp_path):
    _write_spice_product_shape(tmp_path)

    assert wrap.build_agent_run_command(
        ["python", "-m", "pip", "--version"], repo_root=tmp_path
    ) == [sys.executable, "-m", "pip", "--version"]
    assert wrap.build_agent_run_command(
        ["proxy", "python", "-m", "pip", "--version"], repo_root=tmp_path
    ) == [sys.executable, "-m", "pip", "--version"]


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


def test_wrapper_non_shell_commands_scrub_shell_reexec_environment(
    tmp_path, monkeypatch
):
    monkeypatch.setenv("ZDOTDIR", "hook")
    monkeypatch.setenv("BASH_ENV", "hook")
    monkeypatch.setenv(shellhook.SHELL_HOOK_REEXEC_STAGE_ENV, "1")
    monkeypatch.setenv(SHELL_TRACE_ENV, "preserved")

    env = wrap.build_agent_run_environment(["true"], repo_root=tmp_path)

    assert env is not None
    assert "ZDOTDIR" not in env
    assert "BASH_ENV" not in env
    assert shellhook.SHELL_HOOK_REEXEC_STAGE_ENV not in env
    assert env[SHELL_TRACE_ENV] == "preserved"


def test_wrapper_preserves_shell_hook_environment_for_explicit_reexec_stage(
    tmp_path, monkeypatch
):
    _write_spice_product_shape(tmp_path)
    monkeypatch.setenv("ZDOTDIR", "hook")
    monkeypatch.setenv("BASH_ENV", "hook")
    monkeypatch.setenv(shellhook.SHELL_HOOK_REEXEC_STAGE_ENV, "1")

    env = wrap.build_agent_run_environment(
        ["zsh", "-c", "true"],
        repo_root=tmp_path,
        preserve_shell_hook_env=True,
    )

    assert env is not None
    assert env["ZDOTDIR"] == "hook"
    assert env["BASH_ENV"] == "hook"
    assert env[shellhook.SHELL_HOOK_REEXEC_STAGE_ENV] == "1"


def test_wrapper_installs_shell_hook_environment_for_child_shell_commands(
    tmp_path, monkeypatch
):
    monkeypatch.delenv("ZDOTDIR", raising=False)
    monkeypatch.delenv("BASH_ENV", raising=False)
    monkeypatch.setenv(shellhook.SHELL_HOOK_REEXEC_STAGE_ENV, "1")

    env = wrap.build_agent_run_environment(["zsh", "-c", "true"], repo_root=tmp_path)

    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    assert env is not None
    assert env["ZDOTDIR"] == str(hook_dir)
    assert env["BASH_ENV"] == str(hook_dir / shellhook.BASH_HOOK_NAME)
    assert env[shellhook.SHELL_HOOK_REEXEC_STAGE_ENV] == "1"
    assert env[shellhook.SHELL_HOOK_REPO_ROOT_ENV] == str(tmp_path.resolve())


def test_wrapper_redirects_zsh_compdump_outside_shellhooks_dir(tmp_path, monkeypatch):
    monkeypatch.delenv("ZDOTDIR", raising=False)
    monkeypatch.delenv("BASH_ENV", raising=False)
    monkeypatch.delenv("ZSH_COMPDUMP", raising=False)
    monkeypatch.setenv(shellhook.SHELL_HOOK_REEXEC_STAGE_ENV, "1")
    monkeypatch.setenv("HOME", str(tmp_path))

    env = wrap.build_agent_run_environment(["zsh", "-c", "true"], repo_root=tmp_path)

    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    assert env is not None
    assert env["ZSH_COMPDUMP"] == str(tmp_path / ".zcompdump")
    assert not env["ZSH_COMPDUMP"].startswith(str(hook_dir))


def test_wrapper_redirects_zsh_compdump_to_original_zdotdir_when_set(
    tmp_path, monkeypatch
):
    zdotdir = tmp_path / "zdotdir"
    monkeypatch.setenv("ZDOTDIR", str(zdotdir))
    monkeypatch.delenv("BASH_ENV", raising=False)
    monkeypatch.delenv("ZSH_COMPDUMP", raising=False)
    monkeypatch.setenv(shellhook.SHELL_HOOK_REEXEC_STAGE_ENV, "1")

    env = wrap.build_agent_run_environment(["zsh", "-c", "true"], repo_root=tmp_path)

    assert env is not None
    assert env["ZSH_COMPDUMP"] == str(zdotdir / ".zcompdump")


def test_wrapper_preserves_caller_zsh_compdump_when_already_set(tmp_path, monkeypatch):
    custom_dump = str(tmp_path / "custom" / ".zcompdump")
    monkeypatch.delenv("ZDOTDIR", raising=False)
    monkeypatch.delenv("BASH_ENV", raising=False)
    monkeypatch.setenv("ZSH_COMPDUMP", custom_dump)
    monkeypatch.setenv(shellhook.SHELL_HOOK_REEXEC_STAGE_ENV, "1")

    env = wrap.build_agent_run_environment(["zsh", "-c", "true"], repo_root=tmp_path)

    assert env is not None
    assert env["ZSH_COMPDUMP"] == custom_dump


def test_agent_run_shell_command_loads_wrappers_without_manual_hook_env(
    tmp_path, monkeypatch
):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    trace = tmp_path / "trace.log"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    rtk = bin_dir / "rtk"
    rtk.write_text(
        f'#!/bin/sh\nprintf \'rtk:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
        encoding="utf-8",
    )
    rtk.chmod(0o755)
    monkeypatch.delenv(shellhook.ZDOTDIR_ENV, raising=False)
    monkeypatch.delenv(shellhook.BASH_ENV_ENV, raising=False)
    monkeypatch.delenv(shellhook.SHELL_HOOK_REEXEC_STAGE_ENV, raising=False)
    monkeypatch.setenv("PATH", str(bin_dir) + os.pathsep + os.environ.get("PATH", ""))
    monkeypatch.setenv(SHELL_TRACE_ENV, str(trace))

    exit_code = wrap.run_agent_command(
        tmp_path,
        [zsh, "-c", "grep needle /dev/null"],
        stderr=io.StringIO(),
    )

    assert exit_code == 0
    lines = _trace_lines(trace, expected_prefix="rtk:")
    assert "rtk:grep needle /dev/null" in lines


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
        _expected_rtk_wrapper_lines(["run", "proxy", "grep", "find", "git"])
    )
    assert env[shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV] == ""
    assert env[shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV] == ""
    zshenv = (hook_dir / ".zshenv").read_text(encoding="utf-8")
    assert "spice agent shell-hook zshenv" in zshenv
    assert "spice agent run --" not in zshenv
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
                "rtk": ["grep", "git"],
                "pytest": {"command": ["$SPICE_SHELL_HOOK_PYTHON", "-m", "pytest"]},
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
            *_expected_rtk_wrapper_lines(["grep", "git"]),
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
    monkeypatch.setenv(shellhook.ZDOTDIR_ENV, str(real_zdotdir))
    monkeypatch.setenv(shellhook.BASH_ENV_ENV, str(real_bash_env))

    env = lifecycle.agent_environment(tmp_path)

    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    assert env[shellhook.ZDOTDIR_ENV] == str(hook_dir)
    assert env[shellhook.BASH_ENV_ENV] == str(hook_dir / shellhook.BASH_HOOK_NAME)
    assert env[shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV] == str(real_zdotdir)
    assert env[shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV] == str(real_bash_env)
    zshenv = (hook_dir / ".zshenv").read_text(encoding="utf-8")
    bashenv = (hook_dir / shellhook.BASH_HOOK_NAME).read_text(encoding="utf-8")
    assert "spice agent shell-hook zshenv" in zshenv
    assert "spice agent shell-hook bash_env" in bashenv
    assert "spice agent run --" not in zshenv
    assert "spice agent steer" not in zshenv
    assert "--watch --parent-pid" not in zshenv
    rendered_zshenv = shellhook.render_shell_steering_hook_for_surface(
        "zshenv", env=env
    )
    rendered_bashenv = shellhook.render_shell_steering_hook_for_surface(
        shellhook.BASH_HOOK_NAME, env=env
    )
    assert f"export {shellhook.ZDOTDIR_ENV}={real_zdotdir}" in rendered_zshenv
    assert f". {real_zdotdir / '.zshenv'}" in rendered_zshenv
    assert f"export {shellhook.BASH_ENV_ENV}={real_bash_env}" in rendered_bashenv
    assert f". {real_bash_env}" in rendered_bashenv


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


def test_shell_steering_files_are_stable_across_original_env_changes():
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    first_zshenv = (hook_dir / ".zshenv").read_text(encoding="utf-8")
    first_bashenv = (hook_dir / shellhook.BASH_HOOK_NAME).read_text(encoding="utf-8")

    assert (hook_dir / ".zshenv").read_text(encoding="utf-8") == first_zshenv
    assert (hook_dir / shellhook.BASH_HOOK_NAME).read_text(
        encoding="utf-8"
    ) == first_bashenv
    assert "spice agent shell-hook zshenv" in first_zshenv
    assert "spice agent shell-hook bash_env" in first_bashenv
    assert "spice agent run --" not in first_zshenv
    assert shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV not in first_bashenv
    assert shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV not in first_bashenv


def test_shell_hook_renderer_responds_to_runtime_environment_changes():
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    first_env = {
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        **shellhook.shell_steering_runtime_environment(
            base_env={
                shellhook.ZDOTDIR_ENV: "/real-zdotdir-one",
                shellhook.BASH_ENV_ENV: "/real-bash-one",
            },
            python_command=["agent-python-one"],
        ),
    }
    second_env = {
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        **shellhook.shell_steering_runtime_environment(
            base_env={
                shellhook.ZDOTDIR_ENV: "/real-zdotdir-two",
                shellhook.BASH_ENV_ENV: "/real-bash-two",
            },
            python_command=["agent-python-two"],
        ),
    }

    first = shellhook.render_shell_steering_hook_for_surface("zshenv", env=first_env)
    second = shellhook.render_shell_steering_hook_for_surface("zshenv", env=second_env)

    assert "agent-python-one -m spice agent run --preserve-shell-hook-env --" in first
    assert ". /real-zdotdir-one/.zshenv" in first
    assert "agent-python-two -m spice agent run --preserve-shell-hook-env --" in second
    assert ". /real-zdotdir-two/.zshenv" in second


def test_shell_hook_renderer_adds_ordered_agent_wrapper_functions(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={"common": {"rtk": ["grep", "find", "git"]}},
    )
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        **shellhook.shell_steering_runtime_environment(
            base_env={},
            python_command=["agent-python"],
            repo_root=tmp_path,
        ),
    }

    rendered = shellhook.render_shell_steering_hook_for_surface("zshenv", env=env)

    assert shellhook.render_agent_wrapper_lines(
        tmp_path
    ) == _expected_rtk_wrapper_lines(["grep", "find", "git"])
    assert '\ngrep() {\n  rtk grep "$@"\n}\n' in rendered
    assert '\nfind() {\n  rtk find "$@"\n}\n' in rendered
    assert '\ngit() {\n  rtk git "$@"\n}\n' in rendered


def test_shell_hook_renderer_uses_builtin_common_agent_wrapper_default(tmp_path):
    assert shellhook.render_agent_wrapper_lines(
        tmp_path
    ) == _expected_rtk_wrapper_lines(["run", "proxy", "grep", "find", "git"])


def test_shell_hook_renderer_explicit_common_group_inherits_builtin_default(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={},
    )
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        **shellhook.shell_steering_runtime_environment(
            base_env={},
            python_command=["agent-python"],
            repo_root=tmp_path,
        ),
    }

    rendered = shellhook.render_shell_steering_hook_for_surface("zshenv", env=env)

    assert shellhook.render_agent_wrapper_lines(
        tmp_path
    ) == _expected_rtk_wrapper_lines(["run", "proxy", "grep", "find", "git"])
    assert '\nrun() {\n  rtk run "$@"\n}\n' in rendered
    assert '\nproxy() {\n  rtk proxy "$@"\n}\n' in rendered
    assert '\ngrep() {\n  rtk grep "$@"\n}\n' in rendered


def test_shell_hook_renderer_project_common_group_overrides_builtin_default(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=None,
        groups={"common": {"rtk": ["grep"]}},
    )

    assert shellhook.render_agent_wrapper_lines(
        tmp_path
    ) == _expected_rtk_wrapper_lines(["grep"])


def test_shell_hook_renderer_project_common_can_add_pytest_wrapper(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=None,
        groups={
            "common": {
                "rtk": ["run", "proxy", "grep", "find", "git"],
                "pytest": {"command": ["$SPICE_SHELL_HOOK_PYTHON", "-m", "pytest"]},
            }
        },
    )

    assert (
        shellhook.render_agent_wrapper_lines(tmp_path)
        == _expected_project_common_with_pytest_wrapper_lines()
    )


def test_shell_hook_renderer_accepts_direct_command_wrapper(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["tests"],
        groups={"tests": {"pytest": {"command": ["python", "-m", "pytest"]}}},
    )

    assert shellhook.render_agent_wrapper_lines(
        tmp_path
    ) == _expected_python_module_wrapper_lines(["pytest"])


def test_shell_hook_renderer_honors_empty_agent_wrapper_list(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=[],
        groups={"common": {"rtk": ["grep"]}},
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == []


def test_shell_hook_renderer_fails_loudly_for_path_wrapper_selectors(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["shells"],
        groups={"shells": {"dash": ["/bin/sh", "sh"]}},
    )
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        **shellhook.shell_steering_runtime_environment(
            base_env={},
            python_command=["agent-python"],
            repo_root=tmp_path,
        ),
    }

    with pytest.raises(SpiceError, match="requires the redirector stage"):
        shellhook.render_shell_steering_hook_for_surface("zshenv", env=env)


def test_shell_hook_renderer_fails_loudly_for_path_wrapper_commands(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["shells"],
        groups={"shells": {"pytest": {"command": ["/bin/python", "-m", "pytest"]}}},
    )
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        **shellhook.shell_steering_runtime_environment(
            base_env={},
            python_command=["agent-python"],
            repo_root=tmp_path,
        ),
    }

    with pytest.raises(SpiceError, match="path wrapper command"):
        shellhook.render_shell_steering_hook_for_surface("zshenv", env=env)


def test_shell_hook_renderer_fails_loudly_for_duplicate_wrapper_selectors(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["base", "shells"],
        groups={"base": {"rtk": ["sh"]}, "shells": {"dash": ["sh"]}},
    )
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        **shellhook.shell_steering_runtime_environment(
            base_env={},
            python_command=["agent-python"],
            repo_root=tmp_path,
        ),
    }

    with pytest.raises(SpiceError, match="configured by both"):
        shellhook.render_shell_steering_hook_for_surface("zshenv", env=env)


def test_agent_shell_hook_cli_renders_dynamic_surface(monkeypatch, capsys):
    runtime_env = shellhook.shell_steering_runtime_environment(
        base_env={}, python_command=["agent-python"]
    )
    for name, value in runtime_env.items():
        monkeypatch.setenv(name, value)
    monkeypatch.setenv(shellhook.BASH_ENV_ENV, "/tmp/spice-hook/bash_env")

    result = agent_cli.handle_agent(
        SimpleNamespace(agent_action="shell-hook", surface=shellhook.BASH_HOOK_NAME)
    )

    assert result == 0
    rendered = capsys.readouterr().out
    assert "agent-python -m spice agent run --preserve-shell-hook-env --" in rendered
    assert "cannot agent-run reexec noninteractive shell" in rendered


def test_agent_shell_hook_cli_fails_loudly_without_runtime_state(monkeypatch):
    for name in (
        shellhook.SHELL_HOOK_PYTHON_ENV,
        shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV,
        shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV,
    ):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv(shellhook.BASH_ENV_ENV, "/tmp/spice-hook/bash_env")

    with pytest.raises(SpiceError, match="missing required"):
        agent_cli.handle_agent(
            SimpleNamespace(agent_action="shell-hook", surface=shellhook.BASH_HOOK_NAME)
        )


def test_shell_hook_renderer_rejects_unsupported_surface():
    with pytest.raises(SpiceError, match="unsupported shell-hook surface"):
        shellhook.render_shell_steering_hook_for_surface("fish", env={})


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
    assert f"after:{hook_dir}:unset" in lines
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
    assert lines[0].startswith(
        f"fake:{hook_dir}:unset:-m spice agent run --preserve-shell-hook-env --"
    )
    assert lines[1:] == ["real:.zshenv", "real:.zprofile", "real:.zlogin"]


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
    agent_run_lines = [
        line
        for line in lines
        if "-m spice agent run --preserve-shell-hook-env --" in line
    ]
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
    assert f"after:{hook_dir / shellhook.BASH_HOOK_NAME}" in lines
    assert lines.count(f"real-bash:{real_bash_env}") == 2


def test_zshenv_hook_execs_noninteractive_command_under_agent_run_once(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    trace = tmp_path / "trace.log"
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    base_env = {}
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
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
    agent_run_lines = [
        line
        for line in lines
        if "-m spice agent run --preserve-shell-hook-env --" in line
    ]
    assert len(agent_run_lines) == 1
    assert agent_run_lines[0].startswith(f"fake:{hook_dir}:unset:")
    assert f" {zsh} -c " in agent_run_lines[0]
    assert f"ran:{hook_dir}:unset" in lines


def test_zshenv_hook_loads_wrapper_functions_after_agent_run_reexec(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    trace = tmp_path / "trace.log"
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    rtk = bin_dir / "rtk"
    rtk.write_text(
        (f'#!/bin/sh\nprintf \'rtk:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n'),
        encoding="utf-8",
    )
    rtk.chmod(0o755)
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        "PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", ""),
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(
            base_env={},
            python_command=[str(fake_python)],
            repo_root=tmp_path,
        ),
    }

    subprocess.run([zsh, "-c", "grep needle /dev/null"], check=True, env=env)

    lines = _trace_lines(trace, expected_prefix="rtk:")
    assert "rtk:grep needle /dev/null" in lines


def test_bash_env_hook_execs_noninteractive_command_under_agent_run_once(tmp_path):
    bash = shutil.which("bash")
    if bash is None:
        pytest.skip("bash is not installed")
    trace = tmp_path / "trace.log"
    fake_python = _fake_spice_python(tmp_path, run_agent_commands=True)
    base_env = {}
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
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
    agent_run_lines = [
        line
        for line in lines
        if "-m spice agent run --preserve-shell-hook-env --" in line
    ]
    assert len(agent_run_lines) == 1
    assert agent_run_lines[0].startswith(
        f"fake:unset:{hook_dir / shellhook.BASH_HOOK_NAME}:"
    )
    assert f" {bash} -c " in agent_run_lines[0]
    assert f"ran:{hook_dir / shellhook.BASH_HOOK_NAME}" in lines


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
                command = value["command"]
                lines.append(
                    f"{_toml_key(wrapper)} = {{ command = ["
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
        *_expected_rtk_wrapper_lines(["run", "proxy", "grep", "find", "git"]),
        *_expected_active_python_module_wrapper_lines(["pytest"]),
    ]


def _expected_rtk_wrapper_lines(selectors: list[str]) -> list[str]:
    lines: list[str] = []
    for selector in selectors:
        lines.extend(["", f"{selector}() {{", f'  rtk {selector} "$@"', "}"])
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
    shell_hook_exec = (
        'if [ "$1" = "-m" ] && [ "$2" = "spice" ] '
        '&& [ "$3" = "agent" ] && [ "$4" = "shell-hook" ]; then\n'
        f'  exec {shellhook.shell_quote(sys.executable)} "$@"\n'
        "fi\n"
    )
    agent_run_exec = (
        (
            'if [ "$1" = "-m" ] && [ "$2" = "spice" ] '
            '&& [ "$3" = "agent" ] && [ "$4" = "run" ] '
            '&& [ "$5" = "--preserve-shell-hook-env" ] && [ "$6" = "--" ]; then\n'
            "  shift 6\n"
            '  exec "$@"\n'
            "fi\n"
            'if [ "$1" = "-m" ] && [ "$2" = "spice" ] '
            '&& [ "$3" = "agent" ] && [ "$4" = "run" ] '
            '&& [ "$5" = "--" ]; then\n'
            "  shift 5\n"
            '  exec "$@"\n'
            "fi\n"
        )
        if run_agent_commands
        else ""
    )
    path.write_text(
        (
            "#!/bin/sh\n"
            f"{shell_hook_exec}"
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
