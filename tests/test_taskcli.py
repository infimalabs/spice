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
    claimstate,
    cli as task_cli,
    config,
    create,
    identity,
    ops,
    render,
)


ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ACTOR_B = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    if shutil.which("task") is None:
        pytest.skip("Taskwarrior binary is required")
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-a")
    monkeypatch.setenv(config.TASK_BACKEND_ENV, str(backend))
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
    assert "[--all | --project PROJECT]" in help_text
    assert "--limit N" in help_text
    assert "--project PROJECT" in help_text
    assert "--status {pending,waiting,completed,deleted}" in help_text
    assert "Actor-route scope:" in help_text
    assert "spice task list --status pending --limit 20" in help_text
    assert "Explicit project scope:" in help_text
    assert "spice task list --project serve.ui --status pending --limit 20" in help_text
    assert "Global scope:" in help_text
    assert "spice task list --all --status pending" in help_text


def test_task_list_scoped_empty_points_to_matching_global_and_project_rows(
    task_repo, monkeypatch, capsys
):
    assert task_repo.is_dir()
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_B)
    create.add(
        "Peer global board row",
        project="task.cli",
        origin="ack:20260101T000000000000Z",
        acceptance=["row remains globally visible"],
    )
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)

    scoped_output = _task_list_through_cli(capsys, "--status", "pending")
    global_output = _task_list_through_cli(capsys, "--all", "--status", "pending")
    project_output = _task_list_through_cli(
        capsys, "--project", "task.cli", "--status", "pending"
    )

    assert scoped_output.splitlines() == [
        (
            "scope actor-route filter ( "
            f"project:{config.private_project(ACTOR_A)} or "
            f"origin_thread.is:{ACTOR_A} )"
        ),
        (
            "no tasks in scope; use --all for global rows or --project PROJECT "
            "for one project"
        ),
    ]
    assert global_output.splitlines()[0] == "scope global --all"
    assert "Peer global board row" in global_output
    assert project_output.splitlines()[0] == ("scope explicit-project project:task.cli")
    assert "Peer global board row" in project_output


def _task_list_through_cli(capsys, *options: str) -> str:
    args = build_parser().parse_args(["task", "list", *options])
    assert args.func(args) == 0
    return capsys.readouterr().out.strip()


def test_task_list_parse_error_points_to_limit_example(capsys):
    parser = build_parser()

    with pytest.raises(SystemExit) as exc_info:
        parser.parse_args(["task", "list", "--limt", "20"])

    error = capsys.readouterr().err
    assert exc_info.value.code == 2
    assert "Try `spice task list --help` for the exact contract." in error
    assert "spice task list --limit 20" in error


def test_task_document_cli_help_names_apply_stdin_dry_run_and_family_export(capsys):
    parser = build_parser()

    with pytest.raises(SystemExit):
        parser.parse_args(["task", "ingest", "--help"])
    ingest_help = capsys.readouterr().out
    assert "Apply a markdown task document" in ingest_help
    assert "Markdown path, or - to read standard input." in ingest_help
    assert "--dry-run" in ingest_help
    assert "--origin ORIGIN" in ingest_help
    assert "spice task ingest plan.md --project task.plan" in ingest_help

    with pytest.raises(SystemExit):
        parser.parse_args(["task", "ledger", "--help"])
    ledger_help = capsys.readouterr().out
    assert "Export a task family as normal-form markdown." in ledger_help
    assert "spice task ledger TASK-1k4Q5gJw" in ledger_help


def test_task_add_after_accepts_space_separated_dependencies():
    args = build_parser().parse_args(
        [
            "task",
            "add",
            "Follow-up title",
            "--after",
            "TASK-20260101T000000000001Z",
            "TASK-20260101T000000000002Z",
            "--project",
            "task.unit",
        ]
    )

    assert args.after == [
        "TASK-20260101T000000000001Z",
        "TASK-20260101T000000000002Z",
    ]
    assert args.title == "Follow-up title"


