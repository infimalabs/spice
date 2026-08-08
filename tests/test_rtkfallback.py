"""RTK rewrite degradation preserves the native command path."""

from __future__ import annotations

import io
import shlex
import subprocess
import sys
import time
from pathlib import Path
from types import SimpleNamespace
from typing import cast

import pytest

from spice.config import edit, layers, values
from spice.agent import driver as agent_driver
from spice.agent import rtkrewrite
from spice.agent import wrap
from spice.process.groups import ProcessDeadlineExceeded, process_id_is_running

pytestmark = pytest.mark.usefixtures("git_worktree_tmp_path")


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
    _configure_rtk(tmp_path, executable)
    _route_project_python(tmp_path)
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

    direct_exit = wrap.run_agent_command(
        tmp_path,
        ["python", "-c", "print('native')"],
        popen_factory=record,
        stderr=io.StringIO(),
    )
    shell_exit = wrap.run_agent_command(
        tmp_path,
        ["zsh", "-c", "git status --short"],
        popen_factory=record,
        stderr=io.StringIO(),
    )

    assert {
        "exit_codes": [direct_exit, shell_exit],
        "rtk_environments": rtk_environments,
        "child_outcomes": child_outcomes,
    } == {
        "exit_codes": [0, 0],
        "rtk_environments": ["preserved", "preserved"],
        "child_outcomes": [
            ([*wrap.UV_PYTHON_COMMAND, "-c", "print('native')"], "preserved"),
            (["zsh", "-c", "git status --short"], "preserved"),
        ],
    }


