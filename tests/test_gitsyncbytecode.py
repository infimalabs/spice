"""Stale bytecode is purged when boundaries rewrites the working tree."""

from __future__ import annotations

import os
import py_compile
import shutil
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.tasks.git import boundaries, merging, plumbing
from tests.test_permissionhelpers import REQUIRES_MODE_BIT_DENIAL
from tests.test_taskgitsync import (
    ACTOR_A,
    _configure_git_identity,
    _git,
    _repo_with_upstream,
    _run,
)

OLD_MODULE_SOURCE = 'VALUE = "aaaaaa"\n'
NEW_MODULE_SOURCE = 'VALUE = "bbbbbb"\n'
FIND_SPEC_PROBE = "import importlib.util; print(importlib.util.find_spec('pkg'))"
VALUE_PROBE = "import pkg.mod; print(pkg.mod.VALUE)"
TreeMove = Callable[..., boundaries.SyncResult]


def _seed_package_baseline(tmp_path: Path, module_source: str) -> Path:
    """Publish a baseline holding ``pkg`` and compile its bytecode locally."""
    repo = _repo_with_upstream(tmp_path)
    (repo / ".gitignore").write_text("__pycache__\n", encoding="utf-8")
    package = repo / "pkg"
    package.mkdir()
    (package / "__init__.py").write_text("", encoding="utf-8")
    (package / "mod.py").write_text(module_source, encoding="utf-8")
    _run(repo, "git", "add", ".")
    _run(repo, "git", "commit", "-m", "seed package")
    _run(repo, "git", "push", "origin", "main")
    py_compile.compile(str(package / "__init__.py"), doraise=True)
    py_compile.compile(str(package / "mod.py"), doraise=True)
    return repo


def _peer_pushes(tmp_path: Path, mutate: Callable[[Path], None]) -> None:
    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(tmp_path / "remote.git"), str(peer))
    _configure_git_identity(peer)
    mutate(peer)
    _run(peer, "git", "add", "-A")
    _run(peer, "git", "commit", "-m", "peer change")
    _run(peer, "git", "push", "origin", "main")


def _publish_agent_work(repo: Path) -> None:
    (repo / "agent.txt").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "agent.txt")
    _run(repo, "git", "commit", "-m", "agent work")
    boundaries.integrate_and_publish(
        "TASK-1k98v0WX",
        repo_root=repo,
        meta={
            "title": "Publish task work",
            "actor": ACTOR_A,
            "phase": "todo",
            "project": "task.unit",
        },
    )


def _fresh_interpreter_output(repo: Path, probe: str) -> str:
    completed = subprocess.run(
        [sys.executable, "-c", probe],
        cwd=repo,
        capture_output=True,
        check=True,
        text=True,
    )
    return completed.stdout.strip()


def test_publication_merge_deleting_package_purges_bytecode_and_directory(tmp_path):
    repo = _seed_package_baseline(tmp_path, OLD_MODULE_SOURCE)

    def delete_package(peer: Path) -> None:
        shutil.rmtree(peer / "pkg")

    _peer_pushes(tmp_path, delete_package)
    _publish_agent_work(repo)

    # `.agents` is the generated worktree skill the landing rematerializes, the
    # same ignored artifact the claim and launch advances write. It is listed
    # here because this assertion enumerates the whole tree: the subject is that
    # `pkg` and its bytecode are gone, not that the tree holds nothing else.
    assert sorted(entry.name for entry in repo.iterdir()) == [
        ".agents",
        ".git",
        ".gitignore",
        "README.md",
        "agent.txt",
    ]
    assert _git(repo, "status", "--porcelain") == ""
    assert _fresh_interpreter_output(repo, FIND_SPEC_PROBE) == "None"


def test_publication_merge_changing_module_defeats_mtime_size_collision(tmp_path):
    assert len(OLD_MODULE_SOURCE) == len(NEW_MODULE_SOURCE)
    repo = _seed_package_baseline(tmp_path, OLD_MODULE_SOURCE)
    module_path = repo / "pkg" / "mod.py"
    compiled_before = sorted((repo / "pkg" / "__pycache__").glob("mod.*"))
    assert len(compiled_before) == 1
    source_mtime = module_path.stat().st_mtime

    def rewrite_module(peer: Path) -> None:
        (peer / "pkg" / "mod.py").write_text(NEW_MODULE_SOURCE, encoding="utf-8")

    _peer_pushes(tmp_path, rewrite_module)
    _publish_agent_work(repo)

    # Re-arm the hazard the purge must defeat: identical byte size plus the
    # restored mtime is exactly the (mtime, size) validation key a surviving
    # .pyc from the pre-merge source would still match.
    os.utime(module_path, (source_mtime, source_mtime))

    assert module_path.read_text(encoding="utf-8") == NEW_MODULE_SOURCE
    assert _fresh_interpreter_output(repo, VALUE_PROBE) == "bbbbbb"


