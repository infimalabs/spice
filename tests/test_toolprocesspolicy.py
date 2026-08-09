"""Every synchronous subprocess uses a named deadline or lifetime contract."""

from __future__ import annotations

import ast
import subprocess
import sys
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.process.groups import ProcessDeadlineExceeded
from spice.process import groups, tool
from spice.serve import typecheck as serve_typecheck
from spice.studies import typecheck as python_typecheck

PROJECT_ROOT = Path(__file__).resolve().parents[1]
EXPECTED_EXTENSION_TOOL_TIMEOUT_SECONDS = 180.0
# Both ways a caller reaches a named tool policy. A new entry point that this
# set does not know is a bounded-tool surface the catalog below cannot see, so
# adding one here is what keeps the catalog a gate rather than a sample.
TOOL_POLICY_ENTRY_POINTS = {"run_tool_command", "run_streamed_tool_command"}
EXPECTED_DIRECT_SUBPROCESS_SEAMS = {
    "spice/agent/judgeadapter.py:main:run",
    "spice/agent/lifecycle.py:spawn_agent:Popen",
    "spice/agent/lifecycle.py:spawn_agent_supervisor:Popen",
    "spice/agent/watchdog.py:spawn_supervised_agent:Popen",
    "spice/process/groups.py:_force_windows_process_tree:run",
    "spice/process/groups.py:_posix_pid_has_live_state:run",
    "spice/process/groups.py:_posix_process_group_has_live_member:run",
    "spice/process/groups.py:_stream_until_exit:Popen",
    "spice/process/groups.py:run_bounded_process_group:Popen",
    "spice/process/tool.py:run_parent_lifetime_command:run",
    "spice/serve/runtimeinstall.py:restart_replaced_runtime:run",
    "spice/tasks/tw.py:run:run",
}
EXPECTED_TOOL_POLICY_CALLERS = {
    "coverage": {"spice/studies/subsumption.py:record_subsumption:capture=false"},
    "extension": {
        "spice/hooks/doctor.py:_spice_runtime_probe_for_python:capture=true",
        "spice/hooks/doctor.py:_worktree_venv_check:capture=true",
        "spice/hooks/precommit.py:_run_policy_command_step:capture=true",
        "spice/studies/reachability.py:_scan_command_reachability_provider:capture=true",
    },
    "hook": {"spice/hooks/precommit.py:_run_python_format_guard:capture=true"},
    "probe": {
        "spice/agent/driver.py:operator_color_scheme:capture=true",
        # Doctor's required Taskwarrior version check is a bounded availability probe.
        "spice/hooks/doctor.py:_taskwarrior_check:capture=true",
        "spice/studies/subsumption.py:_require_coverage_dependencies:capture=true",
        "spice/tasks/ops.py:rtk_usage_nudge:capture=true",
    },
    "release": {
        "spice/commandownership.py:_mounted_parent_version:capture=true",
        "spice/release.py:github_release_url:capture=true",
        "spice/release.py:run:capture=capture",
        "spice/releasenotes.py:is_ancestor:capture=true",
        "spice/tasks/graphs/handout.py:generate:capture=true",
    },
    "study": {"spice/studies/mutations.py:_collect_test_nodeids:capture=true"},
    "suite": {"spice/studies/suiteseam.py:_measure_suite:capture=stream"},
    "typecheck": {
        "spice/process/tool.py:run_typecheck_command:capture=true",
        "spice/studies/pythonruntime.py:_uv_project_interpreter:capture=true",
    },
}


def test_direct_subprocess_seams_match_the_explicit_policy_catalog():
    assert _direct_subprocess_seams() == EXPECTED_DIRECT_SUBPROCESS_SEAMS


def test_each_bounded_tool_policy_has_a_catalogued_production_caller():
    assert _tool_policy_callers() == EXPECTED_TOOL_POLICY_CALLERS


