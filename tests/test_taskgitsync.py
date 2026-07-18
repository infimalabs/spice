"""Task git conflict recovery, landing guards, and helper behavior."""

from __future__ import annotations

import subprocess
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

import pytest

from spice.process import git
from spice.errors import SpiceError
from spice.process.groups import ProcessDeadlineExceeded
from spice.tasks import gitsync

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


@dataclass(frozen=True)
class GitsyncOutcome:
    state: str
    message: str


def _gitsync_outcome(operation: Callable[[], object]) -> GitsyncOutcome:
    try:
        operation()
    except gitsync.MergeConflict as exc:
        return GitsyncOutcome("recoverable-conflict", str(exc))
    except SpiceError as exc:
        return GitsyncOutcome("rejected", str(exc))
    return GitsyncOutcome("published", "publication completed")


def test_integrate_and_publish_conflict_guides_resolution_and_retry(tmp_path):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")

    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "agent work")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "README.md").write_text("baseline work\n", encoding="utf-8")
    _run(peer, "git", "add", "README.md")
    _run(peer, "git", "commit", "-m", "baseline work")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")

    with pytest.raises(gitsync.MergeConflict) as exc_info:
        gitsync.integrate_and_publish("TASK-1jN54zJL", repo_root=repo)

    message = str(exc_info.value)
    assert "README.md" in message
    assert "keep the merge state open" in message
    assert "commit while MERGE_HEAD exists" in message
    assert "git status --short" in message
    assert "git rev-parse --verify MERGE_HEAD" in message
    assert "git add -- README.md" in message
    assert 'spice task done TASK-1jN54zJL --validation "..."' in message
    assert _git(repo, "rev-parse", "--verify", "MERGE_HEAD") == upstream_head
    assert _git(repo, "status", "--porcelain") == "UU README.md"

    (repo / "README.md").write_text("resolved work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(
        repo,
        "git",
        "commit",
        "-m",
        "Resolve baseline overlap for TASK-1jN54zJL",
    )

    result = gitsync.integrate_and_publish("TASK-1jN54zJL", repo_root=repo)
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert captured["done_upstream_head"] == upstream_head
    assert _merge_parents(repo, merge_head)[0] == upstream_head
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""


def test_integrate_and_publish_refuses_landing_that_rewinds_peer_paths(tmp_path):
    # The merge-storm shape: a conflicted lane resolves the baseline overlap
    # by keeping its own side wholesale, silently dropping peer work its task
    # never touched. The landing must be refused before anything publishes,
    # and the refusal recipe must lead to a clean republish.
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")

    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "agent work")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "README.md").write_text("baseline work\n", encoding="utf-8")
    (peer / "peer.txt").write_text("peer feature\n", encoding="utf-8")
    _run(peer, "git", "add", "README.md", "peer.txt")
    _run(peer, "git", "commit", "-m", "peer feature")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")

    with pytest.raises(gitsync.MergeConflict):
        gitsync.integrate_and_publish("TASK-1jN54zJS", repo_root=repo)

    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "rm", "-f", "peer.txt")
    _run(repo, "git", "commit", "-m", "Resolve baseline overlap sloppily")

    with pytest.raises(SpiceError) as exc_info:
        gitsync.integrate_and_publish("TASK-1jN54zJS", repo_root=repo)

    message = str(exc_info.value)
    assert "refusing to publish" in message
    assert "peer.txt" in message
    assert f"git checkout {upstream_head} -- peer.txt" in message
    assert "peer work already landed on the shared branch" in message
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == (
        upstream_head
    )

    _run(repo, "git", "checkout", upstream_head, "--", "peer.txt")
    _run(repo, "git", "commit", "-m", "Restore baseline content")

    result = gitsync.integrate_and_publish("TASK-1jN54zJS", repo_root=repo)
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert _merge_parents(repo, merge_head)[0] == upstream_head
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert (repo / "peer.txt").read_text(encoding="utf-8") == "peer feature\n"
    assert (repo / "README.md").read_text(encoding="utf-8") == "agent work\n"
    assert _git(repo, "status", "--porcelain") == ""


