"""Inline TASK creation from assistant ACK messages."""

from __future__ import annotations

import io
import json
import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent import sidechannelnotify, watchdog
from spice.agent.driver import CLAUDE_DRIVER, DRIVER
from spice.mail.feedback import supervisor_feedback_line
from spice.mail.inbox import (
    collect_acked_inbox_items,
    collect_inbox_items,
    compose_inbox_text,
    write_inbox_item,
)
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.store import (
    TASK_FILTER_SOURCE_AUTO_CREATE,
    ServeTeamStore,
    TeamConfig,
)
from spice.tasks import alloc, config, identity, tw

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ACTOR_MEMBER = thread_actor_id(ACTOR)
INBOX_KEY = "20260104T000000000004Z"


def _allowed_project_stems() -> list[str]:
    return list(config.assignable_stems())


def _ack_feedback(kind: str, *keys: str) -> str:
    return supervisor_feedback_line(kind, keys=list(keys))


def _task_created_feedback(handle: str, project: str, route_feedback: str) -> str:
    return supervisor_feedback_line(
        "task.created",
        handles=[handle],
        projects=[project],
        routes=[route_feedback],
        **{"allowed-project-stems": _allowed_project_stems()},
    )


def _task_backlog_note_feedback() -> str:
    return supervisor_feedback_line(
        "task.backlog-note",
        message=watchdog.INLINE_TASK_BACKLOG_NOTE,
    )


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
    store = ServeTeamStore()
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
    assert [item.name for item in collect_acked_inbox_items(task_repo)] == [
        f"{INBOX_KEY}.txt"
    ]
    assert len(rows) == 1
    assert rows[0]["description"] == "Inline follow-up"
    assert rows[0]["project"] == "task.unit"
    assert rows[0]["acceptance"] == "Inline task exists"
    assert rows[0]["origin_thread"] == ACTOR
    assert rows[0][config.TASK_CREATION_SURFACE_UDA] == config.TASK_CREATION_SURFACE_CLI
    handle = identity.render_handle(rows[0])
    assert handle in log.getvalue()
    assert "route_filter=skipped:task.unit:no_team" in log.getvalue()
    feedback = sidechannelnotify.consume_side_channel_notices(task_repo)
    assert feedback == [
        _ack_feedback("ack.archived", INBOX_KEY),
        _task_created_feedback(
            handle,
            "task.unit",
            "route_filter=skipped:task.unit:no_team",
        ),
        _task_backlog_note_feedback(),
    ]
    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert store.current_team_for_agent(ACTOR) is None
    assert sidechannelnotify.consume_side_channel_notices(task_repo) == []


def test_claude_stdout_scanner_archives_ack_and_task_after_thinking_block(
    task_repo, quiet_supervisor
):
    write_inbox_item(
        task_repo,
        f"{INBOX_KEY}.txt",
        compose_inbox_text(body="capture this", priority=None, stop=False),
    )
    log = io.StringIO()
    gate = watchdog.MaximReminderGate()
    scanner = watchdog.JsonStdoutScanner(
        lambda text: watchdog.process_supervised_assistant_message(
            task_repo, text, log, gate
        ),
        CLAUDE_DRIVER.normalize_transcript_line,
        on_compaction=gate.note_compaction,
    )

    scanner.process_line(
        json.dumps(
            {
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "checking steering"},
                        {
                            "type": "text",
                            "text": (
                                f"ACK {INBOX_KEY}: captured.\n"
                                "TASK title=Claude follow-up | project=task.unit | "
                                "acceptance=Text after thinking still processes"
                            ),
                        },
                    ],
                },
            }
        )
    )
    scanner.close()

    rows = tw.export(["status:pending"])
    assert collect_inbox_items(task_repo) == []
    assert [item.name for item in collect_acked_inbox_items(task_repo)] == [
        f"{INBOX_KEY}.txt"
    ]
    assert len(rows) == 1
    assert rows[0]["description"] == "Claude follow-up"
    assert rows[0]["project"] == "task.unit"
    assert rows[0]["acceptance"] == "Text after thinking still processes"
    handle = identity.render_handle(rows[0])
    feedback = sidechannelnotify.consume_side_channel_notices(task_repo)
    assert feedback == [
        _ack_feedback("ack.archived", INBOX_KEY),
        _task_created_feedback(
            handle,
            "task.unit",
            "route_filter=skipped:task.unit:no_team",
        ),
        _task_backlog_note_feedback(),
    ]


