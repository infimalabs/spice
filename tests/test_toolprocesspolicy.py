"""Every synchronous subprocess uses a named deadline or lifetime contract."""

from __future__ import annotations

import ast
import sys
from pathlib import Path

import pytest

from spice.procs import ProcessDeadlineExceeded
from spice import toolprocess

PROJECT_ROOT = Path(__file__).resolve().parents[1]
EXPECTED_DIRECT_SUBPROCESS_SEAMS = {
    "spice/agent/driver.py:operator_color_scheme:run",
    "spice/agent/judgeadapter.py:main:run",
    "spice/agent/lifecycle.py:_worktree_dirty:run",
    "spice/agent/lifecycle.py:git_tracks_relative_path:run",
    "spice/agent/lifecycle.py:spawn_agent:Popen",
    "spice/agent/lifecycle.py:spawn_agent_supervisor:Popen",
    "spice/agent/shadow.py:_git:run",
    "spice/agent/watchdog.py:spawn_supervised_agent:Popen",
    "spice/gitprocess.py:run_git_command:run",
    "spice/procs.py:_force_windows_process_tree:run",
    "spice/procs.py:_posix_pid_has_live_state:run",
    "spice/procs.py:_posix_process_group_has_live_member:run",
    "spice/procs.py:run_bounded_process_group:Popen",
    "spice/tasks/ops.py:rtk_usage_nudge:run",
    "spice/tasks/tw.py:run:run",
    "spice/toolprocess.py:run_parent_lifetime_command:run",
}


def test_direct_subprocess_seams_match_the_explicit_policy_catalog():
    assert _direct_subprocess_seams() == EXPECTED_DIRECT_SUBPROCESS_SEAMS


@pytest.mark.parametrize("policy", sorted(toolprocess.TOOL_POLICY_TIMEOUT_SECONDS))
def test_each_bounded_tool_policy_reports_stalled_command_identity(policy, monkeypatch):
    monkeypatch.setitem(toolprocess.TOOL_POLICY_TIMEOUT_SECONDS, policy, 0.02)
    command = [sys.executable, "-c", "import time; time.sleep(30)"]

    try:
        toolprocess.run_tool_command(
            command,
            policy=policy,
            operation=f"stalled {policy} representative",
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

    monkeypatch.setattr(toolprocess.subprocess, "run", cancelled_parent)
    try:
        toolprocess.run_parent_lifetime_command(["interactive-child"])
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


def _enclosing_function(node: ast.AST, parents: dict[ast.AST, ast.AST]) -> str:
    current = node
    while current in parents:
        current = parents[current]
        if isinstance(current, (ast.FunctionDef, ast.AsyncFunctionDef)):
            return current.name
    return "<module>"
