"""Deterministic RTK rewrite result handling and native-fallback diagnostics."""

from __future__ import annotations

import contextlib
import hashlib
import os
import subprocess
import sys
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, TextIO

from spice import config
from spice.agent.identity import ambient_thread_id
from spice.agent.paths import agent_thread_state_dir
from spice.errors import SpiceError

RTK_REWRITE_SUBCOMMAND = "rewrite"
RTK_REWRITE_MATCH_EXIT_CODE = 3
RTK_REWRITE_NO_MATCH_EXIT_CODE = 1
RTK_REWRITE_SUCCESS_EXIT_CODES = frozenset((0, RTK_REWRITE_MATCH_EXIT_CODE))
RTK_DIAGNOSTIC_EXECUTABLE_CHARS = 160

RtkWarningKey = tuple[str, str, str]
_rtk_warned_keys: set[RtkWarningKey] = set()


@dataclass(frozen=True)
class RtkRewriteDecision:
    rewritten: str | None = None
    failure_class: str = ""
    failure_signature: str = ""


def rewrite_command_text(
    *args: str,
    repo_root: Path | None = None,
    env: Mapping[str, str] | None = None,
    stderr: TextIO | None = None,
    run: Callable[..., subprocess.CompletedProcess[str]] | None = None,
) -> str | None:
    """Return a usable RTK rewrite or select native execution with one warning."""
    executable = config.configured_rtk_executable(repo_root)
    runner = run or subprocess.run
    run_kwargs: dict[str, Any] = {
        "capture_output": True,
        "text": True,
        "check": False,
    }
    if env is not None:
        run_kwargs["env"] = dict(env)
    try:
        completed = runner(
            [executable, RTK_REWRITE_SUBCOMMAND, "--", *args],
            **run_kwargs,
        )
    except OSError as exc:
        failure_class = _launch_failure_class(exc)
        emit_rewrite_diagnostic(
            repo_root,
            stderr,
            executable=executable,
            failure_class=failure_class,
            failure_signature=f"{failure_class}:errno={exc.errno}",
        )
        return None
    decision = _classify_rewrite_result(completed)
    if decision.failure_class:
        emit_rewrite_diagnostic(
            repo_root,
            stderr,
            executable=executable,
            failure_class=decision.failure_class,
            failure_signature=decision.failure_signature,
        )
    return decision.rewritten


def emit_rewrite_diagnostic(
    repo_root: Path | None,
    stderr: TextIO | None,
    *,
    executable: str,
    failure_class: str,
    failure_signature: str,
) -> None:
    """Emit one bounded warning for this thread/executable/failure signature."""
    if not _claim_warning_signature(
        repo_root,
        executable=executable,
        failure_signature=f"{failure_class}:{failure_signature}",
    ):
        return
    display = executable
    if len(display) > RTK_DIAGNOSTIC_EXECUTABLE_CHARS:
        display = display[: RTK_DIAGNOSTIC_EXECUTABLE_CHARS - 1] + "…"
    surface = stderr if stderr is not None else sys.stderr
    surface.write(
        "spice agent run: RTK rewrite degraded to native "
        f"executable={display!r} failure={failure_class}\n"
    )
    surface.flush()


def _classify_rewrite_result(completed: object) -> RtkRewriteDecision:
    returncode = getattr(completed, "returncode", None)
    stdout = getattr(completed, "stdout", None)
    if not isinstance(returncode, int) or not isinstance(stdout, (str, type(None))):
        return RtkRewriteDecision(
            failure_class="invalid-result-shape",
            failure_signature=(
                f"returncode={type(returncode).__name__}:stdout={type(stdout).__name__}"
            ),
        )
    rewritten = (stdout or "").strip()
    if returncode in RTK_REWRITE_SUCCESS_EXIT_CODES and rewritten:
        return RtkRewriteDecision(rewritten=rewritten)
    if returncode == RTK_REWRITE_NO_MATCH_EXIT_CODE and not rewritten:
        return RtkRewriteDecision()
    stdout_shape = "nonempty" if rewritten else "empty"
    if returncode not in (
        *RTK_REWRITE_SUCCESS_EXIT_CODES,
        RTK_REWRITE_NO_MATCH_EXIT_CODE,
    ):
        return RtkRewriteDecision(
            failure_class="unexpected-exit",
            failure_signature=f"exit={returncode}:stdout={stdout_shape}",
        )
    return RtkRewriteDecision(
        failure_class="invalid-result-pair",
        failure_signature=f"exit={returncode}:stdout={stdout_shape}",
    )


def _launch_failure_class(exc: OSError) -> str:
    if isinstance(exc, FileNotFoundError):
        return "launch-not-found"
    if isinstance(exc, PermissionError):
        return "launch-permission"
    return "launch-error"


def _claim_warning_signature(
    repo_root: Path | None, *, executable: str, failure_signature: str
) -> bool:
    thread_id = ambient_thread_id() or ""
    key = (thread_id, executable, failure_signature)
    if key in _rtk_warned_keys:
        return False
    if repo_root is not None and thread_id:
        digest = hashlib.sha256(
            f"{executable}\0{failure_signature}".encode("utf-8")
        ).hexdigest()
        try:
            directory = (
                agent_thread_state_dir(repo_root, thread_id) / "rtk" / "warnings"
            )
            directory.mkdir(parents=True, exist_ok=True)
            descriptor = os.open(
                directory / digest,
                os.O_CREAT | os.O_EXCL | os.O_WRONLY,
                0o600,
            )
        except FileExistsError:
            _rtk_warned_keys.add(key)
            return False
        except (OSError, SpiceError):
            pass
        else:
            with contextlib.suppress(OSError):
                os.close(descriptor)
    _rtk_warned_keys.add(key)
    return True
