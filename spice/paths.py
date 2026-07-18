"""Repo roots, the `.spice/` state directory, and atomic file writes."""

from __future__ import annotations

import contextlib
import errno
import hashlib
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path
from subprocess import CalledProcessError
from typing import Any

from spice.process.git import run_git_command

STATE_DIRNAME = ".spice"
SHARED_ATTACHMENT_DIR = Path("attachments")
INBOX_DIRNAME = "inbox"

# Layout of a total state-backend scratch root: one shared subtree standing in
# for the repository-shared root, one keyed subtree per worktree, and the task
# store's default home when nothing more specific claims it.
STATE_BACKEND_SHARED_DIR = "shared"
STATE_BACKEND_WORKTREES_DIR = "worktrees"
STATE_BACKEND_TASK_DIR = "task"
# Enough hex digits that distinct worktree paths sharing a basename cannot
# collide in practice, short enough to keep the keyed directory readable.
WORKTREE_BACKEND_KEY_DIGEST_CHARS = 12

_state_backend_override: Path | None = None


def set_state_backend(root: str | None) -> None:
    """Redirect every managed-state root under ``root``; ``None`` restores git.

    Process-wide by design: a scratch-backed process (``spice serve
    --backend``) must never resolve managed state into a live git dir, no
    matter which module asks.
    """
    global _state_backend_override
    _state_backend_override = (
        Path(root).expanduser().resolve() if root is not None else None
    )


def _worktree_backend_key(repo_root: Path) -> str:
    # Never consults git: scratch-backed processes may point at roots that are
    # not worktrees at all. The resolved path is the identity.
    resolved = Path(repo_root).resolve()
    digest = hashlib.sha256(str(resolved).encode("utf-8")).hexdigest()
    return f"{resolved.name}-{digest[:WORKTREE_BACKEND_KEY_DIGEST_CHARS]}"


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
    if _state_backend_override is not None:
        return _state_backend_override / STATE_BACKEND_SHARED_DIR
    return git_common_dir(repo_root) / STATE_DIRNAME


def worktree_state_root(repo_root: Path) -> Path:
    """Canonical lane-local managed-state root for one worktree."""
    if _state_backend_override is not None:
        return (
            _state_backend_override
            / STATE_BACKEND_WORKTREES_DIR
            / _worktree_backend_key(repo_root)
        )
    return git_dir(repo_root) / STATE_DIRNAME


def worktree_runtime_state_root(repo_root: Path) -> Path:
    """Mutable worktree-visible state, redirected with the total backend.

    Worktree configuration and hook shims remain operator-authored inputs at
    ``<worktree>/.spice``. Runtime outputs that normally share that visible
    namespace must use this resolver so ``spice serve --backend`` owns them.
    """
    if _state_backend_override is not None:
        return (
            _state_backend_override
            / STATE_BACKEND_WORKTREES_DIR
            / _worktree_backend_key(repo_root)
        )
    return Path(repo_root) / STATE_DIRNAME


def worktree_inbox_dir(repo_root: Path) -> Path:
    """Operator inbox for one worktree: repo-visible, still backend-isolated.

    Steering lands in <worktree>/.spice/inbox so operators and tools can drop
    files without git plumbing. Unlike worktree config and hooks (operator-
    authored inputs), pending steering is live mutable state, so the backend
    override claims it and a scratch-backed process never observes or consumes
    live messages.
    """
    return worktree_runtime_state_root(repo_root) / INBOX_DIRNAME


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