def test_extension_policy_admits_a_150_second_rust_gate_through_one_lookup(
    monkeypatch,
):
    command = ["cargo", "run", "--locked", "--package", "spice", "--", "gate"]
    synthetic_runtime_seconds = 150.0
    calls: list[tuple[str, float, str, str, tuple[str, ...]]] = []

    def fake_bounded(command, **kwargs):
        timeout = kwargs["timeout_seconds"]
        assert synthetic_runtime_seconds < timeout
        calls.append(
            (
                "buffered",
                timeout,
                kwargs["phase"],
                kwargs["input_label"],
                tuple(command),
            )
        )
        return subprocess.CompletedProcess(command, 0, b"", b"")

    def fake_streamed(command, **kwargs):
        timeout = kwargs["timeout_seconds"]
        calls.append(
            (
                "streamed",
                timeout,
                kwargs["phase"],
                kwargs["input_label"],
                tuple(command),
            )
        )
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr(tool, "run_bounded_process_group", fake_bounded)
    monkeypatch.setattr(tool, "run_streamed_process_group", fake_streamed)

    buffered = tool.run_tool_command(
        command,
        policy="extension",
        operation="Rust gate",
        capture_output=True,
    )
    streamed = tool.run_streamed_tool_command(
        command,
        policy="extension",
        operation="Rust gate",
        on_progress=lambda _output, _elapsed: None,
    )

    assert (
        tool.EXTENSION_TOOL_TIMEOUT_SECONDS == EXPECTED_EXTENSION_TOOL_TIMEOUT_SECONDS
    )
    assert (
        tool.TOOL_POLICY_TIMEOUT_SECONDS["extension"]
        == EXPECTED_EXTENSION_TOOL_TIMEOUT_SECONDS
    )
    assert buffered.returncode == streamed.returncode == 0
    assert calls == [
        (
            "buffered",
            EXPECTED_EXTENSION_TOOL_TIMEOUT_SECONDS,
            "tool.extension",
            "Rust gate",
            tuple(command),
        ),
        (
            "streamed",
            EXPECTED_EXTENSION_TOOL_TIMEOUT_SECONDS,
            "tool.extension",
            "Rust gate",
            tuple(command),
        ),
    ]


def test_extension_stall_reports_180_second_rust_gate_and_reaps(monkeypatch):
    command = ["cargo", "run", "--locked", "--package", "spice", "--", "gate"]
    communication: list[tuple[object, float]] = []
    reaped: list[object] = []

    class SyntheticStall:
        def communicate(self, *, input, timeout):
            communication.append((input, timeout))
            raise subprocess.TimeoutExpired(command, timeout)

    stalled = SyntheticStall()
    monkeypatch.setattr(groups.subprocess, "Popen", lambda *_args, **_kwargs: stalled)
    monkeypatch.setattr(
        groups, "_reap_expired_process_group", lambda process: reaped.append(process)
    )

    with pytest.raises(ProcessDeadlineExceeded) as exc_info:
        tool.run_tool_command(
            command,
            policy="extension",
            operation="Rust gate",
            capture_output=True,
        )

    error = exc_info.value
    assert error.phase == "tool.extension"
    assert error.input_label == "Rust gate"
    assert error.timeout_seconds == EXPECTED_EXTENSION_TOOL_TIMEOUT_SECONDS
    assert error.command == tuple(command)
    assert str(error) == (
        "process deadline exceeded phase=tool.extension input=Rust gate "
        "budget=180s command=cargo run --locked --package spice -- gate"
    )
    assert communication == [(None, EXPECTED_EXTENSION_TOOL_TIMEOUT_SECONDS)]
    assert reaped == [stalled]


@pytest.mark.parametrize("policy", sorted(tool.TOOL_POLICY_TIMEOUT_SECONDS))
def test_each_bounded_tool_policy_reports_stalled_command_identity(policy, monkeypatch):
    monkeypatch.setitem(tool.TOOL_POLICY_TIMEOUT_SECONDS, policy, 0.02)
    command = [sys.executable, "-c", "import time; time.sleep(30)"]

    try:
        tool.run_tool_command(
            command,
            policy=policy,
            operation=f"stalled {policy} representative",
            capture_output=True,
        )
    except ProcessDeadlineExceeded as exc:
        terminal = {
            "phase": exc.phase,
            "input": exc.input_label,
            "command": exc.command,
        }
    else:
        terminal = {"phase": "unexpected-success", "input": "", "command": ()}

    assert terminal == {
        "phase": f"tool.{policy}",
        "input": f"stalled {policy} representative",
        "command": tuple(command),
    }


