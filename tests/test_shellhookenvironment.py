"""Agent shell-hook environment and built-in route integration tests."""

import getpass
import io
import json
import os
import shlex
import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent import driver as agent_driver
from spice.agent import lifecycle, shellhook, wrap
from spice.agent.driver import CLAUDE_DRIVER, DRIVER
from spice.tasks import config as task_config
from tests.test_shellhookhelpers import (
    SHELL_TRACE_ENV,
    builtin_common_wrapper_lines,
    expected_python_module_wrapper_lines,
    expected_wrapper_lines,
    trace_lines,
    write_agent_wrapper_config,
    write_spice_product_shape,
)
from tests.test_shellhook import (
    UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND,
    UNSUPPORTED_AGENT_STEER_COMMAND,
)
from tests.test_configtrusthelpers import approve_repository_config


def test_wrapper_route_environment_uses_static_hook_stage_for_shell_execution(
    tmp_path, monkeypatch
):
    write_spice_product_shape(tmp_path)
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
    write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={"common": {"wrap": ["grep"]}},
    )
    approve_repository_config(tmp_path)
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
    for name, value in ambient_env.items():
        monkeypatch.setenv(name, value)

    exit_code = wrap.run_agent_command(
        tmp_path,
        [zsh, "-c", "grep needle /dev/null"],
        stderr=io.StringIO(),
    )

    assert exit_code == 0
    lines = trace_lines(trace, expected_prefix="wrap:")
    assert "wrap:grep needle /dev/null" in lines


def test_wrapper_find_route_sends_unsupported_primaries_to_native_find(
    tmp_path, monkeypatch
):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)
    wrappers = tmp_path / "wrappers.zsh"
    wrappers.write_text(
        "\n".join(shellhook.render_agent_wrapper_lines(tmp_path)) + "\n",
        encoding="utf-8",
    )
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    rtk_bin = bin_dir / "rtk"
    rtk_bin.write_text("#!/bin/sh\nprintf 'rtk:%s\\n' \"$*\"\n", encoding="utf-8")
    rtk_bin.chmod(0o755)
    env = dict(os.environ)  # env-policy: allow
    env["PATH"] = str(bin_dir) + os.pathsep + env.get("PATH", "")  # env-policy: allow
    fixture = tmp_path / "fixture"
    (fixture / "sub").mkdir(parents=True)
    (fixture / "sub" / "needle.txt").write_text("needle\n", encoding="utf-8")
    (fixture / "big.bin").write_bytes(b"\0" * 30_000)
    (fixture / "empty.txt").touch()
    old = fixture / "old.txt"
    old.write_text("old\n", encoding="utf-8")
    os.utime(old, (1_000_000_000, 1_000_000_000))

    def routed_output(args: list[str]) -> str:
        words = " ".join(shlex.quote(word) for word in ["find", str(fixture), *args])
        completed = subprocess.run(
            [zsh, "-f", "-c", f"source {shlex.quote(str(wrappers))}; rtk {words}"],
            capture_output=True,
            text=True,
            env=env,
            check=True,
        )
        return completed.stdout

    cases = [
        ("big.bin", ["-size", "+24k"]),
        ("big.bin", ["-not", "-name", "*.txt"]),
        ("needle.txt", ["-mtime", "-1"]),
        ("needle.txt", ["-path", "*sub*"]),
        ("needle.txt", ["-user", getpass.getuser()]),
        ("needle.txt", ["-newer", str(old)]),
        ("empty.txt", ["-empty"]),
        ("needle.txt", ["-regex", ".*needle.*"]),
        ("needle.txt", ["-perm", "-200"]),
    ]
    for sentinel, args in cases:
        native = subprocess.run(
            ["find", str(fixture), *args], capture_output=True, text=True, check=True
        )
        assert sentinel in native.stdout
        assert routed_output(args) == native.stdout

    kept_cases = [
        [],
        ["-name", "*.txt"],
        ["-type", "d"],
        ["-maxdepth", "1", "-iname", "*.TXT"],
    ]
    for args in kept_cases:
        joined = " ".join(["find", str(fixture), *args])
        assert routed_output(args) == f"rtk:{joined}\n"


