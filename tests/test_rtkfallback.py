"""RTK rewrite degradation preserves the native command path."""

from __future__ import annotations

import io
import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import cast

import pytest

from spice.config import edit, layers, values
from spice.agent import driver as agent_driver
from spice.agent import rtkrewrite
from spice.agent import wrap


class _RecordedProcess:
    pid = 0

    def wait(self) -> int:
        return 0


@pytest.mark.parametrize(
    ("name", "executable", "response", "expected_command", "failure_class"),
    [
        (
            "exit-zero-rewrite",
            "configured-rtk",
            subprocess.CompletedProcess([], 0, stdout="optimized --zero\n", stderr=""),
            ["optimized", "--zero"],
            "",
        ),
        (
            "exit-three-rewrite",
            "configured-rtk",
            subprocess.CompletedProcess([], 3, stdout="optimized --three\n", stderr=""),
            ["optimized", "--three"],
            "",
        ),
        (
            "exit-one-no-match",
            "configured-rtk",
            subprocess.CompletedProcess([], 1, stdout="", stderr=""),
            ["native-tool", "arg"],
            "",
        ),
        (
            "missing-basename",
            "missing-rtk",
            FileNotFoundError(2, "missing", "missing-rtk"),
            ["native-tool", "arg"],
            "launch-not-found",
        ),
        (
            "missing-absolute-path",
            "/missing/rtk",
            FileNotFoundError(2, "missing", "/missing/rtk"),
            ["native-tool", "arg"],
            "launch-not-found",
        ),
        (
            "unexpected-exit",
            "configured-rtk",
            subprocess.CompletedProcess([], 9, stdout="unexpected\n", stderr="boom"),
            ["native-tool", "arg"],
            "unexpected-exit",
        ),
        (
            "invalid-result-pair",
            "configured-rtk",
            subprocess.CompletedProcess([], 0, stdout="", stderr=""),
            ["native-tool", "arg"],
            "invalid-result-pair",
        ),
        (
            "invalid-result-shape",
            "configured-rtk",
            SimpleNamespace(returncode="zero", stdout=[]),
            ["native-tool", "arg"],
            "invalid-result-shape",
        ),
        (
            "malformed-direct-argv",
            "configured-rtk",
            subprocess.CompletedProcess([], 3, stdout="optimized 'open\n", stderr=""),
            ["native-tool", "arg"],
            "malformed-direct-argv",
        ),
    ],
)
def test_direct_rewrite_outcomes_execute_one_expected_child(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    name: str,
    executable: str,
    response: object,
    expected_command: list[str],
    failure_class: str,
) -> None:
    del name
    _configure_rtk(tmp_path, executable)
    wrap._rtk_warned_keys.clear()
    rtk_calls: list[list[str]] = []
    child_calls: list[list[str]] = []

    def run_rtk(command: list[str], **_kwargs: object) -> object:
        rtk_calls.append(command)
        if isinstance(response, OSError):
            raise response
        return response

    _isolate_agent_run(monkeypatch, run_rtk)
    stderr = io.StringIO()
    exit_code = wrap.run_agent_command(
        tmp_path,
        ["native-tool", "arg"],
        popen_factory=lambda command, **_kwargs: _record_child(child_calls, command),
        stderr=stderr,
    )
    expected_warning = (
        ""
        if not failure_class
        else (
            "spice agent run: RTK rewrite degraded to native "
            f"executable={executable!r} failure={failure_class}\n"
        )
    )

    assert {
        "exit_code": exit_code,
        "rtk_calls": rtk_calls,
        "child_calls": child_calls,
        "warning": stderr.getvalue(),
    } == {
        "exit_code": 0,
        "rtk_calls": [[executable, "rewrite", "--", "native-tool", "arg"]],
        "child_calls": [expected_command],
        "warning": expected_warning,
    }