def test_integrate_and_publish_refuses_rename_detected_peer_deletion(tmp_path):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")

    similar = "".join(f"shared line {index}\n" for index in range(80))
    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    (repo / "replacement.txt").write_text(similar, encoding="utf-8")
    _run(repo, "git", "add", "README.md", "replacement.txt")
    _run(repo, "git", "commit", "-m", "agent adds replacement file")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "README.md").write_text("baseline work\n", encoding="utf-8")
    (peer / "peer.txt").write_text(similar, encoding="utf-8")
    _run(peer, "git", "add", "README.md", "peer.txt")
    _run(peer, "git", "commit", "-m", "peer adds similar file")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")

    with pytest.raises(gitsync.MergeConflict):
        gitsync.integrate_and_publish("TASK-1jN54zJV", repo_root=repo)

    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "rm", "-f", "peer.txt")
    _run(repo, "git", "commit", "-m", "Resolve baseline overlap sloppily")

    with pytest.raises(SpiceError) as exc_info:
        gitsync.integrate_and_publish("TASK-1jN54zJV", repo_root=repo)

    message = str(exc_info.value)
    assert "refusing to publish" in message
    assert "peer.txt" in message
    assert f"git checkout {upstream_head} -- peer.txt" in message
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == (
        upstream_head
    )


def test_integrate_and_publish_allows_task_owned_rename(tmp_path):
    repo = _repo_with_upstream(tmp_path)
    _run(repo, "git", "mv", "README.md", "NOTES.md")
    _run(repo, "git", "commit", "-m", "rename readme")

    result = gitsync.integrate_and_publish("TASK-1jN54zJW", repo_root=repo)
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "ls-files") == "NOTES.md"
    assert (repo / "NOTES.md").read_text(encoding="utf-8") == "initial\n"
    assert _git(repo, "status", "--porcelain") == ""


def test_out_of_scope_refusal_guides_git_rm_for_paths_absent_at_upstream(tmp_path):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")

    (repo / "stale.txt").write_text("old peer file\n", encoding="utf-8")
    _run(repo, "git", "add", "stale.txt")
    _run(repo, "git", "commit", "-m", "shared stale file")
    _run(repo, "git", "push", "origin", "main")

    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "agent work")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "README.md").write_text("baseline work\n", encoding="utf-8")
    _run(peer, "git", "rm", "stale.txt")
    _run(peer, "git", "add", "README.md")
    _run(peer, "git", "commit", "-m", "peer deletes stale file")
    _run(peer, "git", "push", "origin", "main")

    with pytest.raises(gitsync.MergeConflict):
        gitsync.integrate_and_publish("TASK-1jN54zJX", repo_root=repo)

    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    (repo / "stale.txt").write_text("old peer file\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md", "stale.txt")
    _run(repo, "git", "commit", "-m", "Resolve baseline overlap sloppily")

    with pytest.raises(SpiceError) as exc_info:
        gitsync.integrate_and_publish("TASK-1jN54zJX", repo_root=repo)

    message = str(exc_info.value)
    assert "refusing to publish" in message
    assert "stale.txt" in message
    assert _refusal_commands(message) == [
        "git rm -- stale.txt",
        'git commit -m "Restore baseline content for TASK-1jN54zJX"',
        'spice task done TASK-1jN54zJX --validation "..."',
    ]

    _run(repo, "git", "rm", "stale.txt")
    _run(repo, "git", "commit", "-m", "Restore baseline deletion")

    result = gitsync.integrate_and_publish("TASK-1jN54zJX", repo_root=repo)
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "ls-files") == "README.md"
    assert _git(repo, "status", "--porcelain") == ""