def test_task_add_after_repeats_for_multiple_dependencies():
    args = build_parser().parse_args(
        [
            "task",
            "add",
            "Follow-up title",
            "--after",
            "TASK-20260101T000000000001Z",
            "--after",
            "TASK-20260101T000000000002Z",
        ]
    )

    assert args.after == [
        "TASK-20260101T000000000001Z",
        "TASK-20260101T000000000002Z",
    ]
    assert args.title == "Follow-up title"


@pytest.mark.parametrize("repeated_flags", [False, True])
def test_task_depends_after_accumulates_all_edges(task_repo, capsys, repeated_flags):
    parent = create.add(
        "Parent waiting on several dependencies",
        project="task.unit",
        acceptance=["parent acceptance"],
        origin="ack:20260101T000000000000Z",
    )
    children = [
        create.add(
            f"Dependency {index}",
            project="task.unit",
            acceptance=[f"dependency {index} acceptance"],
            origin="ack:20260101T000000000000Z",
        )
        for index in range(3)
    ]
    after_args = (
        [value for child in children for value in ("--after", child)]
        if repeated_flags
        else ["--after", *children]
    )
    args = _with_backend(
        build_parser().parse_args(["task", "depends", parent, *after_args])
    )

    assert args.func(args) == 0
    capsys.readouterr()
    row = identity.resolve(parent)
    assert set(row.get("depends", [])) == {
        identity.uuid_of(identity.resolve(child)) for child in children
    }
    assert sorted(
        line.split()[1]
        for line in render.render_show(parent).splitlines()
        if line.startswith("  after ")
    ) == sorted(children)


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


def test_task_wake_parser_accepts_into_project():
    args = build_parser().parse_args(
        ["task", "wake", "OOPS-20260101T000000000001Z", "--into", "task.cli"]
    )

    assert args.task_action == "wake"
    assert args.handles == ["OOPS-20260101T000000000001Z"]
    assert args.into == "task.cli"


def test_task_wake_parser_defaults_into_to_none():
    args = build_parser().parse_args(["task", "wake", "TASK-20260101T000000000001Z"])

    assert args.into is None


def test_task_reclaim_parser_accepts_optional_handle():
    bare = build_parser().parse_args(["task", "reclaim"])
    explicit = build_parser().parse_args(
        ["task", "reclaim", "TASK-20260101T000000000001Z"]
    )

    assert bare.task_action == "reclaim"
    assert bare.handle is None
    assert explicit.task_action == "reclaim"
    assert explicit.handle == "TASK-20260101T000000000001Z"


def test_task_reclaim_parser_rejects_renew_alias():
    with pytest.raises(SystemExit) as exc_info:
        build_parser().parse_args(["task", "renew"])

    assert exc_info.value.code == 2


def test_task_reclaim_renders_result(monkeypatch):
    monkeypatch.setattr(
        claimstate,
        "renew_claim",
        lambda _handle: claimstate.ClaimRenewalResult(
            True,
            "renewed",
            handle="TASK-20260101T000000000001Z",
            claim_until="2026-07-09T06:00:00.000000Z",
        ),
    )

    output = task_cli._reclaim(argparse.Namespace(handle="TASK-20260101T000000000001Z"))

    assert (
        output == "reclaimed TASK-20260101T000000000001Z until "
        "2026-07-09T06:00:00.000000Z"
    )


def test_task_reclaim_renders_noop(monkeypatch):
    monkeypatch.setattr(
        claimstate,
        "renew_claim",
        lambda _handle: claimstate.ClaimRenewalResult(False, "no_active_claim"),
    )

    output = task_cli._reclaim(argparse.Namespace(handle=None))

    assert output == "reclaim skipped no_active_claim"


def test_task_delete_parser_accepts_force_claimed():
    args = build_parser().parse_args(
        [
            "task",
            "delete",
            "TASK-20260101T000000000001Z",
            "--reason",
            "duplicate",
            "--force-claimed",
        ]
    )

    assert args.task_action == "delete"
    assert args.force_claimed is True


