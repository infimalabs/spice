"""Disposable scratch checkouts for studies that execute mutated source.

A scratch root reproduces the caller's effective tested content -- tracked,
staged, unstaged, and untracked files exactly as the worktree presents them,
minus ignored runtime artifacts -- in a disposable directory under the
worktree's managed-state area. The caller's checkout is only ever read:
mutants apply inside the scratch root, so concurrent control-plane commands
and supervisor probes keep observing the caller's real files.

Each root carries an ownership marker naming the owning process. Normal
exits retire the root atomically (rename away, then delete); uncatchable
termination leaves the root behind, and the next invocation scavenges roots
whose owner process is gone without touching live concurrent runs.
"""

from __future__ import annotations

import json
import os
import re
import secrets
import shutil
from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

from spice import paths
from spice.errors import SpiceError
from spice.gitprocess import run_git_command

SCRATCH_STATE_DIR = "mutations/scratch"
OWNER_MARKER_NAME = "SCRATCH_OWNER.json"
TRASH_PREFIX = "trash-"
_RUN_PREFIX = "run-"
_RUN_TOKEN_BYTES = 4
_RUN_NAME_RE = re.compile(r"^run-(\d+)-[0-9a-f]+$")


@dataclass(frozen=True)
class ScratchRecovery:
    removed: tuple[str, ...] = ()


@contextmanager
def scratch_checkout(root: Path) -> Iterator[tuple[Path, ScratchRecovery]]:
    """Yield a seeded disposable scratch root plus the scavenge report.

    Scavenging runs first so an abandoned root from an uncatchable prior
    termination is recovered by the very next study; the fresh root is then
    seeded from the caller's effective snapshot and removed on every exit
    path, including baseline failure and interruption.
    """
    parent = scratch_parent(root)
    parent.mkdir(parents=True, exist_ok=True)
    recovery = scavenge_abandoned_roots(parent)
    name = f"{_RUN_PREFIX}{os.getpid()}-{secrets.token_hex(_RUN_TOKEN_BYTES)}"
    scratch_root = parent / name
    scratch_root.mkdir()
    _write_owner_marker(scratch_root)
    try:
        seed_effective_snapshot(root, scratch_root)
        yield scratch_root, recovery
    finally:
        remove_scratch_root(scratch_root)


def scratch_parent(root: Path) -> Path:
    """The per-worktree directory that holds every scratch root."""
    return paths.worktree_state_path(root, SCRATCH_STATE_DIR)


def seed_effective_snapshot(root: Path, scratch_root: Path) -> list[Path]:
    """Copy the caller's effective tested content into the scratch root.

    ``git ls-files --cached --others --exclude-standard`` enumerates tracked
    plus unignored-untracked names; copying worktree bytes reproduces exactly
    what pytest observes in the caller -- staged-only additions, unstaged
    edits, and untracked files included; worktree deletions and ignored
    runtime artifacts absent -- rather than silently testing clean HEAD.
    """
    result = run_git_command(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=root,
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise SpiceError("mutation scratch seeding requires a git worktree")
    seeded: list[Path] = []
    for name in dict.fromkeys(part for part in result.stdout.split("\0") if part):
        source = root / name
        # Index-only rows whose worktree file is deleted, and nested-repo
        # directory entries, are absent from the effective tested content.
        if not source.is_file():
            continue
        target = scratch_root / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        seeded.append(Path(name))
    return seeded


def scavenge_abandoned_roots(parent: Path) -> ScratchRecovery:
    """Remove scratch roots whose owning process is gone; report the names.

    Liveness comes from the marker pid, with the pid embedded in the root
    name as a fallback for a crash between mkdir and marker write; a root
    whose owner is still alive is left untouched. Trash-prefixed roots are
    partial deletions from a prior exit and are always finished off.
    """
    if not parent.is_dir():
        return ScratchRecovery()
    removed: list[str] = []
    for entry in sorted(parent.iterdir()):
        if not entry.is_dir():
            continue
        if entry.name.startswith(TRASH_PREFIX):
            shutil.rmtree(entry, ignore_errors=True)
            removed.append(entry.name)
            continue
        pid = _owner_pid(entry)
        if pid is not None and _process_alive(pid):
            continue
        remove_scratch_root(entry)
        removed.append(entry.name)
    return ScratchRecovery(removed=tuple(removed))


def remove_scratch_root(scratch_root: Path) -> None:
    """Atomically retire a scratch root: rename away, then delete.

    The rename is the commit point -- once it lands the root can never be
    mistaken for a live run, and a crash mid-delete leaves only a
    trash-prefixed remnant the next scavenge finishes off.
    """
    trash = scratch_root.with_name(f"{TRASH_PREFIX}{scratch_root.name}")
    try:
        os.rename(scratch_root, trash)
    except FileNotFoundError:
        return
    except OSError:
        trash = scratch_root
    shutil.rmtree(trash, ignore_errors=True)


def _write_owner_marker(scratch_root: Path) -> None:
    paths.atomic_write_json(
        scratch_root / OWNER_MARKER_NAME,
        {"pid": os.getpid(), "scratch_root": str(scratch_root)},
        compact=True,
    )


def _owner_pid(entry: Path) -> int | None:
    try:
        marker = json.loads((entry / OWNER_MARKER_NAME).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        marker = {}
    pid = marker.get("pid") if isinstance(marker, dict) else None
    if isinstance(pid, int):
        return pid
    match = _RUN_NAME_RE.match(entry.name)
    return int(match.group(1)) if match else None


def _process_alive(pid: int) -> bool:
    if pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True
