"""Deferred task creation coverage."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.tasks import config, create, identity, ops, tw

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    if shutil.which("task") is None:
        pytest.skip("Taskwarrior binary is required")
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-taskdeferred")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def test_deferred_creation_is_hidden_from_allocator_until_woken(task_repo):
    handle = create.add(
        "Deferred allocator task",
        project="task.unit",
        priority="medium",
        deferred=True,
    )
    row = identity.resolve(handle)

    assert str(row.get("wait") or "").startswith("2099")
    assert handle not in _ready_handles()

    output = ops.wake([handle])
    woken = identity.resolve(handle)

    assert f"woke {handle}: wait:" in output
    assert not str(woken.get("wait") or "")
    assert handle in _ready_handles()


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-b", "main")
    _run(path, "git", "config", "user.email", "spice@example.test")
    _run(path, "git", "config", "user.name", "Spice Tests")
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "initial")
    return path


def _ready_handles() -> set[str]:
    rows = tw.export(["status:pending", "+READY", "-ACTIVE"])
    return {
        identity.render_handle(row)
        for row in rows
        if "oops" not in (row.get("tags") or []) and not str(row.get("claim_by") or "")
    }


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
