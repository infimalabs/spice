"""Task git publication, race convergence, and merge-message behavior."""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from spice.agent import lifecyclebinding
from spice.errors import SpiceError
from spice.tasks.git import boundaries, merging, plumbing
from tests.test_taskgitsync import (
    ACTOR_A,
    UNDECODABLE_SKILL_BYTES,
    _advance_upstream,
    _configure_git_identity,
    _empty_merges,
    _git,
    _gitsync_outcome,
    _init_repo,
    _merge_parents,
    _repo_with_stale_generated_skill,
    _repo_with_tracked_generated_skill,
    _repo_with_upstream,
    _run,
    _uda_map,
)

LANDING_SKILL_META = {
    "title": "Land task work over an advanced packaged skill",
    "actor": ACTOR_A,
    "phase": "todo",
    "project": "task.unit",
}


def _commit_agent_work(repo: Path) -> None:
    """Give the lane a commit of its own so a landing has something to merge."""
    (repo / "agent.txt").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "agent.txt")
    _run(repo, "git", "commit", "-m", "agent work")


def test_integrate_and_publish_creates_baseline_first_merge_and_pushes(tmp_path):
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

    result = boundaries.integrate_and_publish(
        "TASK-1k98v0WX",
        repo_root=repo,
        meta={
            "title": "Publish task work",
            "description": "Longer merge body for reviewers.",
            "actor": ACTOR_A,
            "phase": "todo",
            "project": "task.unit",
        },
    )
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert captured["done_head"] == agent_head
    assert captured["done_ref"] == merge_head
    assert captured["done_upstream"] == "origin/main"
    assert captured["done_upstream_head"] == upstream_head
    assert _git(repo, "rev-parse", "HEAD") == merge_head
    assert _merge_parents(repo, merge_head) == [upstream_head, agent_head]
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""
    message = _git(repo, "log", "-1", "--format=%B", merge_head)
    assert message == (
        "task: Publish task work TASK-1k98v0WX\n\n"
        "Task-Key: 1k98v0WX\n"
        "Task-Phase: todo\n"
        "Task-Project: task.unit\n"
        f"Task-Session: {ACTOR_A}"
    )


def test_integrate_and_publish_collapses_no_op_phase_without_empty_merge(tmp_path):
    # A completion storm reproduction: the agent's phase edits nothing while a
    # peer advances the baseline to a commit that adds no content (its tree
    # equals the shared base). The old emitter minted an empty --no-ff merge
    # here; the phase must instead collapse onto the advanced baseline as a git
    # no-op, leaving zero empty merges in history.
    repo = _repo_with_upstream(tmp_path)
    base = _git(repo, "rev-parse", "HEAD")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(tmp_path / "remote.git"), str(peer))
    _configure_git_identity(peer)
    _run(peer, "git", "commit", "--allow-empty", "-m", "peer review marker")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")
    assert upstream_head != base

    result = boundaries.integrate_and_publish(
        "TASK-1k98v0WX",
        repo_root=repo,
        meta={
            "title": "No-edit review",
            "actor": ACTOR_A,
            "phase": "review",
            "project": "task.unit",
        },
    )
    captured = _uda_map(result.uda_args)

    assert captured["done_head"] == base
    assert captured["done_merge_head"] == upstream_head
    assert captured["done_ref"] == upstream_head
    assert _git(repo, "rev-parse", "HEAD") == upstream_head
    assert _merge_parents(repo, "HEAD") == [base]
    assert _git(repo, "rev-parse", "HEAD^{tree}") == _git(
        repo, "rev-parse", f"{base}^{{tree}}"
    )
    assert (
        _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == upstream_head
    )
    assert _git(repo, "status", "--porcelain") == ""
    assert _empty_merges(repo, "HEAD") == []


def test_integrate_and_publish_no_op_phase_fast_forwards_onto_peer_content(tmp_path):
    # The common storm shape: the reviewer gets a strict descendant baseline
    # carrying a peer's real content while editing nothing itself. The phase must
    # fast-forward onto that content (picking it up) rather than mint a merge,
    # because the merged result adds nothing over upstream.
    repo = _repo_with_upstream(tmp_path)
    base = _git(repo, "rev-parse", "HEAD")
    _advance_upstream(tmp_path)
    peer_clone = tmp_path / "peer"
    upstream_head = _git(peer_clone, "rev-parse", "HEAD")

    result = boundaries.integrate_and_publish(
        "TASK-1k98v0WX",
        repo_root=repo,
        meta={
            "title": "No-edit review over peer content",
            "actor": ACTOR_A,
            "phase": "review",
            "project": "task.unit",
        },
    )
    captured = _uda_map(result.uda_args)

    assert captured["done_merge_head"] == upstream_head
    assert _git(repo, "rev-parse", "HEAD") == upstream_head
    assert (repo / "baseline.txt").read_text(encoding="utf-8") == "baseline work\n"
    assert _merge_parents(repo, "HEAD") == [base]
    assert _empty_merges(repo, "HEAD") == []
    assert _git(repo, "status", "--porcelain") == ""


def test_integrate_and_publish_preserves_divergent_tree_same_commits(tmp_path):
    repo = _repo_with_upstream(tmp_path)
    shared = repo / "shared.txt"
    shared.write_text("base\n", encoding="utf-8")
    _run(repo, "git", "add", "shared.txt")
    _run(repo, "git", "commit", "-m", "shared base")
    _run(repo, "git", "push", "origin", "main")

    peer = tmp_path / "tree-same-peer"
    _run(tmp_path, "git", "clone", str(tmp_path / "remote.git"), str(peer))
    _configure_git_identity(peer)

    shared.write_text("identical result\n", encoding="utf-8")
    _run(repo, "git", "add", "shared.txt")
    _run(repo, "git", "commit", "-m", "agent result")
    agent_head = _git(repo, "rev-parse", "HEAD")

    (peer / "shared.txt").write_text("identical result\n", encoding="utf-8")
    _run(peer, "git", "add", "shared.txt")
    _run(peer, "git", "commit", "-m", "peer identical result")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")

    result = boundaries.integrate_and_publish(
        "TASK-1k98v0TS",
        repo_root=repo,
        meta={
            "title": "Preserve tree-same ancestry",
            "actor": ACTOR_A,
            "phase": "todo",
            "project": "task.boundary",
        },
    )
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert result.notes == [
        "task tree already integrated on baseline; preserved divergent commits "
        "in a tree-same merge"
    ]
    assert _merge_parents(repo, merge_head) == [upstream_head, agent_head]
    assert _git(repo, "rev-parse", f"{merge_head}^{{tree}}") == _git(
        repo, "rev-parse", f"{upstream_head}^{{tree}}"
    )
    assert (
        _run(
            repo, "git", "merge-base", "--is-ancestor", agent_head, merge_head
        ).returncode
        == 0
    )
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""


def test_integrate_and_publish_retries_non_fast_forward_publish_race(
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
    real_run = plumbing.run
    push_attempts = 0

    def racing_run(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
        nonlocal push_attempts
        if args and args[0] == "push" and repo_root == repo:
            push_attempts += 1
            if push_attempts == 1:
                (peer / "baseline.txt").write_text(
                    "baseline raced ahead\n", encoding="utf-8"
                )
                _run(peer, "git", "add", "baseline.txt")
                _run(peer, "git", "commit", "-m", "baseline raced ahead")
                _run(peer, "git", "push", "origin", "main")
        return real_run(repo_root, *args)

    monkeypatch.setattr(plumbing, "run", racing_run)

    result = boundaries.integrate_and_publish(
        "TASK-1jN54zJN",
        repo_root=repo,
        meta={
            "title": "Publish raced task work",
            "actor": ACTOR_A,
            "phase": "todo",
            "project": "task.unit",
        },
    )
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]
    raced_upstream = _git(peer, "rev-parse", "HEAD")
    first_retry_parent, second_retry_parent = _merge_parents(repo, merge_head)

    assert push_attempts == 2
    assert captured["done_head"] == agent_head
    assert captured["done_upstream_head"] == raced_upstream
    assert first_retry_parent == raced_upstream
    assert _merge_parents(repo, second_retry_parent)[1] == agent_head
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""


def test_integrate_and_publish_converges_after_consecutive_publish_races(
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
    real_run = plumbing.run
    push_attempts = 0
    storm_pushes = 3  # completion storm: three peers land ahead back-to-back

    def storming_run(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
        nonlocal push_attempts
        if args and args[0] == "push" and repo_root == repo:
            push_attempts += 1
            if push_attempts <= storm_pushes:
                name = f"peer-{push_attempts}.txt"
                (peer / name).write_text("peer landed first\n", encoding="utf-8")
                _run(peer, "git", "add", name)
                _run(peer, "git", "commit", "-m", f"peer work {push_attempts}")
                _run(peer, "git", "push", "origin", "main")
        return real_run(repo_root, *args)

    monkeypatch.setattr(plumbing, "run", storming_run)

    result = boundaries.integrate_and_publish(
        "TASK-1jN54zJP",
        repo_root=repo,
        meta={
            "title": "Publish storm task work",
            "actor": ACTOR_A,
            "phase": "todo",
            "project": "task.unit",
        },
    )
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert push_attempts == storm_pushes + 1
    assert captured["done_head"] == agent_head
    assert captured["done_upstream_head"] == _git(peer, "rev-parse", "HEAD")
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""


def test_publish_storm_hook_failure_recovers_then_keeps_every_peer_path(
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

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    real_run = plumbing.run
    push_attempts = 0
    update_attempts = 0
    successful_local_heads: list[str] = []

    def storm_then_reject(repo_root: Path, *args: str):
        nonlocal push_attempts, update_attempts
        if repo_root == repo and args and args[0] == "push":
            push_attempts += 1
            if push_attempts <= 2:
                name = f"peer-{push_attempts}.txt"
                (peer / name).write_text(
                    f"peer landed first {push_attempts}\n", encoding="utf-8"
                )
                _run(peer, "git", "add", name)
                _run(peer, "git", "commit", "-m", f"peer work {push_attempts}")
                _run(peer, "git", "push", "origin", "main")
        if repo_root == repo and args[:2] == ("update-ref", "refs/heads/main"):
            update_attempts += 1
            if update_attempts == 3:
                return subprocess.CompletedProcess(
                    ["git", "-C", str(repo), *args],
                    1,
                    stdout="",
                    stderr="reference-transaction hook rejected storm retry\n",
                )
            successful_local_heads.append(args[2])
        return real_run(repo_root, *args)

    monkeypatch.setattr(plumbing, "run", storm_then_reject)

    hook_outcome = _gitsync_outcome(
        lambda: boundaries.integrate_and_publish("TASK-1kCzStorm", repo_root=repo)
    )

    assert hook_outcome.state == "rejected"
    assert "hook rejected storm retry" in hook_outcome.message
    assert push_attempts == 2
    assert update_attempts == 3
    assert _git(repo, "rev-parse", "HEAD") == successful_local_heads[-1]
    assert _git(repo, "status", "--porcelain") == ""
    assert _git(repo, "show", "HEAD:peer-1.txt") == "peer landed first 1"

    result = boundaries.integrate_and_publish("TASK-1kCzStorm", repo_root=repo)
    merge_head = _uda_map(result.uda_args)["done_merge_head"]
    published = _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0]

    assert published == merge_head
    assert _git(repo, "show", f"{merge_head}:agent.txt") == "agent work"
    assert _git(repo, "show", f"{merge_head}:peer-1.txt") == "peer landed first 1"
    assert _git(repo, "show", f"{merge_head}:peer-2.txt") == "peer landed first 2"
    assert _git(repo, "status", "--porcelain") == ""


def test_integrate_and_publish_surfaces_recovery_when_races_never_stop(
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

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    real_run = plumbing.run
    push_attempts = 0
    monkeypatch.setattr(boundaries, "PUBLISH_RACE_RETRY_LIMIT", 2)

    def relentless_run(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
        nonlocal push_attempts
        if args and args[0] == "push" and repo_root == repo:
            push_attempts += 1
            name = f"peer-{push_attempts}.txt"
            (peer / name).write_text("peer landed first\n", encoding="utf-8")
            _run(peer, "git", "add", name)
            _run(peer, "git", "commit", "-m", f"peer work {push_attempts}")
            _run(peer, "git", "push", "origin", "main")
        return real_run(repo_root, *args)

    monkeypatch.setattr(plumbing, "run", relentless_run)

    with pytest.raises(SpiceError, match="publish"):
        boundaries.integrate_and_publish(
            "TASK-1jN54zJQ",
            repo_root=repo,
            meta={
                "title": "Publish unwinnable race",
                "actor": ACTOR_A,
                "phase": "todo",
                "project": "task.unit",
            },
        )

    assert push_attempts == 3  # initial push + bounded retries


def test_integrate_and_publish_reports_local_head_ref_lock_race(tmp_path, monkeypatch):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")
    upstream_head = _git(repo, "rev-parse", "HEAD")

    (repo / "agent.txt").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "agent.txt")
    _run(repo, "git", "commit", "-m", "agent work")
    agent_head = _git(repo, "rev-parse", "HEAD")
    real_run = plumbing.run
    update_attempts = 0
    raced_head = ""

    def racing_run(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
        nonlocal update_attempts, raced_head
        if repo_root == repo and args[:2] == ("update-ref", "refs/heads/main"):
            update_attempts += 1
            raced_head = _git(
                repo,
                "commit-tree",
                _git(repo, "rev-parse", f"{agent_head}^{{tree}}"),
                "-p",
                agent_head,
                "-m",
                "local race",
            )
            _run(
                repo,
                "git",
                "update-ref",
                "refs/heads/main",
                raced_head,
                agent_head,
            )
            return subprocess.CompletedProcess(
                ["git", "-C", str(repo), *args],
                128,
                stdout="",
                stderr=(
                    "fatal: cannot lock ref 'refs/heads/main': "
                    f"is at {raced_head} but expected {agent_head}\n"
                ),
            )
        return real_run(repo_root, *args)

    monkeypatch.setattr(plumbing, "run", racing_run)

    with pytest.raises(SpiceError) as exc_info:
        boundaries.integrate_and_publish(
            "TASK-1jN54zJQ",
            repo_root=repo,
            meta={
                "title": "Publish local head race",
                "actor": ACTOR_A,
                "phase": "todo",
                "project": "task.unit",
            },
        )

    message = str(exc_info.value)
    assert update_attempts == 1
    assert "HEAD moved while spice was advancing the generated task commit" in message
    assert "task state was not advanced" in message
    assert "git status --short" in message
    assert "git rev-parse HEAD" in message
    assert 'spice task done TASK-1jN54zJQ --validation "..."' in message
    assert f"expected_head={agent_head}" in message
    assert f"current_head={raced_head}" in message
    assert _git(repo, "rev-parse", "HEAD") == raced_head
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == (
        upstream_head
    )
    assert _git(repo, "status", "--porcelain") == ""


def test_merge_message_omits_task_description_body():
    message = merging.compose_message(
        "TASK-1k98xkpR",
        {
            "title": "Fix image labels",
            "description": (
                "Operator steering 1k4xgthL: the labels "
                "input_image and view_image look clickable but do not navigate.\n\n"
                "Screenshot references: "
                "/tmp/spice/attachments/sha-a/01-image.png and "
                "/tmp/spice/attachments/sha-b/02-image.png.\n\n"
                "Keep the rendered image context stable for reviewers."
            ),
            "actor": ACTOR_A,
            "phase": "todo",
            "project": "serve.ui",
        },
    )

    assert message == (
        "serve: Fix image labels TASK-1k98xkpR\n\n"
        "Task-Key: 1k98xkpR\n"
        "Task-Phase: todo\n"
        "Task-Project: serve.ui\n"
        f"Task-Session: {ACTOR_A}"
    )


def test_merge_message_uses_fallback_subject_and_trailers_only():
    message = merging.compose_message(
        "TASK-1k98PQrs",
        {
            "title": "",
            "description": (
                "Operator steering 1k4yF2RY: final task merge "
                "commit bodies currently include the task description, which "
                "can read well but carries too many transient details such as "
                "'operator steering ...' wording and links/paths to .spice "
                "inbox artifacts that will not exist for readers later. Adjust "
                "task completion/merge commit body generation."
            ),
            "actor": ACTOR_A,
            "phase": "todo",
            "project": "task",
        },
    )

    assert message == (
        "task: TASK-1k98PQrs\n\n"
        "Task-Key: 1k98PQrs\n"
        "Task-Phase: todo\n"
        "Task-Project: task\n"
        f"Task-Session: {ACTOR_A}"
    )


def test_merge_message_appends_non_todo_phase_after_three_segment_project():
    message = merging.compose_message(
        "SCOPES-1k98xkpR",
        {
            "title": "Review combined selectors",
            "actor": ACTOR_A,
            "phase": "review",
            "project": "lifecycle.config.scopes",
        },
    )

    assert message == (
        "lifecycle: Review combined selectors SCOPES-1k98xkpR (review)\n\n"
        "Task-Key: 1k98xkpR\n"
        "Task-Phase: review\n"
        "Task-Project: lifecycle.config.scopes\n"
        f"Task-Session: {ACTOR_A}"
    )


def test_integrate_and_publish_rematerializes_the_generated_skill(tmp_path):
    """The landing is the third HEAD-moving boundary and the only one that
    carries a peer's packaged skill into a lane that never fast-forwards again.

    New packaged bytes reach this tree through the baseline merge, so a lane
    that never refreshes here keeps serving the stale generated copy for its
    whole life: the next claim correctly observes no advance and correctly
    skips, which is contracted behavior rather than a second bug to fix.
    """
    repo, target, advanced = _repo_with_stale_generated_skill(tmp_path)
    stale = target.read_bytes()
    _commit_agent_work(repo)

    result = boundaries.integrate_and_publish(
        "TASK-1kH7Ld3r", repo_root=repo, meta=LANDING_SKILL_META
    )

    packaged = repo / boundaries.CHECKOUT_PACKAGED_SKILL_RELATIVE_PATH
    assert stale != advanced
    assert packaged.read_bytes() == advanced
    assert target.read_bytes() == packaged.read_bytes()
    assert result.notes == []
    assert _git(repo, "status", "--porcelain") == ""


def test_integrate_and_publish_reports_an_undecodable_skill_without_losing_the_landing(
    tmp_path,
):
    """The refresh runs after the merge has published, so it may only report.

    An exception escaping here would fail a landing whose merge is already on
    the remote, which is why the note carries the failure and the published
    ref is asserted alongside it.
    """
    repo, target, advanced = _repo_with_stale_generated_skill(
        tmp_path, advanced=UNDECODABLE_SKILL_BYTES
    )
    stale = target.read_bytes()
    _commit_agent_work(repo)

    result = boundaries.integrate_and_publish(
        "TASK-1kH7Ld3r", repo_root=repo, meta=LANDING_SKILL_META
    )

    assert result.notes == [
        "generated skill refresh failed: 'utf-8' codec can't decode byte 0xff "
        "in position 0: invalid start byte"
    ]
    assert target.read_bytes() == stale
    assert (repo / boundaries.CHECKOUT_PACKAGED_SKILL_RELATIVE_PATH).read_bytes() == (
        advanced
    )
    landed = _git(repo, "rev-parse", "HEAD")
    assert _uda_map(result.uda_args)["done_merge_head"] == landed
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == landed


def test_integrate_and_publish_keeps_a_tracked_generated_skill(tmp_path):
    repo, target, advanced = _repo_with_tracked_generated_skill(tmp_path)
    tracked = target.read_bytes()
    _commit_agent_work(repo)

    result = boundaries.integrate_and_publish(
        "TASK-1kH7Ld3r", repo_root=repo, meta=LANDING_SKILL_META
    )

    assert result.notes == []
    assert target.read_bytes() == tracked
    assert (repo / boundaries.CHECKOUT_PACKAGED_SKILL_RELATIVE_PATH).read_bytes() == (
        advanced
    )
    assert _git(repo, "status", "--porcelain") == ""


def test_integrate_and_publish_landing_without_a_head_move_leaves_assets_alone(
    tmp_path, monkeypatch
):
    """A phase that publishes nothing must not rewrite generated state."""
    repo = _repo_with_upstream(tmp_path)
    target = repo / lifecyclebinding.WORKTREE_SKILL_RELATIVE_PATH
    target.parent.mkdir(parents=True)
    target.write_bytes(b"generated sentinel\n")
    before = _git(repo, "rev-parse", "HEAD")

    def fail_if_refreshed(repo_root):
        raise AssertionError("a landing that moved no HEAD rematerialized the skill")

    monkeypatch.setattr(
        boundaries, "_refresh_generated_skill_after_advance", fail_if_refreshed
    )

    result = boundaries.integrate_and_publish(
        "TASK-1kH7Ld3r", repo_root=repo, meta=LANDING_SKILL_META
    )

    assert _git(repo, "rev-parse", "HEAD") == before
    assert target.read_bytes() == b"generated sentinel\n"
    assert result.notes == []


def test_integrate_and_publish_leaves_generated_assets_alone_when_it_cannot_land(
    tmp_path, monkeypatch
):
    """A conflict and a refusal both stop before any generated rewrite.

    Each raises while HEAD sits on a merge that never reached the baseline, so
    refreshing there would rewrite the lane's skill from a tree no peer will
    ever see. Both shapes are asserted rather than left to the call's position
    in the function, because that position is what a later edit can move.
    """
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")
    target = repo / lifecyclebinding.WORKTREE_SKILL_RELATIVE_PATH
    target.parent.mkdir(parents=True)
    target.write_bytes(b"generated sentinel\n")

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

    def fail_if_refreshed(repo_root):
        raise AssertionError("a landing that never published rematerialized the skill")

    monkeypatch.setattr(
        boundaries, "_refresh_generated_skill_after_advance", fail_if_refreshed
    )

    with pytest.raises(boundaries.MergeConflict):
        boundaries.integrate_and_publish(
            "TASK-1kH7Ld3r", repo_root=repo, meta=LANDING_SKILL_META
        )
    assert target.read_bytes() == b"generated sentinel\n"

    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "rm", "-f", "peer.txt")
    _run(repo, "git", "commit", "-m", "Resolve baseline overlap sloppily")

    with pytest.raises(SpiceError):
        boundaries.integrate_and_publish(
            "TASK-1kH7Ld3r", repo_root=repo, meta=LANDING_SKILL_META
        )
    assert target.read_bytes() == b"generated sentinel\n"
