"""Single-install runtime hermeticity contracts."""

from __future__ import annotations

import sys
from pathlib import Path

from spice.agent import lifecycle, wrap
from spice.agent.driver import CLAUDE_DRIVER, DRIVER
from spice.cli import entry

INSTALLED_DISPATCH_EXIT_CODE = 41


def test_installed_spice_dispatches_without_worktree_reexec(tmp_path, monkeypatch):
    _write_spice_product_shape(tmp_path)
    seen: dict[str, object] = {}

    def fake_dispatch(argv: list[str]) -> int:
        seen["argv"] = list(argv)
        return INSTALLED_DISPATCH_EXIT_CODE

    def fail_execvpe(*_args, **_kwargs):
        raise AssertionError("spice entrypoint re-derived runtime from worktree")

    monkeypatch.setattr(entry, "repo_root_from_cwd", lambda: tmp_path)
    monkeypatch.setattr(entry, "_dispatch", fake_dispatch)
    monkeypatch.setattr(entry.os, "execvpe", fail_execvpe)

    assert entry.main(["task", "status"]) == INSTALLED_DISPATCH_EXIT_CODE
    assert seen == {"argv": ["task", "status"]}


def test_agent_runtime_env_uses_installed_tool_without_worktree_import_path(
    tmp_path,
    monkeypatch,
):
    _write_spice_product_shape(tmp_path)
    monkeypatch.delenv("PYTHONPATH", raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)

    direct_command_env = wrap.build_agent_run_environment(
        ["pytest"], repo_root=tmp_path
    )
    launch_env = lifecycle.agent_environment(tmp_path)

    assert direct_command_env is None
    assert "PYTHONPATH" not in launch_env


def test_python_routes_to_deployment_interpreter_when_worktree_has_venv(
    tmp_path,
    monkeypatch,
):
    worktree_python = tmp_path / ".venv" / "bin" / "python"
    worktree_python.parent.mkdir(parents=True)
    worktree_python.write_text("# worktree python placeholder\n", encoding="utf-8")
    monkeypatch.setenv("VIRTUAL_ENV", str(tmp_path / ".venv"))

    python_command = wrap.build_agent_run_command(
        ["python", "-c", "import sys"], repo_root=tmp_path
    )
    python3_command = wrap.build_agent_run_command(
        ["python3", "-c", "import sys"], repo_root=tmp_path
    )

    assert python_command == [sys.executable, "-c", "import sys"]
    assert python3_command == [sys.executable, "-c", "import sys"]


def test_worktree_selection_operates_worker_without_runtime_rederivation(
    tmp_path,
    monkeypatch,
):
    main_tree = tmp_path / "main"
    worker_tree = tmp_path / "worker"
    main_tree.mkdir()
    worker_tree.mkdir()
    _write_spice_product_shape(worker_tree)
    seen: dict[str, object] = {}

    def fake_resolve(target: str, *, cwd: Path) -> Path:
        seen["target"] = target
        seen["resolve_cwd"] = cwd
        return worker_tree

    def fake_dispatch(argv: list[str]) -> int:
        seen["argv"] = list(argv)
        seen["dispatch_cwd"] = Path.cwd()
        return INSTALLED_DISPATCH_EXIT_CODE

    def fail_execvpe(*_args, **_kwargs):
        raise AssertionError("worker worktree re-derived spice runtime")

    monkeypatch.chdir(main_tree)
    monkeypatch.setattr(entry, "repo_root_from_cwd", lambda: main_tree)
    monkeypatch.setattr(entry, "resolve_worktree_target", fake_resolve)
    monkeypatch.setattr(entry, "_dispatch", fake_dispatch)
    monkeypatch.setattr(entry.os, "execvpe", fail_execvpe)

    assert (
        entry.main(["--worktree", "worker", "task", "status"])
        == INSTALLED_DISPATCH_EXIT_CODE
    )
    assert seen == {
        "target": "worker",
        "resolve_cwd": main_tree,
        "argv": ["task", "status"],
        "dispatch_cwd": worker_tree,
    }


def _write_spice_product_shape(repo_root: Path) -> None:
    for relative in (
        "spice/__main__.py",
        "spice/cli/entry.py",
        "spice/agent/wrap.py",
    ):
        path = repo_root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("# test spice product shape\n", encoding="utf-8")
