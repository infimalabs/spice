"""Non-blocking health reporting for the optional RTK command optimizer."""

from __future__ import annotations

import json
import re
import shlex
import subprocess
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from spice.config import values
from spice.agent.rtkrewrite import (
    RTK_REWRITE_MATCH_EXIT_CODE,
    RTK_REWRITE_SUCCESS_EXIT_CODES,
)

RTK_MINIMUM_VERSION = (0, 42, 4)
RTK_MINIMUM_VERSION_TEXT = ".".join(str(part) for part in RTK_MINIMUM_VERSION)
RTK_VERSION_PATTERN = re.compile(r"\brtk\s+(\d+)\.(\d+)\.(\d+)\b", re.IGNORECASE)
RTK_PROTOCOL_PROBE = ("git", "status")


@dataclass(frozen=True)
class RtkHealth:
    executable: str
    state: str
    detail: str
    version: str = ""

    @property
    def active(self) -> bool:
        return self.state == "active"

    @property
    def mode(self) -> str:
        return "active" if self.active else "native"

    def activation_status_line(self) -> str:
        payload = {
            "detail": self.detail,
            "executable": self.executable,
            "mode": self.mode,
            "state": self.state,
            "version": self.version or None,
        }
        return "rtk_status=" + json.dumps(
            payload, separators=(",", ":"), sort_keys=True
        )

    def verification_command(self) -> str:
        executable = shlex.quote(self.executable)
        return (
            f"{executable} --version && "
            f"{executable} rewrite -- {' '.join(RTK_PROTOCOL_PROBE)}"
        )


def probe_rtk_health(
    repo_root: Path | None,
    *,
    run: Callable[..., subprocess.CompletedProcess[str]] | None = None,
) -> RtkHealth:
    """Probe the configured executable exactly and always return a health state."""
    executable = values.configured_rtk_executable(repo_root)
    runner = run or subprocess.run
    version_result, launch_detail = _run_probe(runner, [executable, "--version"])
    if version_result is None:
        return RtkHealth(executable, "missing", launch_detail)

    version_output = _result_output(version_result)
    version_match = RTK_VERSION_PATTERN.search(version_output)
    if version_result.returncode != 0 or version_match is None:
        return RtkHealth(
            executable,
            "protocol-invalid",
            f"could not validate RTK version from {version_output!r}",
        )
    version_tuple = tuple(int(part) for part in version_match.groups())
    version = ".".join(str(part) for part in version_tuple)
    if version_tuple < RTK_MINIMUM_VERSION:
        return RtkHealth(
            executable,
            "obsolete",
            f"RTK {version} is older than required {RTK_MINIMUM_VERSION_TEXT}",
            version,
        )

    rewrite_command = [executable, "rewrite", "--", *RTK_PROTOCOL_PROBE]
    rewrite_result, launch_detail = _run_probe(runner, rewrite_command)
    if rewrite_result is None:
        return RtkHealth(executable, "missing", launch_detail, version)
    rewritten = _stdout_text(rewrite_result).strip()
    if rewrite_result.returncode in RTK_REWRITE_SUCCESS_EXIT_CODES and rewritten:
        return RtkHealth(
            executable,
            "active",
            f"rewrite protocol valid (exit {rewrite_result.returncode})",
            version,
        )
    stdout_shape = "nonempty" if rewritten else "empty"
    return RtkHealth(
        executable,
        "protocol-invalid",
        (
            "rewrite probe returned "
            f"exit {rewrite_result.returncode} with {stdout_shape} stdout; "
            f"expected exit 0 or {RTK_REWRITE_MATCH_EXIT_CODE} with nonempty stdout"
        ),
        version,
    )


def _run_probe(
    runner: Callable[..., subprocess.CompletedProcess[str]], command: list[str]
) -> tuple[subprocess.CompletedProcess[str] | None, str]:
    try:
        return (
            runner(command, capture_output=True, text=True, check=False),
            "",
        )
    except OSError as exc:
        return None, f"launch failed: {type(exc).__name__}: {exc}"


def _result_output(result: subprocess.CompletedProcess[str]) -> str:
    return (_stdout_text(result) or _stderr_text(result)).strip()


def _stdout_text(result: subprocess.CompletedProcess[str]) -> str:
    return result.stdout if isinstance(result.stdout, str) else ""


def _stderr_text(result: subprocess.CompletedProcess[str]) -> str:
    return result.stderr if isinstance(result.stderr, str) else ""