def test_supervised_ack_reports_unmatched_keys(task_repo, quiet_supervisor):
    missing_key = "20260104T000000000099Z"
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        f"ACK {missing_key}: nothing pending under this key.",
        log,
        watchdog.MaximReminderGate(),
    )

    assert collect_acked_inbox_items(task_repo) == []
    feedback = sidechannelnotify.consume_side_channel_notices(task_repo)
    assert feedback == [_ack_feedback("ack.unmatched", missing_key)]


def test_supervised_ack_reports_noop_when_no_key_is_named(task_repo, quiet_supervisor):
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        "ACK: I saw it.",
        log,
        watchdog.MaximReminderGate(),
    )

    feedback = sidechannelnotify.consume_side_channel_notices(task_repo)
    assert feedback == [
        supervisor_feedback_line("ack.noop", message=watchdog.ACK_NOOP_MESSAGE)
    ]


def test_supervised_marker_examples_do_not_emit_feedback_or_tasks(
    task_repo, quiet_supervisor
):
    missing_key = "20260104T000000000099Z"
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        (
            "Example output:\n"
            "```text\n"
            f"ACK {missing_key}: fenced example.\n"
            "ACK: no-key fenced example.\n"
            "TASK title=Fenced | project=task.unit | acceptance=Should not create\n"
            "```\n"
            f"> ACK {missing_key}: quoted example.\n"
            f"docs/design/example.md:137:ACK {missing_key}: rendered source output.\n"
            "    TASK title=Indented | project=task.unit | acceptance=Should not create"
        ),
        log,
        watchdog.MaximReminderGate(),
    )

    assert collect_acked_inbox_items(task_repo) == []
    assert tw.export(["status:pending"]) == []
    assert sidechannelnotify.consume_side_channel_notices(task_repo) == []


def test_supervised_ack_reports_already_acked_keys(task_repo, quiet_supervisor):
    write_inbox_item(
        task_repo,
        f"{INBOX_KEY}.txt",
        compose_inbox_text(body="capture this", priority=None, stop=False),
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        f"ACK {INBOX_KEY}: first.",
        log,
        watchdog.MaximReminderGate(),
    )
    sidechannelnotify.consume_side_channel_notices(task_repo)
    watchdog.process_supervised_assistant_message(
        task_repo,
        f"ACK {INBOX_KEY}: repeated.",
        log,
        watchdog.MaximReminderGate(),
    )

    feedback = sidechannelnotify.consume_side_channel_notices(task_repo)
    assert feedback == [_ack_feedback("ack.already-acked", INBOX_KEY)]


def test_supervised_standalone_task_directive_creates_task(task_repo, quiet_supervisor):
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_MEMBER], config=TeamConfig(lifetime="Drive")
    )
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
    assert rows[0][config.TASK_CREATION_SURFACE_UDA] == config.TASK_CREATION_SURFACE_CLI
    handle = identity.render_handle(rows[0])
    assert handle in log.getvalue()
    assert "route_filter=added:task.unit:auto:create" in log.getvalue()
    team_config = store.team_config(team.team_id)
    assert team_config.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in team_config.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE}
    ]
    assigned = alloc.next_task()
    assert identity.render_handle(assigned or {}) == handle
    feedback = sidechannelnotify.consume_side_channel_notices(task_repo)
    assert feedback == [
        _task_created_feedback(
            handle,
            "task.unit",
            "route_filter=added:task.unit:auto:create",
        ),
        _task_backlog_note_feedback(),
    ]
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
    assert feedback[0] == supervisor_feedback_line(
        "task.error",
        error=(
            "batch add rejected: line 2: project 'task' has depth 1; public task "
            "projects require at least 2 dotted segments, such as task.example"
        ),
        **{"allowed-project-stems": _allowed_project_stems()},
    )
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
