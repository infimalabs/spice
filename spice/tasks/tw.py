"""Thin Taskwarrior process layer: run commands, export rows, capture context.

All agents share one database, so there is no sync step; a write is
authoritative the instant Taskwarrior's per-command lock releases.
"""

from __future__ import annotations

import json
import shlex
import shutil
import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

from spice.agent.identity import ambient_thread_id
from spice.errors import SpiceError
from spice.gitprocess import run_git_command
from spice.tasks import config

_MUTATING_COMMANDS = frozenset({"add", "annotate", "delete", "done", "modify"})
TASK_COMMAND_TIMEOUT_SECONDS = 120.0


def require_task_binary() -> None:
    if not shutil.which("task"):
        raise SpiceError("Taskwarrior binary not found; install `task` first")


def run(
    args: list[str],
    *,
    check: bool = True,
    overrides: list[str] | None = None,
    taskrc: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    require_task_binary()
    selected_taskrc = taskrc or config.bootstrap()
    command = [
        "task",
        f"rc:{selected_taskrc}",
        # Taskwarrior's rc parser truncates a value at '#' with no escape
        # form, so a backend path carrying taskrc comment characters cannot
        # ride the file; the command-line override outranks the file and
        # preserves the exact configured data path verbatim.
        f"rc.data.location={selected_taskrc.parent / 'data'}",
        "rc.confirmation=no",
        "rc.bulk=0",
        "rc.verbose=nothing",
        *(overrides or []),
        *args,
    ]
    action = _task_action(args)
    try:
        result = subprocess.run(
            command,
            cwd=config.repo_root(),
            capture_output=True,
            check=False,
            text=True,
            timeout=TASK_COMMAND_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as exc:
        raise SpiceError(
            f"Taskwarrior {action} timed out after {TASK_COMMAND_TIMEOUT_SECONDS:g}s: "
            f"{shlex.join(command)}"
        ) from exc
    if check and result.returncode != 0:
        detail = (result.stderr or result.stdout or "").strip()
        raise SpiceError(f"task command failed: {' '.join(args)}\n{detail}")
    if result.returncode == 0:
        reason = _mutation_reason(args)
        if reason:
            config.mark_task_backend_changed(reason, root=selected_taskrc.parent)
    return result


def export(
    filters: list[str] | None = None,
    *,
    overrides: list[str] | None = None,
    taskrc: Path | None = None,
) -> list[dict[str, Any]]:
    result = run([*(filters or []), "export"], overrides=overrides, taskrc=taskrc)
    data = json.loads(result.stdout or "[]")
    if not isinstance(data, list):
        raise SpiceError("Taskwarrior export did not return a JSON array")
    return [row for row in data if isinstance(row, dict)]


def _mutation_reason(args: list[str]) -> str:
    for arg in args:
        if arg in _MUTATING_COMMANDS:
            return arg
    return ""


def _task_action(args: list[str]) -> str:
    mutation = _mutation_reason(args)
    if mutation:
        return f"{mutation} mutation"
    command = next((arg for arg in reversed(args) if not arg.startswith("rc.")), "run")
    return f"{command} operation"


def now_iso() -> str:
    return datetime.now(UTC).isoformat(timespec="microseconds").replace("+00:00", "Z")


TW_DATETIME_FORMAT = "%Y%m%dT%H%M%SZ"


def canonical_utc(when: datetime) -> str:
    """Render an aware datetime in Taskwarrior's canonical compact UTC form.

    Taskwarrior parses extended ISO timestamps carrying fractional seconds as
    local time, shifting the stored instant by the process UTC offset. The
    compact export form is read back as UTC in every timezone, so generated
    Taskwarrior dates must cross the process boundary in this shape.
    """
    return when.astimezone(UTC).strftime(TW_DATETIME_FORMAT)


def future_utc(seconds: int) -> str:
    return canonical_utc(datetime.now(UTC) + timedelta(seconds=seconds))


def canonical_actor(actor: str) -> str:
    """Dash-stripped lowercase hex; safe as a UDA value and an rc-key segment.

    Taskwarrior rejects dashes in an rc key (e.g. an urgency coefficient keyed
    on a UUID value), so actor tokens are stored canonicalised. The sentinel
    becomes 32 zeros.
    """
    return "".join(c for c in actor.lower() if c.isalnum())


def current_actor() -> str:
    return canonical_actor(ambient_thread_id() or config.SENTINEL_ACTOR)


def _git(*args: str) -> str:
    result = run_git_command(
        ["git", "-C", str(config.repo_root()), *args],
        capture_output=True,
        check=False,
        text=True,
    )
    return result.stdout.strip() if result.returncode == 0 else ""


def current_branch() -> str:
    return _git("branch", "--show-current")


def worktree_clean() -> bool:
    return _git("status", "--porcelain") == ""


def require_clean_worktree(action: str) -> None:
    if not worktree_clean():
        raise SpiceError(
            f"{action} requires a clean worktree; commit or stash your changes first"
        )


def claim_head() -> str:
    return _git("rev-parse", "HEAD")
