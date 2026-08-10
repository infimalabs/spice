"""Flex jitter follows authored content across Git and worktree boundaries."""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from spice import flexprovenance
from spice.agent.paths import write_agent_thread_pointer
from spice.errors import SpiceError
from spice.flexprovenance import FlexProvenanceResolver, preload_flex_provenance
from spice.hooks import precommit
from spice.policyconfig import jittered_flex_limit, resolve_policy

AUTHOR_ACTOR = "019fa76d7ca1721183f3bd3ada11c2ec"
PEER_ACTOR = "019fa76d88b173819bf8772fa69bea27"
LIFECYCLE_PATH = Path("spice/agent/lifecycle.py")
BASE_LIMIT = 1000
STATIC_FLEX = 1500
AUTHORED_LINE_COUNT = 1493
AUTHOR_FLEX_LIMIT = 1495
PEER_FLEX_LIMIT = 1475
UNATTRIBUTED_FLEX_LIMIT = 1500


def test_new_candidate_content_uses_active_author_then_blob_identity(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    path = Path("new.py")
    (repo / path).write_text("value = 1\n", encoding="utf-8")

    authored = FlexProvenanceResolver(repo, "actor-a").resolve(path)
    actorless = FlexProvenanceResolver(repo, "").resolve(path)

    assert authored.source == "active-author"
    assert authored.seed == "actor-a"
    assert authored.blob
    assert actorless.source == "candidate-blob"
    assert actorless.seed == authored.blob


def test_published_task_session_is_cached_across_linked_worktrees(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    _write(repo, "shared.py", "base = True\n")
    _commit_all(repo, "seed")
    _git(repo, "switch", "-c", "author")
    _write(repo, "shared.py", "authored = True\n")
    _commit_all(repo, "author content")
    _git(repo, "switch", "main")
    _git(
        repo,
        "merge",
        "--no-ff",
        "author",
        "-m",
        f"land author content\n\nTask-Session: {AUTHOR_ACTOR}",
    )
    _publish_main_ref(repo)

    peer = tmp_path / "peer"
    _git(repo, "worktree", "add", "-q", "-b", "peer", str(peer), "HEAD")
    author_resolver = FlexProvenanceResolver(repo, AUTHOR_ACTOR)
    peer_resolver = FlexProvenanceResolver(peer, PEER_ACTOR)
    preload_flex_provenance(author_resolver, (Path("shared.py"),))
    preload_flex_provenance(peer_resolver, (Path("shared.py"),))

    author = author_resolver.resolve(Path("shared.py"))
    repeated = author_resolver.resolve(Path("shared.py"))
    observed_by_peer = peer_resolver.resolve(Path("shared.py"))

    assert author.source == "published-task-session"
    assert author.seed == AUTHOR_ACTOR
    assert repeated is author
    assert observed_by_peer == author


def test_historical_commit_without_task_session_uses_commit_identity(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    _write(repo, "historical.py", "answer = 42\n")
    commit = _commit_all(repo, "historical content")

    provenance = FlexProvenanceResolver(repo, PEER_ACTOR).resolve(Path("historical.py"))

    assert provenance.source == "published-commit"
    assert provenance.seed == commit
    assert provenance.commit == commit
    assert provenance.blob


def test_preload_batches_candidate_hashes_and_published_history(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    paths = (Path("first.py"), Path("space name.py"))
    for path in paths:
        _write(repo, path.as_posix(), f"VALUE = {path.name!r}\n")
    commit = _commit_all(repo, "published pair")
    _write(repo, "untracked.py", "VALUE = 'untracked'\n")
    resolver = FlexProvenanceResolver(repo, PEER_ACTOR)
    calls: list[tuple[str, object]] = []
    original_run_git_command = flexprovenance.run_git_command
    original_git_run = flexprovenance.git_run

    def counted_run_git_command(command, **kwargs):
        if "hash-object" in command:
            calls.append(("hash-object", kwargs.get("input")))
        return original_run_git_command(command, **kwargs)

    def counted_git_run(repo_root, *args, **kwargs):
        if args and args[0] == "log":
            calls.append(("log", args))
        return original_git_run(repo_root, *args, **kwargs)

    monkeypatch.setattr(
        flexprovenance,
        "run_git_command",
        counted_run_git_command,
    )
    monkeypatch.setattr(flexprovenance, "git_run", counted_git_run)

    preload_flex_provenance(resolver, paths)
    resolved = tuple(resolver.resolve(path) for path in paths)
    repeated = tuple(resolver.resolve(path) for path in paths)

    assert resolved == repeated
    assert [item.source for item in resolved] == [
        "published-commit",
        "published-commit",
    ]
    assert [item.commit for item in resolved] == [commit, commit]
    assert [name for name, _detail in calls] == ["hash-object", "log"]
    assert calls[0][1] == "first.py\nspace name.py\n"


def test_preload_refuses_malformed_bulk_hash_output(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    _write(repo, "tracked.py", "VALUE = 1\n")
    _commit_all(repo, "tracked source")
    resolver = FlexProvenanceResolver(repo, PEER_ACTOR)

    monkeypatch.setattr(
        flexprovenance,
        "run_git_command",
        lambda _command, **_kwargs: subprocess.CompletedProcess(
            [],
            0,
            stdout="not-an-object-id\n",
            stderr="",
        ),
    )

    with pytest.raises(SpiceError, match="hashing returned malformed output"):
        preload_flex_provenance(resolver, (Path("tracked.py"),))


def test_cross_actor_merge_retains_authored_file_shape_ceiling(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    _write(
        repo,
        "spice.toml",
        "\n".join(
            [
                "[policy.limits]",
                f"file_loc = {BASE_LIMIT}",
                "file_bytes = 1000000",
                "",
                "[policy.flex]",
                "ratio = 1.5",
                "jitter_percent = 5",
                "",
            ]
        ),
    )
    _write(repo, LIFECYCLE_PATH.as_posix(), "base = True\n")
    seed = _commit_all(repo, "seed")

    peer = tmp_path / "peer"
    _git(repo, "worktree", "add", "-q", "-b", "peer", str(peer), seed)

    _git(repo, "switch", "-c", "author")
    _write(
        repo,
        LIFECYCLE_PATH.as_posix(),
        "authored = True\n" * AUTHORED_LINE_COUNT,
    )
    _commit_all(repo, "author lifecycle content")
    _git(repo, "switch", "main")
    _git(
        repo,
        "merge",
        "--no-ff",
        "author",
        "-m",
        f"land lifecycle content\n\nTask-Session: {AUTHOR_ACTOR}",
    )
    _publish_main_ref(repo)

    write_agent_thread_pointer(repo, AUTHOR_ACTOR)
    write_agent_thread_pointer(peer, PEER_ACTOR)
    _git(peer, "merge", "--no-ff", "--no-commit", "refs/remotes/origin/main")

    author_policy = resolve_policy(repo)
    peer_policy = resolve_policy(peer)
    author_shape = author_policy.jittered_file_shape_for_path(LIFECYCLE_PATH)
    peer_shape = peer_policy.jittered_file_shape_for_path(LIFECYCLE_PATH)

    assert (
        jittered_flex_limit(BASE_LIMIT, STATIC_FLEX, LIFECYCLE_PATH, AUTHOR_ACTOR)
        == AUTHOR_FLEX_LIMIT
    )
    assert (
        jittered_flex_limit(BASE_LIMIT, STATIC_FLEX, LIFECYCLE_PATH, PEER_ACTOR)
        == PEER_FLEX_LIMIT
    )
    assert (
        jittered_flex_limit(BASE_LIMIT, STATIC_FLEX, LIFECYCLE_PATH, "")
        == UNATTRIBUTED_FLEX_LIMIT
    )
    assert author_shape.line_flex_limit == AUTHOR_FLEX_LIMIT
    assert peer_shape.line_flex_limit == author_shape.line_flex_limit
    assert peer_policy.flex_actor_id == PEER_ACTOR
    assert peer_policy.flex_seed_for_path(LIFECYCLE_PATH) == AUTHOR_ACTOR
    precommit._run_file_loc_guard(peer, [LIFECYCLE_PATH])


def _init_repo(repo: Path) -> Path:
    repo.mkdir()
    _git(repo, "init", "-q", "-b", "main")
    _git(repo, "config", "user.name", "Spice Test")
    _git(repo, "config", "user.email", "spice@example.test")
    return repo


def _write(repo: Path, path: str, content: str) -> None:
    target = repo / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content, encoding="utf-8")


def _commit_all(repo: Path, message: str) -> str:
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", message)
    return _git(repo, "rev-parse", "HEAD")


def _publish_main_ref(repo: Path) -> None:
    _git(repo, "update-ref", "refs/remotes/origin/main", "HEAD")
    _git(repo, "symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main")


def _git(repo: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
    )
    return completed.stdout.strip()
