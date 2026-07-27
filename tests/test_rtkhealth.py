"""RTK health probes select optimization or native-command mode."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from spice.config import edit, layers, values
from spice.agent.rtkhealth import RTK_FIDELITY_PROBE, probe_rtk_health

FIDELITY_REWRITE = "rtk grep --count -E al+pha -"


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
    run = _staged_run(executable, rewrite_exit, ("1\n", 0), calls)

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
            list(RTK_FIDELITY_PROBE),
            [executable, "rewrite", "--", *RTK_FIDELITY_PROBE],
            ["rtk", "grep", "--count", "-E", "al+pha", "-"],
        ],
        "state": "active",
        "mode": "active",
        "version": "0.42.4",
        "command": (
            f"{_quoted(executable)} --version && "
            f"{_quoted(executable)} rewrite -- git status; echo; "
            "printf 'alpha\\nbeta\\n' | rg --count al+pha -; "
            f"printf 'alpha\\nbeta\\n' | $({_quoted(executable)} "
            "rewrite -- rg --count al+pha -)"
        ),
    }


def test_health_probe_separates_a_verified_answer_from_an_unchecked_one(
    tmp_path: Path,
) -> None:
    """Both outcomes stay active, so the detail is what tells them apart."""
    _configure_executable(tmp_path, "rtk")
    verified_calls: list[list[str]] = []
    unchecked_calls: list[list[str]] = []
    staged = _staged_run("rtk", 0, ("1\n", 0), unchecked_calls)

    def unlaunchable_search(
        command: list[str], **kwargs: object
    ) -> subprocess.CompletedProcess[str]:
        if command == list(RTK_FIDELITY_PROBE):
            raise FileNotFoundError(2, "No such file or directory", "rg")
        return staged(command, **kwargs)

    verified = probe_rtk_health(
        tmp_path, run=_staged_run("rtk", 0, ("1\n", 0), verified_calls)
    )
    unchecked = probe_rtk_health(tmp_path, run=unlaunchable_search)

    assert {
        "verified": (verified.state, verified.detail),
        "unchecked": (unchecked.state, unchecked.detail),
        "details_differ": verified.detail != unchecked.detail,
    } == {
        "verified": (
            "active",
            "rewrite protocol valid (exit 0); rewrite preserved the answer (counted 1)",
        ),
        "unchecked": (
            "active",
            "rewrite protocol valid (exit 0); "
            "answer unchecked: rg --count al+pha - reported no count",
        ),
        "details_differ": True,
    }


def test_health_probe_reports_a_rewrite_that_counted_a_different_answer(
    tmp_path: Path,
) -> None:
    """A rewrite answering zero where the written command answers one is reported."""
    _configure_executable(tmp_path, "rtk")
    calls: list[list[str]] = []
    run = _staged_run("rtk", 0, ("", 1), calls)

    health = probe_rtk_health(tmp_path, run=run)
    payload = json.loads(health.activation_status_line().removeprefix("rtk_status="))

    assert {
        "state": health.state,
        "mode": health.mode,
        "detail": health.detail,
        "payload_state": payload["state"],
        "payload_detail": payload["detail"],
        "searched": calls[2:],
    } == {
        "state": "rewrite-unfaithful",
        "mode": "native",
        "detail": (
            "rewriting rg --count al+pha - changed its answer: "
            "as written it counted 1, rewritten it counted 0"
        ),
        "payload_state": "rewrite-unfaithful",
        "payload_detail": (
            "rewriting rg --count al+pha - changed its answer: "
            "as written it counted 1, rewritten it counted 0"
        ),
        "searched": [
            list(RTK_FIDELITY_PROBE),
            ["rtk", "rewrite", "--", *RTK_FIDELITY_PROBE],
            ["rtk", "grep", "--count", "-E", "al+pha", "-"],
        ],
    }


def test_health_probe_carries_its_own_subject_and_leaves_the_checkout_alone(
    tmp_path: Path,
) -> None:
    """The fidelity search reads its subject from stdin, so probing writes nothing."""
    _configure_executable(tmp_path, "rtk")
    calls: list[list[str]] = []
    stdin_texts: list[object] = []

    def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        stdin_texts.append(kwargs.get("input"))
        return _staged_run("rtk", 0, ("", 1), calls)(command, **kwargs)

    before = _tree_entries(tmp_path)
    health = probe_rtk_health(tmp_path, run=run)
    after = _tree_entries(tmp_path)

    assert {
        "entries": after,
        "state": health.state,
        "subjects": stdin_texts[2:],
    } == {
        "entries": before,
        "state": "rewrite-unfaithful",
        "subjects": ["alpha\nbeta\n", None, "alpha\nbeta\n"],
    }


def _staged_run(
    executable: str,
    rewrite_exit: int,
    rewritten: tuple[str, int],
    calls: list[list[str]],
):
    """A runner answering each probe stage, with the rewritten search controllable."""

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(command)
        if command[1:] == ["--version"]:
            return subprocess.CompletedProcess(command, 0, "rtk 0.42.4\n", "")
        if command[1:] == ["rewrite", "--", "git", "status"]:
            return subprocess.CompletedProcess(
                command, rewrite_exit, "rtk git status --short\n", ""
            )
        if command[1:2] == ["rewrite"]:
            return subprocess.CompletedProcess(
                command, rewrite_exit, f"{FIDELITY_REWRITE}\n", ""
            )
        if command == list(RTK_FIDELITY_PROBE):
            return subprocess.CompletedProcess(command, 0, "1\n", "")
        return subprocess.CompletedProcess(command, rewritten[1], rewritten[0], "")

    return run


def _tree_entries(root: Path) -> dict[str, bytes | None]:
    return {
        str(path.relative_to(root)): path.read_bytes() if path.is_file() else None
        for path in sorted(root.rglob("*"))
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