@pytest.mark.parametrize(
    "tree_move",
    [boundaries.prepare_for_claim, boundaries.fast_forward_if_safe],
    ids=["prepare-for-claim", "fast-forward-if-safe"],
)
def test_tree_move_does_not_follow_replacement_package_symlink(
    tmp_path: Path, tree_move: TreeMove
) -> None:
    repo = _seed_package_baseline(tmp_path, OLD_MODULE_SOURCE)
    external = tmp_path / "external-package"
    external_cache = external / "__pycache__"
    external_cache.mkdir(parents=True)
    (external / "sentinel.txt").write_text("outside package\n", encoding="utf-8")
    (external_cache / "mod.cpython-313.pyc").write_bytes(b"outside bytecode\n")
    before = _file_bytes(external)

    def replace_package_with_symlink(peer: Path) -> None:
        shutil.rmtree(peer / "pkg")
        (peer / "pkg").symlink_to(external, target_is_directory=True)

    _peer_pushes(tmp_path, replace_package_with_symlink)

    tree_move(repo_root=repo)

    assert (repo / "pkg").is_symlink() is True
    assert _file_bytes(external) == before


def test_tree_move_does_not_follow_cache_directory_symlink(tmp_path: Path) -> None:
    repo = _seed_package_baseline(tmp_path, OLD_MODULE_SOURCE)
    external_cache = tmp_path / "external-cache"
    external_cache.mkdir()
    (external_cache / "mod.cpython-313.pyc").write_bytes(b"outside bytecode\n")
    before = _file_bytes(external_cache)
    shutil.rmtree(repo / "pkg" / "__pycache__")
    (repo / "pkg" / "__pycache__").symlink_to(external_cache, target_is_directory=True)

    def rewrite_module(peer: Path) -> None:
        (peer / "pkg" / "mod.py").write_text(NEW_MODULE_SOURCE, encoding="utf-8")

    _peer_pushes(tmp_path, rewrite_module)

    boundaries.prepare_for_claim(repo)

    assert (repo / "pkg" / "__pycache__").is_symlink() is True
    assert _file_bytes(external_cache) == before


@pytest.mark.parametrize(
    "tree_move",
    [boundaries.prepare_for_claim, boundaries.fast_forward_if_safe],
    ids=["prepare-for-claim", "fast-forward-if-safe"],
)
@REQUIRES_MODE_BIT_DENIAL
def test_tree_move_survives_denied_cache_cleanup_and_reports_it(
    tmp_path: Path, tree_move: TreeMove
) -> None:
    repo = _seed_package_baseline(tmp_path, OLD_MODULE_SOURCE)

    def rewrite_module(peer: Path) -> None:
        (peer / "pkg" / "mod.py").write_text(NEW_MODULE_SOURCE, encoding="utf-8")

    _peer_pushes(tmp_path, rewrite_module)
    cache = repo / "pkg" / "__pycache__"
    cache.chmod(0o500)
    try:
        result = tree_move(repo_root=repo)
    finally:
        cache.chmod(0o755)

    assert {
        "head": _git_out(repo, "rev-parse", "HEAD"),
        "clean": _git_out(repo, "status", "--porcelain"),
        "module": (repo / "pkg" / "mod.py").read_text(encoding="utf-8"),
        "guidance": sum(
            "stale bytecode kept for pkg/mod.py" in note for note in result.notes
        ),
        "kept_artifacts": len(sorted(cache.glob("mod.*"))),
    } == {
        "head": _git_out(repo, "rev-parse", "origin/main"),
        "clean": "",
        "module": NEW_MODULE_SOURCE,
        "guidance": 1,
        "kept_artifacts": 1,
    }


