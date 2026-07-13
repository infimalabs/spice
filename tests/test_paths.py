from __future__ import annotations

import subprocess
from pathlib import Path

from spice.paths import (
    git_common_dir,
    git_dir,
    shared_state_path,
    shared_state_root,
    worktree_state_path,
    worktree_state_root,
)


def test_canonical_state_roots_preserve_shared_and_linked_worktree_ownership(tmp_path):
    repo = _repo_with_linked_worktree(tmp_path)
    linked = tmp_path / "linked"

    assert shared_state_root(repo) == git_common_dir(repo) / ".spice"
    assert shared_state_root(linked) == shared_state_root(repo)
    assert worktree_state_root(repo) == git_dir(repo) / ".spice"
    assert worktree_state_root(linked) == git_dir(linked) / ".spice"
    assert shared_state_path(linked, "data/state.sqlite3") == (
        git_common_dir(repo) / ".spice" / "data" / "state.sqlite3"
    )
    assert worktree_state_path(linked, "agents/thread/state.json") == (
        git_dir(linked) / ".spice" / "agents" / "thread" / "state.json"
    )


def _repo_with_linked_worktree(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(
        ["git", "config", "user.email", "test@example.com"], cwd=repo, check=True
    )
    subprocess.run(["git", "config", "user.name", "Test"], cwd=repo, check=True)
    (repo / "README.md").write_text("test\n", encoding="utf-8")
    subprocess.run(["git", "add", "README.md"], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-qm", "init"], cwd=repo, check=True)
    subprocess.run(
        ["git", "worktree", "add", "-q", "-b", "linked", str(tmp_path / "linked")],
        cwd=repo,
        check=True,
    )
    return repo
