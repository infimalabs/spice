"""Every synchronous subprocess uses a named deadline or lifetime contract."""

from __future__ import annotations

import ast
import sys
from pathlib import Path

import pytest

from spice.process.groups import ProcessDeadlineExceeded
from spice.process import tool

PROJECT_ROOT = Path(__file__).resolve().parents[1]
EXPECTED_DIRECT_SUBPROCESS_SEAMS = {
    "spice/agent/judgeadapter.py:main:run",
    "spice/agent/lifecycle.py:spawn_agent:Popen",
    "spice/agent/lifecycle.py:spawn_agent_supervisor:Popen",
    "spice/agent/watchdog.py:spawn_supervised_agent:Popen",
    "spice/process/groups.py:_force_windows_process_tree:run",
    "spice/process/groups.py:_posix_pid_has_live_state:run",
    "spice/process/groups.py:_posix_process_group_has_live_member:run",
    "spice/process/groups.py:run_bounded_process_group:Popen",
    "spice/process/tool.py:run_parent_lifetime_command:run",
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
        "spice/tasks/ops.py:rtk_usage_nudge:capture=true",
    },
    "release": {
        "spice/release.py:_is_ancestor:capture=true",
        "spice/release.py:github_release_url:capture=true",
        "spice/release.py:run:capture=capture",
        "spice/tasks/graphs/handout.py:generate:capture=true",
    },
    "study": {"spice/studies/mutations.py:_collect_test_nodeids:capture=true"},
    "typecheck": {
        "spice/serve/typecheck.py:_run_serve_web_typecheck_argv:capture=true",
        "spice/studies/typecheck.py:_uv_project_interpreter:capture=true",
        "spice/studies/typecheck.py:run_python_typecheck:capture=true",
    },
}


def test_direct_subprocess_seams_match_the_explicit_policy_catalog():
    assert _direct_subprocess_seams() == EXPECTED_DIRECT_SUBPROCESS_SEAMS


def test_each_bounded_tool_policy_has_a_catalogued_production_caller():
    assert _tool_policy_callers() == EXPECTED_TOOL_POLICY_CALLERS


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
            if node.func.id != "run_tool_command":
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
            capture = (
                _ast_value_label(capture_keyword.value)
                if capture_keyword is not None
                else "missing"
            )
            callers.setdefault(policy, set()).add(
                f"{relative}:{function}:capture={capture}"
            )
    return callers


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
