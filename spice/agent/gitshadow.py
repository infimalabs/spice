"""Lane self-tracking: the agent's branch tracks itself so its ``git status``
never moves when origin advances.

The integration upstream is not named in branch config; it lives in
``origin/HEAD``, read by the task control plane (see ``spice.tasks.gitsync``).
Nothing is injected into the agent environment — the tracking is plain worktree
config written once at agent setup, so every reader (status, pull, the control
plane) agrees without env trickery.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

from spice.errors import SpiceError


def ensure_lane_self_tracking(repo_root: Path | None) -> None:
    """Point the lane's branch at itself so the agent's upstream never moves.

    The self value must be the *sole* merge value — a residual upstream would
    make ``@{upstream}`` and ``git pull`` see two branches (octopus) — so any
    existing values are cleared from every writable scope before the self value
    is written.
    """
    if repo_root is None:
        return
    branch = current_git_branch(repo_root)
    if not branch:
        return
    for scope in ("--worktree", "--local"):
        # Tolerate exit 5 (key absent); clearing both scopes guarantees the
        # self value is the only one git reads for @{upstream}.
        _git(repo_root, "config", scope, "--unset-all", f"branch.{branch}.remote")
        _git(repo_root, "config", scope, "--unset-all", f"branch.{branch}.merge")
    _git(repo_root, "config", "--worktree", f"branch.{branch}.remote", ".")
    written = _git(
        repo_root,
        "config",
        "--worktree",
        f"branch.{branch}.merge",
        f"refs/heads/{branch}",
    )
    if written.returncode != 0:
        raise SpiceError(
            f"could not write lane self-tracking for {branch}: "
            f"{written.stderr.strip()}; is extensions.worktreeConfig enabled? "
            "(run spice dev install-hooks)"
        )
    ensure_origin_head(repo_root)


def ensure_origin_head(repo_root: Path) -> None:
    """Ensure ``origin/HEAD`` names origin's default branch (the sync baseline).

    Set it only when an origin remote exists and the symref is missing, so a
    deliberately repointed ``origin/HEAD`` is left untouched.
    """
    if _git(repo_root, "remote", "get-url", "origin").returncode != 0:
        return
    if _git(repo_root, "symbolic-ref", "refs/remotes/origin/HEAD").returncode == 0:
        return
    _git(repo_root, "remote", "set-head", "origin", "--auto")


def current_git_branch(repo_root: Path | None) -> str:
    if repo_root is None:
        return ""
    completed = _git(repo_root, "symbolic-ref", "--quiet", "--short", "HEAD")
    return completed.stdout.strip() if completed.returncode == 0 else ""


def _git(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        check=False,
        text=True,
    )
