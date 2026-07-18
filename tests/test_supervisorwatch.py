"""Supervisor lane watch: nudge when the bound agent is dirty but unclaimed."""

import subprocess

from spice.agent import lifecycle
from spice.agent import watchdog
from spice.process import git as processgit
from spice.tasks import claimstate


def _init_git_repo(path):
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)


def _capture_feedback(monkeypatch):
    calls = []
    monkeypatch.setattr(
        watchdog, "publish_supervisor_feedback", lambda *a, **k: calls.append((a, k))
    )
    return calls


def test_flag_uncaptured_lane_nudges_when_dirty_and_unclaimed(tmp_path, monkeypatch):
    _init_git_repo(tmp_path)
    (tmp_path / "dirty.txt").write_text("uncommitted", encoding="utf-8")
    monkeypatch.setattr(claimstate, "active_claim", lambda _actor: None)
    calls = _capture_feedback(monkeypatch)

    lifecycle._flag_uncaptured_lane(tmp_path, "thread-x", tmp_path / "log.txt")

    assert len(calls) == 1
    assert calls[0][0][2] == "lane.uncaptured"
    assert calls[0][1]["message"] == lifecycle.LANE_UNCAPTURED_NUDGE


def test_flag_uncaptured_lane_silent_when_a_task_is_claimed(tmp_path, monkeypatch):
    _init_git_repo(tmp_path)
    (tmp_path / "dirty.txt").write_text("uncommitted", encoding="utf-8")
    monkeypatch.setattr(claimstate, "active_claim", lambda _actor: {"uuid": "held"})
    calls = _capture_feedback(monkeypatch)

    lifecycle._flag_uncaptured_lane(tmp_path, "thread-x", tmp_path / "log.txt")

    assert calls == []


def test_flag_uncaptured_lane_silent_when_tree_is_clean(tmp_path, monkeypatch):
    _init_git_repo(tmp_path)
    monkeypatch.setattr(claimstate, "active_claim", lambda _actor: None)
    calls = _capture_feedback(monkeypatch)

    lifecycle._flag_uncaptured_lane(tmp_path, "thread-x", tmp_path / "log.txt")

    assert calls == []


def test_flag_uncaptured_lane_completes_when_git_cannot_launch(tmp_path, monkeypatch):
    events = []

    def unavailable_git(_command, *, timeout_seconds, **_kwargs):
        events.append(f"probe-budget:{timeout_seconds:g}")
        raise FileNotFoundError("git unavailable")

    monkeypatch.setattr(processgit, "run_bounded_process_group", unavailable_git)
    monkeypatch.setattr(claimstate, "active_claim", lambda _actor: None)
    monkeypatch.setattr(
        watchdog,
        "publish_supervisor_feedback",
        lambda *_args, **_kwargs: events.append("feedback-published"),
    )

    lifecycle._flag_uncaptured_lane(tmp_path, "thread-x", tmp_path / "log.txt")
    events.append("supervisor-returned")

    assert events == [
        f"probe-budget:{processgit.GIT_PROBE_TIMEOUT_SECONDS:g}",
        "supervisor-returned",
    ]
