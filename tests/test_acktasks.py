"""Inline TASK creation from assistant ACK messages."""

from __future__ import annotations

import io
import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent import sidechannelnotify, watchdog
from spice.agent.driver import DRIVER
from spice.mail.inbox import (
    collect_archived_inbox_items,
    collect_inbox_items,
    compose_inbox_text,
    write_inbox_item,
)
from spice.tasks import config, identity, tw

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
INBOX_KEY = "20260104T000000000004Z"


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-acktasks")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


@pytest.fixture
def quiet_supervisor(monkeypatch):
    monkeypatch.setattr(watchdog, "record_supervised_lane_metrics", lambda _repo: None)
    monkeypatch.setattr(
        watchdog,
        "publish_maxim_hits_as_inbox",
        lambda _repo, _text, **_kwargs: [],
    )


def test_supervised_ack_creates_inline_task_and_archives_inbox(
    task_repo, quiet_supervisor
):
    write_inbox_item(
        task_repo,
        f"{INBOX_KEY}.txt",
        compose_inbox_text(body="capture this", priority=None, stop=False),
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        (
            f"ACK {INBOX_KEY}: captured.\n"
            "TASK title=Inline follow-up | project=task.unit | "
            "acceptance=Inline task exists"
        ),
        log,
        watchdog.MaximReminderGate(),
    )

    rows = tw.export(["status:pending"])
    assert collect_inbox_items(task_repo) == []
    assert [item.name for item in collect_archived_inbox_items(task_repo)] == [
        f"{INBOX_KEY}.txt"
    ]
    assert len(rows) == 1
    assert rows[0]["description"] == "Inline follow-up"
    assert rows[0]["project"] == "task.unit"
    assert rows[0]["acceptance"] == "Inline task exists"
    handle = identity.render_handle(rows[0])
    assert handle in log.getvalue()
    feedback = sidechannelnotify.consume_side_channel_notices(task_repo)
    assert feedback == [
        f"ack_archived={INBOX_KEY}",
        f"inline_task_created={handle}",
    ]
    assert sidechannelnotify.consume_side_channel_notices(task_repo) == []


def test_supervised_standalone_task_directive_creates_task(task_repo, quiet_supervisor):
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        (
            "Queued the follow-up.\n"
            "TASK title=Standalone follow-up | project=task.unit | "
            "acceptance=Standalone task exists"
        ),
        log,
        watchdog.MaximReminderGate(),
    )

    rows = tw.export(["status:pending"])
    assert len(rows) == 1
    assert rows[0]["description"] == "Standalone follow-up"
    assert rows[0]["project"] == "task.unit"
    assert rows[0]["acceptance"] == "Standalone task exists"
    handle = identity.render_handle(rows[0])
    assert handle in log.getvalue()
    feedback = sidechannelnotify.consume_side_channel_notices(task_repo)
    assert feedback == [f"inline_task_created={handle}"]
    assert sidechannelnotify.consume_side_channel_notices(task_repo) == []


def test_supervised_standalone_task_batch_rejects_without_partial_creation(
    task_repo, quiet_supervisor
):
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        (
            "TASK title=Would otherwise create | project=task.unit | acceptance=ok\n"
            "TASK title=Invalid project depth | project=task | acceptance=bad"
        ),
        log,
        watchdog.MaximReminderGate(),
    )

    assert tw.export(["status:pending"]) == []
    assert "spice inline task supervisor error: batch add rejected" in log.getvalue()
    feedback = sidechannelnotify.consume_side_channel_notices(task_repo)
    assert len(feedback) == 1
    assert "inline_task_error=batch add rejected" in feedback[0]
    assert sidechannelnotify.consume_side_channel_notices(task_repo) == []


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
