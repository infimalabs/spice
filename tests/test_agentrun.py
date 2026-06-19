"""Agent-run dispatch stays usable while the worktree is conflicted."""

from __future__ import annotations

import builtins
import io

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