@REQUIRES_MODE_BIT_DENIAL
def test_head_advance_completes_when_cache_cleanup_is_denied(tmp_path: Path) -> None:
    repo = _seed_package_baseline(tmp_path, OLD_MODULE_SOURCE)
    old_head = _git_out(repo, "rev-parse", "HEAD")
    (repo / "pkg" / "mod.py").write_text(NEW_MODULE_SOURCE, encoding="utf-8")
    _run(repo, "git", "commit", "-am", "advance module")
    new_head = _git_out(repo, "rev-parse", "HEAD")
    _run(repo, "git", "reset", "--hard", old_head)
    cache = repo / "pkg" / "__pycache__"
    cache.chmod(0o500)
    try:
        merging._materialize_and_update_head(
            repo,
            new_head=new_head,
            expected_head=old_head,
            label="TASK-1k98v0WX",
            action="advance branch for fault probe",
        )
    finally:
        cache.chmod(0o755)

    assert {
        "head": _git_out(repo, "rev-parse", "HEAD"),
        "clean": _git_out(repo, "status", "--porcelain"),
        "module": (repo / "pkg" / "mod.py").read_text(encoding="utf-8"),
        "kept_artifacts": len(sorted(cache.glob("mod.*"))),
    } == {
        "head": new_head,
        "clean": "",
        "module": NEW_MODULE_SOURCE,
        "kept_artifacts": 1,
    }


@REQUIRES_MODE_BIT_DENIAL
def test_conflict_materialization_completes_when_cache_cleanup_is_denied(
    tmp_path: Path,
) -> None:
    repo = _seed_package_baseline(tmp_path, OLD_MODULE_SOURCE)
    agent_head = _git_out(repo, "rev-parse", "HEAD")
    (repo / "pkg" / "mod.py").write_text(NEW_MODULE_SOURCE, encoding="utf-8")
    _run(repo, "git", "commit", "-am", "upstream module change")
    upstream_head = _git_out(repo, "rev-parse", "HEAD")
    merged_tree = _git_out(repo, "rev-parse", f"{upstream_head}^{{tree}}")
    base_blob = _git_out(repo, "rev-parse", f"{agent_head}:pkg/mod.py")
    theirs_blob = _git_out(repo, "rev-parse", f"{upstream_head}:pkg/mod.py")
    _run(repo, "git", "reset", "--hard", agent_head)
    cache = repo / "pkg" / "__pycache__"
    cache.chmod(0o500)
    try:
        merging.materialize_merge_conflict(
            repo,
            merged_tree=merged_tree,
            conflict_records=[
                f"100644 {base_blob} 1\tpkg/mod.py",
                f"100644 {theirs_blob} 3\tpkg/mod.py",
            ],
            agent_head=agent_head,
            upstream_head=upstream_head,
            message="fault probe merge",
        )
    finally:
        cache.chmod(0o755)

    merge_head = repo / _git_out(repo, "rev-parse", "--git-path", "MERGE_HEAD")
    orig_head = repo / _git_out(repo, "rev-parse", "--git-path", "ORIG_HEAD")
    unmerged = [
        line.split() for line in _git_out(repo, "ls-files", "--unmerged").splitlines()
    ]
    assert {
        "merge_head": merge_head.read_text(encoding="utf-8"),
        "orig_head": orig_head.read_text(encoding="utf-8"),
        "module": (repo / "pkg" / "mod.py").read_text(encoding="utf-8"),
        "stages": [(entry[2], entry[3]) for entry in unmerged],
        "kept_artifacts": len(sorted(cache.glob("mod.*"))),
    } == {
        "merge_head": f"{upstream_head}\n",
        "orig_head": f"{agent_head}\n",
        "module": NEW_MODULE_SOURCE,
        "stages": [("1", "pkg/mod.py"), ("3", "pkg/mod.py")],
        "kept_artifacts": 1,
    }


def _install_diff_read_failure(monkeypatch: pytest.MonkeyPatch) -> None:
    real_read = plumbing.read

    def read_with_injected_diff_failure(root: Path, *args: str) -> str:
        if args and args[0] == "diff":
            raise SpiceError("injected: cleanup diff unavailable")
        return real_read(root, *args)

    monkeypatch.setattr(plumbing, "read", read_with_injected_diff_failure)


