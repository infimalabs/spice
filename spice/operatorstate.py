"""One-release migration for operator-owned worktree state.

Operator-authored configuration and initialization authority belong beside
the rest of one worktree's durable state, under that worktree's Git directory.
The visible ``<worktree>/.spice`` paths used before v0.30.0 are accepted only
as untracked upgrade inputs, moved once, and then refused.
"""

from __future__ import annotations

import os
import stat
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from spice.errors import SpiceError
from spice.paths import (
    STATE_DIRNAME,
    atomic_write_json,
    fsync_directory,
    worktree_state_path,
)
from spice.process.git import run_git_command

OPERATOR_STATE_RELOCATION_RELEASE = "v0.30.0"
OPERATOR_STATE_MIGRATION_SCHEMA_VERSION = 1


@dataclass(frozen=True)
class OperatorStatePath:
    """One operator-owned file withdrawn from the visible worktree."""

    key: str
    label: str
    withdrawn_relative: Path
    canonical_relative: Path


WORKTREE_CONFIG_PATH = OperatorStatePath(
    key="worktree-config",
    label="worktree configuration",
    withdrawn_relative=Path(STATE_DIRNAME) / "config" / "spice.toml",
    canonical_relative=Path("config") / "spice.toml",
)
INITIALIZATION_RECEIPT_PATH = OperatorStatePath(
    key="initialization-receipt",
    label="initialization receipt",
    withdrawn_relative=Path(STATE_DIRNAME) / "init-receipt.json",
    canonical_relative=Path("init-receipt.jsonl"),
)


def operator_state_path(repo_root: Path, declared: OperatorStatePath) -> Path:
    """Return one file's canonical worktree-Git-dir location without mutation."""
    return worktree_state_path(
        repo_root.expanduser().resolve(), declared.canonical_relative
    )


def prepare_operator_state_path(
    repo_root: Path,
    declared: OperatorStatePath,
    *,
    canonical_path: Path | None = None,
    migrate: Callable[[Path, Path], None] | None = None,
) -> Path:
    """Move one untracked predecessor once, or refuse a withdrawn path.

    A tracked predecessor is repository-delivered input, not operator-local
    upgrade state, so it is never honored. Once either a canonical file or the
    durable migration marker exists, a predecessor that reappears is refused
    rather than silently becoming a second configuration or authority source.
    """
    resolved_root = repo_root.expanduser().resolve()
    canonical = canonical_path or operator_state_path(resolved_root, declared)
    withdrawn = resolved_root / declared.withdrawn_relative
    _refuse_symlinked_withdrawn_path(
        resolved_root,
        withdrawn,
        declared,
        canonical,
    )
    if not withdrawn.exists() and not withdrawn.is_symlink():
        return canonical

    marker = operator_state_migration_marker(resolved_root, declared)
    if _tracked_path(resolved_root, declared.withdrawn_relative):
        raise SpiceError(
            f"remove {declared.withdrawn_relative.as_posix()} from the repository; "
            f"tracked {declared.label} path {withdrawn} was withdrawn in "
            f"{OPERATOR_STATE_RELOCATION_RELEASE} and is never honored; use the "
            f"worktree-local path {canonical}"
        )
    if marker.exists() or canonical.exists():
        raise SpiceError(
            f"remove {withdrawn}; {declared.label} path was withdrawn in "
            f"{OPERATOR_STATE_RELOCATION_RELEASE} and has already been migrated; "
            f"use {canonical}"
        )
    if withdrawn.is_symlink() or not withdrawn.is_file():
        raise SpiceError(
            f"remove {withdrawn}; {declared.label} path was withdrawn in "
            f"{OPERATOR_STATE_RELOCATION_RELEASE} and cannot be migrated because "
            "it is not a regular file"
        )

    try:
        canonical.parent.mkdir(parents=True, exist_ok=True)
        if migrate is None:
            os.replace(withdrawn, canonical)
        else:
            migrate(withdrawn, canonical)
        fsync_directory(canonical.parent)
        fsync_directory(withdrawn.parent)
        atomic_write_json(
            marker,
            {
                "schema_version": OPERATOR_STATE_MIGRATION_SCHEMA_VERSION,
                "release": OPERATOR_STATE_RELOCATION_RELEASE,
                "kind": declared.key,
                "withdrawn_path": declared.withdrawn_relative.as_posix(),
                "canonical_path": str(canonical),
            },
            write_if_changed=True,
        )
        _remove_empty_withdrawn_parent(withdrawn.parent, resolved_root)
    except OSError as exc:
        raise SpiceError(
            f"could not migrate {declared.label} from {withdrawn} to {canonical}: {exc}"
        ) from exc
    return canonical


def _refuse_symlinked_withdrawn_path(
    repo_root: Path,
    withdrawn: Path,
    declared: OperatorStatePath,
    canonical: Path,
) -> None:
    """Inspect every withdrawn-path component without following redirects."""
    current = repo_root
    for component in declared.withdrawn_relative.parts:
        current /= component
        try:
            mode = current.lstat().st_mode
        except (FileNotFoundError, NotADirectoryError):
            return
        except OSError as exc:
            raise SpiceError(
                f"could not inspect withdrawn {declared.label} path {withdrawn} "
                f"for {OPERATOR_STATE_RELOCATION_RELEASE} migration: {exc}"
            ) from exc
        if not stat.S_ISLNK(mode):
            continue
        role = "path" if current == withdrawn else "ancestor"
        raise SpiceError(
            f"remove {current}; {declared.label} path {withdrawn} was withdrawn in "
            f"{OPERATOR_STATE_RELOCATION_RELEASE} and cannot be migrated because "
            f"its {role} {current} is a symlink; use {canonical}"
        )


def operator_state_migration_marker(
    repo_root: Path, declared: OperatorStatePath
) -> Path:
    return worktree_state_path(
        repo_root.expanduser().resolve(),
        Path("migrations") / f"{OPERATOR_STATE_RELOCATION_RELEASE}-{declared.key}.json",
    )


def _tracked_path(repo_root: Path, relative: Path) -> bool:
    result = run_git_command(
        [
            "git",
            "-C",
            str(repo_root),
            "ls-files",
            "--error-unmatch",
            "--",
            relative.as_posix(),
        ],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode == 0:
        return True
    if result.returncode == 1:
        return False
    detail = (result.stderr or result.stdout or "").strip()
    raise SpiceError(
        f"could not determine whether withdrawn operator-state path "
        f"{relative.as_posix()} is tracked: {detail or f'git exited {result.returncode}'}"
    )


def _remove_empty_withdrawn_parent(parent: Path, repo_root: Path) -> None:
    visible_state_root = repo_root / STATE_DIRNAME
    current = parent
    while current != visible_state_root:
        try:
            current.rmdir()
        except OSError:
            return
        current = current.parent
