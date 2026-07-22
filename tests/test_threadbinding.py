"""Worktree thread binding driven from the hook points, not from activation."""

from __future__ import annotations

import argparse
import io
import json
from pathlib import Path
import subprocess
from typing import Any

import pytest

from spice.agent import cli as agent_cli
from spice.agent import wrap
from spice.agent.driver import (
    CLAUDE_DRIVER,
    CODEX_DRIVER,
    DRIVER,
    POST_TOOL_HOOK_EVENT,
    SPICE_AGENT_DRIVER_ENV,
)
from spice.agent.lifecyclebinding import (
    agent_state_path,
    agent_status,
    bind_ambient_agent_thread,
)
from spice.agent.paths import read_agent_thread_pointer
from spice.mail.inbox import compose_inbox_text, write_inbox_item

AMBIENT_THREAD = "f2249a9f-b996-41e2-9e18-54cb381cc634"
CANONICAL_THREAD = "f2249a9fb99641e29e1854cb381cc634"
SUCCESSOR_THREAD = "3c1d7e04-5a2b-4f6c-8d9e-0a1b2c3d4e5f"
CANONICAL_SUCCESSOR = "3c1d7e045a2b4f6c8d9e0a1b2c3d4e5f"


@pytest.fixture(autouse=True)
def _git_worktree_tmp_path(tmp_path: Path) -> None:
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)


@pytest.fixture
def ambient(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(DRIVER.thread_id_env, AMBIENT_THREAD)


def _post_tool_hook_args(repo_root: Path) -> argparse.Namespace:
    return argparse.Namespace(
        agent_action="post-tool-hook",
        repo_root=str(repo_root),
        event_name=POST_TOOL_HOOK_EVENT,
    )


def _fire_post_tool_hook(repo_root: Path) -> int:
    return agent_cli.handle_agent(_post_tool_hook_args(repo_root))


def _state_record(repo_root: Path) -> dict[str, Any]:
    return json.loads(agent_state_path(repo_root).read_text(encoding="utf-8"))


def test_post_tool_hook_binds_ambient_thread_to_a_worktree_never_activated(
    tmp_path: Path,
    ambient: None,
    capsys: Any,
) -> None:
    """A single hook fire is enough to make the lane discoverable."""
    assert read_agent_thread_pointer(tmp_path) == ""

    assert _fire_post_tool_hook(tmp_path) == 0

    capsys.readouterr()
    assert agent_status(tmp_path).thread_id == CANONICAL_THREAD
    assert read_agent_thread_pointer(tmp_path) == CANONICAL_THREAD
    record = _state_record(tmp_path)
    assert record["thread_id"] == CANONICAL_THREAD
    assert record["mode"] == "bind"
    assert record["driver"] == DRIVER.name


def test_agent_run_stage_binds_ambient_thread(
    tmp_path: Path,
    ambient: None,
) -> None:
    """A lane bound only through ordinary shell commands is discoverable."""
    stderr = io.StringIO()

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["--", "true"],
        popen_factory=lambda *args, **kwargs: _CompletedProcess(),
        stderr=stderr,
    )

    assert exit_code == 0
    assert agent_status(tmp_path).thread_id == CANONICAL_THREAD
    assert read_agent_thread_pointer(tmp_path) == CANONICAL_THREAD


def test_activation_binding_after_a_hook_fire_writes_identical_state(
    tmp_path: Path,
    ambient: None,
    capsys: Any,
) -> None:
    """Both entry points share one binding function, so both converge."""
    _fire_post_tool_hook(tmp_path)
    capsys.readouterr()
    after_hook = agent_state_path(tmp_path).read_bytes()

    status = bind_ambient_agent_thread(tmp_path)

    assert status.thread_id == CANONICAL_THREAD
    assert agent_state_path(tmp_path).read_bytes() == after_hook


def test_repeated_hook_fires_rebind_the_same_thread_without_rewriting_state(
    tmp_path: Path,
    ambient: None,
    capsys: Any,
) -> None:
    """Writes stay epoch-frequency even though the hook fires per command."""
    _fire_post_tool_hook(tmp_path)
    capsys.readouterr()
    first = agent_state_path(tmp_path)
    first_stat = first.stat().st_mtime_ns
    first_bytes = first.read_bytes()

    for _ in range(3):
        _fire_post_tool_hook(tmp_path)
    capsys.readouterr()

    assert agent_status(tmp_path).thread_id == CANONICAL_THREAD
    assert first.stat().st_mtime_ns == first_stat
    assert first.read_bytes() == first_bytes


def test_hook_fire_rebinds_when_the_ambient_thread_changes(
    tmp_path: Path,
    ambient: None,
    monkeypatch: pytest.MonkeyPatch,
    capsys: Any,
) -> None:
    """A renewed session rebinds the worktree to the successor thread."""
    _fire_post_tool_hook(tmp_path)
    capsys.readouterr()
    assert agent_status(tmp_path).thread_id == CANONICAL_THREAD

    monkeypatch.setenv(DRIVER.thread_id_env, SUCCESSOR_THREAD)
    _fire_post_tool_hook(tmp_path)
    capsys.readouterr()

    assert agent_status(tmp_path).thread_id == CANONICAL_SUCCESSOR
    assert read_agent_thread_pointer(tmp_path) == CANONICAL_SUCCESSOR


