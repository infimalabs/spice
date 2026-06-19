"""Git-backed agent runtime state paths."""

from __future__ import annotations

from pathlib import Path

from spice.agent.driver import driver_for
from spice.agent.identity import canonical_thread_id
from spice.paths import atomic_write_text, git_common_dir, git_dir

AGENT_STATE_GIT_ROOT = Path("spice") / "agents"
THREAD_ID_FILENAME = "thread-id"


def agent_worktree_state_dir(repo_root: Path) -> Path:
    return (
        git_dir(repo_root) / AGENT_STATE_GIT_ROOT / driver_for(repo_root).state_dirname
    )


def agent_thread_state_dir(repo_root: Path, thread_id: str) -> Path:
    canonical = canonical_thread_id(thread_id)
    return (
        git_common_dir(repo_root)
        / AGENT_STATE_GIT_ROOT
        / driver_for(repo_root).state_dirname
        / canonical
    )


def agent_thread_pointer_path(repo_root: Path) -> Path:
    return agent_worktree_state_dir(repo_root) / THREAD_ID_FILENAME


def read_agent_thread_pointer(repo_root: Path) -> str:
    try:
        raw = agent_thread_pointer_path(repo_root).read_text(encoding="utf-8")
    except OSError:
        return ""
    return canonical_thread_id(raw)


def write_agent_thread_pointer(repo_root: Path, thread_id: str) -> None:
    canonical = canonical_thread_id(thread_id)
    if canonical:
        atomic_write_text(agent_thread_pointer_path(repo_root), f"{canonical}\n")


def current_agent_thread_id(repo_root: Path) -> str:
    return read_agent_thread_pointer(repo_root)


def agent_state_dir(repo_root: Path) -> Path:
    thread_id = current_agent_thread_id(repo_root)
    if thread_id:
        return agent_thread_state_dir(repo_root, thread_id)
    return agent_worktree_state_dir(repo_root)