@pytest.mark.parametrize(
    ("caller", "operation"),
    (
        ("serve", "run serve web typecheck"),
        ("python", "run Python typecheck"),
    ),
)
@pytest.mark.parametrize(
    ("returncode", "stdout", "stderr", "expected_error"),
    (
        (0, "", "", None),
        (
            7,
            "stdout failure\n",
            "",
            "typecheck-tool --flag 'two words' exited 7:\nstdout failure",
        ),
        (
            9,
            "",
            "stderr failure\n",
            "typecheck-tool --flag 'two words' exited 9:\nstderr failure",
        ),
    ),
)
def test_typecheck_callers_share_the_stable_exit_contract(
    tmp_path,
    monkeypatch,
    caller,
    operation,
    returncode,
    stdout,
    stderr,
    expected_error,
):
    argv = ("typecheck-tool", "--flag", "two words")
    calls: list[tuple[list[str], dict[str, object]]] = []

    def fake_run(command, **kwargs):
        calls.append((command, kwargs))
        return subprocess.CompletedProcess(command, returncode, stdout, stderr)

    monkeypatch.setattr(tool, "run_tool_command", fake_run)
    if caller == "serve":
        monkeypatch.setattr(
            serve_typecheck, "serve_web_js_targets", lambda _root: ("app.js",)
        )
        monkeypatch.setattr(serve_typecheck, "check_app_types_js", lambda _root: None)
        monkeypatch.setattr(
            serve_typecheck, "serve_web_typecheck_argv", lambda _targets: argv
        )

        def invoke():
            return serve_typecheck.run_serve_web_typecheck(tmp_path)

    else:
        monkeypatch.setattr(
            python_typecheck, "python_typecheck_targets", lambda _root: ("app",)
        )
        monkeypatch.setattr(
            python_typecheck,
            "python_typecheck_argv",
            lambda _root, _targets: argv,
        )

        def invoke():
            return python_typecheck.run_python_typecheck(tmp_path)

    if expected_error is None:
        assert invoke() is None
    else:
        with pytest.raises(SpiceError) as exc_info:
            invoke()
        assert str(exc_info.value) == expected_error

    assert calls == [
        (
            list(argv),
            {
                "policy": "typecheck",
                "operation": operation,
                "capture_output": True,
                "text": True,
                "cwd": tmp_path,
                "check": False,
            },
        )
    ]


def test_parent_lifetime_command_propagates_parent_cancellation(monkeypatch):
    events: list[str] = []

    def cancelled_parent(command, **_kwargs):
        events.append(f"started:{command[0]}")
        raise KeyboardInterrupt

    monkeypatch.setattr(tool.subprocess, "run", cancelled_parent)
    try:
        tool.run_parent_lifetime_command(["interactive-child"])
    except KeyboardInterrupt:
        events.append("parent-cancelled")

    assert events == ["started:interactive-child", "parent-cancelled"]


def _direct_subprocess_seams() -> set[str]:
    seams: set[str] = set()
    for path in sorted((PROJECT_ROOT / "spice").rglob("*.py")):
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        parents: dict[ast.AST, ast.AST] = {}
        for parent in ast.walk(tree):
            for child in ast.iter_child_nodes(parent):
                parents[child] = parent
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call) or not isinstance(
                node.func, ast.Attribute
            ):
                continue
            if not isinstance(node.func.value, ast.Name):
                continue
            if node.func.value.id != "subprocess" or node.func.attr not in {
                "run",
                "Popen",
            }:
                continue
            function = _enclosing_function(node, parents)
            relative = path.relative_to(PROJECT_ROOT).as_posix()
            seams.add(f"{relative}:{function}:{node.func.attr}")
    return seams


def _tool_policy_callers() -> dict[str, set[str]]:
    callers: dict[str, set[str]] = {}
    for path in sorted((PROJECT_ROOT / "spice").rglob("*.py")):
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        parents: dict[ast.AST, ast.AST] = {}
        for parent in ast.walk(tree):
            for child in ast.iter_child_nodes(parent):
                parents[child] = parent
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call) or not isinstance(node.func, ast.Name):
                continue
            if node.func.id not in TOOL_POLICY_ENTRY_POINTS:
                continue
            policy_keyword = next(
                (keyword for keyword in node.keywords if keyword.arg == "policy"),
                None,
            )
            if policy_keyword is None or not isinstance(
                policy_keyword.value, ast.Constant
            ):
                continue
            policy = policy_keyword.value.value
            if not isinstance(policy, str):
                continue
            function = _enclosing_function(node, parents)
            relative = path.relative_to(PROJECT_ROOT).as_posix()
            capture_keyword = next(
                (
                    keyword
                    for keyword in node.keywords
                    if keyword.arg == "capture_output"
                ),
                None,
            )
            capture = _capture_label(node.func.id, capture_keyword)
            callers.setdefault(policy, set()).add(
                f"{relative}:{function}:capture={capture}"
            )
    return callers


def _capture_label(entry_point: str, capture_keyword: ast.keyword | None) -> str:
    """Describe how one call site takes the child's output.

    The streamed entry point has no `capture_output` switch to read: it always
    both retains the complete output and forwards it while the child runs, so
    its call sites are catalogued under their own label rather than sharing the
    buffered surface's true/false/missing vocabulary.
    """
    if entry_point == "run_streamed_tool_command":
        return "stream"
    if capture_keyword is None:
        return "missing"
    return _ast_value_label(capture_keyword.value)


def _ast_value_label(node: ast.AST) -> str:
    if isinstance(node, ast.Constant):
        return str(node.value).lower()
    if isinstance(node, ast.Name):
        return node.id
    return type(node).__name__


def _enclosing_function(node: ast.AST, parents: dict[ast.AST, ast.AST]) -> str:
    current = node
    while current in parents:
        current = parents[current]
        if isinstance(current, (ast.FunctionDef, ast.AsyncFunctionDef)):
            return current.name
    return "<module>"