@pytest.mark.parametrize(
    ("path_name", "raw_args", "driver", "expected_rtk_calls"),
    [
        (
            "shell-string",
            ["zsh", "-c", "git status --short"],
            agent_driver.CODEX_DRIVER,
            1,
        ),
        (
            "trailing-exec",
            [
                "/bin/zsh",
                "-c",
                "snapshot=ready\nexec '/bin/zsh' -c 'git status --short'",
            ],
            agent_driver.CODEX_DRIVER,
            2,
        ),
        (
            "driver-envelope",
            [
                "zsh",
                "-c",
                "source /tmp/snapshot.sh && eval 'git show HEAD' "
                "< /dev/null && pwd -P >| /tmp/cwd",
            ],
            agent_driver.CLAUDE_DRIVER,
            2,
        ),
    ],
)
def test_missing_companion_preserves_each_native_command_envelope(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    path_name: str,
    raw_args: list[str],
    driver: agent_driver.AgentDriver,
    expected_rtk_calls: int,
) -> None:
    del path_name
    executable = "/missing/envelope-rtk"
    _configure_rtk(tmp_path, executable)
    wrap._rtk_warned_keys.clear()
    rtk_calls: list[list[str]] = []
    child_calls: list[list[str]] = []

    def missing_rtk(command: list[str], **_kwargs: object) -> object:
        rtk_calls.append(command)
        raise FileNotFoundError(2, "missing", executable)

    _isolate_agent_run(monkeypatch, missing_rtk)
    monkeypatch.setattr(wrap, "driver_for", lambda _repo_root: driver)
    stderr = io.StringIO()
    exit_code = wrap.run_agent_command(
        tmp_path,
        raw_args,
        popen_factory=lambda command, **_kwargs: _record_child(child_calls, command),
        stderr=stderr,
    )

    assert {
        "exit_code": exit_code,
        "rtk_call_count": len(rtk_calls),
        "child_calls": child_calls,
        "warning_lines": stderr.getvalue().splitlines(),
    } == {
        "exit_code": 0,
        "rtk_call_count": expected_rtk_calls,
        "child_calls": [raw_args],
        "warning_lines": [
            "spice agent run: RTK rewrite degraded to native "
            f"executable={executable!r} failure=launch-not-found"
        ],
    }


