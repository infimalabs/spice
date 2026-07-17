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


def test_clean_discarded_merge_recipe_restores_staged_content(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    head = _git(repo, "rev-parse", "HEAD")
    peer = _peer_change(repo, "peer.txt", "peer content\n")
    _git(repo, "merge", "--no-ff", "--no-commit", peer)
    _git(repo, "read-tree", "HEAD")

    diagnostic = precommit._merge_integrity_diagnostic(repo)
    assert "Recover without removing MERGE_HEAD:" in diagnostic
    completed = _run_recipe(repo, diagnostic)
    assert completed.returncode == 0

    precommit._run_merge_integrity_guard(repo)
    _git(repo, "commit", "--no-verify", "-m", "recovered clean merge")

    assert _git(repo, "show", "-s", "--format=%P", "HEAD") == f"{head} {peer}"
    assert _git(repo, "show", "HEAD:peer.txt") == "peer content"


def test_conflicted_discarded_merge_recipe_restarts_the_merge(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    peer = _peer_change(repo, "story.txt", "peer resolution\n")
    _write(repo / "story.txt", "main resolution\n")
    _git(repo, "add", "story.txt")
    _git(repo, "commit", "--no-verify", "-m", "main change")
    head = _git(repo, "rev-parse", "HEAD")
    _run(repo, "merge", "--no-ff", peer, check=False)
    _git(repo, "read-tree", "HEAD")

    diagnostic = precommit._merge_integrity_diagnostic(repo)
    assert "The discarded merge is conflicted; restart it:" in diagnostic
    assert "git --literal-pathspecs checkout HEAD -- story.txt" in diagnostic
    _run_recipe(repo, diagnostic)

    assert _git(repo, "rev-parse", "MERGE_HEAD") == peer
    stages = _git(repo, "ls-files", "--unmerged")
    assert [line.split("\t")[1] for line in stages.splitlines()] == [
        "story.txt",
        "story.txt",
        "story.txt",
    ]
    _write(repo / "story.txt", "combined resolution\n")
    _git(repo, "add", "story.txt")

    precommit._run_merge_integrity_guard(repo)
    _git(repo, "commit", "--no-verify", "-m", "recovered conflicted merge")

    assert _git(repo, "show", "-s", "--format=%P", "HEAD") == f"{head} {peer}"
    assert _git(repo, "show", "HEAD:story.txt") == "combined resolution"


def test_modify_delete_recipe_restarts_with_head_absent_path(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    _git(repo, "switch", "-c", "peer")
    _write(repo / "story.txt", "peer integrated content\n")
    _git(repo, "add", "story.txt")
    _git(repo, "commit", "--no-verify", "-m", "peer modifies story")
    peer = _git(repo, "rev-parse", "HEAD")
    _git(repo, "switch", "main")
    _git(repo, "rm", "story.txt")
    _git(repo, "commit", "--no-verify", "-m", "main deletes story")
    head = _git(repo, "rev-parse", "HEAD")
    _run(repo, "merge", "--no-ff", peer, check=False)
    _git(repo, "read-tree", "HEAD")
    _write(repo / "unrelated.txt", "unrelated work survives\n")

    diagnostic = precommit._merge_integrity_diagnostic(repo)
    assert "The discarded merge is conflicted; restart it:" in diagnostic
    assert "git --literal-pathspecs clean -f -d -x -- story.txt" in diagnostic
    completed = _run_recipe(repo, diagnostic)
    assert completed.returncode == 0

    assert _git(repo, "rev-parse", "MERGE_HEAD") == peer
    stages = _git(repo, "ls-files", "--unmerged")
    assert [line.split("\t")[1] for line in stages.splitlines()] == [
        "story.txt",
        "story.txt",
    ]
    assert (repo / "unrelated.txt").read_text(encoding="utf-8") == (
        "unrelated work survives\n"
    )
    _write(repo / "story.txt", "combined restored content\n")
    _git(repo, "add", "story.txt")

    precommit._run_merge_integrity_guard(repo)
    _git(repo, "commit", "--no-verify", "-m", "recovered modify-delete merge")

    assert _git(repo, "show", "-s", "--format=%P", "HEAD") == f"{head} {peer}"
    assert _git(repo, "show", "HEAD:story.txt") == "combined restored content"


def _run_recipe(repo: Path, diagnostic: str) -> subprocess.CompletedProcess[str]:
    """Execute the diagnostic's printed recovery lines exactly as printed."""
    recipe = "\n".join(
        line.strip() for line in diagnostic.splitlines() if line.startswith("  ")
    )
    return subprocess.run(
        ["bash", "-c", recipe],
        capture_output=True,
        cwd=repo,
        text=True,
    )


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
