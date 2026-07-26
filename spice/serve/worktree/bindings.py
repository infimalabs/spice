"""Resolve one authoritative worktree binding for each agent thread."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from spice.agent.identity import canonical_thread_id
from spice.agent.lifecycle import AgentStatus, agent_status
from spice.agent.paths import (
    AgentWorktreeStatePaths,
    agent_thread_pointer_lock,
    agent_thread_pointer_path,
    read_agent_thread_pointer,
    resolve_agent_worktree_state_paths,
)
from spice.errors import SpiceError
from spice.serve.worktree.target import WorktreeTarget


@dataclass(frozen=True)
class _TargetThreadBinding:
    target: WorktreeTarget
    status: AgentStatus
    state_paths: AgentWorktreeStatePaths
    pointer_path: Path
    pointer_fingerprint: tuple[int, int, int, int]


def reconcile_target_thread_bindings(
    targets: list[WorktreeTarget],
) -> dict[str, str]:
    """Keep a reused thread bound to its one authoritative worktree target.

    A running binding is authoritative. When every copy is idle, the unique
    newest launch is authoritative: it is the lane the session most recently
    inhabited. Ambiguous runtime state is an error, not a target-name or
    filesystem-order tie-break. Stale pointer files are removed, while their
    per-thread directories and transcripts remain intact.
    """
    by_thread: dict[str, list[_TargetThreadBinding]] = {}
    resolved = {target.id: "" for target in targets}
    for target in targets:
        binding = _target_thread_binding(target)
        if binding is None:
            continue
        thread_id = binding.status.thread_id
        by_thread.setdefault(thread_id, []).append(binding)
    for thread_id, bindings in by_thread.items():
        owner = (
            bindings[0]
            if len(bindings) == 1
            else _authoritative_binding(thread_id, bindings)
        )
        resolved[owner.target.id] = thread_id
        for binding in bindings:
            if binding.target.id != owner.target.id:
                _clear_stale_binding(thread_id, binding)
    return resolved


def _target_thread_binding(
    target: WorktreeTarget,
) -> _TargetThreadBinding | None:
    # Discovery deliberately admits an existing fallback root even when it is
    # not a registered git worktree. Such a target has no lane-local git state
    # and is authoritatively unbound; only registered worktrees can own a
    # thread pointer.
    if not (target.repo_root / ".git").exists():
        return None
    state_paths = resolve_agent_worktree_state_paths(target.repo_root)
    pointer_path = agent_thread_pointer_path(
        target.repo_root,
        state_paths=state_paths,
    )
    thread_id = read_agent_thread_pointer(
        target.repo_root,
        state_paths=state_paths,
    )
    if not thread_id:
        return None
    status = agent_status(target.repo_root, state_paths=state_paths)
    if canonical_thread_id(status.thread_id) != thread_id:
        raise SpiceError(
            "retry the serve target refresh after the agent binding settles; "
            f"thread pointer changed while reading target {target.id}"
        )
    try:
        stat = pointer_path.stat()
    except OSError as exc:
        raise SpiceError(
            "retry the serve target refresh after the agent binding settles; "
            f"cannot verify thread pointer for target {target.id}: {exc}"
        ) from exc
    return _TargetThreadBinding(
        target=target,
        status=status,
        state_paths=state_paths,
        pointer_path=pointer_path,
        pointer_fingerprint=_pointer_fingerprint(stat),
    )


def _authoritative_binding(
    thread_id: str, bindings: list[_TargetThreadBinding]
) -> _TargetThreadBinding:
    running = [binding for binding in bindings if binding.status.running]
    if len(running) == 1:
        return running[0]
    if running:
        raise _ambiguous_binding_error(
            thread_id, running, "multiple copies are running"
        )
    started = [(_started_at(binding), binding) for binding in bindings]
    newest = max(timestamp for timestamp, _binding in started)
    candidates = [binding for timestamp, binding in started if timestamp == newest]
    if len(candidates) != 1:
        raise _ambiguous_binding_error(
            thread_id, candidates, "multiple idle copies have the same newest start"
        )
    return candidates[0]


def _started_at(binding: _TargetThreadBinding) -> datetime:
    # Start stamps are read strictly here rather than through the shared
    # transcript reader: choosing the newest copy of a thread compares real
    # instants, so zoneless or malformed text is an ambiguity to surface rather
    # than a UTC guess that could pick the wrong worktree.
    raw = str(binding.status.started_at or "").strip()
    try:
        parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            raise ValueError("timestamp has no timezone")
        return parsed
    except ValueError as exc:
        raise SpiceError(
            "start or import the thread in exactly one intended worktree; "
            f"duplicate thread {binding.status.thread_id} has no usable started_at "
            f"for target {binding.target.id}"
        ) from exc


def _ambiguous_binding_error(
    thread_id: str,
    bindings: list[_TargetThreadBinding],
    reason: str,
) -> SpiceError:
    target_ids = ", ".join(sorted(binding.target.id for binding in bindings))
    return SpiceError(
        "start or import the thread in exactly one intended worktree; "
        f"cannot choose a lane for duplicate thread {thread_id}: {reason}: {target_ids}"
    )


def _clear_stale_binding(thread_id: str, binding: _TargetThreadBinding) -> None:
    # Revalidate and unlink while holding the same lock as atomic pointer
    # replacement. Otherwise a writer can replace the verified inode in the
    # check-to-unlink gap and have its new binding deleted as stale.
    with agent_thread_pointer_lock(
        binding.target.repo_root,
        state_paths=binding.state_paths,
    ):
        current = read_agent_thread_pointer(
            binding.target.repo_root,
            state_paths=binding.state_paths,
        )
        try:
            stat = binding.pointer_path.stat()
        except FileNotFoundError:
            return
        if (
            current != thread_id
            or _pointer_fingerprint(stat) != binding.pointer_fingerprint
        ):
            raise SpiceError(
                "retry the serve target refresh after the agent binding settles; "
                f"thread pointer changed while clearing stale target {binding.target.id}"
            )
        binding.pointer_path.unlink()


def _pointer_fingerprint(stat: object) -> tuple[int, int, int, int]:
    return (
        int(getattr(stat, "st_dev")),
        int(getattr(stat, "st_ino")),
        int(getattr(stat, "st_mtime_ns")),
        int(getattr(stat, "st_size")),
    )
