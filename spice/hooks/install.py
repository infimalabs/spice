"""Install the git hook shims spice owns into a target repo.

Shims are generated under `.spice/hooks/` and activated through the
worktree-local `core.hooksPath`, so nothing spice writes collides with hooks
the repo may already track. The shims invoke the ambient `spice` command
directly; runtime resolution belongs to that command, not to generated hook
files.
"""

from __future__ import annotations

import stat
from pathlib import Path

from spice.config import git_worktree_config_get, git_worktree_config_set
from spice.paths import STATE_DIRNAME, git_common_dir

HOOKS_DIRNAME = "hooks"
HOOK_ARGS = {
    "pre-commit": "dev pre-commit",
    "commit-msg": 'dev commit-msg "$1"',
    "reference-transaction": 'dev reference-transaction "$1"',
}


def hook_shim_content(args: str) -> str:
    return (
        "\n".join(["#!/usr/bin/env sh", "", "set -eu", "", f"exec spice {args}"]) + "\n"
    )


def hooks_dir(repo_root: Path) -> Path:
    return repo_root / STATE_DIRNAME / HOOKS_DIRNAME


def install_hooks_for_repo(repo_root: Path) -> list[str]:
    """Write the shims and point `core.hooksPath` at them; return detail rows."""
    rows: list[str] = []
    directory = hooks_dir(repo_root)
    directory.mkdir(parents=True, exist_ok=True)
    for name, args in HOOK_ARGS.items():
        path = directory / name
        content = hook_shim_content(args)
        if not path.exists() or path.read_text(encoding="utf-8") != content:
            path.write_text(content, encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        rows.append(f"hook {name} -> {path.relative_to(repo_root).as_posix()}")
    relative_hooks = directory.relative_to(repo_root).as_posix()
    if git_worktree_config_get(repo_root, "core.hooksPath") != relative_hooks:
        _enable_worktree_config(repo_root)
        git_worktree_config_set(repo_root, "core.hooksPath", relative_hooks)
    rows.append(f"core.hooksPath={relative_hooks}")
    return rows


def _enable_worktree_config(repo_root: Path) -> None:
    # `git config --worktree` requires extensions.worktreeConfig in multi-
    # worktree repos; setting it in the common config is idempotent.
    import subprocess

    subprocess.run(
        ["git", "-C", str(repo_root), "config", "extensions.worktreeConfig", "true"],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def state_gitignore_row() -> str:
    return f"{STATE_DIRNAME}/"


def exclude_rows() -> list[str]:
    from spice.agent.lifecycle import WORKTREE_SKILL_RELATIVE_PATH

    return [state_gitignore_row(), WORKTREE_SKILL_RELATIVE_PATH.as_posix()]


def init_repo(repo_root: Path) -> list[str]:
    """`spice init`: hooks, skill copy, state scaffolding."""
    from spice.agent.lifecycle import (
        WORKTREE_SKILL_GITIGNORE_RELATIVE_PATH,
        WORKTREE_SKILL_RELATIVE_PATH,
        materialize_worktree_skill,
    )

    rows = install_hooks_for_repo(repo_root)
    if materialize_worktree_skill(repo_root) is not None:
        rows.append(f"skill={WORKTREE_SKILL_RELATIVE_PATH.as_posix()}")
        skill_ignore = repo_root / WORKTREE_SKILL_GITIGNORE_RELATIVE_PATH
        if skill_ignore.is_file():
            rows.append(
                f"skill_ignore={WORKTREE_SKILL_GITIGNORE_RELATIVE_PATH.as_posix()}"
            )
    # `.spice/` and the materialized skill copy are machine-local; exclude
    # them in the *common* git dir so the rule holds for every worktree (a
    # linked worktree's `.git` is a file).
    exclude = git_common_dir(repo_root) / "info" / "exclude"
    try:
        existing = exclude.read_text(encoding="utf-8")
    except OSError:
        existing = ""
    missing = [row for row in exclude_rows() if row not in existing.splitlines()]
    if missing:
        exclude.parent.mkdir(parents=True, exist_ok=True)
        with exclude.open("a", encoding="utf-8") as handle:
            if existing and not existing.endswith("\n"):
                handle.write("\n")
            handle.writelines(row + "\n" for row in missing)
        rows.extend(f"git_exclude+={row}" for row in missing)
    rows.append("ready: spice serve | spice agent ensure | spice task status")
    return rows
