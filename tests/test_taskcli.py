"""Task CLI parser and list ergonomics."""

from __future__ import annotations

import argparse

import pytest

from spice.cli.parser import build_parser
from spice.tasks import cli as task_cli, render


def test_task_list_help_shows_limit_filters_and_examples(capsys):
    parser = build_parser()

    with pytest.raises(SystemExit):
        parser.parse_args(["task", "list", "--help"])

    help_text = capsys.readouterr().out
    assert "--limit N" in help_text
    assert "--project PROJECT" in help_text
    assert "--status {pending,waiting,completed,deleted}" in help_text
    assert "spice task list --limit 20" in help_text
    assert "spice task list --project serve.ui --status pending --limit 20" in help_text


def test_task_list_parse_error_points_to_limit_example(capsys):
    parser = build_parser()

    with pytest.raises(SystemExit) as exc_info:
        parser.parse_args(["task", "list", "--limt", "20"])

    error = capsys.readouterr().err
    assert exc_info.value.code == 2
    assert "Try `spice task list --help` for the exact contract." in error
    assert "spice task list --limit 20" in error


def test_task_list_limit_filters_project_stem_and_sorts_newest(monkeypatch):
    rows = [
        _row(
            "Serve UI oldest",
            project="serve.ui",
            incepted="20260612T000000000001Z",
        ),
        _row(
            "Task newest ignored",
            project="task.cli",
            incepted="20260612T000000000004Z",
        ),
        _row(
            "Serve API newest",
            project="serve.api",
            incepted="20260612T000000000003Z",
        ),
        _row(
            "Serve UI middle",
            project="serve.ui",
            incepted="20260612T000000000002Z",
        ),
    ]
    seen: dict[str, object] = {}

    def fake_visible_rows(actor: str, filters: list[str]) -> list[dict[str, object]]:
        seen["actor"] = actor
        seen["filters"] = filters
        return rows

    monkeypatch.setattr("spice.tasks.tw.current_actor", lambda: "actor-a")
    monkeypatch.setattr(task_cli.ops, "visible_rows", fake_visible_rows)

    output = task_cli._list(
        argparse.Namespace(all=False, status=None, project="serve", limit=2)
    )

    assert seen == {"actor": "actor-a", "filters": ["status:pending"]}
    lines = output.splitlines()
    assert "Serve API newest" in lines[0]
    assert "Serve UI middle" in lines[1]
    assert "Task newest ignored" not in output
    assert "Serve UI oldest" not in output


def test_task_list_status_filter_uses_visible_rows(monkeypatch):
    seen: dict[str, object] = {}

    def fake_visible_rows(actor: str, filters: list[str]) -> list[dict[str, object]]:
        seen["actor"] = actor
        seen["filters"] = filters
        return [
            _row(
                "Waiting task",
                project="task.cli",
                status="waiting",
                incepted="20260612T000000000001Z",
            )
        ]

    monkeypatch.setattr("spice.tasks.tw.current_actor", lambda: "actor-a")
    monkeypatch.setattr(task_cli.ops, "visible_rows", fake_visible_rows)

    output = task_cli._list(
        argparse.Namespace(all=False, status="waiting", project=None, limit=None)
    )

    assert seen == {"actor": "actor-a", "filters": ["status:waiting"]}
    assert "Waiting task" in output


def test_task_list_all_marks_completed_and_deleted_rows(monkeypatch):
    rows = [
        _row(
            "Live task",
            project="task.render",
            incepted="20260612T000000000001Z",
            status="pending",
            phase="todo",
        ),
        _row(
            "Completed task",
            project="task.render",
            incepted="20260612T000000000002Z",
            status="completed",
            phase="review",
        ),
        _row(
            "Deleted task",
            project="task.render",
            incepted="20260612T000000000003Z",
            status="deleted",
            phase="todo",
        ),
    ]
    seen: dict[str, object] = {}

    def fake_export(filters: list[str] | None = None) -> list[dict[str, object]]:
        seen["filters"] = filters
        return rows

    monkeypatch.setattr("spice.tasks.tw.export", fake_export)

    output = task_cli._list(
        argparse.Namespace(all=True, status=None, project=None, limit=None)
    )
    live_line = next(line for line in output.splitlines() if "Live task" in line)
    completed_line = next(
        line for line in output.splitlines() if "Completed task" in line
    )
    deleted_line = next(line for line in output.splitlines() if "Deleted task" in line)

    assert seen == {"filters": []}
    assert "[todo]" in live_line
    assert "[done]" in completed_line
    assert "[review]" not in completed_line
    assert "[deleted]" in deleted_line
    assert "[todo]" not in deleted_line


def test_task_show_replaces_sentinel_rehydrate_commands(monkeypatch):
    sentinel = "0" * 32
    row = _row(
        "Sentinel task",
        project="task.render",
        incepted="20260612T065825463453Z",
        status="pending",
        phase="todo",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "0",
            "urgency": "9.2",
            "origin_thread": sentinel,
            "origin_worktree": "/tmp/origin",
            "claim_thread": sentinel,
            "claim_worktree": "/tmp/claim",
            "claim_context_start": "2026-06-12T07:15:18.621994Z",
            "claim_context_end": "2026-06-12T07:25:18.621994Z",
            "claim_context_turn": "turn-a",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.ops, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test")

    assert (
        "origin_rehydrate - "
        "(no origin session exists; sentinel thread has no transcript)" in output
    )
    assert (
        "claim_rehydrate - "
        "(no claim session exists; sentinel thread has no transcript)" in output
    )
    assert f"spice session briefing {sentinel}" not in output
    assert f"spice session turns {sentinel}" not in output


def test_task_show_resolves_relative_archived_attachments_to_origin(monkeypatch):
    first_ref = ".spice/inbox/archive/20260102T000000000004Z.attachments/01-image.png"
    second_ref = ".spice/inbox/archive/20260102T000000000004Z.attachments/02-image.png"
    third_ref = ".spice/inbox/archive/20260102T000000000004Z.attachments/03-image.png"
    absolute_ref = (
        "/tmp/origin/.spice/inbox/archive/"
        "20260102T000000000004Z.attachments/04-image.png"
    )
    row = _row(
        "Portable attachments",
        project="task.render",
        incepted="20260612T065825463453Z",
        status="pending",
        phase="todo",
    )
    row.update(
        {
            "task_description": f"Inspect {first_ref}. Already absolute {absolute_ref}",
            "acceptance": f"Open {second_ref};",
            "phase_i": "0",
            "urgency": "9.2",
            "origin_thread": "origin-thread",
            "origin_worktree": "/tmp/origin",
            "claim_thread": "claim-thread",
            "claim_worktree": "/tmp/claim",
            "annotations": [
                {"description": f"note: {third_ref}:"},
                {"description": f"duplicate: {first_ref}"},
            ],
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.ops, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test")

    assert "origin_attachments:" in output
    assert f"  {first_ref} -> /tmp/origin/{first_ref}" in output
    assert f"  {second_ref} -> /tmp/origin/{second_ref}" in output
    assert f"  {third_ref} -> /tmp/origin/{third_ref}" in output
    assert f"{absolute_ref} ->" not in output
    assert output.count(f"{first_ref} ->") == 1


def _row(
    description: str,
    *,
    project: str,
    incepted: str,
    status: str = "pending",
    phase: str = "todo",
) -> dict[str, object]:
    return {
        "description": description,
        "project": project,
        "status": status,
        "phase": phase,
        "priority": "M",
        "incepted": incepted,
        "entry": incepted,
    }
