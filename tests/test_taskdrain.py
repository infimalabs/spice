"""Drain allocator regression coverage."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.serve.teams import ServeTeamStore, TeamConfig
from spice.tasks import config, identity, ops

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
PEER_ACTOR = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-drain")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def test_drain_phase_boundary_sees_configured_assignable_stem(task_repo, monkeypatch):
    (task_repo / "pyproject.toml").write_text(
        '[tool.spice.tasks]\nstems = ["paintball"]\n', encoding="utf-8"
    )
    _run(task_repo, "git", "add", "pyproject.toml")
    _run(task_repo, "git", "commit", "-m", "configure paintball stem")
    ServeTeamStore().create_team(
        members=[ACTOR_A, PEER_ACTOR], config=TeamConfig(lifetime="Drain")
    )
    handle = ops.add(
        "Drain sees configured stem",
        project="paintball.docs",
        priority="medium",
        acceptance=["drain sees repo-defined assignable stems"],
    )

    assigned = ops.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert assigned["project"] == "paintball.docs"

    ops.done(handle, validation=["implementation complete"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    review = ops.next_task()

    assert identity.render_handle(review or {}) == handle
    assert review["phase"] == "review"
    assert review["project"] == "paintball.docs"


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-b", "main")
    _configure_git_identity(path)
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "initial")
    return path


def _configure_git_identity(repo: Path) -> None:
    _run(repo, "git", "config", "user.email", "spice@example.test")
    _run(repo, "git", "config", "user.name", "Spice Tests")


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
