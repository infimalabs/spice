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
from spice.mail.ackstate import ACK_DISPOSITION_REFUSED, ack_state_records
from spice.mail.feedback import supervisor_feedback_line
from spice.mail.inbox import (
    collect_acked_inbox_items,
    collect_refused_inbox_items,
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
from spice.tasks import alloc, config, identity, ops, tw

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


def test_supervised_ack_missing_acceptance_routes_inline_task_to_plan(
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
            "TASK title=Inline plan follow-up | project=task.unit"
        ),
        log,
        watchdog.MaximReminderGate(),
    )

    rows = tw.export(["status:pending"])
    assert collect_inbox_items(task_repo) == []
    assert len(rows) == 1
    assert rows[0]["description"] == "Inline plan follow-up"
    assert rows[0]["project"] == "task.unit"
    assert rows[0]["phase"] == "plan"
    assert ops.phases_of(rows[0]) == ["plan", "todo", "review"]
    assert not str(rows[0].get("acceptance") or "")
    assert rows[0]["origin"] == f"ack:{INBOX_KEY}"
    assert rows[0][config.TASK_CREATION_SURFACE_UDA] == config.TASK_CREATION_SURFACE_CLI


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
            f"docs/design/experimental/example.md:137:ACK {missing_key}: rendered source output.\n"
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


def test_supervised_nack_records_refused_ackstate(task_repo, quiet_supervisor):
    inbox_name = f"{INBOX_KEY}.txt"
    inbox_text = compose_inbox_text(
        body="decline this steering", priority="urgent", stop=False
    )
    write_inbox_item(task_repo, inbox_name, inbox_text)
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        f"NACK {INBOX_KEY}: refusing because this conflicts with policy.",
        log,
        watchdog.MaximReminderGate(),
    )

    refused = collect_refused_inbox_items(task_repo)
    records = ack_state_records(task_repo)
    feedback = sidechannelnotify.consume_side_channel_notices(task_repo)
    assert [(item.name, item.text, item.disposition) for item in refused] == [
        (inbox_name, inbox_text, ACK_DISPOSITION_REFUSED)
    ]
    assert [
        (record.key, record.inbox_name, record.ack_content, record.disposition)
        for record in records
    ] == [
        (
            INBOX_KEY,
            inbox_name,
            "refusing because this conflicts with policy.",
            ACK_DISPOSITION_REFUSED,
        )
    ]
    assert feedback == [_ack_feedback("nack.refused", INBOX_KEY)]


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
            f"acceptance=Standalone task exists | origin=ack:{INBOX_KEY}"
        ),
        log,
        watchdog.MaximReminderGate(),
    )

    rows = tw.export(["status:pending"])
    assert len(rows) == 1
    assert rows[0]["description"] == "Standalone follow-up"
    assert rows[0]["project"] == "task.unit"
    assert rows[0]["acceptance"] == "Standalone task exists"
    assert rows[0]["origin"] == f"ack:{INBOX_KEY}"
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


def test_supervised_standalone_task_without_origin_is_refused(
    task_repo, quiet_supervisor
):
    """A TASK directive with no same-message ACK, no origin= field, and no
    active claim has no provenance and must be refused with guidance."""
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        (
            "Queued the follow-up.\n"
            "TASK title=Rootless follow-up | project=task.unit | acceptance=nope"
        ),
        log,
        watchdog.MaximReminderGate(),
    )

    assert tw.export(["status:pending"]) == []
    assert "task creation requires an origin" in log.getvalue()


def test_supervised_ack_message_task_inherits_ack_origin(task_repo, quiet_supervisor):
    write_inbox_item(
        task_repo,
        f"{INBOX_KEY}.txt",
        compose_inbox_text(body="capture this", priority=None, stop=False),
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        (
            f"ACK {INBOX_KEY}: captured the request.\n"
            "TASK title=Captured from steering | project=task.unit | "
            "acceptance=Origin inherited from the ack"
        ),
        log,
        watchdog.MaximReminderGate(),
    )

    rows = tw.export(["status:pending"])
    assert len(rows) == 1
    assert rows[0]["origin"] == f"ack:{INBOX_KEY}"


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


def _annotations(row) -> list[str]:
    return [str(item.get("description") or "") for item in row.get("annotations") or []]


def _claimed_task(title: str) -> str:
    from spice.tasks import create, ops

    handle = create.add(
        title,
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["steering lands on the active task"],
    )
    ops.claim(handle)
    return handle