def test_out_of_scope_refusal_partitions_mixed_present_and_absent_paths(tmp_path):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    (repo / "conflict.txt").write_text("shared conflict base\n", encoding="utf-8")
    (repo / "stale.txt").write_text("old peer file\n", encoding="utf-8")
    _run(repo, "git", "add", "conflict.txt", "stale.txt")
    _run(repo, "git", "commit", "-m", "shared conflict and stale files")
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")

    (repo / "agent.txt").write_text("agent work\n", encoding="utf-8")
    (repo / "conflict.txt").write_text("agent conflict work\n", encoding="utf-8")
    _run(repo, "git", "add", "agent.txt", "conflict.txt")
    _run(repo, "git", "commit", "-m", "agent work")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "README.md").write_text("peer baseline work\n", encoding="utf-8")
    (peer / "conflict.txt").write_text("peer conflict work\n", encoding="utf-8")
    _run(peer, "git", "rm", "stale.txt")
    _run(peer, "git", "add", "README.md", "conflict.txt")
    _run(peer, "git", "commit", "-m", "peer baseline and stale deletion")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")

    with pytest.raises(gitsync.MergeConflict):
        gitsync.integrate_and_publish("TASK-1jN54zJY", repo_root=repo)

    (repo / "README.md").write_text("initial\n", encoding="utf-8")
    (repo / "conflict.txt").write_text("agent conflict work\n", encoding="utf-8")
    (repo / "stale.txt").write_text("old peer file\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md", "conflict.txt", "stale.txt")
    _run(repo, "git", "commit", "-m", "Resolve baseline overlap sloppily")

    with pytest.raises(SpiceError) as exc_info:
        gitsync.integrate_and_publish("TASK-1jN54zJY", repo_root=repo)

    message = str(exc_info.value)
    assert "refusing to publish" in message
    assert "README.md" in message
    assert "stale.txt" in message
    assert _refusal_commands(message) == [
        f"git checkout {upstream_head} -- README.md",
        "git rm -- stale.txt",
        'git commit -m "Restore baseline content for TASK-1jN54zJY"',
        'spice task done TASK-1jN54zJY --validation "..."',
    ]


def test_publish_race_retry_enforces_out_of_scope_guard(tmp_path, monkeypatch):
    # Defense in depth for the race-retry choke point: if a retry round ever
    # produces a head that rewinds peer paths (simulated here by adopting the
    # prior merge head's tree instead of the freshly merged tree), the push
    # must be refused just like the primary publish path.
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")

    (repo / "agent.txt").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "agent.txt")
    _run(repo, "git", "commit", "-m", "agent work")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    real_run = gitsync._run
    push_attempts = 0

    def racing_run(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
        nonlocal push_attempts
        if args and args[0] == "push" and repo_root == repo:
            push_attempts += 1
            if push_attempts == 1:
                (peer / "raced.txt").write_text("peer raced ahead\n", encoding="utf-8")
                _run(peer, "git", "add", "raced.txt")
                _run(peer, "git", "commit", "-m", "peer raced ahead")
                _run(peer, "git", "push", "origin", "main")
        return real_run(repo_root, *args)

    monkeypatch.setattr(gitsync, "_run", racing_run)
    real_synth = gitsync._synthesize_and_fast_forward
    synth_calls = 0

    def adopting_synth(repo_root, treeish, first_parent, second_parent, message, **kw):
        nonlocal synth_calls
        synth_calls += 1
        if synth_calls == 2:
            treeish = second_parent
        return real_synth(
            repo_root, treeish, first_parent, second_parent, message, **kw
        )

    monkeypatch.setattr(gitsync, "_synthesize_and_fast_forward", adopting_synth)

    with pytest.raises(SpiceError) as exc_info:
        gitsync.integrate_and_publish("TASK-1jN54zJT", repo_root=repo)

    message = str(exc_info.value)
    raced_upstream = _git(peer, "rev-parse", "HEAD")
    assert "refusing to publish" in message
    assert "raced.txt" in message
    assert push_attempts == 1
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == (
        raced_upstream
    )


def test_integrate_and_publish_computes_merge_before_materializing_tree(
    tmp_path, monkeypatch
):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")

    (repo / "agent.txt").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "agent.txt")
    _run(repo, "git", "commit", "-m", "agent work")
    agent_head = _git(repo, "rev-parse", "HEAD")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "baseline.txt").write_text("baseline work\n", encoding="utf-8")
    _run(peer, "git", "add", "baseline.txt")
    _run(peer, "git", "commit", "-m", "baseline work")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")
    real_run = gitsync._run
    observed: dict[str, str] = {}

    def observe_atomic_update(repo_root: Path, *args: str):
        if repo_root == repo and args[:2] == ("update-ref", "refs/heads/main"):
            candidate = args[2]
            observed["head_before_update"] = _git(repo, "rev-parse", "HEAD")
            observed["index_tree_before_update"] = _git(repo, "write-tree")
            observed["candidate_tree"] = _git(
                repo, "rev-parse", f"{candidate}^{{tree}}"
            )
            observed["baseline_content"] = (repo / "baseline.txt").read_text(
                encoding="utf-8"
            )
        return real_run(repo_root, *args)

    monkeypatch.setattr(gitsync, "_run", observe_atomic_update)

    result = gitsync.integrate_and_publish(
        "TASK-1jN54zJR",
        repo_root=repo,
        meta={
            "title": "Publish missing merge head cleanup",
            "actor": ACTOR_A,
            "phase": "review",
            "project": "task.unit",
        },
    )
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert observed == {
        "head_before_update": agent_head,
        "index_tree_before_update": observed["candidate_tree"],
        "candidate_tree": observed["candidate_tree"],
        "baseline_content": "baseline work\n",
    }
    assert captured["done_head"] == agent_head
    assert captured["done_upstream_head"] == upstream_head
    assert _merge_parents(repo, merge_head) == [upstream_head, agent_head]
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""


