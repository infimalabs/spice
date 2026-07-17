"""Stale bytecode is purged when gitsync rewrites the working tree."""

from __future__ import annotations

import os
import py_compile
import shutil
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path

import pytest

from spice.tasks import gitsync
from tests.test_taskgitsync import (
    ACTOR_A,
    _configure_git_identity,
    _repo_with_upstream,
    _run,
)

OLD_MODULE_SOURCE = 'VALUE = "aaaaaa"\n'
NEW_MODULE_SOURCE = 'VALUE = "bbbbbb"\n'
FIND_SPEC_PROBE = "import importlib.util; print(importlib.util.find_spec('pkg'))"
VALUE_PROBE = "import pkg.mod; print(pkg.mod.VALUE)"
TreeMove = Callable[..., gitsync.SyncResult]


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
    gitsync.integrate_and_publish(
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

    assert sorted(entry.name for entry in repo.iterdir()) == [
        ".git",
        ".gitignore",
        "README.md",
        "agent.txt",
    ]
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
    [gitsync.prepare_for_claim, gitsync.fast_forward_if_safe],
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

    gitsync.prepare_for_claim(repo)

    assert (repo / "pkg" / "__pycache__").is_symlink() is True
    assert _file_bytes(external_cache) == before


def _file_bytes(root: Path) -> dict[Path, bytes]:
    return {
        path.relative_to(root): path.read_bytes()
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }
