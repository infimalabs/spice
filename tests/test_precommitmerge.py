"""Positive Git-state coverage for the pre-commit merge-integrity guard."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

from spice.hooks import precommit

PROJECT_ROOT = Path(__file__).resolve().parents[1]


def test_merge_integrity_guard_accepts_clean_merge_tree(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    head = _git(repo, "rev-parse", "HEAD")
    peer = _peer_change(repo, "peer.txt", "peer content\n")
    _git(repo, "merge", "--no-ff", "--no-commit", peer)

    precommit._run_merge_integrity_guard(repo)
    _git(repo, "commit", "--no-verify", "-m", "clean merge")

    assert _git(repo, "show", "-s", "--format=%P", "HEAD") == f"{head} {peer}"
    assert _git(repo, "show", "HEAD:peer.txt") == "peer content"


def test_merge_integrity_guard_accepts_resolved_conflict_tree(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    peer = _peer_change(repo, "story.txt", "peer resolution\n")
    _write(repo / "story.txt", "main resolution\n")
    _git(repo, "add", "story.txt")
    _git(repo, "commit", "--no-verify", "-m", "main change")
    head = _git(repo, "rev-parse", "HEAD")
    _run(repo, "merge", "--no-ff", "--no-commit", peer, check=False)
    _write(repo / "story.txt", "combined resolution\n")
    _git(repo, "add", "story.txt")

    precommit._run_merge_integrity_guard(repo)
    _git(repo, "commit", "--no-verify", "-m", "resolved merge")

    assert _git(repo, "show", "-s", "--format=%P", "HEAD") == f"{head} {peer}"
    assert _git(repo, "show", "HEAD:story.txt") == "combined resolution"


def test_merge_integrity_guard_accepts_ordinary_commit(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    _write(repo / "ordinary.txt", "ordinary change\n")
    _git(repo, "add", "ordinary.txt")

    precommit._run_merge_integrity_guard(repo)
    _git(repo, "commit", "--no-verify", "-m", "ordinary commit")

    assert _git(repo, "show", "HEAD:ordinary.txt") == "ordinary change"


def _repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    _run(tmp_path, "init", "-b", "main", str(repo))
    _git(repo, "config", "user.email", "spice@example.test")
    _git(repo, "config", "user.name", "Spice Tests")
    _write(repo / "story.txt", "base story\n")
    _git(repo, "add", "story.txt")
    _git(repo, "commit", "--no-verify", "-m", "base")
    return repo


def _peer_change(repo: Path, name: str, text: str) -> str:
    _git(repo, "switch", "-c", "peer")
    _write(repo / name, text)
    _git(repo, "add", name)
    _git(repo, "commit", "--no-verify", "-m", "peer change")
    peer = _git(repo, "rev-parse", "HEAD")
    _git(repo, "switch", "main")
    return peer


def _write(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")


def _git(repo: Path, *args: str) -> str:
    return _run(repo, *args).stdout.strip()


def _run(
    repo: Path, *args: str, check: bool = True
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()  # env-policy: allow
    env["PYTHONPATH"] = os.pathsep.join(
        entry for entry in (str(PROJECT_ROOT), env.get("PYTHONPATH", "")) if entry
    )
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        check=False,
        env=env,
        text=True,
    )
    if check and result.returncode != 0:
        raise AssertionError(
            f"git {' '.join(args)} failed with {result.returncode}\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result
