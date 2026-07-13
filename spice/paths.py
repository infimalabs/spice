"""Repo roots, the `.spice/` state directory, and atomic file writes."""

from __future__ import annotations

import contextlib
import json
import os
import shutil
import sys
from pathlib import Path
from subprocess import CalledProcessError
from typing import Any

from spice.gitprocess import run_git_command

STATE_DIRNAME = ".spice"
SHARED_ATTACHMENT_DIR = Path("spice") / "attachments"


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


def shared_attachment_root(repo_root: Path) -> Path:
    return git_common_dir(repo_root) / SHARED_ATTACHMENT_DIR


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


def atomic_write_text(path: Path, text: str) -> Path:
    """Durably write `text` through a same-directory fsync + rename."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    try:
        with tmp.open("w", encoding="utf-8") as handle:
            handle.write(text)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
        directory_fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        with contextlib.suppress(FileNotFoundError):
            tmp.unlink()
    return path


def atomic_write_json(path: Path, payload: Any, *, compact: bool = False) -> Path:
    if compact:
        text = json.dumps(payload, separators=(",", ":")) + "\n"
    else:
        text = json.dumps(payload, indent=2, sort_keys=True) + "\n"
    return atomic_write_text(path, text)


def fsync_directory(directory: Path) -> None:
    try:
        descriptor = os.open(directory, os.O_RDONLY)
    except OSError:
        return
    try:
        with contextlib.suppress(OSError):
            os.fsync(descriptor)
    finally:
        os.close(descriptor)