def test_task_edit_rewrites_description_composed_with_priority(task_repo, capsys):
    handle = create.add(
        "Refresh my description",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        description="sketch that predates the landed mechanism",
        priority="low",
    )
    new_text = "landed mechanism recorded; sketch rewritten in place"

    args = build_parser().parse_args(
        ["task", "edit", handle, "--description", new_text, "--priority", "high"]
    )
    assert args.func(args) == 0

    row = identity.resolve(handle)
    assert str(row.get("task_description")) == new_text
    assert row["priority"] == "H"

    show = build_parser().parse_args(["task", "show", handle])
    capsys.readouterr()
    assert show.func(show) == 0
    assert f"description {new_text}" in capsys.readouterr().out


def test_task_edit_help_documents_description(capsys):
    parser = build_parser()

    with pytest.raises(SystemExit):
        parser.parse_args(["task", "edit", "--help"])

    help_text = capsys.readouterr().out
    assert "--description DESCRIPTION" in help_text
    assert "Replace the task description body." in help_text


def test_task_add_help_documents_every_repeatable_batch_field(capsys):
    with pytest.raises(SystemExit) as exc_info:
        build_parser().parse_args(["task", "add", "--help"])

    assert exc_info.value.code == 0
    help_text = " ".join(capsys.readouterr().out.split())
    assert (
        "Repeat collection fields flow=..., tags=..., after=..., and acceptance=... "
        "to accrue values in input order."
    ) in help_text


def test_task_add_title_flag_is_alias_for_positional(task_repo, capsys):
    args = build_parser().parse_args(
        [
            "task",
            "add",
            "--title",
            "Alias title lands",
            "--project",
            "task.unit",
            "--origin",
            "ack:20260101T000000000000Z",
        ]
    )

    assert args.func(args) == 0
    created = capsys.readouterr().out.split()[1]
    row = identity.resolve(created)

    assert row["description"] == "Alias title lands"
    assert row[config.TASK_CREATION_SURFACE_UDA] == config.TASK_CREATION_SURFACE_CLI


def test_task_add_missing_acceptance_routes_to_plan(task_repo, capsys):
    args = build_parser().parse_args(
        [
            "task",
            "add",
            "Plan routed CLI task",
            "--project",
            "task.unit",
            "--due",
            "2026-08-01",
            "--origin",
            "ack:20260101T000000000000Z",
        ]
    )

    assert args.func(args) == 0
    created = capsys.readouterr().out.split()[1]
    row = identity.resolve(created)

    assert row["description"] == "Plan routed CLI task"
    assert row["project"] == "task.unit"
    assert row["phase"] == "plan"
    assert claimstate.phases_of(row) == ["plan", "todo", "review"]
    assert not str(row.get("acceptance") or "")
    assert row["origin"] == "ack:20260101T000000000000Z"
    assert str(row.get("due") or "").startswith("20260801")


def test_task_add_missing_acceptance_honors_explicit_flow(task_repo, capsys):
    args = build_parser().parse_args(
        [
            "task",
            "add",
            "Explicit flow CLI task",
            "--project",
            "task.unit",
            "--flow",
            "todo,review",
            "--origin",
            "ack:20260101T000000000000Z",
        ]
    )

    assert args.func(args) == 0
    created = capsys.readouterr().out.split()[1]
    row = identity.resolve(created)

    assert row["description"] == "Explicit flow CLI task"
    assert row["phase"] == "todo"
    assert claimstate.phases_of(row) == ["todo", "review"]
    assert not str(row.get("acceptance") or "")


def test_task_add_suspect_wording_routes_to_plan_and_marks_row(task_repo, capsys):
    args = build_parser().parse_args(
        [
            "task",
            "add",
            "Adopting CLI task",
            "--project",
            "task.unit",
            "--acceptance",
            "Accepted tasks still route through plan when wording is suspect",
            "--origin",
            "ack:20260101T000000000000Z",
        ]
    )

    assert args.func(args) == 0
    created = capsys.readouterr().out.split()[1]
    row = identity.resolve(created)
    annotations = [ann.get("description", "") for ann in row.get("annotations") or []]

    assert row["description"] == "Adopting CLI task"
    assert row["phase"] == "plan"
    assert claimstate.phases_of(row) == ["plan", "todo", "review"]
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"
    assert row[config.TASK_CREATION_SURFACE_UDA] == config.TASK_CREATION_SURFACE_CLI
    assert row["origin"] == "ack:20260101T000000000000Z"
    assert any(
        "adopting" in ann and "self-correction required" in ann for ann in annotations
    )


