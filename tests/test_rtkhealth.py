"""RTK health probes select optimization or native-command mode."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from spice.config import edit, layers, values
from spice.agent.rtkhealth import probe_rtk_health


@pytest.mark.parametrize(
    ("executable", "rewrite_exit"),
    [
        ("rtk", 0),
        ("alternate-rtk", 3),
        ("/opt/Spice Tools/rtk", 3),
    ],
)
def test_health_probe_uses_exact_executable_and_accepts_supported_rewrites(
    tmp_path: Path,
    executable: str,
    rewrite_exit: int,
) -> None:
    _configure_executable(tmp_path, executable)
    calls: list[list[str]] = []

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(command)
        if command[1:] == ["--version"]:
            return subprocess.CompletedProcess(command, 0, "rtk 0.42.4\n", "")
        return subprocess.CompletedProcess(
            command, rewrite_exit, "rtk git status --short\n", ""
        )

    health = probe_rtk_health(tmp_path, run=run)

    assert {
        "calls": calls,
        "state": health.state,
        "mode": health.mode,
        "version": health.version,
        "command": health.verification_command(),
    } == {
        "calls": [
            [executable, "--version"],
            [executable, "rewrite", "--", "git", "status"],
        ],
        "state": "active",
        "mode": "active",
        "version": "0.42.4",
        "command": (
            f"{_quoted(executable)} --version && "
            f"{_quoted(executable)} rewrite -- git status"
        ),
    }


@pytest.mark.parametrize(
    ("name", "executable", "responses", "expected"),
    [
        (
            "missing",
            "/missing/rtk",
            [FileNotFoundError(2, "missing", "/missing/rtk")],
            ("missing", "native", ""),
        ),
        (
            "obsolete",
            "old-rtk",
            [subprocess.CompletedProcess([], 0, "rtk 0.41.9\n", "")],
            ("obsolete", "native", "0.41.9"),
        ),
        (
            "version-invalid",
            "odd-rtk",
            [subprocess.CompletedProcess([], 0, "unknown version\n", "")],
            ("protocol-invalid", "native", ""),
        ),
        (
            "rewrite-invalid",
            "broken-rtk",
            [
                subprocess.CompletedProcess([], 0, "rtk 0.42.4\n", ""),
                subprocess.CompletedProcess([], 1, "", ""),
            ],
            ("protocol-invalid", "native", "0.42.4"),
        ),
    ],
)
def test_health_probe_reports_native_mode_for_each_degraded_state(
    tmp_path: Path,
    name: str,
    executable: str,
    responses: list[object],
    expected: tuple[str, str, str],
) -> None:
    del name
    _configure_executable(tmp_path, executable)
    pending = iter(responses)
    calls: list[list[str]] = []

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(command)
        response = next(pending)
        if isinstance(response, OSError):
            raise response
        return response  # type: ignore[return-value]

    health = probe_rtk_health(tmp_path, run=run)
    payload = json.loads(health.activation_status_line().removeprefix("rtk_status="))

    assert {
        "outcome": (health.state, health.mode, health.version),
        "first_call": calls[0],
        "payload": payload,
    } == {
        "outcome": expected,
        "first_call": [executable, "--version"],
        "payload": {
            "detail": health.detail,
            "executable": executable,
            "mode": "native",
            "state": expected[0],
            "version": expected[2] or None,
        },
    }


def _configure_executable(repo_root: Path, executable: str) -> None:
    edit.set_scope_section(
        repo_root,
        layers.WORKTREE_SOURCE,
        values.RTK_KEY,
        {values.RTK_EXECUTABLE_KEY: executable},
    )


def _quoted(executable: str) -> str:
    if " " in executable:
        return f"'{executable}'"
    return executable
