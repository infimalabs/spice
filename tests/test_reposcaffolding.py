"""Shared repository and task-backend scaffolding for tests.

The helpers stay purpose-specific because the small Git fixtures deliberately
do not all establish the same state: some receive an existing directory, some
configure an identity, and only ``init_committed_repo`` creates a first commit.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.tasks import config

TASK_ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


def run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run one checked command and retain both output streams for assertions."""
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)


def init_committed_repo(path: Path) -> Path:
    """Create an identified main-branch repository with one initial commit."""
    path.mkdir()
    run(path, "git", "init", "-b", "main")
    run(path, "git", "config", "user.email", "spice@example.test")
    run(path, "git", "config", "user.name", "Spice Tests")
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    run(path, "git", "add", "README.md")
    run(path, "git", "commit", "-m", "initial")
    return path


def init_existing_repo(path: Path) -> None:
    """Initialize an existing directory without identity or a commit."""
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)


def init_identified_repo(path: Path) -> None:
    """Create a repository with an identity but no initial commit."""
    path.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)
    subprocess.run(
        ["git", "config", "user.email", "spice@example.test"], cwd=path, check=True
    )
    subprocess.run(["git", "config", "user.name", "Spice Tests"], cwd=path, check=True)


def init_empty_repo(path: Path) -> Path:
    """Create an output-captured repository without identity or a commit."""
    path.mkdir()
    run(path, "git", "init", "-b", "main")
    return path


def init_quiet_empty_repo(path: Path) -> Path:
    """Create a quiet repository without identity or a commit."""
    path.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)
    return path


def make_task_repo_fixture(repo_initializer, *, actor: str = TASK_ACTOR_A):
    """Build the common Taskwarrior fixture around a caller's repo policy."""

    @pytest.fixture
    def task_repo(tmp_path, monkeypatch):
        repo = repo_initializer(tmp_path / "repo")
        backend = tmp_path / "task-backend"
        monkeypatch.chdir(repo)
        monkeypatch.setenv(DRIVER.thread_id_env, actor)
        monkeypatch.setenv("CODEX_TURN_ID", "turn-a")
        config.set_backend(str(backend))
        try:
            yield repo
        finally:
            config.set_backend(None)

    return task_repo
