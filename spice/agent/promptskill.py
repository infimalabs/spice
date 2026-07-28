"""Neutral-prompt skill discovery extracted from the lifecycle facade."""

from pathlib import Path

from spice.agent.lifecyclebinding import available_skill_path as _available_skill_path
from spice.errors import SpiceError


def available_skill_path(repo_root: Path, *, required: bool) -> Path | None:
    """Resolve the worktree skill without choosing a second packaged source."""
    return _available_skill_path(repo_root, required=required)


def resolve_agent_prompt_skill_path(repo_root: Path) -> Path:
    located = available_skill_path(repo_root, required=True)
    if located is None:
        raise SpiceError("missing spice skill")
    return located


def prompt_skill_invocation_path(repo_root: Path, skill_path: Path) -> Path:
    if not skill_path.is_absolute():
        return skill_path
    try:
        return skill_path.resolve().relative_to(repo_root.resolve())
    except ValueError:
        return skill_path
