"""Git-backed agent runtime state paths."""

from __future__ import annotations

from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path

from spice.agent.identity import canonical_thread_id
from spice.locking import bounded_exclusive_lock
from spice.paths import atomic_write_text, worktree_state_path

# One agent per worktree, driver-agnostic: lifecycle state (the thread pointer,
# per-thread state, logs) is NOT namespaced by driver, so switching driver
# (Codex<->Claude) renews the single running slot instead of stranding a
# parallel per-driver pointer. The driver is recorded in the agent state record.
AGENT_STATE_GIT_ROOT = Path("agents")
THREAD_ID_FILENAME = "thread-id"
THREAD_ID_LOCK_FILENAME = "thread-id.lock"
THREAD_ID_LOCK_TIMEOUT_SECONDS = 10.0


def agent_worktree_state_dir(repo_root: Path) -> Path:
    return worktree_state_path(repo_root, AGENT_STATE_GIT_ROOT)


def agent_thread_state_dir(repo_root: Path, thread_id: str) -> Path:
    canonical = canonical_thread_id(thread_id)
    return worktree_state_path(repo_root, AGENT_STATE_GIT_ROOT / canonical)


def agent_thread_pointer_path(repo_root: Path) -> Path:
    return agent_worktree_state_dir(repo_root) / THREAD_ID_FILENAME


@contextmanager
def agent_thread_pointer_lock(repo_root: Path) -> Iterator[None]:
    """Serialize pathname replacement and removal for one worktree pointer."""
    lock_path = agent_worktree_state_dir(repo_root) / THREAD_ID_LOCK_FILENAME
    with bounded_exclusive_lock(
        lock_path,
        timeout_seconds=THREAD_ID_LOCK_TIMEOUT_SECONDS,
        action="update agent thread pointer",
    ):
        yield


def read_agent_thread_pointer(repo_root: Path) -> str:
    try:
        raw = agent_thread_pointer_path(repo_root).read_text(encoding="utf-8")
    except OSError:
        return ""
    return canonical_thread_id(raw)


def write_agent_thread_pointer(repo_root: Path, thread_id: str) -> None:
    canonical = canonical_thread_id(thread_id)
    if canonical:
        with agent_thread_pointer_lock(repo_root):
            atomic_write_text(agent_thread_pointer_path(repo_root), f"{canonical}\n")


def current_agent_thread_id(repo_root: Path) -> str:
    return read_agent_thread_pointer(repo_root)


def agent_state_dir(repo_root: Path) -> Path:
    thread_id = current_agent_thread_id(repo_root)
    if thread_id:
        return agent_thread_state_dir(repo_root, thread_id)
    return agent_worktree_state_dir(repo_root)