def test_agent_environment_does_not_inject_worktree_spice_pythonpath(
    tmp_path, monkeypatch
):
    write_spice_product_shape(tmp_path)
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
    assert env[shellhook.SHELL_HOOK_REPO_ROOT_ENV] == str(tmp_path.resolve())
    assert shellhook.SHELL_HOOK_WRAPPERS_ENV.startswith(
        "SPICE_SHELL_HOOK_"  # env-policy: allow
    )
    assert env[shellhook.SHELL_HOOK_WRAPPERS_ENV] == "\n".join(
        shellhook.render_agent_wrapper_lines(tmp_path)
    )
    assert env[shellhook.SHELL_HOOK_WRAPPERS_ENV] == "\n".join(
        builtin_common_wrapper_lines()
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


def test_agent_environment_binds_taskrc_to_selected_spice_backend(
    tmp_path, monkeypatch
):
    backend = tmp_path / "selected-task-backend"
    monkeypatch.setenv(task_config.TASK_BACKEND_ENV, str(backend))
    monkeypatch.setenv(shellhook.TASKRC_ENV, str(tmp_path / "operator-taskrc"))
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)

    env = lifecycle.agent_environment(tmp_path)

    taskrc = Path(env[shellhook.TASKRC_ENV])
    assert taskrc == backend / "taskrc"
    assert taskrc.is_file()
    assert (backend / "data").is_dir()


def test_agent_environment_binds_taskrc_to_default_shared_backend(
    tmp_path, monkeypatch
):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    monkeypatch.delenv(task_config.TASK_BACKEND_ENV, raising=False)

    env = shellhook.apply_shell_steering_environment(
        repo,
        base_env={"HOME": str(tmp_path)},
    )

    taskrc = Path(env[shellhook.TASKRC_ENV])
    assert taskrc == repo / ".git" / ".spice" / "taskrc"
    assert taskrc.is_file()
    assert (taskrc.parent / "data").is_dir()


def test_native_task_verbs_use_spice_backend_across_nested_shells(
    tmp_path, monkeypatch
):
    task_binary = shutil.which("task")
    if task_binary is None:
        pytest.skip("Taskwarrior is not installed")
    shell = shutil.which("sh")
    if shell is None:
        pytest.skip("a POSIX shell is not installed")
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    backend = tmp_path / "task-backend"
    ambient = tmp_path / "unrelated-cwd"
    ambient.mkdir()
    monkeypatch.chdir(ambient)
    base_env = dict(os.environ)  # env-policy: allow
    base_env[task_config.TASK_BACKEND_ENV] = str(backend)
    env = shellhook.apply_shell_steering_environment(repo, base_env=base_env)
    taskrc = Path(env[shellhook.TASKRC_ENV])

    def run_native(*args: str) -> subprocess.CompletedProcess[str]:
        inner = shlex.join([task_binary, *args])
        outer = shlex.join([shell, "-c", inner])
        return subprocess.run(
            [shell, "-c", outer],
            env=env,
            text=True,
            capture_output=True,
            check=True,
        )

    run_native("add", "Native original")
    created = json.loads(run_native("export").stdout)
    run_native(created[0]["uuid"], "modify", "Native revised")
    listed = run_native("list").stdout
    exported = json.loads(run_native("export").stdout)
    data_location = run_native("_get", "rc.data.location").stdout.strip()

    assert Path(env[shellhook.TASKRC_ENV]) == taskrc
    assert taskrc.is_file()
    assert Path(data_location) == backend / "data"
    assert "Native revised" in listed
    assert [row["description"] for row in exported] == ["Native revised"]


def test_agent_environment_precomputes_configured_shell_wrapper_block(
    tmp_path, monkeypatch
):
    write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={
            "common": {
                "wrap": ["grep", "git"],
                "pytest": {"argv": ["python", "-m", "pytest"]},
            }
        },
    )
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)

    env = lifecycle.agent_environment(tmp_path)

    assert env[shellhook.SHELL_HOOK_WRAPPERS_ENV] == "\n".join(
        shellhook.render_shell_runtime_wrapper_lines(tmp_path)
    )
    assert env[shellhook.SHELL_HOOK_WRAPPERS_ENV] == "\n".join(
        [
            *expected_wrapper_lines("wrap", ["grep", "git"]),
            *expected_python_module_wrapper_lines(["pytest"]),
            *shellhook.render_project_python_wrapper_lines(tmp_path),
        ]
    )


def test_repo_spice_dev_wrapper_routes_pytest_through_dev_seam():
    repo = Path(__file__).resolve().parents[1]

    rendered = shellhook.render_agent_wrapper_lines(repo)

    pytest_start = rendered.index("pytest() {") - 1
    assert rendered[pytest_start : pytest_start + 4] == [
        "",
        "pytest() {",
        '  spice dev pytest "$@"',
        "}",
    ]


def test_repo_pytest_wrapper_word_yields_rtk_rewrite():
    repo = Path(__file__).resolve().parents[1]

    assert "pytest" in shellhook.rtk_rewrite_yield_selectors(repo)