def test_task_reword_clears_active_claim_marker(task_repo, capsys):
    handle = create.add(
        "Adopting CLI resolve",
        project="task.unit",
        acceptance=["parent bookend acceptance exists"],
        origin="ack:20260101T000000000000Z",
    )
    child = create.add(
        "Concrete CLI child",
        project="task.unit",
        acceptance=["child node has acceptance"],
        origin="ack:20260101T000000000000Z",
    )
    ops.depends(handle, [child])
    ops.claim(handle)
    args = _with_backend(
        build_parser().parse_args(
            [
                "task",
                "reword",
                "--reason",
                "accepted child board exists",
            ]
        )
    )

    assert args.func(args) == 0
    output = capsys.readouterr().out
    row = identity.resolve(handle)
    annotations = [ann.get("description", "") for ann in row.get("annotations") or []]

    assert f"reworded {handle}" in output
    assert not str(row.get(config.TASK_WORDING_REVIEW_UDA) or "")
    assert any(item.startswith("suspect wording:") for item in annotations)
    assert "wording review resolved: accepted child board exists" in annotations


def test_task_depends_not_after_cli_drops_edge(task_repo, capsys):
    handle = create.add(
        "Plan holding a CLI dependency edge",
        project="task.unit",
        acceptance=["parent bookend acceptance exists"],
        origin="ack:20260101T000000000000Z",
    )
    child = create.add(
        "Dependency dropped through the CLI",
        project="task.unit",
        acceptance=["child node has acceptance"],
        origin="ack:20260101T000000000000Z",
    )
    ops.depends(handle, [child])

    args = _with_backend(
        build_parser().parse_args(["task", "depends", handle, "--not-after", child])
    )

    assert args.func(args) == 0
    output = capsys.readouterr().out
    row = identity.resolve(handle)
    assert handle in output
    assert row.get("depends", []) == []


def test_task_add_deferred_flag_creates_waiting_task(task_repo, capsys):
    args = build_parser().parse_args(
        [
            "task",
            "add",
            "Deferred CLI task",
            "--project",
            "task.unit",
            "--deferred",
            "--origin",
            "ack:20260101T000000000000Z",
        ]
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
        origin="ack:20260101T000000000000Z",
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
            "flow=todo | flow=review | tags=review | tags=accrual | "
            "acceptance=Spawned review follow-up can render as a task card | "
            "acceptance=Every review criterion accrues",
        ]
    )
    args.backend = str(config.backend_root())

    assert args.func(args) == 0
    out = capsys.readouterr().out
    spawned = re.search(r"spawned (\S+)", out).group(1)
    row = identity.resolve(spawned)

    assert row["description"] == "CLI spawned follow-up"
    assert claimstate.phases_of(row) == ["todo", "review"]
    assert row["tags"] == ["accrual", "review"]
    assert row["acceptance"] == (
        "Spawned review follow-up can render as a task card | "
        "Every review criterion accrues"
    )
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
            "--origin",
            "ack:20260101T000000000000Z",
        ]
    )

    assert args.func(args) == 0
    out = capsys.readouterr().out
    created = re.search(r"OOPS-\S+", out).group(0)
    row = identity.resolve(created)

    assert row["description"] == "wrapper hiccup"
    assert row["task_description"] == "Longer triage context for the board."
    assert row["project"] == config.OOPS_PROJECT
    assert row["phase"] == "plan"
    assert row.get("tags", []) == []
    assert str(row.get(config.TASK_CREATION_SURFACE_UDA) or "") == ""


def test_task_oops_accepts_priority_style_severity_shorthand(task_repo, capsys):
    args = build_parser().parse_args(
        [
            "task",
            "oops",
            "wrapper",
            "hiccup",
            "--severity",
            "H",
            "--origin",
            "ack:20260101T000000000000Z",
        ]
    )

    assert args.func(args) == 0
    out = capsys.readouterr().out
    created = re.search(r"OOPS-\S+", out).group(0)
    row = identity.resolve(created)

    assert "[high]" in out
    assert row["priority"] == "H"
    assert row["project"] == config.OOPS_PROJECT
    assert row["phase"] == "plan"
    assert row.get("tags", []) == []


def test_task_add_rejects_oops_system_project(task_repo):
    assert task_repo.is_dir()

    with pytest.raises(SpiceError, match="reserved for system task creation"):
        create.add(
            "Manual oops project",
            project=config.OOPS_PROJECT,
            priority="medium",
            acceptance=["oops is system-created only"],
        )


def test_task_list_project_scope_filters_board_and_sorts_newest(monkeypatch):
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

    def fake_export(filters: list[str] | None = None) -> list[dict[str, object]]:
        seen["filters"] = filters
        return rows

    monkeypatch.setattr("spice.tasks.tw.export", fake_export)

    output = task_cli._list(
        argparse.Namespace(all=False, status=None, project="serve", limit=2)
    )

    assert seen == {
        "filters": [
            "(",
            "status:pending",
            "or",
            "(",
            "status:waiting",
            "and",
            "+ACTIVE",
            ")",
            ")",
        ],
    }
    lines = output.splitlines()
    assert lines[0] == "scope explicit-project project:serve"
    assert "Serve API newest" in lines[1]
    assert "Serve UI middle" in lines[2]
    assert "Task newest ignored" not in output
    assert "Serve UI oldest" not in output


def test_task_list_status_filter_uses_visible_rows(monkeypatch):
    seen: dict[str, object] = {}

    def fake_visible_rows_with_scope(
        actor: str, filters: list[str]
    ) -> tuple[list[dict[str, object]], list[str]]:
        seen["actor"] = actor
        seen["filters"] = filters
        return (
            [
                _row(
                    "Waiting task",
                    project="task.cli",
                    status="waiting",
                    incepted="20260612T000000000001Z",
                )
            ],
            ["project:task.cli"],
        )

    monkeypatch.setattr("spice.tasks.tw.current_actor", lambda: "actor-a")
    monkeypatch.setattr(
        task_cli.alloc, "visible_rows_with_scope", fake_visible_rows_with_scope
    )

    output = task_cli._list(
        argparse.Namespace(all=False, status="waiting", project=None, limit=None)
    )

    assert seen == {"actor": "actor-a", "filters": ["+WAITING"]}
    assert output.splitlines()[0] == "scope actor-route filter project:task.cli"
    assert "Waiting task" in output


def test_task_list_explicit_hidden_project_uses_raw_export(monkeypatch):
    rows = [
        _row(
            "Hidden oops item",
            project=config.OOPS_PROJECT,
            incepted="20260612T000000000001Z",
        )
    ]
    seen: dict[str, object] = {}

    def fake_export(filters: list[str] | None = None) -> list[dict[str, object]]:
        seen["filters"] = filters
        return rows

    monkeypatch.setattr("spice.tasks.tw.export", fake_export)

    output = task_cli._list(
        argparse.Namespace(
            all=False, status=None, project=config.OOPS_PROJECT, limit=None
        )
    )

    assert seen == {
        "filters": [
            "(",
            "status:pending",
            "or",
            "(",
            "status:waiting",
            "and",
            "+ACTIVE",
            ")",
            ")",
        ]
    }
    assert output.splitlines()[0] == (
        f"scope explicit-project project:{config.OOPS_PROJECT}"
    )
    assert "Hidden oops item" in output


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
    assert output.splitlines()[0] == "scope global --all"
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


def test_task_artifact_cli_stores_text_and_binary_sidecars(task_repo, capsys):
    handle = create.add(
        "Capture task artifacts",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
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
    assert root == task_repo / ".git" / ".spice" / "artifacts" / "tasks"
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
        origin="ack:20260101T000000000000Z",
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