def test_native_bypass_passes_python_through_outside_a_routing_project(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    executable = "missing-passthrough-rtk"
    _configure_rtk(tmp_path, executable)
    monkeypatch.setattr(
        wrap,
        "build_agent_run_environment",
        lambda _raw_args, **_kwargs: {wrap.RTK_DB_PATH_ENV: "preserved"},
    )
    wrap._rtk_warned_keys.clear()
    child_commands: list[list[str]] = []

    def missing_rtk(_command: list[str], **_kwargs: object) -> object:
        raise FileNotFoundError(2, "missing", executable)

    _isolate_agent_run(monkeypatch, missing_rtk)

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["python", "-c", "print('native')"],
        popen_factory=lambda command, **_kwargs: _record_child(child_commands, command),
        stderr=io.StringIO(),
    )

    assert {"exit_code": exit_code, "child_commands": child_commands} == {
        "exit_code": 0,
        "child_commands": [["python", "-c", "print('native')"]],
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


def test_rewrite_deadline_selects_native_and_suppresses_repeat_diagnostic(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    executable = "stalled-rtk"
    _configure_rtk(tmp_path, executable)
    wrap._rtk_warned_keys.clear()
    calls: list[dict[str, object]] = []

    def stalled(command: list[str], **kwargs: object) -> object:
        calls.append({"command": command, **kwargs})
        raise ProcessDeadlineExceeded(
            phase=str(kwargs["phase"]),
            input_label=str(kwargs["input_label"]),
            timeout_seconds=float(kwargs["timeout_seconds"]),
            command=command,
        )

    monkeypatch.setattr(rtkrewrite, "run_bounded_process_group", stalled)
    stderr = io.StringIO()

    first = rtkrewrite.rewrite_command_text(
        "native-tool", repo_root=tmp_path, rtk_executable=executable, stderr=stderr
    )
    second = rtkrewrite.rewrite_command_text(
        "native-tool", repo_root=tmp_path, rtk_executable=executable, stderr=stderr
    )

    assert first is None
    assert second is None
    assert (
        calls
        == [
            {
                "command": [executable, "rewrite", "--", "native-tool"],
                "timeout_seconds": rtkrewrite.RTK_REWRITE_SELECTOR_TIMEOUT_SECONDS,
                "phase": "agent.rtk-rewrite-selector",
                "input_label": "command-selection",
                "capture_output": True,
                "text": True,
                "check": False,
            }
        ]
        * 2
    )
    assert stderr.getvalue().splitlines() == [
        "spice agent run: RTK rewrite degraded to native "
        "executable='stalled-rtk' failure=deadline-exceeded"
    ]


def test_rewrite_deadline_reaps_stalled_descendant_process(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    descendant_pid_path = tmp_path / "descendant.pid"
    provider = (
        "import pathlib, subprocess, sys, time; "
        "child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)']); "
        "pathlib.Path(sys.argv[1]).write_text(str(child.pid), encoding='utf-8'); "
        "time.sleep(60)"
    )
    executable = tmp_path / "stalled-rtk"
    executable.write_text(
        "#!/bin/sh\n"
        f"exec {shlex.quote(sys.executable)} -c {shlex.quote(provider)} "
        f"{shlex.quote(str(descendant_pid_path))}\n",
        encoding="utf-8",
    )
    executable.chmod(0o755)
    wrap._rtk_warned_keys.clear()
    monkeypatch.setattr(rtkrewrite, "RTK_REWRITE_SELECTOR_TIMEOUT_SECONDS", 1.0)

    stderr = io.StringIO()
    rewritten = rtkrewrite.rewrite_command_text(
        "native-tool", rtk_executable=str(executable), stderr=stderr
    )
    descendant_pid = int(descendant_pid_path.read_text(encoding="utf-8"))
    reaping_deadline = time.monotonic() + 2.0
    while process_id_is_running(descendant_pid) and time.monotonic() < reaping_deadline:
        time.sleep(0.01)

    assert rewritten is None
    assert not process_id_is_running(descendant_pid)
    assert stderr.getvalue().splitlines() == [
        "spice agent run: RTK rewrite degraded to native "
        f"executable={str(executable)!r} failure=deadline-exceeded"
    ]


def _configure_rtk(repo_root: Path, executable: str) -> None:
    repo_root.mkdir(exist_ok=True)
    edit.set_scope_section(
        repo_root,
        layers.WORKTREE_SOURCE,
        values.RTK_KEY,
        {values.RTK_EXECUTABLE_KEY: executable},
    )


def _route_project_python(repo_root: Path) -> None:
    (repo_root / "pyproject.toml").write_text('[project]\nname = "routing-probe"\n')


def _isolate_agent_run(
    monkeypatch: pytest.MonkeyPatch, rtk_run, *, preserve_thread: bool = False
) -> None:
    if not preserve_thread:
        monkeypatch.delenv(agent_driver.CODEX_DRIVER.thread_id_env, raising=False)
        monkeypatch.delenv(agent_driver.CLAUDE_DRIVER.thread_id_env, raising=False)

    def bounded_rtk_run(command: list[str], **kwargs: object) -> object:
        kwargs.pop("timeout_seconds")
        kwargs.pop("phase")
        kwargs.pop("input_label")
        return rtk_run(command, **kwargs)

    monkeypatch.setattr(rtkrewrite, "run_bounded_process_group", bounded_rtk_run)
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


ALTERNATION_SUBJECT_LINES = ("alpha", "beta")
ALTERNATION_PATTERN = "alpha|beta"
SPACED_PATTERN = "alpha beta"
SPACED_PATH = "with space.txt"
# A rewrite headed by a word the shell resolves itself is refused before any of
# this runs, so these shapes route through `git`, which RTK does claim. Their
# rewrites are the ones that reach a child process.
SPACED_REWRITE_HEAD = "rtk git log"


def _run_spaced_rewrite(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    command: list[str],
    rewritten: str,
) -> tuple[int, list[list[str]], str]:
    """Run one argv command against a fixed rewrite and report what happened."""
    _configure_rtk(tmp_path, "configured-rtk")
    wrap._rtk_warned_keys.clear()
    child_calls: list[list[str]] = []

    def run_rtk(_command: list[str], **_kwargs: object) -> object:
        return subprocess.CompletedProcess([], 3, stdout=rewritten + "\n", stderr="")

    _isolate_agent_run(monkeypatch, run_rtk)
    stderr = io.StringIO()
    exit_code = wrap.run_agent_command(
        tmp_path,
        command,
        popen_factory=lambda argv, **_kwargs: _record_child(child_calls, argv),
        stderr=stderr,
    )
    return exit_code, child_calls, stderr.getvalue()


@pytest.mark.parametrize(
    ("shape", "written_tail", "rewritten_tail"),
    [
        # The pattern holds the space, so a bare rewrite reads its second word as
        # a separate operand and searches for the first word alone.
        ("spaced-pattern", ["--grep", SPACED_PATTERN], f"--grep {SPACED_PATTERN}"),
        # The path holds the space, so a bare rewrite names two paths that do not
        # exist while the rest of the command survives intact.
        ("spaced-path", ["--", SPACED_PATH], f"-- {SPACED_PATH}"),
    ],
)
def test_spaced_argument_runs_natively_when_a_rewrite_unquotes_it(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    shape: str,
    written_tail: list[str],
    rewritten_tail: str,
) -> None:
    """One argv word stays one word or the caller's own command runs instead."""
    del shape
    command = ["git", "log", *written_tail]
    exit_code, child_calls, warning = _run_spaced_rewrite(
        tmp_path, monkeypatch, command, f"{SPACED_REWRITE_HEAD} {rewritten_tail}"
    )

    assert {
        "exit_code": exit_code,
        "child_calls": child_calls,
        "warning": warning,
    } == {
        "exit_code": 0,
        "child_calls": [command],
        "warning": (
            "spice agent run: RTK rewrite degraded to native "
            "executable='configured-rtk' failure=unquoted-argument\n"
        ),
    }


def test_spaceless_argument_runs_natively_when_a_rewrite_breaks_it_apart(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """One argv word stays one word even when it never carried a space."""
    subject = tmp_path / "subject.txt"
    command = ["grep", "-E", ALTERNATION_PATTERN, str(subject)]
    # Verbatim RTK output for this command: the extended flag survives, so the
    # dialect is intact and only the spacing of the pattern gives it away.
    alternative, rest = ALTERNATION_PATTERN.split("|")
    rewritten = f"rtk grep -E {alternative} |{rest} {subject}"
    exit_code, child_calls, warning = _run_spaced_rewrite(
        tmp_path, monkeypatch, command, rewritten
    )

    assert {
        "exit_code": exit_code,
        "child_calls": child_calls,
        "warning": warning,
    } == {
        "exit_code": 0,
        "child_calls": [command],
        "warning": (
            "spice agent run: RTK rewrite degraded to native "
            "executable='configured-rtk' failure=unquoted-argument\n"
        ),
    }


def test_spaced_argument_keeps_the_rewrite_when_the_word_survives_whole(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A rewrite that preserves the spaced word is still the command that runs."""
    command = ["git", "log", "--grep", SPACED_PATTERN]
    exit_code, child_calls, warning = _run_spaced_rewrite(
        tmp_path,
        monkeypatch,
        command,
        f"{SPACED_REWRITE_HEAD} --grep {shlex.quote(SPACED_PATTERN)}",
    )

    assert {
        "exit_code": exit_code,
        "child_calls": child_calls,
        "warning": warning,
    } == {
        "exit_code": 0,
        "child_calls": [["configured-rtk", "git", "log", "--grep", SPACED_PATTERN]],
        "warning": "",
    }


@pytest.mark.parametrize(
    ("shape", "written_args", "rewrite_template"),
    [
        # RTK reproduces the pattern as separate words here, so the search that
        # would run looks for one alternative in a file named for the other.
        (
            "explicit-file-list",
            ["rg", ALTERNATION_PATTERN, "{subject}"],
            "rtk grep alpha |beta {subject}",
        ),
        # RTK keeps the quoting here and still substitutes a basic-dialect
        # search, so the alternation degrades to a literal instead of splitting.
        (
            "shell-text",
            ["zsh", "-c", f"rg '{ALTERNATION_PATTERN}' {{subject}}"],
            "rtk grep '" + ALTERNATION_PATTERN + "' {subject}",
        ),
        # egrep reads the extended dialect by name, so a caller who writes it is
        # narrowed by the same substitution that narrows a caller who writes rg.
        (
            "written-egrep",
            ["egrep", ALTERNATION_PATTERN, "{subject}"],
            "rtk grep '" + ALTERNATION_PATTERN + "' {subject}",
        ),
        # grep asks for the extended dialect by flag, and the flag does not
        # survive the substitution.
        (
            "written-grep-extended-flag",
            ["grep", "-E", ALTERNATION_PATTERN, "{subject}"],
            "rtk grep '" + ALTERNATION_PATTERN + "' {subject}",
        ),
        # A path argument spelled like an extended-dialect command is still a
        # path; the substituted search reads the basic dialect its name declares.
        (
            "argument-spelled-rg",
            ["rg", ALTERNATION_PATTERN, "rg"],
            "rtk grep '" + ALTERNATION_PATTERN + "' rg",
        ),
    ],
)
def test_alternation_search_runs_natively_when_a_rewrite_narrows_the_dialect(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    shape: str,
    written_args: list[str],
    rewrite_template: str,
) -> None:
    """An extended alternation reaches a search that reads it as one."""
    del shape
    subject = tmp_path / "subject.txt"
    subject.write_text("\n".join(ALTERNATION_SUBJECT_LINES) + "\n")
    executable = "configured-rtk"
    _configure_rtk(tmp_path, executable)
    wrap._rtk_warned_keys.clear()
    command = [part.format(subject=subject) for part in written_args]
    rewritten = rewrite_template.format(subject=subject)
    child_calls: list[list[str]] = []

    def run_rtk(_command: list[str], **_kwargs: object) -> object:
        return subprocess.CompletedProcess([], 3, stdout=rewritten + "\n", stderr="")

    _isolate_agent_run(monkeypatch, run_rtk)
    stderr = io.StringIO()
    exit_code = wrap.run_agent_command(
        tmp_path,
        command,
        popen_factory=lambda argv, **_kwargs: _record_child(child_calls, argv),
        stderr=stderr,
    )

    assert {
        "exit_code": exit_code,
        "subject_lines": tuple(subject.read_text().split()),
        "child_calls": child_calls,
        "warning": stderr.getvalue(),
    } == {
        "exit_code": 0,
        "subject_lines": ALTERNATION_SUBJECT_LINES,
        "child_calls": [command],
        "warning": (
            "spice agent run: RTK rewrite degraded to native "
            f"executable={executable!r} failure=regex-dialect-narrowed\n"
        ),
    }


def test_egrep_substitution_keeps_a_rewrite_of_an_extended_search(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A substituted egrep reads the alternation the caller wrote."""
    subject = tmp_path / "subject.txt"
    _configure_rtk(tmp_path, "configured-rtk")
    wrap._rtk_warned_keys.clear()
    command = ["rg", ALTERNATION_PATTERN, str(subject)]
    rewritten = f"egrep '{ALTERNATION_PATTERN}' {subject}"
    child_calls: list[list[str]] = []

    def run_rtk(_command: list[str], **_kwargs: object) -> object:
        return subprocess.CompletedProcess([], 3, stdout=rewritten + "\n", stderr="")

    _isolate_agent_run(monkeypatch, run_rtk)
    stderr = io.StringIO()
    exit_code = wrap.run_agent_command(
        tmp_path,
        command,
        popen_factory=lambda argv, **_kwargs: _record_child(child_calls, argv),
        stderr=stderr,
    )

    assert {
        "exit_code": exit_code,
        "child_calls": child_calls,
        "warning": stderr.getvalue(),
    } == {
        "exit_code": 0,
        "child_calls": [["egrep", ALTERNATION_PATTERN, str(subject)]],
        "warning": "",
    }
