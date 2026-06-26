"""Task CLI parser and list ergonomics."""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
from pathlib import Path

import pytest

from spice.cli.parser import build_parser
from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.tasks import (
    artifacts,
    cli as task_cli,
    config,
    create,
    identity,
    ops,
    render,
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    if shutil.which("task") is None:
        pytest.skip("Taskwarrior binary is required")
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-a")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


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


def test_task_add_after_leaves_trailing_title_positional():
    args = build_parser().parse_args(
        [
            "task",
            "add",
            "--after",
            "TASK-20260101T000000000001Z",
            "Follow-up title",
            "--project",
            "task.unit",
        ]
    )

    assert args.after == ["TASK-20260101T000000000001Z"]
    assert args.title == "Follow-up title"


def test_task_add_after_repeats_for_multiple_dependencies():
    args = build_parser().parse_args(
        [
            "task",
            "add",
            "--after",
            "TASK-20260101T000000000001Z",
            "--after",
            "TASK-20260101T000000000002Z",
            "Follow-up title",
        ]
    )

    assert args.after == [
        "TASK-20260101T000000000001Z",
        "TASK-20260101T000000000002Z",
    ]
    assert args.title == "Follow-up title"


def test_task_wake_parser_accepts_multiple_handles():
    args = build_parser().parse_args(
        [
            "task",
            "wake",
            "TASK-20260101T000000000001Z",
            "TASK-20260101T000000000002Z",
        ]
    )

    assert args.task_action == "wake"
    assert args.handles == [
        "TASK-20260101T000000000001Z",
        "TASK-20260101T000000000002Z",
    ]


def test_task_wake_parser_rejects_claim_flag():
    with pytest.raises(SystemExit) as exc_info:
        build_parser().parse_args(
            ["task", "wake", "TASK-20260101T000000000001Z", "--claim"]
        )

    assert exc_info.value.code == 2


def test_task_add_title_flag_is_alias_for_positional(task_repo, capsys):
    args = build_parser().parse_args(
        ["task", "add", "--title", "Alias title lands", "--project", "task.unit"]
    )

    assert args.func(args) == 0
    created = capsys.readouterr().out.split()[1]
    row = identity.resolve(created)

    assert row["description"] == "Alias title lands"
    assert row[config.TASK_CREATION_SURFACE_UDA] == config.TASK_CREATION_SURFACE_CLI


def test_task_add_deferred_flag_creates_waiting_task(task_repo, capsys):
    args = build_parser().parse_args(
        ["task", "add", "Deferred CLI task", "--project", "task.unit", "--deferred"]
    )

    assert args.func(args) == 0
    created = capsys.readouterr().out.split()[1]
    row = identity.resolve(created)

    assert row["description"] == "Deferred CLI task"
    assert str(row.get("wait") or "").startswith("2099")
    assert row[config.TASK_CREATION_SURFACE_UDA] == config.TASK_CREATION_SURFACE_CLI


def test_task_review_then_marks_spawned_followup_as_cli_creation_surface(
    task_repo, capsys
):
    assert task_repo.is_dir()
    handle = create.add(
        "Review target for CLI follow-up",
        project="task.unit",
        priority="medium",
        flow=["review"],
        acceptance=["review starts directly for CLI coverage"],
        claim=True,
    )
    args = build_parser().parse_args(
        [
            "task",
            "review",
            handle,
            "--finding",
            "changes",
            "--note",
            "description current; needs follow-up",
            "--then",
            "title=CLI spawned follow-up | project=task.unit | "
            "acceptance=Spawned review follow-up can render as a task card",
        ]
    )
    args.backend = str(config.backend_root())

    assert args.func(args) == 0
    out = capsys.readouterr().out
    spawned = re.search(r"spawned (\S+)", out).group(1)
    row = identity.resolve(spawned)

    assert row["description"] == "CLI spawned follow-up"
    assert row[config.TASK_CREATION_SURFACE_UDA] == config.TASK_CREATION_SURFACE_CLI


def test_task_add_takes_exactly_one_title_form(task_repo):
    args = build_parser().parse_args(
        ["task", "add", "Positional title", "--title", "Flag title"]
    )

    with pytest.raises(SpiceError, match="positional title or --title"):
        args.func(args)


def test_task_oops_description_records_triage_context(task_repo, capsys):
    args = build_parser().parse_args(
        [
            "task",
            "oops",
            "wrapper",
            "hiccup",
            "--description",
            "Longer triage context for the board.",
        ]
    )

    assert args.func(args) == 0
    out = capsys.readouterr().out
    created = re.search(r"OOPS-\S+", out).group(0)
    row = identity.resolve(created)

    assert row["description"] == "wrapper hiccup"
    assert row["task_description"] == "Longer triage context for the board."
    assert row["project"] == config.OOPS_PROJECT
    assert str(row.get(config.TASK_CREATION_SURFACE_UDA) or "") == ""


def test_task_oops_accepts_priority_style_severity_shorthand(task_repo, capsys):
    args = build_parser().parse_args(
        ["task", "oops", "wrapper", "hiccup", "--severity", "H"]
    )

    assert args.func(args) == 0
    out = capsys.readouterr().out
    created = re.search(r"OOPS-\S+", out).group(0)
    row = identity.resolve(created)

    assert "[high]" in out
    assert row["priority"] == "H"
    assert "high" in row["tags"]
    assert row["project"] == config.OOPS_PROJECT


def test_task_add_rejects_oops_system_project(task_repo):
    assert task_repo.is_dir()

    with pytest.raises(SpiceError, match="reserved for system task creation"):
        create.add(
            "Manual oops project",
            project=config.OOPS_PROJECT,
            priority="medium",
            acceptance=["oops is system-created only"],
        )


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
    monkeypatch.setattr(task_cli.alloc, "visible_rows", fake_visible_rows)

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
    monkeypatch.setattr(task_cli.alloc, "visible_rows", fake_visible_rows)

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


def test_task_show_surfaces_creator_rehydrate_action(monkeypatch):
    row = _row(
        "Creator context",
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
            "origin_thread": "origin-thread",
            "origin_worktree": "/tmp/origin",
            "claim_thread": "claim-thread",
            "claim_worktree": "/tmp/claim",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.ops, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test")

    assert (
        "rehydrate:\n  creator context, run: spice session briefing origin-thread"
        in (output)
    )
    assert "--start 2026-06-12T06:53:25.463453Z" in output
    assert "--end 2026-06-12T07:03:25.463453Z" in output


def test_task_show_requires_context_check_before_implementation(monkeypatch):
    row = _row(
        "Current context",
        project="task.render",
        incepted="20260612T065825463453Z",
        status="pending",
        phase="todo",
    )
    row.update(
        {
            "task_description": "Implement only if current",
            "phase_i": "0",
            "urgency": "9.2",
            "origin_thread": "origin-thread",
            "claim_thread": "claim-thread",
            "claim_context_start": "2026-06-12T08:15:18.621994Z",
            "claim_context_end": "2026-06-12T08:25:18.621994Z",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.ops, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test")

    assert "context_check:" in output
    assert "Before editing, run the rehydrate command(s) above" in output
    assert "assert the task description/acceptance still match" in output


def test_task_show_context_check_names_stale_or_shifted_context(monkeypatch):
    row = _row(
        "No transcript context",
        project="task.render",
        incepted="not-a-context-window",
        status="pending",
        phase="verify",
    )
    row.update(
        {
            "task_description": "Verify only if still relevant",
            "phase_i": "1",
            "urgency": "9.2",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.ops, "phases_of", lambda _row: ["todo", "verify"])

    output = render.render_show("TASK-test")

    assert "context_check:" in output
    assert "no transcript rehydrate command is available" in output
    assert "If context shifted or the task is stale" in output
    assert "before changing files" in output


def test_task_show_does_not_add_implementation_context_check_to_review(monkeypatch):
    row = _row(
        "Review context",
        project="task.render",
        incepted="20260612T065825463453Z",
        status="pending",
        phase="review",
    )
    row.update(
        {
            "task_description": "Review already asserts description currency",
            "phase_i": "1",
            "urgency": "9.2",
            "claim_by": "actor-a",
            "origin_thread": "origin-thread",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.ops, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test")

    assert "context_check:" not in output
    assert (
        "next: spice task review TASK-test --finding clean --note "
        '"description current; ..."'
    ) in output


def test_task_show_keeps_creator_rehydrate_for_same_claim_thread(monkeypatch):
    row = _row(
        "Same thread context",
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
            "origin_thread": "same-thread",
            "origin_worktree": "/tmp/repo",
            "claim_thread": "same-thread",
            "claim_worktree": "/tmp/repo",
            "claim_context_start": "2026-06-12T08:15:18.621994Z",
            "claim_context_end": "2026-06-12T08:25:18.621994Z",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.ops, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test")

    assert "creator context, run: spice session briefing same-thread" in output
    assert "--start 2026-06-12T06:53:25.463453Z" in output
    assert "--end 2026-06-12T07:03:25.463453Z" in output
    assert "claim context, run: spice session briefing same-thread" in output
    assert "--start 2026-06-12T08:15:18.621994Z" in output
    assert "--end 2026-06-12T08:25:18.621994Z" in output


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

    assert "rehydrate:" in output
    assert "creator context: unavailable (sentinel thread has no transcript)" in output
    assert "claim context: unavailable (sentinel thread has no transcript)" in output
    assert f"spice session briefing {sentinel}" not in output
    assert f"spice session turns {sentinel}" not in output


def test_task_show_prints_merge_aware_diff_command_for_task_merge(monkeypatch):
    row = _row(
        "Review merge",
        project="task.render",
        incepted="20260612T065825463453Z",
        status="pending",
        phase="review",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "1",
            "urgency": "9.2",
            "claim_by": "actor-a",
            "done_ref": "merge-head",
            "done_merge_head": "merge-head",
            "done_head": "agent-head",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.ops, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test")

    assert "review_commit merge-head (task merge; agent_head agent-head)" in output
    assert (
        "review_diff_command git show -m --first-parent --stat --patch merge-head"
        in output
    )
    assert (
        "review_diff_note task merge commits need merge-aware diff; "
        "plain git show can omit the agent patch"
    ) in output


def test_task_show_omits_merge_aware_diff_command_for_task_head(monkeypatch):
    row = _row(
        "Review direct head",
        project="task.render",
        incepted="20260612T065825463453Z",
        status="pending",
        phase="review",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "1",
            "urgency": "9.2",
            "claim_by": "actor-a",
            "done_ref": "agent-head",
            "done_merge_head": "agent-head",
            "done_head": "agent-head",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.ops, "phases_of", lambda _row: ["todo", "review"])

    output = render.render_show("TASK-test")

    assert "review_commit agent-head (task head)" in output
    assert "review_diff_command" not in output
    assert "plain git show" not in output


def test_task_artifact_cli_stores_text_and_binary_sidecars(task_repo, capsys):
    handle = create.add(
        "Capture task artifacts",
        project="task.unit",
        priority="medium",
        acceptance=["artifact CLI stores text and binary evidence"],
    )
    notes = task_repo / "notes.md"
    image = task_repo / "screen.png"
    notes.write_text("raw notes\n", encoding="utf-8")
    image.write_bytes(b"\x89PNG\r\n\x1a\n")

    add_notes = _with_backend(
        build_parser().parse_args(
            [
                "task",
                "artifact",
                "add",
                handle,
                str(notes),
                "--name",
                "research-notes.md",
                "--type",
                "text/markdown",
                "--retention",
                "prunable",
            ]
        )
    )
    assert add_notes.func(add_notes) == 0
    add_notes_out = capsys.readouterr().out

    add_image = _with_backend(
        build_parser().parse_args(
            [
                "task",
                "artifact",
                "add",
                handle,
                str(image),
                "--type",
                "image/png",
            ]
        )
    )
    assert add_image.func(add_image) == 0
    add_image_out = capsys.readouterr().out

    root = artifacts.artifact_root()
    assert root == task_repo / ".git" / "spice" / "artifacts" / "tasks"
    assert (root / handle / artifacts.MANIFEST_NAME).is_file()
    assert "added A1 research-notes.md text/markdown" in add_notes_out
    assert "retention prunable" in add_notes_out
    assert "added A2 screen.png image/png" in add_image_out

    list_args = _with_backend(
        build_parser().parse_args(["task", "artifact", "list", handle])
    )
    assert list_args.func(list_args) == 0
    listed = capsys.readouterr().out
    assert "A1 research-notes.md text/markdown 10 B prunable" in listed
    assert "A2 screen.png image/png 8 B permanent" in listed

    show_text = _with_backend(
        build_parser().parse_args(["task", "artifact", "show", handle, "A1"])
    )
    assert show_text.func(show_text) == 0
    assert "raw notes\n" in capsys.readouterr().out

    show_binary = _with_backend(
        build_parser().parse_args(["task", "artifact", "show", handle, "A2"])
    )
    assert show_binary.func(show_binary) == 0
    binary_output = capsys.readouterr().out.strip()
    assert binary_output.startswith("path ")
    assert (
        Path(binary_output.removeprefix("path ")).read_bytes() == b"\x89PNG\r\n\x1a\n"
    )

    shown = render.render_show(handle)
    assert "artifacts:" in shown
    assert "A1 research-notes.md text/markdown 10 B prunable" in shown
    assert f"spice task artifact show {handle} A1" in shown


def test_task_artifact_prune_is_dry_run_until_apply(task_repo, tmp_path, capsys):
    handle = create.add(
        "Prune completed artifact",
        project="task.unit",
        priority="medium",
        flow=["todo"],
        acceptance=["prunable artifacts are removed only with --apply"],
        claim=True,
    )
    artifact_path = tmp_path / "prune-me.txt"
    artifact_path.write_text("temporary evidence\n", encoding="utf-8")
    add_args = _with_backend(
        build_parser().parse_args(
            [
                "task",
                "artifact",
                "add",
                handle,
                str(artifact_path),
                "--type",
                "text/plain",
                "--retention",
                "prunable",
            ]
        )
    )
    assert add_args.func(add_args) == 0
    capsys.readouterr()
    ops.done(handle, validation=["single-phase task completed for prune"])

    dry_run = _with_backend(build_parser().parse_args(["task", "artifact", "prune"]))
    assert dry_run.func(dry_run) == 0
    dry_output = capsys.readouterr().out
    assert f"would prune {handle} A1 prune-me.txt" in dry_output
    assert "dry_run true; pass --apply to remove" in dry_output
    assert "A1 prune-me.txt" in artifacts.list_artifacts(handle)

    apply = _with_backend(
        build_parser().parse_args(["task", "artifact", "prune", "--apply"])
    )
    assert apply.func(apply) == 0
    apply_output = capsys.readouterr().out
    assert f"pruned {handle} A1 prune-me.txt" in apply_output
    assert artifacts.list_artifacts(handle) == f"no artifacts for {handle}"


def test_task_show_surfaces_review_note_artifact_citation(monkeypatch):
    row = _row(
        "Review citation",
        project="task.render",
        incepted="20260612T065825463453Z",
        status="completed",
        phase="review",
    )
    row.update(
        {
            "task_description": "",
            "phase_i": "1",
            "urgency": "9.2",
            "review_by": "actor-a",
            "review_finding": "changes",
            "review_note": "See artifact A1 on TASK-test for the raw log.",
        }
    )

    monkeypatch.setattr(render.identity, "resolve", lambda _handle: row)
    monkeypatch.setattr(render.identity, "render_handle", lambda _row: "TASK-test")
    monkeypatch.setattr(render.ops, "phases_of", lambda _row: ["todo", "review"])
    monkeypatch.setattr(
        render.artifacts,
        "render_artifact_lines",
        lambda _handle: ["artifacts:", "  A1 raw.log text/plain 12 B permanent"],
    )

    output = render.render_show("TASK-test")

    assert "review_finding changes" in output
    assert "review_note See artifact A1 on TASK-test for the raw log." in output
    assert "artifacts:\n  A1 raw.log text/plain 12 B permanent" in output


def _with_backend(args: argparse.Namespace) -> argparse.Namespace:
    args.backend = str(config.backend_root())
    return args


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


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-b", "main")
    _configure_git_identity(path)
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "initial")
    return path


def _configure_git_identity(repo: Path) -> None:
    _run(repo, "git", "config", "user.email", "spice@example.test")
    _run(repo, "git", "config", "user.name", "Spice Tests")


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