def test_head_advance_completes_when_cleanup_diff_discovery_fails(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = _seed_package_baseline(tmp_path, OLD_MODULE_SOURCE)
    old_head = _git_out(repo, "rev-parse", "HEAD")
    (repo / "pkg" / "mod.py").write_text(NEW_MODULE_SOURCE, encoding="utf-8")
    _run(repo, "git", "commit", "-am", "advance module")
    new_head = _git_out(repo, "rev-parse", "HEAD")
    _run(repo, "git", "reset", "--hard", old_head)
    _install_diff_read_failure(monkeypatch)

    merging._materialize_and_update_head(
        repo,
        new_head=new_head,
        expected_head=old_head,
        label="TASK-1k98v0WX",
        action="advance branch for discovery fault probe",
    )

    assert {
        "head": _git_out(repo, "rev-parse", "HEAD"),
        "clean": _git_out(repo, "status", "--porcelain"),
        "module": (repo / "pkg" / "mod.py").read_text(encoding="utf-8"),
    } == {
        "head": new_head,
        "clean": "",
        "module": NEW_MODULE_SOURCE,
    }


def test_conflict_materialization_completes_when_diff_discovery_fails(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = _seed_package_baseline(tmp_path, OLD_MODULE_SOURCE)
    agent_head = _git_out(repo, "rev-parse", "HEAD")
    (repo / "pkg" / "mod.py").write_text(NEW_MODULE_SOURCE, encoding="utf-8")
    _run(repo, "git", "commit", "-am", "upstream module change")
    upstream_head = _git_out(repo, "rev-parse", "HEAD")
    merged_tree = _git_out(repo, "rev-parse", f"{upstream_head}^{{tree}}")
    base_blob = _git_out(repo, "rev-parse", f"{agent_head}:pkg/mod.py")
    _run(repo, "git", "reset", "--hard", agent_head)
    _install_diff_read_failure(monkeypatch)

    merging.materialize_merge_conflict(
        repo,
        merged_tree=merged_tree,
        conflict_records=[f"100644 {base_blob} 1\tpkg/mod.py"],
        agent_head=agent_head,
        upstream_head=upstream_head,
        message="discovery fault probe merge",
    )

    merge_head = repo / _git_out(repo, "rev-parse", "--git-path", "MERGE_HEAD")
    unmerged = [
        line.split() for line in _git_out(repo, "ls-files", "--unmerged").splitlines()
    ]
    assert {
        "merge_head": merge_head.read_text(encoding="utf-8"),
        "module": (repo / "pkg" / "mod.py").read_text(encoding="utf-8"),
        "stages": [(entry[2], entry[3]) for entry in unmerged],
    } == {
        "merge_head": f"{upstream_head}\n",
        "module": NEW_MODULE_SOURCE,
        "stages": [("1", "pkg/mod.py")],
    }


def test_prepare_for_claim_reports_unknown_scope_when_diff_discovery_fails(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = _seed_package_baseline(tmp_path, OLD_MODULE_SOURCE)

    def rewrite_module(peer: Path) -> None:
        (peer / "pkg" / "mod.py").write_text(NEW_MODULE_SOURCE, encoding="utf-8")

    _peer_pushes(tmp_path, rewrite_module)
    _install_diff_read_failure(monkeypatch)

    result = boundaries.prepare_for_claim(repo)

    guidance = f"stale bytecode kept for {plumbing.BYTECODE_SCOPE_UNKNOWN}"
    assert {
        "head": _git_out(repo, "rev-parse", "HEAD"),
        "clean": _git_out(repo, "status", "--porcelain"),
        "module": (repo / "pkg" / "mod.py").read_text(encoding="utf-8"),
        "guidance": sum(guidance in note for note in result.notes),
    } == {
        "head": _git_out(repo, "rev-parse", "origin/main"),
        "clean": "",
        "module": NEW_MODULE_SOURCE,
        "guidance": 1,
    }


def test_fast_forward_reports_every_candidate_when_root_open_is_denied(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = _seed_package_baseline(tmp_path, OLD_MODULE_SOURCE)

    def rewrite_module(peer: Path) -> None:
        (peer / "pkg" / "mod.py").write_text(NEW_MODULE_SOURCE, encoding="utf-8")

    _peer_pushes(tmp_path, rewrite_module)

    def deny_root_open(repo_root: Path, directory_flags: int) -> int:
        raise PermissionError(13, "Permission denied", str(repo_root))

    monkeypatch.setattr(plumbing, "_open_worktree_root", deny_root_open)
    cache = repo / "pkg" / "__pycache__"

    result = boundaries.fast_forward_if_safe(repo_root=repo)

    assert {
        "head": _git_out(repo, "rev-parse", "HEAD"),
        "clean": _git_out(repo, "status", "--porcelain"),
        "guidance": sum(
            "stale bytecode kept for pkg/mod.py" in note for note in result.notes
        ),
        "kept_artifacts": len(sorted(cache.glob("mod.*"))),
    } == {
        "head": _git_out(repo, "rev-parse", "origin/main"),
        "clean": "",
        "guidance": 1,
        "kept_artifacts": 1,
    }


def test_close_quietly_contains_descriptor_teardown_failure(tmp_path: Path) -> None:
    fd = os.open(tmp_path, os.O_RDONLY)
    assert (plumbing._close_quietly(fd), plumbing._close_quietly(fd)) == (
        None,
        None,
    )


def _git_out(repo: Path, *args: str) -> str:
    return _run(repo, "git", *args).stdout.strip()


def _file_bytes(root: Path) -> dict[Path, bytes]:
    return {
        path.relative_to(root): path.read_bytes()
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }
