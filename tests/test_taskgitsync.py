"""Task git publication and merge-message behavior."""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.tasks import gitsync

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
GIT_TIMEOUT_RETURN_CODE = 124


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

    result = gitsync.integrate_and_publish(
        "TASK-20260101T000000000001Z",
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
        "Publish task work\n\n"
        "Task: TASK-20260101T000000000001Z\n"
        "Task-Phase: todo\n"
        "Task-Project: task.unit\n"
        f"Task-Session: {ACTOR_A}"
    )


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
    real_run = gitsync._run
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

    monkeypatch.setattr(gitsync, "_run", racing_run)

    result = gitsync.integrate_and_publish(
        "TASK-20260101T000000000004Z",
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
    real_run = gitsync._run
    ff_attempts = 0
    raced_head = ""

    def racing_run(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
        nonlocal ff_attempts, raced_head
        if repo_root == repo and args[:2] == ("merge", "--ff-only"):
            ff_attempts += 1
            (repo / "raced.txt").write_text("local race\n", encoding="utf-8")
            _run(repo, "git", "add", "raced.txt")
            _run(repo, "git", "commit", "-m", "local race")
            raced_head = _git(repo, "rev-parse", "HEAD")
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

    monkeypatch.setattr(gitsync, "_run", racing_run)

    with pytest.raises(SpiceError) as exc_info:
        gitsync.integrate_and_publish(
            "TASK-20260101T000000000006Z",
            repo_root=repo,
            meta={
                "title": "Publish local head race",
                "actor": ACTOR_A,
                "phase": "todo",
                "project": "task.unit",
            },
        )

    message = str(exc_info.value)
    assert ff_attempts == 1
    assert "HEAD moved while spice was advancing the generated task commit" in message
    assert "task state was not advanced" in message
    assert "git status --short" in message
    assert "git rev-parse HEAD" in message
    assert 'spice task done TASK-20260101T000000000006Z --validation "..."' in message
    assert f"expected_head={agent_head}" in message
    assert f"current_head={raced_head}" in message
    assert _git(repo, "rev-parse", "HEAD") == raced_head
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == (
        upstream_head
    )
    assert _git(repo, "status", "--porcelain") == ""


def test_merge_message_omits_task_description_body():
    message = gitsync._compose_message(
        "TASK-20260101T000000000003Z",
        {
            "title": "Fix image labels",
            "description": (
                "Operator steering 20260612T043642083543Z: the labels "
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
        "Fix image labels\n\n"
        "Task: TASK-20260101T000000000003Z\n"
        "Task-Phase: todo\n"
        "Task-Project: serve.ui\n"
        f"Task-Session: {ACTOR_A}"
    )


def test_merge_message_uses_fallback_subject_and_trailers_only():
    message = gitsync._compose_message(
        "TASK-20260101T000000000004Z",
        {
            "title": "",
            "description": (
                "Operator steering 20260612T054500966259Z: final task merge "
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
        "Integrate TASK-20260101T000000000004Z\n\n"
        "Task: TASK-20260101T000000000004Z\n"
        "Task-Phase: todo\n"
        "Task-Project: task\n"
        f"Task-Session: {ACTOR_A}"
    )


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
        gitsync.integrate_and_publish("TASK-20260101T000000000002Z", repo_root=repo)

    message = str(exc_info.value)
    assert "README.md" in message
    assert "keep the merge state open" in message
    assert "commit while MERGE_HEAD exists" in message
    assert "git status --short" in message
    assert "git rev-parse --verify MERGE_HEAD" in message
    assert "git add -- README.md" in message
    assert 'spice task done TASK-20260101T000000000002Z --validation "..."' in message
    assert _git(repo, "rev-parse", "--verify", "MERGE_HEAD") == upstream_head
    assert _git(repo, "status", "--porcelain") == "UU README.md"

    (repo / "README.md").write_text("resolved work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(
        repo,
        "git",
        "commit",
        "-m",
        "Resolve baseline overlap for TASK-20260101T000000000002Z",
    )

    result = gitsync.integrate_and_publish(
        "TASK-20260101T000000000002Z", repo_root=repo
    )
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert captured["done_upstream_head"] == upstream_head
    assert _merge_parents(repo, merge_head)[0] == upstream_head
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""


def test_integrate_and_publish_treats_missing_merge_head_abort_as_cleared(
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
    abort_attempts = 0
    reset_attempts = 0

    def missing_merge_head_abort(
        repo_root: Path, *args: str
    ) -> subprocess.CompletedProcess[str]:
        nonlocal abort_attempts, reset_attempts
        if repo_root == repo and args == ("merge", "--abort"):
            abort_attempts += 1
            (repo / ".git" / "MERGE_HEAD").unlink(missing_ok=True)
            return subprocess.CompletedProcess(
                ["git", "-C", str(repo), *args],
                128,
                stdout="",
                stderr="fatal: There is no merge to abort (MERGE_HEAD missing).\n",
            )
        if repo_root == repo and args == ("reset", "--hard", "HEAD"):
            reset_attempts += 1
        return real_run(repo_root, *args)

    monkeypatch.setattr(gitsync, "_run", missing_merge_head_abort)

    result = gitsync.integrate_and_publish(
        "TASK-20260101T000000000007Z",
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

    assert abort_attempts == 1
    assert reset_attempts == 1
    assert captured["done_head"] == agent_head
    assert captured["done_upstream_head"] == upstream_head
    assert _merge_parents(repo, merge_head) == [upstream_head, agent_head]
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""


def test_integrate_and_publish_hook_aborted_marker_state_guides_retry(
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
    merge_attempts = 0

    def hook_aborted_run(
        repo_root: Path, *args: str
    ) -> subprocess.CompletedProcess[str]:
        nonlocal merge_attempts
        if (
            repo_root == repo
            and args[:3] == ("merge", "--no-ff", "--no-commit")
            and merge_attempts == 0
        ):
            merge_attempts += 1
            (repo / "README.md").write_text(
                "<<<<<<< HEAD\n"
                "agent work\n"
                "=======\n"
                "baseline work\n"
                ">>>>>>> origin/main\n",
                encoding="utf-8",
            )
            return subprocess.CompletedProcess(
                ["git", "-C", str(repo), *args],
                1,
                stdout="",
                stderr="reference-transaction hook failed before MERGE_HEAD\n",
            )
        return real_run(repo_root, *args)

    monkeypatch.setattr(gitsync, "_run", hook_aborted_run)

    with pytest.raises(gitsync.MergeConflict) as exc_info:
        gitsync.integrate_and_publish("TASK-20260101T000000000005Z", repo_root=repo)

    message = str(exc_info.value)
    assert merge_attempts == 1
    assert "without an open MERGE_HEAD" in message
    assert "README.md" in message
    assert "do not use plain `git commit`" in message
    assert "commit while MERGE_HEAD exists" not in message
    assert (
        "git commit-tree $(git write-tree) -p HEAD -p origin/main "
        '-m "Resolve baseline overlap for TASK-20260101T000000000005Z"'
    ) in message
    assert (
        'git update-ref refs/heads/$(git branch --show-current) "$merge_commit"'
        in message
    )
    assert _merge_head_missing(repo)

    (repo / "README.md").write_text("resolved hook-aborted work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    rescue_merge = _git(
        repo,
        "commit-tree",
        _git(repo, "write-tree"),
        "-p",
        "HEAD",
        "-p",
        "origin/main",
        "-m",
        "Resolve baseline overlap for TASK-20260101T000000000005Z",
    )
    _run(
        repo,
        "git",
        "update-ref",
        f"refs/heads/{_git(repo, 'branch', '--show-current')}",
        rescue_merge,
    )
    assert _git(repo, "status", "--porcelain") == ""

    result = gitsync.integrate_and_publish(
        "TASK-20260101T000000000005Z", repo_root=repo
    )
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert captured["done_head"] == rescue_merge
    assert _merge_parents(repo, rescue_merge)[1] == upstream_head
    assert _merge_parents(repo, merge_head) == [upstream_head, rescue_merge]
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""


def test_gitsync_network_commands_are_noninteractive_and_bounded(tmp_path, monkeypatch):
    seen: dict[str, object] = {}

    def fake_run(command: list[str], **kwargs):
        seen["command"] = command
        seen["env"] = kwargs["env"]
        seen["timeout"] = kwargs.get("timeout")
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(gitsync.subprocess, "run", fake_run)

    gitsync._run(tmp_path, "fetch", "origin")

    env = seen["env"]
    assert isinstance(env, dict)
    assert env["GIT_TERMINAL_PROMPT"] == "0"
    assert env["GIT_SSH_COMMAND"] == gitsync.TASK_GIT_SSH_COMMAND
    assert seen["timeout"] == gitsync.GIT_NETWORK_TIMEOUT_SECONDS


def test_gitsync_network_timeout_returns_failed_process(tmp_path, monkeypatch):
    def fake_run(command: list[str], **kwargs):
        raise subprocess.TimeoutExpired(
            command, kwargs["timeout"], output="partial", stderr="stalled"
        )

    monkeypatch.setattr(gitsync.subprocess, "run", fake_run)

    completed = gitsync._run(tmp_path, "fetch", "origin")

    assert completed.returncode == GIT_TIMEOUT_RETURN_CODE
    assert completed.stdout == "partial"
    assert "git fetch timed out after 30s" in completed.stderr


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
        gitsync.integrate_and_publish("TASK-20260101T000000000003Z", repo_root=repo)

    conflicted = (repo / "README.md").read_text(encoding="utf-8")
    assert "<<<<<<<" in conflicted
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "Resolve baseline overlap, badly")

    with pytest.raises(SpiceError, match="conflict markers") as exc_info:
        gitsync.integrate_and_publish("TASK-20260101T000000000003Z", repo_root=repo)

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

    result = gitsync.integrate_and_publish(
        "TASK-20260101T000000000003Z", repo_root=repo
    )
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


def _uda_map(args: list[str]) -> dict[str, str]:
    return dict(item.split(":", 1) for item in args)


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
