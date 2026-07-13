"""Measured Git checkpoints behind task-boundary merge corruption."""

from __future__ import annotations

import subprocess
from pathlib import Path

GIT_CONFLICT_EXIT_CODE = 1
GIT_FATAL_EXIT_CODE = 128


def test_historical_conflict_checkpoint_can_leave_markers_without_merge_parent(
    tmp_path: Path,
) -> None:
    repo, upstream_head = _conflicting_repositories(tmp_path)
    _run(repo, "git", "fetch", "origin")
    _install_rejecting_reference_transaction_hook(repo)
    rejected_merge = _run_unchecked(
        repo,
        "git",
        "merge",
        "--no-ff",
        "--no-commit",
        upstream_head,
    )
    rejected_merge_parent = _run_unchecked(
        repo, "git", "rev-parse", "--verify", "MERGE_HEAD"
    )
    hook_events_path = repo / "reference-transaction-events.log"
    hook_events = (
        hook_events_path.read_text(encoding="utf-8").splitlines()
        if hook_events_path.exists()
        else []
    )
    early_hook_checkpoint = {
        "porcelain_merge": (
            "rejected"
            if rejected_merge.returncode == GIT_FATAL_EXIT_CODE
            else "unexpected"
        ),
        "marker_tree": (
            "untouched"
            if (repo / "README.md").read_text(encoding="utf-8") == "agent work\n"
            else "changed"
        ),
        "reference_transactions": hook_events,
        "merge_parent": (
            "absent"
            if rejected_merge_parent.returncode == GIT_FATAL_EXIT_CODE
            else "present"
        ),
    }
    assert early_hook_checkpoint == {
        "porcelain_merge": "rejected",
        "marker_tree": "untouched",
        "reference_transactions": ["prepared ORIG_HEAD", "aborted ORIG_HEAD"],
        "merge_parent": "absent",
    }

    _reference_transaction_hook_path(repo).unlink()
    conflicted_merge = _run_unchecked(
        repo,
        "git",
        "merge",
        "--no-ff",
        "--no-commit",
        upstream_head,
    )
    merge_head_path = repo / _git(repo, "rev-parse", "--git-path", "MERGE_HEAD")
    parent_before_cleanup = _git(repo, "rev-parse", "--verify", "MERGE_HEAD")
    merge_head_path.unlink()
    parent_after_cleanup = _run_unchecked(
        repo, "git", "rev-parse", "--verify", "MERGE_HEAD"
    )
    marker_cleanup_checkpoint = {
        "porcelain_merge": (
            "conflicted"
            if conflicted_merge.returncode == GIT_CONFLICT_EXIT_CODE
            else "unexpected"
        ),
        "marker_tree": (
            "materialized"
            if "<<<<<<<" in (repo / "README.md").read_text(encoding="utf-8")
            else "missing"
        ),
        "merge_parent_before_cleanup": parent_before_cleanup,
        "cleanup": "removed MERGE_HEAD",
        "merge_parent_after_cleanup": (
            "absent"
            if parent_after_cleanup.returncode == GIT_FATAL_EXIT_CODE
            else "present"
        ),
    }
    assert marker_cleanup_checkpoint == {
        "porcelain_merge": "conflicted",
        "marker_tree": "materialized",
        "merge_parent_before_cleanup": upstream_head,
        "cleanup": "removed MERGE_HEAD",
        "merge_parent_after_cleanup": "absent",
    }


def test_historical_ref_first_checkpoint_records_parent_before_merged_tree(
    tmp_path: Path,
) -> None:
    repo = _repo_with_upstream(tmp_path)
    (repo / "agent.txt").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "agent.txt")
    _run(repo, "git", "commit", "-m", "agent work")
    agent_head = _git(repo, "rev-parse", "HEAD")
    agent_tree = _git(repo, "rev-parse", "HEAD^{tree}")
    _advance_upstream(tmp_path)
    _run(repo, "git", "fetch", "origin")
    upstream_head = _git(repo, "rev-parse", "origin/main")
    merged_tree = _git(repo, "merge-tree", "--write-tree", agent_head, upstream_head)
    merge_commit = _git(
        repo,
        "commit-tree",
        merged_tree,
        "-p",
        upstream_head,
        "-p",
        agent_head,
        "-m",
        "historical ref-first checkpoint",
    )

    # Freeze the old ref-first window: the branch parent advances while the
    # checked-out index and worktree still represent the agent tree.
    _run(repo, "git", "update-ref", "refs/heads/main", merge_commit, agent_head)
    historical_checkpoint = {
        "head": _git(repo, "rev-parse", "HEAD"),
        "head_tree": _git(repo, "rev-parse", "HEAD^{tree}"),
        "index_tree": _git(repo, "write-tree"),
        "worktree_baseline_path": (
            "present" if (repo / "baseline.txt").exists() else "absent"
        ),
    }
    assert historical_checkpoint == {
        "head": merge_commit,
        "head_tree": merged_tree,
        "index_tree": agent_tree,
        "worktree_baseline_path": "absent",
    }


def _conflicting_repositories(tmp_path: Path) -> tuple[Path, str]:
    repo = _repo_with_upstream(tmp_path)
    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "agent work")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(tmp_path / "remote.git"), str(peer))
    _configure_git_identity(peer)
    (peer / "README.md").write_text("baseline work\n", encoding="utf-8")
    (peer / "peer.txt").write_text("peer feature\n", encoding="utf-8")
    _run(peer, "git", "add", "README.md", "peer.txt")
    _run(peer, "git", "commit", "-m", "baseline work with peer feature")
    _run(peer, "git", "push", "origin", "main")
    return repo, _git(peer, "rev-parse", "HEAD")


def _repo_with_upstream(tmp_path: Path) -> Path:
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")
    return repo


def _advance_upstream(tmp_path: Path) -> None:
    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(tmp_path / "remote.git"), str(peer))
    _configure_git_identity(peer)
    (peer / "baseline.txt").write_text("baseline work\n", encoding="utf-8")
    _run(peer, "git", "add", "baseline.txt")
    _run(peer, "git", "commit", "-m", "baseline work")
    _run(peer, "git", "push", "origin", "main")


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


def _install_rejecting_reference_transaction_hook(repo: Path) -> None:
    hook = _reference_transaction_hook_path(repo)
    hook.write_text(
        "#!/bin/sh\n"
        "while read old new ref; do\n"
        '  printf \'%s %s\\n\' "$1" "$ref" >> reference-transaction-events.log\n'
        "done\n"
        'case "$1" in prepared) exit 1 ;; *) exit 0 ;; esac\n',
        encoding="utf-8",
    )
    hook.chmod(0o755)


def _reference_transaction_hook_path(repo: Path) -> Path:
    return repo / _git(repo, "rev-parse", "--git-path", "hooks/reference-transaction")


def _git(repo: Path, *args: str) -> str:
    return _run(repo, "git", *args).stdout.strip()


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)


def _run_unchecked(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=False, capture_output=True, text=True)