def test_native_bypass_preserves_worktree_routing_and_child_environment(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    executable = "missing-routing-rtk"
    project_root = tmp_path / "project"
    plain_root = tmp_path / "plain"
    _configure_rtk(project_root, executable)
    _configure_rtk(plain_root, executable)
    # Routing is scoped to a Spice project, and the pyproject marker is the whole
    # scope: inside one a bare interpreter becomes the project's uv interpreter,
    # outside one it stays the operator's own python. Running the identical argv
    # under both roots is what proves the native bypass still routes rather than
    # merely passing the command through -- a single routed reading cannot tell
    # those two apart.
    (project_root / "pyproject.toml").write_text(
        '[project]\nname = "routed"\nversion = "0"\n', encoding="utf-8"
    )
    monkeypatch.setattr(
        wrap,
        "build_agent_run_environment",
        lambda _raw_args, **_kwargs: {wrap.RTK_DB_PATH_ENV: "preserved"},
    )
    wrap._rtk_warned_keys.clear()
    rtk_environments: list[str] = []
    child_outcomes: list[tuple[list[str], str]] = []

    def missing_rtk(_command: list[str], **kwargs: object) -> object:
        environment = kwargs.get("env")
        value = str(cast(dict[str, str], environment).get(wrap.RTK_DB_PATH_ENV, ""))
        rtk_environments.append(value)
        raise FileNotFoundError(2, "missing", executable)

    _isolate_agent_run(monkeypatch, missing_rtk)

    def record(command: list[str], **kwargs: object) -> _RecordedProcess:
        environment = kwargs.get("env")
        value = str(cast(dict[str, str], environment).get(wrap.RTK_DB_PATH_ENV, ""))
        child_outcomes.append((command, value))
        return _RecordedProcess()

    def bypass(repo_root: Path, raw_args: list[str]) -> int:
        return wrap.run_agent_command(
            repo_root, raw_args, popen_factory=record, stderr=io.StringIO()
        )

    interpreter_args = ["python", "-c", "print('native')"]
    routed_exit = bypass(project_root, interpreter_args)
    unrouted_exit = bypass(plain_root, interpreter_args)
    shell_exit = bypass(project_root, ["zsh", "-c", "git status --short"])

    assert {
        "exit_codes": [routed_exit, unrouted_exit, shell_exit],
        "rtk_environments": rtk_environments,
        "child_outcomes": child_outcomes,
        "marker_decides": child_outcomes[0][0] != child_outcomes[1][0],
    } == {
        "exit_codes": [0, 0, 0],
        "rtk_environments": ["preserved", "preserved", "preserved"],
        "child_outcomes": [
            (["uv", "run", "python", "-c", "print('native')"], "preserved"),
            (["python", "-c", "print('native')"], "preserved"),
            (["zsh", "-c", "git status --short"], "preserved"),
        ],
        "marker_decides": True,
    }


def test_warning_dedup_keys_thread_executable_and_failure_signature(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    executable = "missing-dedup-rtk"
    _configure_rtk(tmp_path, executable)
    monkeypatch.setenv(agent_driver.DRIVER.thread_id_env, "thread-one")
    monkeypatch.delenv(agent_driver.CLAUDE_DRIVER.thread_id_env, raising=False)

    def state_dir(_root: Path, thread_id: str) -> Path:
        return tmp_path / "agent-state" / thread_id

    monkeypatch.setattr(rtkrewrite, "agent_thread_state_dir", state_dir)
    monkeypatch.setattr(wrap, "agent_thread_state_dir", state_dir)
    mode = {"value": "missing"}
    child_calls: list[list[str]] = []

    def rtk_result(_command: list[str], **_kwargs: object) -> object:
        if mode["value"] == "missing":
            raise FileNotFoundError(2, "missing", executable)
        return subprocess.CompletedProcess([], 9, stdout="unexpected", stderr="")

    _isolate_agent_run(monkeypatch, rtk_result, preserve_thread=True)
    stderr = io.StringIO()

    def invoke() -> None:
        wrap._rtk_warned_keys.clear()
        wrap.run_agent_command(
            tmp_path,
            ["native-tool"],
            popen_factory=lambda command, **_kwargs: _record_child(
                child_calls, command
            ),
            stderr=stderr,
        )

    invoke()
    invoke()
    _configure_rtk(tmp_path, "other-missing-rtk")
    invoke()
    mode["value"] = "unexpected"
    invoke()
    monkeypatch.setenv(agent_driver.DRIVER.thread_id_env, "thread-two")
    invoke()

    assert {
        "children": child_calls,
        "warnings": stderr.getvalue().splitlines(),
    } == {
        "children": [["native-tool"]] * 5,
        "warnings": [
            "spice agent run: RTK rewrite degraded to native "
            "executable='missing-dedup-rtk' failure=launch-not-found",
            "spice agent run: RTK rewrite degraded to native "
            "executable='other-missing-rtk' failure=launch-not-found",
            "spice agent run: RTK rewrite degraded to native "
            "executable='other-missing-rtk' failure=unexpected-exit",
            "spice agent run: RTK rewrite degraded to native "
            "executable='other-missing-rtk' failure=unexpected-exit",
        ],
    }


def _configure_rtk(repo_root: Path, executable: str) -> None:
    repo_root.mkdir(exist_ok=True)
    edit.set_scope_section(
        repo_root,
        layers.WORKTREE_SOURCE,
        values.RTK_KEY,
        {values.RTK_EXECUTABLE_KEY: executable},
    )


def _isolate_agent_run(
    monkeypatch: pytest.MonkeyPatch, rtk_run, *, preserve_thread: bool = False
) -> None:
    if not preserve_thread:
        monkeypatch.delenv(agent_driver.CODEX_DRIVER.thread_id_env, raising=False)
        monkeypatch.delenv(agent_driver.CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.setattr(wrap.subprocess, "run", rtk_run)
    monkeypatch.setattr(
        wrap,
        "bind_ambient_thread_for_shell_stage",
        lambda _repo_root, **_kwargs: None,
    )
    monkeypatch.setattr(
        wrap,
        "emit_initial_side_channel_payload",
        lambda _repo_root, **_kwargs: (),
    )
    monkeypatch.setattr(
        wrap,
        "start_agent_side_channel_watch",
        lambda *_args, **_kwargs: None,
    )


def _record_child(calls: list[list[str]], command: list[str]) -> _RecordedProcess:
    calls.append(command)
    return _RecordedProcess()