def test_hook_fire_without_an_ambient_thread_keeps_state_and_delivers_steering(
    tmp_path: Path,
    ambient: None,
    monkeypatch: pytest.MonkeyPatch,
    capsys: Any,
) -> None:
    """Steering delivery never depends on the binding finding a thread."""
    _fire_post_tool_hook(tmp_path)
    capsys.readouterr()
    bound = agent_state_path(tmp_path).read_bytes()
    write_inbox_item(
        tmp_path,
        "1jN54zJP.txt",
        compose_inbox_text(
            body="steering with no ambient thread", priority=None, stop=False
        ),
    )

    monkeypatch.delenv(DRIVER.thread_id_env)
    assert _fire_post_tool_hook(tmp_path) == 0

    response = json.loads(capsys.readouterr().out)
    context = response["hookSpecificOutput"]["additionalContext"]
    assert "steering with no ambient thread" in context
    assert agent_state_path(tmp_path).read_bytes() == bound
    assert agent_status(tmp_path).thread_id == CANONICAL_THREAD


def test_bind_refuses_a_foreign_worktree_thread_and_keeps_the_local_one(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A crossed hook offering another lane's session id is refused, not seated.

    Seating a thread whose conversation lives under a different worktree is the
    seed of the spice-e brick: `ensure` would later resume-loop on a session
    this worktree can never open. The bind keeps the worktree's own thread.
    """
    config_dir = tmp_path / "claude"
    projects = config_dir / "projects"
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(config_dir))
    monkeypatch.setenv(SPICE_AGENT_DRIVER_ENV, "claude")
    monkeypatch.delenv(CODEX_DRIVER.thread_id_env, raising=False)

    local_dashed = "768bcba1-a66f-4d22-9ce7-bcf65b5d16aa"
    local_canonical = "768bcba1a66f4d229ce7bcf65b5d16aa"
    foreign_dashed = "019f8806-85c0-7312-b89f-6bfc6cdd0bb5"
    foreign_canonical = "019f880685c07312b89f6bfc6cdd0bb5"

    # A legitimate local session records this worktree's own cwd, so its ambient
    # thread binds normally.
    local_project = projects / "-local"
    local_project.mkdir(parents=True)
    (local_project / f"{local_dashed}.jsonl").write_text(
        json.dumps({"type": "user", "cwd": str(tmp_path.resolve())}) + "\n",
        encoding="utf-8",
    )
    monkeypatch.setenv(CLAUDE_DRIVER.thread_id_env, local_dashed)
    bound = bind_ambient_agent_thread(tmp_path)
    assert bound.thread_id == local_canonical

    # A crossed hook now offers another worktree's session id, its transcript
    # recorded under a different cwd.
    other_root = tmp_path / "wt-other"
    other_root.mkdir()
    foreign_project = projects / "-wt-other"
    foreign_project.mkdir(parents=True)
    (foreign_project / f"{foreign_dashed}.jsonl").write_text(
        json.dumps({"type": "user", "cwd": str(other_root.resolve())}) + "\n",
        encoding="utf-8",
    )
    monkeypatch.setenv(CLAUDE_DRIVER.thread_id_env, foreign_dashed)
    after = bind_ambient_agent_thread(tmp_path)

    # The foreign id is refused; the worktree keeps its own thread, distinct from
    # the crisscross candidate that would have bricked its next start.
    assert after.thread_id == local_canonical
    assert after.thread_id != foreign_canonical
    assert read_agent_thread_pointer(tmp_path) == local_canonical


def test_bind_seats_a_new_session_whose_transcript_is_not_written_yet(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An absent transcript is a fresh session mid-startup, not a foreign one.

    The guard refuses only *provably* foreign threads; a session that has yet to
    flush its transcript must still bind, or a lane could never record its own
    first thread.
    """
    config_dir = tmp_path / "claude"
    (config_dir / "projects").mkdir(parents=True)
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(config_dir))
    monkeypatch.setenv(SPICE_AGENT_DRIVER_ENV, "claude")
    monkeypatch.delenv(CODEX_DRIVER.thread_id_env, raising=False)

    fresh_dashed = "3c1d7e04-5a2b-4f6c-8d9e-0a1b2c3d4e5f"
    fresh_canonical = "3c1d7e045a2b4f6c8d9e0a1b2c3d4e5f"
    monkeypatch.setenv(CLAUDE_DRIVER.thread_id_env, fresh_dashed)

    status = bind_ambient_agent_thread(tmp_path)

    assert status.thread_id == fresh_canonical
    assert read_agent_thread_pointer(tmp_path) == fresh_canonical


class _CompletedProcess:
    """A spawned command that exits cleanly, so the run stage is exercised."""

    pid = 4242

    def wait(self) -> int:
        return 0
