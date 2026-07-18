"""Deterministic completion contracts for session rehydration providers."""

from __future__ import annotations

import sys
import time
from pathlib import Path

import pytest

from spice.cli.parser import build_parser
from spice.process.groups import (
    ProcessDeadlineExceeded,
    process_id_is_running,
    run_bounded_process_group,
)
from spice.sessions import briefingpressure
from spice.sessions import cli as session_cli
from spice.sessions import deadline as deadline_module
from spice.sessions.deadline import RehydrationDeadlineExceeded
from spice.studies import complexity

FAST_DEADLINE_SECONDS = 0.1
COMMAND_RETURN_BUDGET_SECONDS = 1.0
PROCESS_EXIT_BUDGET_SECONDS = 1.0
# The group-termination handshake starts two interpreters and writes a pid file
# before its deadline may fire; under full-suite xdist load those spawns blow
# far past FAST_DEADLINE_SECONDS, so that one test gets a generous deadline.
SPAWN_HANDSHAKE_DEADLINE_SECONDS = 5.0


@pytest.mark.parametrize(
    ("action", "provider_name", "extra_args"),
    [
        ("briefing", "render_briefing", []),
        ("sweep", "render_sweep", ["--count", "2"]),
    ],
)
def test_rehydration_commands_report_end_to_end_deadline_with_transcript_input(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    action: str,
    provider_name: str,
    extra_args: list[str],
) -> None:
    transcript = tmp_path / "hung-provider.jsonl"
    transcript.write_text("", encoding="utf-8")

    def hung_analysis_provider(*_args: object, **_kwargs: object) -> str:
        time.sleep(COMMAND_RETURN_BUDGET_SECONDS * 10)
        return "unreachable"

    monkeypatch.setattr(session_cli, provider_name, hung_analysis_provider)
    args = build_parser().parse_args(
        [
            "session",
            action,
            str(transcript),
            *extra_args,
            "--deadline-seconds",
            str(FAST_DEADLINE_SECONDS),
        ]
    )

    started_at = time.monotonic()
    with pytest.raises(RehydrationDeadlineExceeded) as raised:
        args.func(args)
    elapsed = time.monotonic() - started_at

    assert elapsed < COMMAND_RETURN_BUDGET_SECONDS
    assert raised.value.action == action
    assert raised.value.inputs == (str(transcript),)
    assert "phase=end-to-end-render" in str(raised.value)


def test_bounded_provider_terminates_process_group_and_reports_phase_input(
    tmp_path: Path,
) -> None:
    child_pid_path = tmp_path / "child.pid"
    provider = (
        "import pathlib,subprocess,sys,time; "
        "child=subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)']); "
        "pathlib.Path(sys.argv[1]).write_text(str(child.pid)); "
        "time.sleep(60)"
    )

    with pytest.raises(ProcessDeadlineExceeded) as raised:
        run_bounded_process_group(
            [sys.executable, "-c", provider, str(child_pid_path)],
            timeout_seconds=SPAWN_HANDSHAKE_DEADLINE_SECONDS,
            phase="briefing-complexity-current",
            input_label=f"repository={tmp_path} paths=hung.py",
            text=True,
        )

    child_pid = int(child_pid_path.read_text(encoding="utf-8"))
    assert _wait_for_process_exit(child_pid)
    assert raised.value.phase == "briefing-complexity-current"
    assert raised.value.input_label == f"repository={tmp_path} paths=hung.py"


def test_briefing_complexity_hung_provider_reports_current_analysis_input(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "hung.py"
    source.write_text("def hung():\n    return 1\n", encoding="utf-8")
    monkeypatch.setattr(complexity, "require_lizard", lambda: "lizard")
    monkeypatch.setattr(complexity, "run_bounded_process_group", _hung_provider)

    with pytest.raises(ProcessDeadlineExceeded) as raised:
        briefingpressure._scan_dirty_complexity_pressure(
            [Path("hung.py")],
            repo_root=tmp_path,
            suffixes=(".py",),
            ccn_threshold=20,
            length_threshold=80,
        )

    assert raised.value.phase == "briefing-complexity-current"
    assert raised.value.input_label == f"repository={tmp_path} paths=hung.py"


def test_briefing_git_hung_provider_reports_repository_input(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(briefingpressure, "run_bounded_process_group", _hung_provider)

    with pytest.raises(ProcessDeadlineExceeded) as raised:
        briefingpressure._git_read(tmp_path, "status", "--short")

    assert raised.value.phase == "briefing-git-posture"
    assert raised.value.input_label == f"repository={tmp_path}"


@pytest.mark.parametrize("action", ["briefing", "sweep"])
def test_rehydration_deadline_covers_slow_transcript_resolution(
    action: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    def slow_resolution(_inputs: list[str]) -> list[Path]:
        time.sleep(COMMAND_RETURN_BUDGET_SECONDS * 10)
        return []

    monkeypatch.setattr(session_cli, "resolve_files", slow_resolution)
    args = build_parser().parse_args(
        [
            "session",
            action,
            "slow-thread",
            "--deadline-seconds",
            str(FAST_DEADLINE_SECONDS),
        ]
    )

    terminal = _rehydration_terminal(lambda: args.func(args))

    assert terminal == {
        "action": action,
        "inputs": ("slow-thread",),
        "phase": "end-to-end-render",
    }


def test_rehydration_deadline_uses_portable_interrupt_without_setitimer(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(deadline_module, "_can_use_setitimer", lambda: False)

    def slow_render() -> None:
        deadline_module.run_with_rehydration_deadline(
            lambda: time.sleep(COMMAND_RETURN_BUDGET_SECONDS * 10),
            action="briefing",
            inputs=("portable.jsonl",),
            timeout_seconds=FAST_DEADLINE_SECONDS,
        )

    terminal = _rehydration_terminal(slow_render)

    assert terminal == {
        "action": "briefing",
        "inputs": ("portable.jsonl",),
        "phase": "end-to-end-render",
    }


def test_briefing_repo_root_timeout_reports_cwd_identity(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(briefingpressure, "run_bounded_process_group", _hung_provider)

    try:
        briefingpressure._briefing_repo_root_from_cwd(tmp_path)
    except ProcessDeadlineExceeded as exc:
        terminal = {"phase": exc.phase, "input": exc.input_label}
    else:
        terminal = {"phase": "unexpected-success", "input": ""}

    assert terminal == {
        "phase": "briefing-repo-root",
        "input": f"cwd={tmp_path.resolve()}",
    }


def _rehydration_terminal(action) -> dict[str, object]:
    try:
        action()
    except RehydrationDeadlineExceeded as exc:
        return {
            "action": exc.action,
            "inputs": exc.inputs,
            "phase": "end-to-end-render",
        }
    return {"action": "unexpected-success", "inputs": (), "phase": ""}


def _hung_provider(
    _command: list[str],
    *,
    phase: str,
    input_label: str,
    **_kwargs: object,
) -> object:
    return run_bounded_process_group(
        [sys.executable, "-c", "import time; time.sleep(60)"],
        timeout_seconds=FAST_DEADLINE_SECONDS,
        phase=phase,
        input_label=input_label,
        text=True,
    )


def _wait_for_process_exit(pid: int) -> bool:
    deadline = time.monotonic() + PROCESS_EXIT_BUDGET_SECONDS
    while time.monotonic() < deadline:
        if not process_id_is_running(pid):
            return True
        time.sleep(0.01)
    return not process_id_is_running(pid)