def test_retired_ack_annotates_active_task_with_key_and_response(
    task_repo, quiet_supervisor
):
    handle = _claimed_task("Active work amended by steering")
    write_inbox_item(
        task_repo,
        f"{INBOX_KEY}.txt",
        compose_inbox_text(
            body="tighten the acceptance to cover retries",
            priority=None,
            stop=False,
        ),
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        f"ACK {INBOX_KEY}: acceptance now covers retry storms.",
        log,
        watchdog.MaximReminderGate(),
    )

    row = identity.resolve(handle)
    assert collect_inbox_items(task_repo) == []
    assert f"ack {INBOX_KEY}: acceptance now covers retry storms." in _annotations(row)


def test_bare_ack_annotation_falls_back_to_steering_body(task_repo, quiet_supervisor):
    handle = _claimed_task("Active work amended by bare ack")
    write_inbox_item(
        task_repo,
        f"{INBOX_KEY}.txt",
        compose_inbox_text(
            body="fold the edge case into the description",
            priority=None,
            stop=False,
        ),
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        f"ACK {INBOX_KEY}",
        log,
        watchdog.MaximReminderGate(),
    )

    row = identity.resolve(handle)
    notes = _annotations(row)
    assert any(
        note.startswith(f"ack {INBOX_KEY}: ")
        and "fold the edge case into the description" in note
        for note in notes
    ), notes


def test_review_feedback_ack_never_mirrors_to_active_task(task_repo, quiet_supervisor):
    """The annotation mirror captures operator steering only; review facts
    already live on the task via review_* UDAs and annotations."""
    handle = _claimed_task("Active work during review ack")
    write_inbox_item(
        task_repo,
        f"{INBOX_KEY}.txt",
        # The emitted form: [REVIEW] body prefix, no Priority: header.
        "[REVIEW] Peer feedback for TASK-1: \n\n> tighten the tests\n",
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        f"ACK {INBOX_KEY}: review feedback absorbed.",
        log,
        watchdog.MaximReminderGate(),
    )

    row = identity.resolve(handle)
    assert collect_inbox_items(task_repo) == []
    assert [item.name for item in collect_acked_inbox_items(task_repo)] == [
        f"{INBOX_KEY}.txt"
    ]
    assert all(not note.startswith("ack ") for note in _annotations(row))


def test_maxim_reminder_ack_never_mirrors_to_active_task(task_repo, quiet_supervisor):
    handle = _claimed_task("Active work during maxim ack")
    write_inbox_item(
        task_repo,
        f"{INBOX_KEY}.txt",
        "[MAXIM] prefer one obvious seam over two clever ones\n",
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        f"ACK {INBOX_KEY}: maxim noted.",
        log,
        watchdog.MaximReminderGate(),
    )

    row = identity.resolve(handle)
    assert collect_inbox_items(task_repo) == []
    assert all(not note.startswith("ack ") for note in _annotations(row))


def test_retired_ack_without_active_claim_skips_annotation(task_repo, quiet_supervisor):
    write_inbox_item(
        task_repo,
        f"{INBOX_KEY}.txt",
        compose_inbox_text(body="steering with no claim", priority=None, stop=False),
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        f"ACK {INBOX_KEY}: noted.",
        log,
        watchdog.MaximReminderGate(),
    )

    assert collect_inbox_items(task_repo) == []
    assert [item.name for item in collect_acked_inbox_items(task_repo)] == [
        f"{INBOX_KEY}.txt"
    ]
    assert "spice ack annotate: no active claim" in log.getvalue()


def test_ack_annotation_failure_never_blocks_retirement(
    task_repo, quiet_supervisor, monkeypatch
):
    from spice.tasks import ops

    _claimed_task("Active work with failing annotate")
    write_inbox_item(
        task_repo,
        f"{INBOX_KEY}.txt",
        compose_inbox_text(body="steering survives failure", priority=None, stop=False),
    )
    monkeypatch.setattr(
        ops, "annotate", lambda *_a, **_k: (_ for _ in ()).throw(RuntimeError("boom"))
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        task_repo,
        f"ACK {INBOX_KEY}: noted.",
        log,
        watchdog.MaximReminderGate(),
    )

    assert collect_inbox_items(task_repo) == []
    assert [item.name for item in collect_acked_inbox_items(task_repo)] == [
        f"{INBOX_KEY}.txt"
    ]
    assert "spice ack annotate supervisor error: boom" in log.getvalue()


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
