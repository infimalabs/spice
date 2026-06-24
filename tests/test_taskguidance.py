"""Focused task output guidance for continuing allocator work."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.tasks import alloc, config, create, identity, ops, render

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
KEEP_DRAINING = (
    "keep working until no allocator-selected work remains or a real blocker exists"
)
STEER_EXPLICIT_DIRECTION = (
    "run spice task next only when explicitly directed to continue allocator work"
)
STEER_MANUAL_CLAIM = (
    "manual task claims are exceptional and usually require explicit operator direction"
)
TASK_CAPTURE_IMMEDIATE = (
    "capture operator task-creation requests immediately with a TASK directive "
    "that starts on its own line"
)
TASK_CAPTURE_NOT_ALLOCATOR = "immediate task capture is not allocator selection"
TASK_CAPTURE_ACK_EXAMPLE = "ACK <key>: captured the request."
TASK_CAPTURE_LINE_EXAMPLE = "TASK title=... | project=<stem.child> | acceptance=..."


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-task-guidance")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


@pytest.mark.parametrize("lifetime", ["Drive", "Drain"])
def test_task_done_and_review_outputs_keep_draining_guidance(
    task_repo, monkeypatch, lifetime
):
    assert task_repo.is_dir()
    handle = create.add(
        "Exercise task next guidance",
        project="task.guidance",
        priority="medium",
        acceptance=["post-boundary guidance is explicit"],
    )
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.guidance"], "lifetime": lifetime},
    )
    ops.claim(handle)

    done_output = ops.done(handle, validation=["guidance checked"])

    assert (
        "next: YOU ARE NOT DONE. Run spice task next for reviewer assignment; "
        "self-review only if next assigns it"
    ) in done_output
    assert KEEP_DRAINING in done_output
    assert (
        "next: YOU ARE NOT DONE. Run spice task next for reviewer assignment"
        in render.render_show(handle)
    )

    assigned = alloc.next_task()
    assert identity.render_handle(assigned or {}) == handle

    review_output = ops.review(handle, finding="clean", note="description current")

    assert (
        f"next: YOU ARE NOT DONE. Run spice task next; {KEEP_DRAINING}" in review_output
    )


def test_steer_task_done_and_review_outputs_make_continuation_explicit(
    task_repo, monkeypatch
):
    assert task_repo.is_dir()
    handle = create.add(
        "Exercise steer task guidance",
        project="task.guidance",
        priority="medium",
        acceptance=["steer guidance is explicit-direction only"],
    )
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.guidance"], "lifetime": "Steer"},
    )
    ops.claim(handle)

    done_output = ops.done(handle, validation=["steer guidance checked"])

    assert "YOU ARE NOT DONE" not in done_output
    assert KEEP_DRAINING not in done_output
    assert "next: review assignment pending" in done_output
    assert STEER_EXPLICIT_DIRECTION in done_output
    assert TASK_CAPTURE_IMMEDIATE in done_output
    assert TASK_CAPTURE_ACK_EXAMPLE in done_output
    assert TASK_CAPTURE_LINE_EXAMPLE in done_output
    assert TASK_CAPTURE_NOT_ALLOCATOR in done_output
    assert STEER_MANUAL_CLAIM in done_output
    assert "self-review only if next assigns it" in done_output
    shown = render.render_show(handle)
    assert "YOU ARE NOT DONE" not in shown
    assert STEER_EXPLICIT_DIRECTION in shown
    assert TASK_CAPTURE_IMMEDIATE in shown
    assert TASK_CAPTURE_ACK_EXAMPLE in shown
    assert TASK_CAPTURE_LINE_EXAMPLE in shown
    assert TASK_CAPTURE_NOT_ALLOCATOR in shown
    assert STEER_MANUAL_CLAIM in shown

    assigned = alloc.next_task()
    assert identity.render_handle(assigned or {}) == handle

    review_output = ops.review(handle, finding="clean", note="description current")

    assert "YOU ARE NOT DONE" not in review_output
    assert KEEP_DRAINING not in review_output
    assert "next: phase boundary reached" in review_output
    assert STEER_EXPLICIT_DIRECTION in review_output
    assert TASK_CAPTURE_IMMEDIATE in review_output
    assert TASK_CAPTURE_ACK_EXAMPLE in review_output
    assert TASK_CAPTURE_LINE_EXAMPLE in review_output
    assert TASK_CAPTURE_NOT_ALLOCATOR in review_output
    assert STEER_MANUAL_CLAIM in review_output


def test_task_claim_outputs_drive_to_completion_guidance(task_repo):
    assert task_repo.is_dir()
    handle = create.add(
        "Exercise task claim guidance",
        project="task.guidance",
        priority="medium",
        acceptance=["claim guidance is explicit"],
    )

    claim_output = ops.claim(handle)

    assert ops.claim_drive_line(handle) in claim_output


def _gain_runner(
    total_commands: float,
    avg_savings_pct: float,
    calls: list[list[str]] | None = None,
):
    import json as _json

    payload = _json.dumps(
        {
            "summary": {
                "total_commands": total_commands,
                "avg_savings_pct": avg_savings_pct,
            }
        }
    )

    def runner(cmd, **kwargs):
        if calls is not None:
            calls.append(list(cmd))
        return subprocess.CompletedProcess(cmd, 0, stdout=payload, stderr="")

    return runner


def test_rtk_usage_nudge_fires_when_savings_are_poor(monkeypatch):
    monkeypatch.setattr(
        ops.subprocess, "run", _gain_runner(ops.RTK_NUDGE_MIN_COMMANDS + 4, 2.0)
    )

    nudge = ops.rtk_usage_nudge()

    assert nudge is not None
    assert nudge.startswith("rtk:")
    assert "discretely" in nudge


def test_rtk_usage_nudge_reads_current_project_gain(monkeypatch):
    calls: list[list[str]] = []
    monkeypatch.setattr(
        ops.subprocess,
        "run",
        _gain_runner(ops.RTK_NUDGE_MIN_COMMANDS + 4, 2.0, calls),
    )

    ops.rtk_usage_nudge()

    assert calls == [["rtk", "gain", "--project", "-f", "json"]]


def test_rtk_usage_nudge_stays_silent_when_feeding_rtk_well(monkeypatch):
    monkeypatch.setattr(
        ops.subprocess,
        "run",
        _gain_runner(
            ops.RTK_NUDGE_MIN_COMMANDS + 4, ops.RTK_NUDGE_SAVINGS_FLOOR_PCT + 10.0
        ),
    )

    assert ops.rtk_usage_nudge() is None


def test_task_next_output_drives_allocated_task_to_completion(task_repo, monkeypatch):
    assert task_repo.is_dir()
    next_handle = create.add(
        "Exercise next allocation guidance",
        project="task.guidance",
        priority="medium",
        acceptance=["next allocation guidance is explicit"],
    )
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.guidance"], "lifetime": "Drive"},
    )

    next_output = render.render_next()

    assert ops.claim_drive_line(next_handle) in next_output


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-b", "main")
    _run(path, "git", "config", "user.email", "spice@example.test")
    _run(path, "git", "config", "user.name", "Spice Tests")
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "initial")
    return path


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
