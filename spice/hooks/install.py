"""Install the git hook shims spice owns into a target repo.

Shims are generated under `.spice/hooks/` and activated through the
worktree-local `core.hooksPath`, so nothing spice writes collides with hooks
the repo may already track. The shims invoke the ambient `spice` command
directly; runtime resolution belongs to that command, not to generated hook
files.
"""

from __future__ import annotations

from pathlib import Path

from spice.hooks.initplan import (
    GATE_HOOK_ARGS as GATE_HOOK_ARGS,
    HOOK_ARGS as HOOK_ARGS,
    HOOKS_DIRNAME,
    STATE_GITIGNORE_CONTENT,
    InitializationMode,
    apply_initialization_plan,
    hook_shim_content as hook_shim_content,
    initialization_detail_rows,
    plan_initialization,
)
from spice.paths import STATE_DIRNAME


def hooks_dir(repo_root: Path) -> Path:
    return repo_root / STATE_DIRNAME / HOOKS_DIRNAME


def install_hooks_for_repo(repo_root: Path) -> list[str]:
    """Write the shims and point `core.hooksPath` at them; return detail rows."""
    plan = plan_initialization(
        repo_root,
        InitializationMode.FULL,
        include_agent_skill=False,
    )
    apply_initialization_plan(plan)
    return initialization_detail_rows(plan, include_ready=False)


def materialize_state_gitignore(repo_root: Path) -> bool:
    """Exclude `.spice/` via a generated directory-local `.gitignore`.

    Mirrors the generated skill-copy ignore file so every spice-owned worktree
    directory carries its own self-ignoring marker instead of relying on the
    git-dir `info/exclude`. Returns True when it (re)wrote the file.
    """
    target = repo_root / STATE_DIRNAME / ".gitignore"
    try:
        if (
            target.is_file()
            and target.read_text(encoding="utf-8") == STATE_GITIGNORE_CONTENT
        ):
            return False
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(STATE_GITIGNORE_CONTENT, encoding="utf-8")
    except OSError:
        return False
    return True
