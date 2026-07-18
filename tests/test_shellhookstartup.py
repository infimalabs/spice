"""Shell startup-file reexec behavior for the steering hooks."""

import os
from pathlib import Path
import shutil
import subprocess

import pytest

from spice.agent import shellhook
from tests.test_shellhookhelpers import (
    SHELL_TRACE_ENV,
    completed_process_detail,
    fake_spice_executable,
    trace_lines,
    write_agent_wrapper_config,
)
from tests.test_shellhook import SHELL_HOOK_FAILURE_EXIT_CODE

INTERACTIVE_ZSH_TIMEOUT_SECONDS = 15


def _run_interactive_zsh(
    zsh: str, *, input_text: str, env: dict[str, str], trace: Path
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            [zsh, "-i"],
            input=input_text,
            text=True,
            capture_output=True,
            check=False,
            env=env,
            start_new_session=True,
            timeout=INTERACTIVE_ZSH_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as error:
        trace_text = (
            trace.read_text(encoding="utf-8") if trace.exists() else "<missing>"
        )
        pytest.fail(
            f"detached interactive zsh exceeded {error.timeout}s\n"
            f"stdout={error.stdout!r}\n"
            f"stderr={error.stderr!r}\n"
            f"trace={trace_text!r}",
            pytrace=False,
        )


def test_zshenv_hook_reexec_restores_for_nested_shells(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    home = tmp_path / "home"
    home.mkdir()
    trace = tmp_path / "trace.log"
    fake_spice = fake_spice_executable(tmp_path, run_agent_commands=True)
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
        "PATH": str(fake_spice.parent)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(base_env=base_env),
    }

    subprocess.run([zsh, "-c", command], check=True, env=env)

    lines = trace_lines(trace, expected_prefix="after:")
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
    fake_spice = fake_spice_executable(tmp_path, run_agent_commands=True)
    for name in shellhook.ZSH_HOOK_NAMES:
        (home / name).write_text(
            f"print -r -- 'real:{name}' >> \"${{{SHELL_TRACE_ENV}}}\"\n",
            encoding="utf-8",
        )
    base_env = {"HOME": str(home)}
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        "HOME": str(home),
        "PATH": str(fake_spice.parent)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(base_env=base_env),
    }

    # Edge-triggered on the login shell's own exit: the reexec `exec`s the real
    # shell, so when the process returns the startup-file trace is complete.
    # No timeout deadline (load-sensitive) and no in-shell sleep are needed.
    subprocess.run([zsh, "-lc", ":"], check=True, env=env)

    lines = trace_lines(trace, expected_prefix="real:")
    assert lines[0].startswith("fake:unset:unset:agent run --")
    assert lines[1:] == ["real:.zshenv", "real:.zprofile", "real:.zlogin"]


