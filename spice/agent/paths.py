"""Git-backed agent runtime state paths."""

from __future__ import annotations

from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

from spice.agent.identity import canonical_thread_id
from spice.locking import bounded_exclusive_lock
from spice.paths import atomic_write_text, worktree_state_root

# One agent per worktree, driver-agnostic: lifecycle state (the thread pointer,
# per-thread state, logs) is NOT namespaced by driver, so switching driver
# (Codex<->Claude) renews the single running slot instead of stranding a
# parallel per-driver pointer. The driver is recorded in the agent state record.
AGENT_STATE_GIT_ROOT = Path("agents")
THREAD_ID_FILENAME = "thread-id"
THREAD_ID_LOCK_FILENAME = "thread-id.lock"
THREAD_ID_LOCK_TIMEOUT_SECONDS = 10.0


@dataclass(frozen=True)
class AgentWorktreeStatePaths:
    """Paths derived from one lane-local Git state-root resolution."""

    root: Path

    @classmethod
    def resolve(cls, repo_root: Path) -> AgentWorktreeStatePaths:
        return cls(root=worktree_state_root(repo_root))

    @property
    def agent_dir(self) -> Path:
        return self.root / AGENT_STATE_GIT_ROOT

    @property
    def thread_pointer(self) -> Path:
        return self.agent_dir / THREAD_ID_FILENAME

    @property
    def thread_pointer_lock(self) -> Path:
        return self.agent_dir / THREAD_ID_LOCK_FILENAME

    def thread_dir(self, thread_id: str) -> Path:
        return self.agent_dir / canonical_thread_id(thread_id)


def resolve_agent_worktree_state_paths(repo_root: Path) -> AgentWorktreeStatePaths:
    return AgentWorktreeStatePaths.resolve(repo_root)


def _state_paths(
    repo_root: Path,
    state_paths: AgentWorktreeStatePaths | None,
) -> AgentWorktreeStatePaths:
    return state_paths or resolve_agent_worktree_state_paths(repo_root)


def agent_worktree_state_dir(
    repo_root: Path,
    *,
    state_paths: AgentWorktreeStatePaths | None = None,
) -> Path:
    return _state_paths(repo_root, state_paths).agent_dir


def agent_thread_state_dir(
    repo_root: Path,
    thread_id: str,
    *,
    state_paths: AgentWorktreeStatePaths | None = None,
) -> Path:
    return _state_paths(repo_root, state_paths).thread_dir(thread_id)


def agent_thread_pointer_path(
    repo_root: Path,
    *,
    state_paths: AgentWorktreeStatePaths | None = None,
) -> Path:
    return _state_paths(repo_root, state_paths).thread_pointer


@contextmanager
def agent_thread_pointer_lock(
    repo_root: Path,
    *,
    state_paths: AgentWorktreeStatePaths | None = None,
) -> Iterator[None]:
    """Serialize pathname replacement and removal for one worktree pointer."""
    lock_path = _state_paths(repo_root, state_paths).thread_pointer_lock
    with bounded_exclusive_lock(
        lock_path,
        timeout_seconds=THREAD_ID_LOCK_TIMEOUT_SECONDS,
        action="update agent thread pointer",
    ):
        yield


def read_agent_thread_pointer(
    repo_root: Path,
    *,
    state_paths: AgentWorktreeStatePaths | None = None,
) -> str:
    try:
        raw = agent_thread_pointer_path(
            repo_root,
            state_paths=state_paths,
        ).read_text(encoding="utf-8")
    except OSError:
        return ""
    return canonical_thread_id(raw)


def write_agent_thread_pointer(
    repo_root: Path,
    thread_id: str,
    *,
    state_paths: AgentWorktreeStatePaths | None = None,
) -> None:
    canonical = canonical_thread_id(thread_id)
    if canonical:
        resolved_paths = _state_paths(repo_root, state_paths)
        with agent_thread_pointer_lock(repo_root, state_paths=resolved_paths):
            atomic_write_text(
                agent_thread_pointer_path(repo_root, state_paths=resolved_paths),
                f"{canonical}\n",
            )


def current_agent_thread_id(
    repo_root: Path,
    *,
    state_paths: AgentWorktreeStatePaths | None = None,
) -> str:
    return read_agent_thread_pointer(repo_root, state_paths=state_paths)


def agent_state_dir(
    repo_root: Path,
    *,
    state_paths: AgentWorktreeStatePaths | None = None,
) -> Path:
    resolved_paths = _state_paths(repo_root, state_paths)
    thread_id = current_agent_thread_id(repo_root, state_paths=resolved_paths)
    if thread_id:
        return agent_thread_state_dir(
            repo_root,
            thread_id,
            state_paths=resolved_paths,
        )
    return agent_worktree_state_dir(repo_root, state_paths=resolved_paths)
