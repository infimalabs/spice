"""Agent wrapper routing and shell steering contracts."""

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


def test_wrapper_plain_commands_scrub_shell_reexec_environment(tmp_path, monkeypatch):
    monkeypatch.setenv("ZDOTDIR", "hook")
    monkeypatch.setenv("BASH_ENV", "hook")
    monkeypatch.setenv(SHELL_TRACE_ENV, "preserved")

    env = wrap.build_agent_run_environment(["true"], repo_root=tmp_path)

    assert env is not None
    assert "ZDOTDIR" not in env
    assert "BASH_ENV" not in env
    assert env[SHELL_TRACE_ENV] == "preserved"


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
    assert env[shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV] == ""
    assert env[shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV] == ""
    zshenv = (hook_dir / ".zshenv").read_text(encoding="utf-8")
    assert "spice agent shell-hook zshenv" in zshenv
    assert "spice agent run --" not in zshenv
    assert "spice agent steer" not in zshenv
    assert "--watch --parent-pid" not in zshenv


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

    assert "agent-python-one -m spice agent run --" in first
    assert ". /real-zdotdir-one/.zshenv" in first
    assert "agent-python-two -m spice agent run --" in second
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
    assert "agent-python -m spice agent run --" in rendered
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
    assert "after:unset:unset" in lines
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

    subprocess.run([zsh, "-lc", "sleep 0.1"], check=True, env=env)

    lines = _trace_lines(trace, expected_prefix="real:")
    assert lines[0].startswith("fake:unset:unset:-m spice agent run --")
    assert lines[1:] == ["real:.zshenv", "real:.zprofile", "real:.zlogin"]


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
    assert f"after:{real_bash_env}" in lines
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

    completed = subprocess.run([zsh, "-c", command], check=False, env=env)

    assert completed.returncode == 7
    lines = _trace_lines(trace, expected_prefix="ran:")
    agent_run_lines = [line for line in lines if "-m spice agent run --" in line]
    assert len(agent_run_lines) == 1
    assert agent_run_lines[0].startswith("fake:unset:unset:")
    assert f" {zsh} -c " in agent_run_lines[0]
    assert "ran:unset:unset" in lines


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
    agent_run_lines = [line for line in lines if "-m spice agent run --" in line]
    assert len(agent_run_lines) == 1
    assert agent_run_lines[0].startswith("fake:unset:unset:")
    assert f" {bash} -c " in agent_run_lines[0]
    assert "ran:unset" in lines


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
    repo: Path, *, order: list[str] | None, groups: dict[str, dict[str, list[str]]]
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
        for wrapper, selectors in entries.items():
            lines.append(
                f"{wrapper} = ["
                + ", ".join(f'"{selector}"' for selector in selectors)
                + "]"
            )
    (repo / "pyproject.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")


def _expected_rtk_wrapper_lines(selectors: list[str]) -> list[str]:
    lines: list[str] = []
    for selector in selectors:
        lines.extend(["", f"{selector}() {{", f'  rtk {selector} "$@"', "}"])
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
