"""The suite seam reports a running suite instead of going dark until it exits."""

from __future__ import annotations

import sys
import tempfile
import time
from pathlib import Path

import pytest

from spice.process.groups import process_id_is_running, run_streamed_process_group
from spice.studies import suiteseam
from spice.studies.suiteseam import SuiteSeamPlan

STREAM_DEADLINE_SECONDS = 30.0
# The handshake child announces itself, waits to be released by a file the
# parent only writes after seeing that announcement, then speaks again. If the
# parent were reading through a buffer that fills until exit, the release would
# never be written and this child would run until the deadline -- so the run
# completing at all is the proof that output arrived mid-flight.
HANDSHAKE_CHILD = (
    "import pathlib, sys, time\n"
    "release = pathlib.Path(sys.argv[1])\n"
    "print('EARLY', flush=True)\n"
    "deadline = time.monotonic() + 20.0\n"
    "while not release.exists() and time.monotonic() < deadline:\n"
    "    time.sleep(0.01)\n"
    "print('LATE', flush=True)\n"
)
RED_CHILD = (
    "import sys, time\n"
    "print('BEFORE the pause')\n"
    "sys.stdout.flush()\n"
    "time.sleep(0.4)\n"
    "print('AFTER the pause', file=sys.stderr)\n"
    "sys.exit(3)\n"
)
SILENT_CHILD = "import time\ntime.sleep(1.0)\n"
CHATTY_LINES = 20
CHATTY_CHILD = (
    "import time\n"
    f"for _ in range({CHATTY_LINES}):\n"
    "    print('tick', flush=True)\n"
    "    time.sleep(0.05)\n"
)


def _plan(*source: str) -> SuiteSeamPlan:
    return SuiteSeamPlan(
        reason="a declared seam moved",
        matches=("core/tw.py",),
        argv=(sys.executable, "-c", *source),
        declared_seconds=1,
    )


def test_suite_output_reaches_the_caller_while_the_suite_is_still_running(tmp_path):
    release = tmp_path / "release"
    seen: list[str] = []

    def progress(chunk: str, _elapsed: float) -> None:
        seen.append(chunk)
        if "EARLY" in chunk:
            release.write_text("go", encoding="utf-8")

    result = run_streamed_process_group(
        [sys.executable, "-c", HANDSHAKE_CHILD, str(release)],
        timeout_seconds=STREAM_DEADLINE_SECONDS,
        phase="tool.suite",
        input_label="prove output arrives before exit",
        on_progress=progress,
        cwd=tmp_path,
    )

    assert result.returncode == 0
    assert "EARLY" in result.stdout and "LATE" in result.stdout
    early = next(index for index, chunk in enumerate(seen) if "EARLY" in chunk)
    late = next(index for index, chunk in enumerate(seen) if "LATE" in chunk)
    assert early < late


def test_a_red_suite_still_refuses_with_everything_it_printed(tmp_path):
    outcome = suiteseam._measure_suite(tmp_path, _plan(RED_CHILD))

    assert outcome.returncode == 3
    refusal = suiteseam._red_suite_refusal("TASK-1kJ46S8b", outcome)
    assert "BEFORE the pause" in refusal
    assert "AFTER the pause" in refusal


def test_a_silent_suite_still_says_it_is_alive(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr(suiteseam, "SUITE_HEARTBEAT_SECONDS", 0.1)

    outcome = suiteseam._measure_suite(tmp_path, _plan(SILENT_CHILD))

    assert outcome.returncode == 0
    heartbeats = [
        line
        for line in capsys.readouterr().out.splitlines()
        if line.startswith("suite seam: still running after ")
    ]
    assert len(heartbeats) >= 1


def test_a_talking_suite_is_left_to_talk(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr(suiteseam, "SUITE_HEARTBEAT_SECONDS", 0.1)

    suiteseam._measure_suite(tmp_path, _plan(CHATTY_CHILD))

    printed = capsys.readouterr().out
    assert printed.splitlines() == ["tick"] * CHATTY_LINES


def test_the_streamed_runner_cleans_up_its_sink(tmp_path, monkeypatch):
    monkeypatch.setattr(tempfile, "tempdir", str(tmp_path))

    run_streamed_process_group(
        [sys.executable, "-c", "print('done')"],
        timeout_seconds=STREAM_DEADLINE_SECONDS,
        phase="tool.suite",
        input_label="prove the sink is removed",
        on_progress=lambda _chunk, _elapsed: None,
        cwd=tmp_path,
    )

    assert sorted(path.name for path in Path(tmp_path).iterdir()) == []


def test_a_progress_failure_reaps_the_streamed_child(tmp_path):
    seen: list[str] = []

    def reject_progress(chunk: str, _elapsed: float) -> None:
        seen.append(chunk)
        if chunk:
            raise RuntimeError("progress sink closed")

    with pytest.raises(RuntimeError, match="progress sink closed"):
        run_streamed_process_group(
            [
                sys.executable,
                "-c",
                "import os,time; print(os.getpid(), flush=True); time.sleep(30)",
            ],
            timeout_seconds=STREAM_DEADLINE_SECONDS,
            phase="tool.suite",
            input_label="prove callback failures reap the child",
            on_progress=reject_progress,
            cwd=tmp_path,
        )

    child_pid = int("".join(seen).strip())
    reaping_deadline = time.monotonic() + 2
    while process_id_is_running(child_pid) and time.monotonic() < reaping_deadline:
        time.sleep(0.02)
    assert not process_id_is_running(child_pid)