def test_zshrc_hook_sources_real_interactive_zshrc_and_loads_wrappers(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={"common": {"wrap": ["grep"]}},
    )
    home = tmp_path / "home"
    home.mkdir()
    trace = tmp_path / "trace.log"
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
        "PATH": str(bin_dir)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        shellhook.SHELL_HOOK_WRAPPERS_ENV: "\n".join(
            shellhook.render_agent_wrapper_lines(tmp_path)
        ),
        **shellhook.shell_steering_runtime_environment(
            base_env=base_env,
            repo_root=tmp_path,
        ),
    }

    completed = _run_interactive_zsh(
        zsh,
        input_text=(
            f'print -r -- "histfile:$HISTFILE" >> "${{{SHELL_TRACE_ENV}}}"\n'
            "grep needle /dev/null\n"
            "exit\n"
        ),
        env=env,
        trace=trace,
    )

    assert completed.returncode == 0, completed_process_detail(completed, trace)
    lines = trace_lines(trace, expected_prefix="wrap:")
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
    write_agent_wrapper_config(
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
        "PATH": str(bin_dir)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
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

    completed = _run_interactive_zsh(
        zsh,
        input_text="pre-commit --all-files\nexit\n",
        env=env,
        trace=trace,
    )

    assert completed.returncode == 0, completed_process_detail(completed, trace)
    lines = trace_lines(trace, expected_prefix="spice:")
    assert "spice:dev pre-commit --all-files" in lines


def test_zsh_login_hook_reexec_does_not_loop_when_active_zdotdir_is_hook(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    home = tmp_path / "home"
    home.mkdir()
    trace = tmp_path / "trace.log"
    fake_spice = fake_spice_executable(tmp_path, run_agent_commands=True)
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = {
        "HOME": str(home),
        "PATH": str(fake_spice.parent)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(
            base_env={shellhook.ZDOTDIR_ENV: str(hook_dir)},
        ),
    }

    subprocess.run(
        [zsh, "-lc", f"printf 'ran\\n' >> \"${{{SHELL_TRACE_ENV}}}\""],
        check=True,
        env=env,
        timeout=2,
    )

    lines = trace_lines(trace, expected_prefix="ran")
    agent_run_lines = [line for line in lines if "agent run --" in line]
    assert len(agent_run_lines) == 1
    assert lines[-1] == "ran"


def test_bash_env_hook_reexec_restores_for_nested_shells(tmp_path):
    bash = shutil.which("bash")
    if bash is None:
        pytest.skip("bash is not installed")
    home = tmp_path / "home"
    home.mkdir()
    trace = tmp_path / "trace.log"
    fake_spice = fake_spice_executable(tmp_path, run_agent_commands=True)
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
        "PATH": str(fake_spice.parent)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(base_env=base_env),
    }

    subprocess.run([bash, "-c", command], check=True, env=env)

    lines = trace_lines(trace, expected_prefix="after:")
    assert f"after:{static_hook_dir / shellhook.BASH_HOOK_NAME}" in lines
    assert lines.count(f"real-bash:{real_bash_env}") == 2


def test_zshenv_hook_execs_noninteractive_command_under_agent_run_once(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    trace = tmp_path / "trace.log"
    fake_spice = fake_spice_executable(tmp_path, run_agent_commands=True)
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
        "PATH": str(fake_spice.parent)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
        "SHELL": zsh,
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(base_env=base_env),
    }

    completed = subprocess.run([zsh, "-c", command], check=False, env=env, timeout=2)

    assert completed.returncode == 7
    lines = trace_lines(trace, expected_prefix="ran:")
    agent_run_lines = [line for line in lines if "agent run --" in line]
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
    fake_spice = fake_spice_executable(tmp_path, run_agent_commands=True)
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    base_env = {
        "PATH": str(fake_spice.parent)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
        "SHELL": zsh,
        SHELL_TRACE_ENV: str(trace),
    }
    env = shellhook.apply_shell_steering_environment(
        tmp_path,
        base_env=base_env,
    )
    command = (
        "printf 'ran:%s:%s\\n' "
        f'"${{{shellhook.ZDOTDIR_ENV}-unset}}" '
        f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
        f'>> "${{{SHELL_TRACE_ENV}}}"; '
        "exit 7"
    )

    completed = subprocess.run([zsh, "-c", command], check=False, env=env, timeout=2)

    assert completed.returncode == 7
    lines = trace_lines(trace, expected_prefix="ran:")
    agent_run_lines = [line for line in lines if "agent run --" in line]
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
    write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={"common": {"wrap": ["grep"]}},
    )
    trace = tmp_path / "trace.log"
    fake_spice = fake_spice_executable(tmp_path, run_agent_commands=True)
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
        "PATH": str(fake_spice.parent)
        + os.pathsep
        + str(bin_dir)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
        shellhook.ZDOTDIR_ENV: str(hook_dir),
        SHELL_TRACE_ENV: str(trace),
        shellhook.SHELL_HOOK_WRAPPERS_ENV: "\n".join(
            shellhook.render_agent_wrapper_lines(tmp_path)
        ),
        **shellhook.shell_steering_runtime_environment(
            base_env={},
            repo_root=tmp_path,
        ),
    }

    subprocess.run([zsh, "-c", "grep needle /dev/null"], check=True, env=env)

    lines = trace_lines(trace, expected_prefix="wrap:")
    assert "wrap:grep needle /dev/null" in lines


