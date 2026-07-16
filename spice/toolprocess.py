"""Named subprocess policies for bounded tools and parent-lifetime children."""

from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any, Literal

from spice.procs import run_bounded_process_group

ToolPolicy = Literal[
    "coverage",
    "extension",
    "hook",
    "release",
    "study",
    "typecheck",
]

COVERAGE_TOOL_TIMEOUT_SECONDS = 600.0
EXTENSION_TOOL_TIMEOUT_SECONDS = 120.0
HOOK_TOOL_TIMEOUT_SECONDS = 300.0
RELEASE_TOOL_TIMEOUT_SECONDS = 300.0
STUDY_TOOL_TIMEOUT_SECONDS = 120.0
TYPECHECK_TOOL_TIMEOUT_SECONDS = 300.0

TOOL_POLICY_TIMEOUT_SECONDS: dict[ToolPolicy, float] = {
    "coverage": COVERAGE_TOOL_TIMEOUT_SECONDS,
    "extension": EXTENSION_TOOL_TIMEOUT_SECONDS,
    "hook": HOOK_TOOL_TIMEOUT_SECONDS,
    "release": RELEASE_TOOL_TIMEOUT_SECONDS,
    "study": STUDY_TOOL_TIMEOUT_SECONDS,
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


def run_parent_lifetime_command(
    command: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    check: bool = False,
) -> subprocess.CompletedProcess[Any]:
    """Run an interactive foreground child until child exit or parent cancellation."""
    return subprocess.run(command, cwd=cwd, env=env, check=check)