def test_reference_hook_failure_restores_clean_pre_merge_state(tmp_path, monkeypatch):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")

    (repo / "agent.txt").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "agent.txt")
    _run(repo, "git", "commit", "-m", "agent work")
    agent_head = _git(repo, "rev-parse", "HEAD")
    agent_tree = _git(repo, "rev-parse", "HEAD^{tree}")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "baseline.txt").write_text("baseline work\n", encoding="utf-8")
    _run(peer, "git", "add", "baseline.txt")
    _run(peer, "git", "commit", "-m", "baseline work")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")

    real_run = gitsync._run
    observed: dict[str, str] = {}

    def reject_ref_transaction(repo_root: Path, *args: str):
        if repo_root == repo and args[:2] == ("update-ref", "refs/heads/main"):
            candidate = args[2]
            observed["head"] = _git(repo, "rev-parse", "HEAD")
            observed["materialized_tree"] = _git(repo, "write-tree")
            observed["candidate_tree"] = _git(
                repo, "rev-parse", f"{candidate}^{{tree}}"
            )
            return subprocess.CompletedProcess(
                ["git", "-C", str(repo), *args],
                1,
                stdout="",
                stderr="reference-transaction hook rejected prepared update\n",
            )
        return real_run(repo_root, *args)

    monkeypatch.setattr(gitsync, "_run", reject_ref_transaction)

    hook_outcome = _gitsync_outcome(
        lambda: gitsync.integrate_and_publish("TASK-1kCzAtomic", repo_root=repo)
    )

    assert hook_outcome.state == "rejected"
    assert "reference-transaction hook rejected" in hook_outcome.message
    assert observed == {
        "head": agent_head,
        "materialized_tree": observed["candidate_tree"],
        "candidate_tree": observed["candidate_tree"],
    }
    assert _git(repo, "rev-parse", "HEAD") == agent_head
    assert _git(repo, "write-tree") == agent_tree
    assert _git(repo, "status", "--porcelain") == ""
    assert _git(repo, "ls-files") == "README.md\nagent.txt"
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == (
        upstream_head
    )


