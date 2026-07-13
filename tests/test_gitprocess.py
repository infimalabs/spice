"""Bounded git subprocess execution."""

from __future__ import annotations

import shlex
import subprocess
import sys
import time

from spice import gitprocess
from spice.errors import SpiceError
from spice.procs import process_id_is_running


def test_git_deadline_reaps_stalled_descendant_and_preserves_command_identity(
    tmp_path, monkeypatch
):
    descendant_pid_path = tmp_path / "descendant.pid"
    child = (
        "import os,signal,sys,time;"
        "from pathlib import Path;"
        "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
        "Path(sys.argv[1]).write_text(str(os.getpid()),encoding='utf-8');"
        "time.sleep(30)"
    )
    parent = (
        "import subprocess,sys,time;"
        "from pathlib import Path;"
        "path=Path(sys.argv[1]);"
        "subprocess.Popen([sys.executable,'-c',sys.argv[2],str(path)]);"
        "deadline=time.monotonic()+5;"
        "\nwhile not path.exists() and time.monotonic()<deadline: time.sleep(0.01);"
        "\ntime.sleep(30)"
    )
    command = [sys.executable, "-c", parent, str(descendant_pid_path), child]
    monkeypatch.setenv(gitprocess.GIT_TIMEOUT_ENV, "1")

    try:
        gitprocess.run_git_command(command, capture_output=True, text=True)
    except SpiceError as exc:
        message = str(exc)
    else:
        message = "unexpected success"

    descendant_pid = int(descendant_pid_path.read_text(encoding="utf-8"))
    reaping_deadline = time.monotonic() + 2
    while process_id_is_running(descendant_pid) and time.monotonic() < reaping_deadline:
        time.sleep(0.02)
    descendant_state = "running" if process_id_is_running(descendant_pid) else "reaped"

    assert {"message": message, "descendant": descendant_state} == {
        "message": (
            f"git command timed out after 1s: {shlex.join(command)}; "
            f"increase {gitprocess.GIT_TIMEOUT_ENV} for a slower repository"
        ),
        "descendant": "reaped",
    }


def test_git_timeout_environment_applies_to_local_commands(monkeypatch):
    seen: dict[str, object] = {}

    def fake_run(command: list[str], **kwargs):
        seen["command"] = command
        seen["timeout"] = kwargs["timeout_seconds"]
        seen["phase"] = kwargs["phase"]
        seen["input"] = kwargs["input_label"]
        seen["capture_output"] = kwargs["capture_output"]
        return subprocess.CompletedProcess(command, 0, stdout="")

    monkeypatch.setenv(gitprocess.GIT_TIMEOUT_ENV, "37.5")
    monkeypatch.setattr(gitprocess, "run_bounded_process_group", fake_run)

    gitprocess.run_git_command(["git", "status"], capture_output=True, text=True)

    assert seen == {
        "command": ["git", "status"],
        "timeout": 37.5,
        "phase": "git",
        "input": "git status",
        "capture_output": True,
    }


def test_git_timeout_configuration_accepts_only_positive_finite_values(monkeypatch):
    spawned: list[float] = []

    def fake_run(command, **kwargs):
        spawned.append(kwargs["timeout_seconds"])
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(gitprocess, "run_bounded_process_group", fake_run)
    outcomes: dict[str, dict[str, object]] = {}
    for raw in ("37.5", "nan", "inf", "-inf"):
        monkeypatch.setenv(gitprocess.GIT_TIMEOUT_ENV, raw)
        try:
            gitprocess.run_git_command(["git", "status"], capture_output=True)
        except SpiceError as exc:
            outcomes[raw] = {"state": "rejected", "message": str(exc)}
        else:
            outcomes[raw] = {"state": "accepted", "timeout": spawned[-1]}

    invalid = f"{gitprocess.GIT_TIMEOUT_ENV} must be a positive finite number"
    assert {"outcomes": outcomes, "spawned": spawned} == {
        "outcomes": {
            "37.5": {"state": "accepted", "timeout": 37.5},
            "nan": {"state": "rejected", "message": invalid},
            "inf": {"state": "rejected", "message": invalid},
            "-inf": {"state": "rejected", "message": invalid},
        },
        "spawned": [37.5],
    }
