"""Install the git hook shims spice owns into a target repo.

Shims are generated under `.spice/hooks/` and activated through the
worktree-local `core.hooksPath`, so nothing spice writes collides with hooks
the repo may already track. The shims bake in the absolute interpreter of the
installation that ran `spice init`, but when the repo itself provides spice
source they put the worktree first on PYTHONPATH before running `python -m
spice`. The tracked `spice.sh` shim uses the same precedence; ordinary target
repos use the installed product.
"""

from __future__ import annotations

import stat
import sys
import shlex
from pathlib import Path

from spice.config import git_worktree_config_get, git_worktree_config_set
from spice.paths import STATE_DIRNAME, git_common_dir, worktree_spice_source

HOOKS_DIRNAME = "hooks"
HOOK_ARGS = {
    "pre-commit": "dev pre-commit --hook",
    "commit-msg": 'dev commit-msg "$1"',
    "reference-transaction": 'dev reference-transaction "$1"',
}
AGENT_SH = """#!/usr/bin/env sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)

if [ -f "$repo_root/spice/__main__.py" ] && [ -f "$repo_root/spice/cli/entry.py" ] && [ -f "$repo_root/spice/agent/wrap.py" ]; then
    if [ ! -x "$repo_root/.venv/bin/python" ]; then
        echo "spice.sh: local spice checkout requires $repo_root/.venv/bin/python" >&2
        exit 127
    fi
    export PYTHONPATH="$repo_root${PYTHONPATH:+:$PYTHONPATH}"
    if ! probe=$("$repo_root/.venv/bin/python" -c 'import spice.cli.entry, spice.agent.wrap' 2>&1); then
        echo "spice.sh: the local spice checkout at $repo_root cannot import; repair the file named below (look for conflict markers), or run the installed spice entrypoint until the checkout is fixed" >&2
        printf '%s\\n' "$probe" >&2
        exit 127
    fi
    exec "$repo_root/.venv/bin/python" -m spice agent run -- "$@"
fi

exec spice agent run -- "$@"
"""


def hook_shim_content(repo_root: Path, args: str) -> str:
    lines = ["#!/usr/bin/env sh", "", "set -eu", ""]
    if worktree_spice_source(repo_root) is not None:
        quoted_root = shlex.quote(str(repo_root.expanduser().resolve()))
        lines.append(f"export PYTHONPATH={quoted_root}${{PYTHONPATH:+:$PYTHONPATH}}")
        lines.append("")
    lines.append(f"exec {shlex.quote(sys.executable)} -m spice {args}")
    return "\n".join(lines) + "\n"


def hooks_dir(repo_root: Path) -> Path:
    return repo_root / STATE_DIRNAME / HOOKS_DIRNAME


def install_hooks_for_repo(repo_root: Path) -> list[str]:
    """Write the shims and point `core.hooksPath` at them; return detail rows."""
    rows: list[str] = []
    directory = hooks_dir(repo_root)
    directory.mkdir(parents=True, exist_ok=True)
    for name, args in HOOK_ARGS.items():
        path = directory / name
        content = hook_shim_content(repo_root, args)
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


def write_agent_shim(repo_root: Path) -> Path:
    path = repo_root / "spice.sh"
    if not path.exists() or path.read_text(encoding="utf-8") != AGENT_SH:
        path.write_text(AGENT_SH, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


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
    """`spice init`: hooks, agent shim, skill copy, state scaffolding."""
    from spice.agent.lifecycle import (
        WORKTREE_SKILL_RELATIVE_PATH,
        materialize_worktree_skill,
    )

    rows = install_hooks_for_repo(repo_root)
    shim = write_agent_shim(repo_root)
    rows.append(f"agent_shim={shim.relative_to(repo_root).as_posix()}")
    if materialize_worktree_skill(repo_root) is not None:
        rows.append(f"skill={WORKTREE_SKILL_RELATIVE_PATH.as_posix()}")
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
