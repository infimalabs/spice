"""Bounded git subprocess execution shared by control-plane callers."""

from __future__ import annotations

import math
import os
import shlex
import subprocess
from collections.abc import Sequence
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.process.groups import ProcessDeadlineExceeded, run_bounded_process_group

GIT_TIMEOUT_ENV = "SPICE_GIT_TIMEOUT_SECONDS"  # env-policy: allow
DEFAULT_GIT_TIMEOUT_SECONDS = 120.0
GIT_PROBE_TIMEOUT_SECONDS = 10.0
GIT_PROBE_TIMEOUT_RETURNCODE = 124


def git_timeout_seconds(default: float = DEFAULT_GIT_TIMEOUT_SECONDS) -> float:
    """Return the configured positive git deadline in seconds."""
    raw = os.environ.get(GIT_TIMEOUT_ENV, "").strip()  # env-policy: allow
    if not raw:
        return default
    try:
        timeout = float(raw)
    except ValueError as exc:
        raise SpiceError(f"{GIT_TIMEOUT_ENV} must be a positive finite number") from exc
    if not math.isfinite(timeout) or timeout <= 0:
        raise SpiceError(f"{GIT_TIMEOUT_ENV} must be a positive finite number")
    return timeout


def run_git_command(
    command: Sequence[str],
    *,
    default_timeout_seconds: float = DEFAULT_GIT_TIMEOUT_SECONDS,
    **kwargs: Any,
) -> subprocess.CompletedProcess[str]:
    """Run one git argv under the configured deadline or fail with its identity."""
    timeout = git_timeout_seconds(default_timeout_seconds)
    argv = list(command)
    capture_output = bool(kwargs.pop("capture_output", False))
    check = bool(kwargs.pop("check", False))
    cwd = kwargs.pop("cwd", None)
    env = kwargs.pop("env", None)
    input_data = kwargs.pop("input", None)
    text = bool(kwargs.pop("text", False))
    if kwargs:
        names = ", ".join(sorted(kwargs))
        raise TypeError(f"unsupported git process options: {names}")
    try:
        return run_bounded_process_group(
            argv,
            timeout_seconds=timeout,
            phase="git",
            input_label=shlex.join(argv),
            cwd=cwd,
            text=text,
            env=env,
            input_data=input_data,
            capture_output=capture_output,
            check=check,
        )
    except ProcessDeadlineExceeded as exc:
        raise SpiceError(
            f"git command timed out after {timeout:g}s: {shlex.join(argv)}; "
            f"increase {GIT_TIMEOUT_ENV} for a slower repository"
        ) from exc


def git_run(
    repo_root: Path,
    *args: str,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run one git argv against a repository and return the completed process."""
    return run_git_command(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        text=True,
        env=env,
    )


def git_read(repo_root: Path, *args: str, env: dict[str, str] | None = None) -> str:
    """Return stripped stdout from a successful git argv, or empty text."""
    result = git_run(repo_root, *args, env=env)
    if result.returncode != 0:
        return ""
    return result.stdout.strip()


def git_lines(repo_root: Path, *args: str) -> list[str]:
    """Return the non-empty stdout lines from a successful git argv."""
    return [line for line in git_read(repo_root, *args).splitlines() if line.strip()]


def git_probe(
    repo_root: Path,
    *args: str,
    timeout_seconds: float = GIT_PROBE_TIMEOUT_SECONDS,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Probe git under a short deadline, degrading to a synthetic failure result."""
    argv = ["git", "-C", str(repo_root), *args]
    try:
        return run_bounded_process_group(
            argv,
            timeout_seconds=timeout_seconds,
            phase="git.probe",
            input_label=shlex.join(argv),
            text=True,
            env=env,
            capture_output=True,
        )
    except (OSError, ProcessDeadlineExceeded):
        return subprocess.CompletedProcess(
            argv, returncode=GIT_PROBE_TIMEOUT_RETURNCODE, stdout="", stderr=""
        )


def git_probe_read(
    repo_root: Path, *args: str, env: dict[str, str] | None = None
) -> str:
    """Return stripped stdout from a successful probe, or empty text."""
    result = git_probe(repo_root, *args, env=env)
    if result.returncode != 0:
        return ""
    return result.stdout.strip()
