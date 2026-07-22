"""Control-plane Git primitives everything else in this package runs on.

Every Git command spice runs on an agent's behalf goes through here, under one
environment: no terminal prompt, batch-mode SSH, and a bounded timeout on the
two commands that reach the network. A caller that built its own invocation
could hang an agent's shell on a credential prompt, so none do.

This is the floor of `spice.tasks.git` and imports nothing from its siblings, so
`merging` and `boundaries` can both stand on it without a cycle. They reach it
by module name — `plumbing.run(...)`, never a name bound at import — which
leaves one attribute for a test to intercept and have the whole package see it.

Bytecode purging lives here for the same reason: it is what has to happen after
any tree move, and tree moves happen at every level above.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

from spice.errors import SpiceError
from spice.process.git import DEFAULT_GIT_TIMEOUT_SECONDS, run_git_command


GIT_NETWORK_TIMEOUT_SECONDS = 30
TASK_GIT_SSH_COMMAND = "ssh -o BatchMode=yes -o ConnectTimeout=5"
_NETWORK_COMMANDS = {"fetch", "push"}


def run(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    env = _control_plane_git_env()
    command = ["git", "-C", str(repo_root), *args]
    kwargs = {
        "capture_output": True,
        "check": False,
        "env": env,
        "text": True,
    }
    timeout = (
        GIT_NETWORK_TIMEOUT_SECONDS if args and args[0] in _NETWORK_COMMANDS else None
    )
    return run_git_command(
        command,
        default_timeout_seconds=timeout or DEFAULT_GIT_TIMEOUT_SECONDS,
        **kwargs,
    )


def run_with_input(
    repo_root: Path, *args: str, input_text: str
) -> subprocess.CompletedProcess[str]:
    env = _control_plane_git_env()
    command = ["git", "-C", str(repo_root), *args]
    return run_git_command(
        command,
        default_timeout_seconds=DEFAULT_GIT_TIMEOUT_SECONDS,
        capture_output=True,
        check=False,
        env=env,
        input=input_text,
        text=True,
    )


def _control_plane_git_env() -> dict[str, str]:
    env = dict(os.environ)  # env-policy: allow
    env["GIT_TERMINAL_PROMPT"] = "0"
    env["GIT_SSH_COMMAND"] = TASK_GIT_SSH_COMMAND
    return env


def read(repo_root: Path, *args: str) -> str:
    completed = run(repo_root, *args)
    return completed.stdout.strip() if completed.returncode == 0 else ""


def is_ancestor(repo_root: Path, ancestor: str, descendant: str) -> bool:
    return (
        run(repo_root, "merge-base", "--is-ancestor", ancestor, descendant).returncode
        == 0
    )


def purge_stale_bytecode(repo_root: Path, before: str, after: str) -> list[str]:
    """Delete bytecode orphaned by a tree move from ``before`` to ``after``.

    Tree moves (``read-tree --reset -u``, ``merge --ff-only``) rewrite tracked
    files only, so untracked ``__pycache__`` entries survive every move. A
    deleted module's bytecode keeps its package directory alive as an
    importable namespace package, and a modified module can be shadowed by
    bytecode whose (mtime, size) validation key still matches. The diff
    between the move's endpoints is the whole truth: every ``.py`` path it
    lists drops its compiled artifacts, and directories that would survive
    only because of that bytecode are pruned.

    Cleanup is strictly best-effort and never raises: diff discovery,
    repository descriptor open, per-source traversal, and descriptor teardown
    failures are all contained here so the surrounding Git transaction stays
    coherent even when cleanup is impossible. Sources whose compiled
    artifacts may survive are returned so callers with a reporting channel
    can surface manual cleanup guidance; when discovery itself fails the
    report names the unknown scope instead of a source list. Skipping
    platforms without descriptor-relative cleanup stays silent because that
    is a permanent capability gap, not a failed cleanup of these sources.
    """
    if not before or not after or before == after:
        return []
    try:
        listing = read(
            repo_root, "diff", "--name-only", "--no-renames", "-z", before, after
        )
    except (OSError, ValueError, subprocess.SubprocessError, SpiceError):
        return [BYTECODE_SCOPE_UNKNOWN]
    if not _supports_safe_bytecode_purge():
        return []
    candidates: list[tuple[str, Path]] = []
    for name in listing.split("\0"):
        if not name.endswith(".py"):
            continue
        source = Path(name)
        if source.is_absolute() or any(
            part in {"", ".", ".."} for part in source.parts
        ):
            continue
        candidates.append((name, source))
    if not candidates:
        return []
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    try:
        root_fd = _open_worktree_root(repo_root, directory_flags)
    except OSError:
        return [name for name, _ in candidates]
    blocked: list[str] = []
    try:
        for name, source in candidates:
            if _purge_source_bytecode(root_fd, source, directory_flags):
                blocked.append(name)
    finally:
        _close_quietly(root_fd)
    return blocked


def _open_worktree_root(repo_root: Path, directory_flags: int) -> int:
    return os.open(repo_root.resolve(), directory_flags)


def _close_quietly(fd: int) -> None:
    """Best-effort descriptor close: teardown cannot break the purge contract."""
    try:
        os.close(fd)
    except OSError:
        pass


BYTECODE_SCOPE_UNKNOWN = "unidentified modules (cleanup diff unavailable)"


def bytecode_cleanup_note(blocked: list[str]) -> str:
    listed = ", ".join(sorted(blocked))
    return (
        f"stale bytecode kept for {listed}: automatic cleanup was interrupted; "
        "remove the matching __pycache__ entries manually"
    )


def _supports_safe_bytecode_purge() -> bool:
    """Whether this platform offers descriptor-relative, no-follow cleanup."""
    return bool(
        hasattr(os, "O_DIRECTORY")
        and hasattr(os, "O_NOFOLLOW")
        and os.open in os.supports_dir_fd
        and os.unlink in os.supports_dir_fd
        and os.rmdir in os.supports_dir_fd
        and os.scandir in os.supports_fd
    )


def _purge_source_bytecode(root_fd: int, source: Path, directory_flags: int) -> bool:
    """Purge one source's cache through no-follow directory descriptors.

    Every lookup below the already-open worktree root is relative to a trusted
    directory descriptor. A changed source parent or ``__pycache__`` replaced
    by a symlink therefore stops cleanup instead of redirecting an unlink.
    ``unlinkat`` removes a matching entry itself and never follows a final
    symlink.

    Never raises. Returns True when compiled artifacts may survive because
    the operating system refused a step (permissions, symlink refusal);
    returns False when cleanup completed or there was nothing to clean.
    """
    parent_parts = source.parent.parts
    parent_fds: list[int] = []
    current_fd = root_fd
    try:
        for part in parent_parts:
            try:
                current_fd = os.open(
                    part,
                    directory_flags,
                    dir_fd=current_fd,
                )
            except (FileNotFoundError, NotADirectoryError):
                return False
            except OSError:
                return True
            parent_fds.append(current_fd)
        try:
            cache_fd = os.open(
                "__pycache__",
                directory_flags,
                dir_fd=current_fd,
            )
        except (FileNotFoundError, NotADirectoryError):
            return False
        except OSError:
            return True
        blocked = False
        try:
            prefix = f"{source.stem}."
            with os.scandir(cache_fd) as entries:
                compiled_names = [
                    entry.name for entry in entries if entry.name.startswith(prefix)
                ]
            for compiled_name in compiled_names:
                try:
                    os.unlink(compiled_name, dir_fd=cache_fd)
                except FileNotFoundError:
                    continue
                except OSError:
                    blocked = True
        except OSError:
            blocked = True
        finally:
            _close_quietly(cache_fd)

        try:
            os.rmdir("__pycache__", dir_fd=current_fd)
        except OSError:
            return blocked
        for index in range(len(parent_parts) - 1, -1, -1):
            parent_fd = root_fd if index == 0 else parent_fds[index - 1]
            try:
                os.rmdir(parent_parts[index], dir_fd=parent_fd)
            except OSError:
                break
        return blocked
    finally:
        for parent_fd in reversed(parent_fds):
            _close_quietly(parent_fd)


def fail(action: str, completed: subprocess.CompletedProcess[str]) -> str:
    detail = "\n".join(
        part.strip()
        for part in (completed.stdout, completed.stderr)
        if part and part.strip()
    )
    suffix = f"\n{detail}" if detail else ""
    return f"could not {action} (git exit {completed.returncode}){suffix}"
