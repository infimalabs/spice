"""Repo roots, the `.spice/` state directory, and atomic file writes.

Library seam: target-repo tools may import the public repo-root, state-dir,
atomic write, JSON read, and tool-resolution helpers; underscored names remain
private.
"""

from __future__ import annotations

import contextlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

STATE_DIRNAME = ".spice"
SHARED_ATTACHMENT_DIR = Path("spice") / "attachments"
WORKTREE_SPICE_REQUIRED_PATHS = (
    Path("spice") / "__main__.py",
    Path("spice") / "cli" / "entry.py",
    Path("spice") / "agent" / "wrap.py",
)


def repo_root_from_cwd(cwd: Path | None = None) -> Path | None:
    """Resolve the enclosing git worktree root, or None outside git."""
    try:
        result = subprocess.run(
            ["git", "-C", str(cwd or Path.cwd()), "rev-parse", "--show-toplevel"],
            capture_output=True,
            check=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
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

    result = subprocess.run(
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

    result = subprocess.run(
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


def worktree_spice_source(repo_root: Path | None) -> Path | None:
    """Return the local spice package when a worktree provides the product.

    Entrypoint precedence is deliberately worktree-true: a repository that
    contains spice's own source tree runs that checkout first by putting the
    worktree root on PYTHONPATH. Ordinary target repositories do not satisfy
    this product-shape check, so they continue to use the installed spice.
    """
    if repo_root is None:
        return None
    root = repo_root.expanduser().resolve()
    if all((root / path).is_file() for path in WORKTREE_SPICE_REQUIRED_PATHS):
        return root / "spice"
    return None


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
    """Write `text` to `path` through a same-directory tmp + rename."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    tmp.write_text(text, encoding="utf-8")
    os.replace(tmp, path)
    return path


def atomic_write_json(path: Path, payload: Any, *, compact: bool = False) -> Path:
    if compact:
        text = json.dumps(payload, separators=(",", ":")) + "\n"
    else:
        text = json.dumps(payload, indent=2, sort_keys=True) + "\n"
    return atomic_write_text(path, text)


def read_json(path: Path) -> dict[str, Any]:
    """Read a JSON object from `path`; missing or malformed reads as {}."""
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return loaded if isinstance(loaded, dict) else {}


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
