"""Repo roots, the `.spice/` state directory, and atomic file writes."""

from __future__ import annotations

import contextlib
import errno
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path
from subprocess import CalledProcessError
from typing import Any

from spice.gitprocess import run_git_command

STATE_DIRNAME = ".spice"
SHARED_ATTACHMENT_DIR = Path("attachments")


def repo_root_from_cwd(cwd: Path | None = None) -> Path | None:
    """Resolve the enclosing git worktree root, or None outside git."""
    try:
        result = run_git_command(
            ["git", "-C", str(cwd or Path.cwd()), "rev-parse", "--show-toplevel"],
            capture_output=True,
            check=True,
            text=True,
        )
    except (OSError, CalledProcessError):
        return None
    raw = result.stdout.strip()
    return Path(raw) if raw else None


def require_repo_root(cwd: Path | None = None) -> Path:
    from spice.errors import SpiceError

    root = repo_root_from_cwd(cwd)
    if root is None:
        raise SpiceError("not inside a git worktree")
    return root


def git_common_dir(root: Path) -> Path:
    """The shared git dir for every worktree of one repository."""
    from spice.errors import SpiceError

    result = run_git_command(
        ["git", "-C", str(root), "rev-parse", "--git-common-dir"],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise SpiceError("not inside a git worktree")
    raw = Path(result.stdout.strip())
    return (raw if raw.is_absolute() else root / raw).resolve()


def git_dir(root: Path) -> Path:
    """The git dir for this specific worktree."""
    from spice.errors import SpiceError

    result = run_git_command(
        ["git", "-C", str(root), "rev-parse", "--git-dir"],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise SpiceError("not inside a git worktree")
    raw = Path(result.stdout.strip())
    return (raw if raw.is_absolute() else root / raw).resolve()


def shared_state_root(repo_root: Path) -> Path:
    """Canonical repository-shared managed-state root."""
    return git_common_dir(repo_root) / STATE_DIRNAME


def worktree_state_root(repo_root: Path) -> Path:
    """Canonical lane-local managed-state root for one worktree."""
    return git_dir(repo_root) / STATE_DIRNAME


def shared_state_path(repo_root: Path, relative: str | Path) -> Path:
    return _managed_state_path(shared_state_root(repo_root), relative)


def worktree_state_path(repo_root: Path, relative: str | Path) -> Path:
    return _managed_state_path(worktree_state_root(repo_root), relative)


def shared_attachment_root(repo_root: Path) -> Path:
    return shared_state_path(repo_root, SHARED_ATTACHMENT_DIR)


def state_dir(repo_root: Path) -> Path:
    return repo_root / STATE_DIRNAME


def runtime_spice_source() -> Path:
    return Path(__file__).resolve().parent


def find_tool(name: str) -> str | None:
    """Resolve a companion executable: spice's own environment wins over PATH.

    Gate backends (ruff, lizard) install alongside the product; git hooks fire
    from whatever shell invoked git, and that shell owes spice nothing
    PATH-wise.
    """
    own_bin = str(Path(sys.executable).parent)
    return shutil.which(name, path=own_bin) or shutil.which(name)


def atomic_write_text(path: Path, text: str, *, write_if_changed: bool = False) -> Path:
    """Durably replace ``path`` with UTF-8 ``text``.

    Each invocation owns a unique same-directory temporary file. Concurrent
    writers therefore produce one complete value; without caller-supplied
    arbitration, the last successful replacement intentionally wins.

    Existing target permissions are retained across replacement. New files use
    ``mkstemp``'s private permissions. ``write_if_changed`` avoids replacement
    entirely when the existing bytes already match, preserving its inode and
    metadata.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    encoded = text.encode("utf-8")
    if write_if_changed:
        try:
            if path.read_bytes() == encoded:
                return path
        except OSError:
            pass

    existing_mode: int | None
    try:
        existing_mode = path.stat().st_mode & 0o7777
    except OSError:
        existing_mode = None

    descriptor, tmp_name = tempfile.mkstemp(
        dir=path.parent,
        prefix=f".{path.name}.",
        suffix=".tmp",
    )
    tmp = Path(tmp_name)
    try:
        if existing_mode is not None:
            os.fchmod(descriptor, existing_mode)
        handle = os.fdopen(descriptor, "wb")
        descriptor = -1
        with handle:
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
        fsync_directory(path.parent)
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        with contextlib.suppress(FileNotFoundError):
            tmp.unlink()
    return path


def atomic_write_json(
    path: Path,
    payload: Any,
    *,
    compact: bool = False,
    sort_keys: bool | None = None,
    write_if_changed: bool = False,
) -> Path:
    ordered = not compact if sort_keys is None else sort_keys
    if compact:
        text = json.dumps(payload, separators=(",", ":"), sort_keys=ordered) + "\n"
    else:
        text = json.dumps(payload, indent=2, sort_keys=ordered) + "\n"
    return atomic_write_text(path, text, write_if_changed=write_if_changed)


def fsync_directory(directory: Path) -> None:
    """Sync directory metadata, except for explicit unsupported-platform errors."""
    if os.name == "nt":
        # Windows does not expose a portable directory-fsync contract.
        return
    try:
        descriptor = os.open(directory, os.O_RDONLY)
    except OSError as exc:
        if exc.errno in {errno.EINVAL, errno.ENOTSUP, errno.EOPNOTSUPP}:
            return
        raise
    try:
        try:
            os.fsync(descriptor)
        except OSError as exc:
            if exc.errno not in {errno.EINVAL, errno.ENOTSUP, errno.EOPNOTSUPP}:
                raise
    finally:
        os.close(descriptor)


def _managed_state_path(root: Path, relative: str | Path) -> Path:
    from spice.errors import SpiceError

    path = Path(relative)
    if path.is_absolute() or ".." in path.parts:
        raise SpiceError(f"managed state path must be relative: {path}")
    return root / path