def test_integrate_and_publish_builds_recoverable_conflict_without_ref_hook(
    tmp_path, monkeypatch
):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")

    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "agent work")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "README.md").write_text("baseline work\n", encoding="utf-8")
    _run(peer, "git", "add", "README.md")
    _run(peer, "git", "commit", "-m", "baseline work")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")
    real_run = gitsync._run
    merge_tree_attempts = 0

    def observe_merge_tree(
        repo_root: Path, *args: str
    ) -> subprocess.CompletedProcess[str]:
        nonlocal merge_tree_attempts
        if repo_root == repo and args[:2] == ("merge-tree", "--write-tree"):
            merge_tree_attempts += 1
        return real_run(repo_root, *args)

    monkeypatch.setattr(gitsync, "_run", observe_merge_tree)

    conflict_outcome = _gitsync_outcome(
        lambda: gitsync.integrate_and_publish("TASK-1jN54zJP", repo_root=repo)
    )

    message = conflict_outcome.message
    assert conflict_outcome.state == "recoverable-conflict"
    assert merge_tree_attempts == 1
    assert "git is paused in a merge state" in message
    assert "README.md" in message
    assert "commit while MERGE_HEAD exists" in message
    assert _git(repo, "rev-parse", "--verify", "MERGE_HEAD") == upstream_head
    assert _git(repo, "status", "--porcelain") == "UU README.md"

    (repo / "README.md").write_text("resolved merge-tree work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(
        repo,
        "git",
        "commit",
        "-m",
        "Resolve baseline overlap for TASK-1jN54zJP",
    )
    rescue_merge = _git(repo, "rev-parse", "HEAD")
    assert _git(repo, "status", "--porcelain") == ""

    result = gitsync.integrate_and_publish("TASK-1jN54zJP", repo_root=repo)
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert captured["done_head"] == rescue_merge
    assert _merge_parents(repo, rescue_merge)[1] == upstream_head
    assert _merge_parents(repo, merge_head) == [upstream_head, rescue_merge]
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""


def _hook_aborted_merge_repositories(tmp_path):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")

    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "agent work")
    agent_head = _git(repo, "rev-parse", "HEAD")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "README.md").write_text("baseline work\n", encoding="utf-8")
    (peer / "peer.txt").write_text("peer feature\n", encoding="utf-8")
    _run(peer, "git", "add", "README.md", "peer.txt")
    _run(peer, "git", "commit", "-m", "baseline work with peer feature")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")
    return repo, agent_head, upstream_head


def _observe_merge_tree(repo, monkeypatch):
    attempts = [0]

    real_run = gitsync._run

    def observing_run(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
        if repo_root == repo and args[:2] == ("merge-tree", "--write-tree"):
            attempts[0] += 1
        return real_run(repo_root, *args)

    monkeypatch.setattr(gitsync, "_run", observing_run)
    return attempts


def _stage_merge_tree_resolution(repo):
    (repo / "README.md").write_text("resolved work\n", encoding="utf-8")
    _run(repo, "git", "add", "--", "README.md")
    merged_tree = _git(repo, "write-tree")
    merged_names = set(_git(repo, "ls-tree", "--name-only", merged_tree).split())
    cached_names = set(_git(repo, "diff", "--cached", "--name-only").split())
    return merged_tree, merged_names, cached_names


def test_merge_tree_conflict_state_preserves_clean_peer_file(tmp_path, monkeypatch):
    repo, agent_head, upstream_head = _hook_aborted_merge_repositories(tmp_path)
    merge_attempts = _observe_merge_tree(repo, monkeypatch)

    conflict_outcome = _gitsync_outcome(
        lambda: gitsync.integrate_and_publish("TASK-1jN54zJT", repo_root=repo)
    )

    message = conflict_outcome.message
    assert conflict_outcome.state == "recoverable-conflict"
    assert merge_attempts == [1]
    assert "commit while MERGE_HEAD exists" in message
    assert _git(repo, "rev-parse", "--verify", "MERGE_HEAD") == upstream_head

    # The clean peer file is already in the index before the agent resolves the
    # overlap, so staging only the conflicted path still writes the whole merge.
    merged_tree, merged_names, cached_names = _stage_merge_tree_resolution(repo)
    assert "peer.txt" in merged_names
    assert "peer.txt" in cached_names

    _run(
        repo,
        "git",
        "commit",
        "-m",
        "Resolve baseline overlap for TASK-1jN54zJT",
    )
    rescue_merge = _git(repo, "rev-parse", "HEAD")
    assert _git(repo, "rev-parse", f"{rescue_merge}^{{tree}}") == merged_tree
    assert _git(repo, "status", "--porcelain") == ""

    result = gitsync.integrate_and_publish("TASK-1jN54zJT", repo_root=repo)
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert captured["done_head"] == rescue_merge
    assert _merge_parents(repo, merge_head) == [upstream_head, rescue_merge]
    published = _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0]
    assert published == merge_head
    published_names = set(_git(repo, "ls-tree", "--name-only", merge_head).split())
    assert "peer.txt" in published_names  # peer work survives to the published merge
    assert _git(repo, "show", f"{merge_head}:peer.txt") == "peer feature"
    assert agent_head in _merge_parents(repo, rescue_merge)


@pytest.mark.parametrize(
    ("args", "expected_timeout"),
    [
        (("fetch", "origin"), gitsync.GIT_NETWORK_TIMEOUT_SECONDS),
        (("status",), git.DEFAULT_GIT_TIMEOUT_SECONDS),
    ],
)
def test_gitsync_commands_are_noninteractive_and_bounded(
    tmp_path, monkeypatch, args, expected_timeout
):
    seen: dict[str, object] = {}

    def fake_run(command: list[str], **kwargs):
        seen["command"] = command
        seen["env"] = kwargs["env"]
        seen["timeout"] = kwargs["timeout_seconds"]
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(git, "run_bounded_process_group", fake_run)

    gitsync._run(tmp_path, *args)

    env = seen["env"]
    assert isinstance(env, dict)
    assert env["GIT_TERMINAL_PROMPT"] == "0"
    assert env["GIT_SSH_COMMAND"] == gitsync.TASK_GIT_SSH_COMMAND
    assert seen["timeout"] == expected_timeout


def test_gitsync_timeout_names_the_bounded_command(tmp_path, monkeypatch):
    def fake_run(command: list[str], **kwargs):
        raise ProcessDeadlineExceeded(
            phase=kwargs["phase"],
            input_label=kwargs["input_label"],
            timeout_seconds=kwargs["timeout_seconds"],
            command=command,
        )

    monkeypatch.setattr(git, "run_bounded_process_group", fake_run)

    with pytest.raises(SpiceError) as exc_info:
        gitsync._run(tmp_path, "fetch", "origin")

    assert str(exc_info.value) == (
        f"git command timed out after {gitsync.GIT_NETWORK_TIMEOUT_SECONDS}s: "
        f"git -C {tmp_path} fetch origin; increase "
        f"{git.GIT_TIMEOUT_ENV} for a slower repository"
    )


def test_integrate_and_publish_refuses_committed_conflict_markers(tmp_path):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")

    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "agent work")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "README.md").write_text("baseline work\n", encoding="utf-8")
    _run(peer, "git", "add", "README.md")
    _run(peer, "git", "commit", "-m", "baseline work")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")

    with pytest.raises(gitsync.MergeConflict):
        gitsync.integrate_and_publish("TASK-1jN54zJM", repo_root=repo)

    conflicted = (repo / "README.md").read_text(encoding="utf-8")
    assert "<<<<<<<" in conflicted
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "Resolve baseline overlap, badly")

    with pytest.raises(SpiceError, match="conflict markers") as exc_info:
        gitsync.integrate_and_publish("TASK-1jN54zJM", repo_root=repo)

    message = str(exc_info.value)
    assert "README.md" in message
    assert "git add -- README.md" in message
    assert "git commit --amend --no-edit" in message
    assert (
        _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == upstream_head
    )

    (repo / "README.md").write_text("resolved work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "--amend", "--no-edit")

    result = gitsync.integrate_and_publish("TASK-1jN54zJM", repo_root=repo)
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert _merge_parents(repo, merge_head)[0] == upstream_head
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head


def test_branch_upstream_target_reads_branch_merge_under_shadow_env(
    tmp_path, monkeypatch
):
    repo = tmp_path / "agent"
    repo.mkdir()
    _run(repo, "git", "init", "-b", "lane")
    _configure_git_identity(repo)
    _run(repo, "git", "remote", "add", "origin", str(tmp_path / "remote.git"))
    _run(repo, "git", "config", "branch.lane.remote", "origin")
    _run(repo, "git", "config", "branch.lane.merge", "refs/heads/trunk")

    shadow_config = tmp_path / "system-shadow.gitconfig"
    shadow_config.write_text(
        '[branch "lane"]\n\tmerge = refs/heads/lane\n', encoding="utf-8"
    )
    monkeypatch.setenv("GIT_CONFIG_SYSTEM", str(shadow_config))
    monkeypatch.setenv("GIT_CONFIG_COUNT", "1")
    monkeypatch.setenv("GIT_CONFIG_KEY_0", "branch.lane.remote")
    monkeypatch.setenv("GIT_CONFIG_VALUE_0", ".")

    assert gitsync.branch_upstream_target(repo) == ("origin", "origin/trunk")


def test_branch_upstream_target_uses_origin_head_only_as_backstop(tmp_path):
    repo = tmp_path / "agent"
    repo.mkdir()
    _run(repo, "git", "init", "-b", "lane")
    _configure_git_identity(repo)
    _run(repo, "git", "remote", "add", "origin", str(tmp_path / "remote.git"))
    _run(
        repo,
        "git",
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/dev",
    )

    assert gitsync.branch_upstream_target(repo) == ("origin", "origin/dev")


def test_fast_forward_if_safe_reports_updated_then_current(tmp_path):
    repo = _repo_with_upstream(tmp_path)
    _advance_upstream(tmp_path)

    advanced = gitsync.fast_forward_if_safe(repo)
    assert advanced.notes == ["updated working tree to the current baseline"]

    assert gitsync.fast_forward_if_safe(repo).notes == ["current"]


def test_prepare_for_agent_launch_reports_updated_then_current(tmp_path):
    repo = _repo_with_upstream(tmp_path)
    _advance_upstream(tmp_path)

    advanced = gitsync.prepare_for_agent_launch(repo)

    assert advanced.notes == ["updated working tree to the current baseline"]
    assert gitsync.prepare_for_agent_launch(repo).notes == ["current"]


def test_prepare_for_agent_launch_accepts_current_local_only_tree(tmp_path):
    repo = _init_repo(tmp_path / "agent")

    assert gitsync.prepare_for_agent_launch(repo).notes == ["current:local-only"]


def test_prepare_for_agent_launch_preserves_dirty_user_work(tmp_path):
    repo = _repo_with_upstream(tmp_path)
    (repo / "dirty.txt").write_text("uncommitted\n", encoding="utf-8")
    _run(repo, "git", "add", "dirty.txt")

    outcome = _gitsync_outcome(lambda: gitsync.prepare_for_agent_launch(repo))

    assert outcome.state == "rejected"
    assert "working tree is dirty" in outcome.message
    assert (repo / "dirty.txt").read_text(encoding="utf-8") == "uncommitted\n"


def test_prepare_for_agent_launch_routes_ahead_work_to_task_control_plane(tmp_path):
    repo = _repo_with_upstream(tmp_path)
    (repo / "ahead.txt").write_text("local commit\n", encoding="utf-8")
    _run(repo, "git", "add", "ahead.txt")
    _run(repo, "git", "commit", "-m", "ahead of baseline")

    outcome = _gitsync_outcome(lambda: gitsync.prepare_for_agent_launch(repo))

    assert outcome.state == "rejected"
    assert "not recorded by a completed task" in outcome.message


def test_prepare_for_agent_launch_reports_divergent_tree_recovery(tmp_path):
    repo = _repo_with_upstream(tmp_path)
    (repo / "local.txt").write_text("local commit\n", encoding="utf-8")
    _run(repo, "git", "add", "local.txt")
    _run(repo, "git", "commit", "-m", "local work")
    _advance_upstream(tmp_path)

    outcome = _gitsync_outcome(lambda: gitsync.prepare_for_agent_launch(repo))

    assert outcome.state == "rejected"
    assert "branch has diverged" in outcome.message
    assert "task Git control plane" in outcome.message


def test_prepare_for_agent_launch_reports_fetch_failure(tmp_path, monkeypatch):
    repo = _repo_with_upstream(tmp_path)
    real_run = gitsync._run

    def fail_fetch(repo_root, *args):
        if args and args[0] == "fetch":
            return subprocess.CompletedProcess(list(args), 128, "", "offline")
        return real_run(repo_root, *args)

    monkeypatch.setattr(gitsync, "_run", fail_fetch)

    outcome = _gitsync_outcome(lambda: gitsync.prepare_for_agent_launch(repo))

    assert outcome.state == "rejected"
    assert "current baseline could not be fetched" in outcome.message
    assert "offline" in outcome.message


def test_fast_forward_if_safe_reports_skipped_dirty(tmp_path):
    repo = _repo_with_upstream(tmp_path)
    (repo / "dirty.txt").write_text("uncommitted\n", encoding="utf-8")
    _run(repo, "git", "add", "dirty.txt")

    assert gitsync.fast_forward_if_safe(repo).notes == ["skipped:dirty"]


def test_fast_forward_if_safe_reports_skipped_ahead(tmp_path):
    repo = _repo_with_upstream(tmp_path)
    (repo / "ahead.txt").write_text("local commit\n", encoding="utf-8")
    _run(repo, "git", "add", "ahead.txt")
    _run(repo, "git", "commit", "-m", "ahead of baseline")

    assert gitsync.fast_forward_if_safe(repo).notes == ["skipped:ahead"]


def test_fast_forward_if_safe_reports_skipped_no_remote(tmp_path):
    repo = _init_repo(tmp_path / "agent")

    assert gitsync.fast_forward_if_safe(repo).notes == ["skipped:no-remote"]


def test_fast_forward_if_safe_reports_skipped_diverged(tmp_path, monkeypatch):
    repo = _repo_with_upstream(tmp_path)
    _advance_upstream(tmp_path)
    real_run = gitsync._run

    def fail_merge(repo_root, *args):
        if "merge" in args:
            return subprocess.CompletedProcess(list(args), 1)
        return real_run(repo_root, *args)

    monkeypatch.setattr(gitsync, "_run", fail_merge)

    assert gitsync.fast_forward_if_safe(repo).notes == ["skipped:diverged"]


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


def _merge_parents(repo: Path, commit: str) -> list[str]:
    return _git(repo, "show", "-s", "--format=%P", commit).split()


def _empty_merges(repo: Path, rev: str) -> list[str]:
    """Merge commits reachable from ``rev`` whose tree equals their mainline's."""
    merges = _git(repo, "rev-list", "--merges", rev).split()
    empty = []
    for merge in merges:
        tree = _git(repo, "rev-parse", f"{merge}^{{tree}}")
        first_parent_tree = _git(repo, "rev-parse", f"{merge}^1^{{tree}}")
        if tree == first_parent_tree:
            empty.append(merge)
    return empty


def _uda_map(args: list[str]) -> dict[str, str]:
    return dict(item.split(":", 1) for item in args)


def _refusal_commands(message: str) -> list[str]:
    lines = message.splitlines()
    start = lines.index("next commands:") + 1
    commands: list[str] = []
    for line in lines[start:]:
        if line.startswith("  "):
            commands.append(line.strip())
            continue
        break
    return commands


def _merge_head_missing(repo: Path) -> bool:
    completed = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "--verify", "MERGE_HEAD"],
        capture_output=True,
        check=False,
        text=True,
    )
    return completed.returncode != 0


def _git(repo: Path, *args: str) -> str:
    return _run(repo, "git", *args).stdout.strip()


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
