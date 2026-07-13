"""Bounded git subprocess execution shared by control-plane callers."""

from __future__ import annotations

import os
import shlex
import subprocess
from collections.abc import Sequence
from typing import Any

from spice.errors import SpiceError

GIT_TIMEOUT_ENV = "SPICE_GIT_TIMEOUT_SECONDS"  # env-policy: allow
DEFAULT_GIT_TIMEOUT_SECONDS = 120.0


def git_timeout_seconds(default: float = DEFAULT_GIT_TIMEOUT_SECONDS) -> float:
    """Return the configured positive git deadline in seconds."""
    raw = os.environ.get(GIT_TIMEOUT_ENV, "").strip()  # env-policy: allow
    if not raw:
        return default
    try:
        timeout = float(raw)
    except ValueError as exc:
        raise SpiceError(f"{GIT_TIMEOUT_ENV} must be a positive number") from exc
    if timeout <= 0:
        raise SpiceError(f"{GIT_TIMEOUT_ENV} must be a positive number")
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
    try:
        return subprocess.run(argv, timeout=timeout, **kwargs)
    except subprocess.TimeoutExpired as exc:
        raise SpiceError(
            f"git command timed out after {timeout:g}s: {shlex.join(argv)}; "
            f"increase {GIT_TIMEOUT_ENV} for a slower repository"
        ) from exc
