"""Agent-run dispatch stays usable while the worktree is conflicted."""

from __future__ import annotations

import builtins
import io
from pathlib import Path

from spice.agent import wrap
from spice.cli import entry

FAKE_AGENT_RUN_EXIT_CODE = 23


def test_agent_run_dispatch_bypasses_full_parser_and_inbox_import(
    tmp_path, monkeypatch
):
    seen: dict[str, object] = {}

    def fake_run_agent_command(repo_root, raw_args):
        seen["repo_root"] = repo_root
        seen["raw_args"] = raw_args
        return FAKE_AGENT_RUN_EXIT_CODE

    monkeypatch.setattr(wrap, "run_agent_command", fake_run_agent_command)
    monkeypatch.setattr(entry, "repo_root_from_cwd", lambda: tmp_path)
    real_import = builtins.__import__

    def guarded_import(name, globals=None, locals=None, fromlist=(), level=0):
        if name in {"spice.cli.parser", "spice.mail.inbox", "spice.serve.cli"}:
            raise AssertionError(f"agent run fast path imported {name}")
        return real_import(name, globals, locals, fromlist, level)

    monkeypatch.setattr(builtins, "__import__", guarded_import)

    assert (
        entry._dispatch(["agent", "run", "--", "git", "status"])
        == FAKE_AGENT_RUN_EXIT_CODE
    )
    assert seen == {"repo_root": tmp_path, "raw_args": ["--", "git", "status"]}


def test_main_does_not_reexec_from_spice_source_worktree(tmp_path, monkeypatch):
    _write_spice_product_shape(tmp_path)
    seen: dict[str, list[str]] = {}

    def fake_dispatch(argv: list[str]) -> int:
        seen["argv"] = argv
        return FAKE_AGENT_RUN_EXIT_CODE

    def fail_exec(*_args, **_kwargs):
        raise AssertionError("spice entry unexpectedly re-execed")

    monkeypatch.setattr(entry, "repo_root_from_cwd", lambda: tmp_path)
    monkeypatch.setattr(entry, "_dispatch", fake_dispatch)
    monkeypatch.setattr(entry.os, "execvpe", fail_exec)

    assert entry.main(["task", "status"]) == FAKE_AGENT_RUN_EXIT_CODE
    assert seen == {"argv": ["task", "status"]}


def test_worktree_route_leaves_spice_invocations_on_installed_runtime(tmp_path):
    _write_spice_product_shape(tmp_path)

    assert wrap.worktree_route_command(
        ["spice", "task", "status"], repo_root=tmp_path
    ) == ["spice", "task", "status"]
    assert wrap.worktree_route_command(
        ["uv", "run", "spice", "task", "status"], repo_root=tmp_path
    ) == ["uv", "run", "spice", "task", "status"]


def test_agent_run_inbox_injection_degrades_when_readout_import_fails(
    tmp_path, monkeypatch
):
    inbox = tmp_path / ".spice" / "inbox"
    inbox.mkdir(parents=True)
    (inbox / "20260101T000000000001Z.txt").write_text("pending\n", encoding="utf-8")
    real_import = builtins.__import__

    def guarded_import(name, globals=None, locals=None, fromlist=(), level=0):
        if name == "spice.mail.readout":
            raise SyntaxError("conflicted inbox readout")
        return real_import(name, globals, locals, fromlist, level)

    monkeypatch.setattr(builtins, "__import__", guarded_import)
    stderr = io.StringIO()

    wrap.AgentInboxInjector(tmp_path, stderr=stderr).inject(force=True)

    assert "Inbox Steering" in stderr.getvalue()
    assert "unavailable=conflicted inbox readout" in stderr.getvalue()


def _write_spice_product_shape(repo_root: Path) -> None:
    for relative in (
        "spice/__main__.py",
        "spice/cli/entry.py",
        "spice/agent/wrap.py",
    ):
        path = repo_root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("# test spice product shape\n", encoding="utf-8")