@pytest.mark.parametrize("shell_name", ["zsh", "bash"])
def test_agent_shell_targets_worktree_python_without_shadowing_global_spice(
    tmp_path, shell_name
):
    shell = shutil.which(shell_name)
    if shell is None:
        pytest.skip(f"{shell_name} is not installed")
    home = tmp_path / "home"
    home.mkdir()
    operator_bin = tmp_path / "operator-bin"
    operator_bin.mkdir()
    venv_bin = tmp_path / ".venv" / "bin"
    venv_bin.mkdir(parents=True)
    executables = {
        venv_bin / "python": "worktree-python",
        venv_bin / "spice": "worktree-spice",
        operator_bin / "spice": "global-spice",
    }
    for path, label in executables.items():
        path.write_text(
            f"#!/bin/sh\nprintf '{label}:%s\\n' \"$*\"\n",
            encoding="utf-8",
        )
        path.chmod(0o755)
    base_env = {
        "HOME": str(home),
        "PATH": str(operator_bin)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
    }
    env = shellhook.apply_shell_steering_environment(tmp_path, base_env=base_env)
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    if shell_name == "zsh":
        env[shellhook.ZDOTDIR_ENV] = str(static_hook_dir)
    else:
        env[shellhook.BASH_ENV_ENV] = str(static_hook_dir / shellhook.BASH_HOOK_NAME)

    completed = subprocess.run(
        [shell, "-c", "python one; python3 two; spice three"],
        text=True,
        capture_output=True,
        check=True,
        env=env,
    )

    assert env["PATH"] == base_env["PATH"]
    assert completed.stdout.splitlines() == [
        "worktree-python:one",
        "worktree-python:two",
        "global-spice:three",
    ]


@pytest.mark.parametrize("shell_name", ["zsh", "bash"])
def test_agent_shell_routes_pytest_arguments_through_worktree_python(
    tmp_path, shell_name
):
    shell = shutil.which(shell_name)
    if shell is None:
        pytest.skip(f"{shell_name} is not installed")
    write_agent_wrapper_config(
        tmp_path,
        order=["spice-dev"],
        groups={
            "spice-dev": {
                "pytest": {"argv": ["python", "-m", "pytest"]},
            }
        },
    )
    home = tmp_path / "home"
    home.mkdir()
    operator_bin = tmp_path / "operator-bin"
    operator_bin.mkdir()
    venv_bin = tmp_path / ".venv" / "bin"
    venv_bin.mkdir(parents=True)
    executables = {
        venv_bin / "python": "worktree-python",
        operator_bin / "pytest": "global-pytest",
    }
    for path, label in executables.items():
        path.write_text(
            f"#!/bin/sh\nprintf '{label}:%s\\n' \"$*\"\n",
            encoding="utf-8",
        )
        path.chmod(0o755)
    base_env = {
        "HOME": str(home),
        "PATH": str(operator_bin)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
    }
    env = shellhook.apply_shell_steering_environment(tmp_path, base_env=base_env)
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    if shell_name == "zsh":
        env[shellhook.ZDOTDIR_ENV] = str(static_hook_dir)
    else:
        env[shellhook.BASH_ENV_ENV] = str(static_hook_dir / shellhook.BASH_HOOK_NAME)

    completed = subprocess.run(
        [shell, "-c", "pytest -q tests/unit --maxfail=1"],
        text=True,
        capture_output=True,
        check=True,
        env=env,
    )

    assert completed.stdout.splitlines() == [
        "worktree-python:-m pytest -q tests/unit --maxfail=1"
    ]


def test_bash_env_hook_execs_noninteractive_command_under_agent_run_once(tmp_path):
    bash = shutil.which("bash")
    if bash is None:
        pytest.skip("bash is not installed")
    trace = tmp_path / "trace.log"
    fake_spice = fake_spice_executable(tmp_path, run_agent_commands=True)
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
        "PATH": str(fake_spice.parent)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        SHELL_TRACE_ENV: str(trace),
        **shellhook.shell_steering_runtime_environment(base_env=base_env),
    }

    completed = subprocess.run([bash, "-c", command], check=False, env=env)

    assert completed.returncode == 6
    lines = trace_lines(trace, expected_prefix="ran:")
    agent_run_lines = [line for line in lines if "agent run --" in line]
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
        "PATH": os.environ.get("PATH", ""),  # env-policy: allow
        shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
        **shellhook.shell_steering_runtime_environment(base_env=base_env),
    }

    completed = subprocess.run(
        [bash, str(script)], capture_output=True, check=False, env=env, text=True
    )

    assert completed.returncode == SHELL_HOOK_FAILURE_EXIT_CODE
    assert "cannot agent-run reexec noninteractive shell" in completed.stderr
