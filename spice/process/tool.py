"""Named subprocess policies for bounded tools and parent-lifetime children."""

from __future__ import annotations

import shlex
import subprocess
from pathlib import Path
from typing import Any, Literal

from spice.errors import SpiceError
from spice.process.groups import run_bounded_process_group

ToolPolicy = Literal[
    "coverage",
    "extension",
    "hook",
    "probe",
    "release",
    "study",
    "suite",
    "typecheck",
]

COVERAGE_TOOL_TIMEOUT_SECONDS = 600.0
EXTENSION_TOOL_TIMEOUT_SECONDS = 120.0
HOOK_TOOL_TIMEOUT_SECONDS = 300.0
PROBE_TOOL_TIMEOUT_SECONDS = 5.0
RELEASE_TOOL_TIMEOUT_SECONDS = 300.0
STUDY_TOOL_TIMEOUT_SECONDS = 120.0
# A whole test suite is the longest-running tool the harness drives; this budget
# is a stall backstop for a suite that hangs, not a target any suite should
# approach.
SUITE_TOOL_TIMEOUT_SECONDS = 900.0
TYPECHECK_TOOL_TIMEOUT_SECONDS = 300.0

TOOL_POLICY_TIMEOUT_SECONDS: dict[ToolPolicy, float] = {
    "coverage": COVERAGE_TOOL_TIMEOUT_SECONDS,
    "extension": EXTENSION_TOOL_TIMEOUT_SECONDS,
    "hook": HOOK_TOOL_TIMEOUT_SECONDS,
    "probe": PROBE_TOOL_TIMEOUT_SECONDS,
    "release": RELEASE_TOOL_TIMEOUT_SECONDS,
    "study": STUDY_TOOL_TIMEOUT_SECONDS,
    "suite": SUITE_TOOL_TIMEOUT_SECONDS,
    "typecheck": TYPECHECK_TOOL_TIMEOUT_SECONDS,
}


def run_tool_command(
    command: list[str],
    *,
    policy: ToolPolicy,
    operation: str,
    cwd: Path | None = None,
    text: bool = False,
    env: dict[str, str] | None = None,
    input_data: str | bytes | None = None,
    capture_output: bool,
    check: bool = False,
) -> subprocess.CompletedProcess[Any]:
    """Run a synchronous tool under its policy deadline and process-group cleanup."""
    return run_bounded_process_group(
        command,
        timeout_seconds=TOOL_POLICY_TIMEOUT_SECONDS[policy],
        phase=f"tool.{policy}",
        input_label=operation,
        cwd=cwd,
        text=text,
        env=env,
        input_data=input_data,
        capture_output=capture_output,
        check=check,
    )


def run_typecheck_command(argv: tuple[str, ...], *, operation: str, cwd: Path) -> None:
    """Run one typecheck command and raise its stable, output-bearing error."""
    result = run_tool_command(
        list(argv),
        policy="typecheck",
        operation=operation,
        capture_output=True,
        text=True,
        cwd=cwd,
        check=False,
    )
    if result.returncode == 0:
        return
    output = "\n".join(
        part for part in (result.stdout.strip(), result.stderr.strip()) if part
    )
    message = f"{shlex.join(argv)} exited {result.returncode}"
    if output:
        message += ":\n" + output
    raise SpiceError(message)


def run_parent_lifetime_command(
    command: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    check: bool = False,
) -> subprocess.CompletedProcess[Any]:
    """Run an interactive foreground child until child exit or parent cancellation."""
    return subprocess.run(command, cwd=cwd, env=env, check=check)
