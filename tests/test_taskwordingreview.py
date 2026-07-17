"""Suspect task wording review clearance."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.tasks import claimstate, config, create, identity, ops, render, wordingreview

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
PEER_ACTOR = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
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


def test_plan_phase_done_blocks_suspect_wording_marker(task_repo):
    handle = _suspect_plan_task_with_accepted_child()
    ops.claim(handle)

    with pytest.raises(SpiceError) as exc_info:
        ops.done(handle, validation=["plan board populated"])

    row = identity.resolve(handle)
    message = str(exc_info.value)
    assert "task done blocked" in message
    assert "still requires suspect-wording self-correction" in message
    assert "spice task reword" in message
    assert row["phase"] == "plan"
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"
    assert not str(row.get("validation") or "")


def test_inline_task_batch_suspect_wording_routes_without_claiming(task_repo):
    results = create.add_batch_results(
        [
            "TASK title=Adopting inline task | project=task.unit | "
            "acceptance=Inline task still needs self-correction | "
            "origin=ack:20260101T000000000000Z"
        ],
        creation_surface=config.TASK_CREATION_SURFACE_CLI,
    )
    row = identity.resolve(results[0].handle)
    annotations = [
        str(item.get("description") or "") for item in row.get("annotations") or []
    ]

    assert row["description"] == "Adopting inline task"
    assert row["phase"] == "plan"
    assert claimstate.phases_of(row) == ["plan", "todo", "review"]
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"
    assert not str(row.get("claim_by") or "")
    assert not str(row.get("start") or "")
    assert any(item.startswith("suspect wording:") for item in annotations)
    assert results[0].wording_matches


@pytest.mark.parametrize(
    "acceptance",
    [
        "Do not use the master label",
        "Do not add a fallback path",
        "Delete the preserveLaneHints option threading",
    ],
)
def test_explicitly_negated_wording_starts_in_todo(task_repo, acceptance):
    handle = create.add(
        "Concrete wording task",
        project="task.unit",
        acceptance=[acceptance],
        origin="ack:20260101T000000000000Z",
    )

    row = identity.resolve(handle)
    assert row["phase"] == "todo"
    assert claimstate.phases_of(row) == ["todo", "review"]


@pytest.mark.parametrize(
    "acceptance",
    [
        "Avoid failure by adding a fallback path",
        "Prevent delay with polling",
    ],
)
def test_prohibition_means_clause_starts_in_plan(task_repo, acceptance):
    handle = create.add(
        "Concrete wording task",
        project="task.unit",
        acceptance=[acceptance],
        origin="ack:20260101T000000000000Z",
    )

    row = identity.resolve(handle)
    assert row["phase"] == "plan"
    assert claimstate.phases_of(row) == ["plan", "todo", "review"]
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"


def test_review_followup_suspect_wording_routes_without_claiming(task_repo):
    reviewed = create.add(
        "Review target for suspect follow-up",
        project="task.unit",
        flow=["review"],
        acceptance=["review can spawn a requested-change task"],
        origin="ack:20260101T000000000000Z",
        claim=True,
    )

    output = ops.review(
        reviewed,
        finding="changes",
        note="description current; needs a follow-up",
        then=[
            "title=Adopting review follow-up | project=task.unit | "
            "acceptance=Follow-up must self-correct before implementation"
        ],
        creation_surface=config.TASK_CREATION_SURFACE_CLI,
    )
    spawned = next(
        line.split()[1] for line in output.splitlines() if line.startswith("spawned ")
    )
    row = identity.resolve(spawned)
    annotations = [
        str(item.get("description") or "") for item in row.get("annotations") or []
    ]

    assert identity.resolve(reviewed)["status"] == "completed"
    assert row["description"] == "Adopting review follow-up"
    assert row["phase"] == "plan"
    assert claimstate.phases_of(row) == ["plan", "todo", "review"]
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"
    assert row[config.TASK_CREATION_SURFACE_UDA] == config.TASK_CREATION_SURFACE_CLI
    assert not str(row.get("claim_by") or "")
    assert not str(row.get("start") or "")
    assert identity.uuid_of(identity.resolve(reviewed)) in row.get("depends", [])
    assert any(item.startswith("suspect wording:") for item in annotations)


def test_task_show_renders_suspect_wording_policy_and_clear_step(task_repo):
    handle = _suspect_plan_task_with_accepted_child()

    shown = render.render_show(handle)

    assert "wording_review required" in shown
    assert "suspect wording automatically prepended plan" in shown
    assert "matched wording remains in annotations" in shown
    assert f'spice task reword {handle} --reason "..."' in shown
    assert "suspect wording:" in shown
    assert "self-correction required" in shown


def test_reword_clears_marker_and_allows_plan_done(task_repo):
    handle = _suspect_plan_task_with_accepted_child()
    ops.claim(handle)

    output = wordingreview.reword(
        handle,
        reason="split into accepted child tasks",
    )
    resolved = identity.resolve(handle)
    annotations = [
        str(item.get("description") or "") for item in resolved.get("annotations") or []
    ]

    assert output == f"reworded {handle}"
    assert not str(resolved.get(config.TASK_WORDING_REVIEW_UDA) or "")
    assert any(item.startswith("suspect wording:") for item in annotations)
    assert "wording review resolved: split into accepted child tasks" in annotations

    done_output = ops.done(handle, validation=["plan board populated"])
    row = identity.resolve(handle)

    assert f"advanced {handle} -> todo" in done_output
    assert row["phase"] == "todo"
    assert not str(row.get(config.TASK_WORDING_REVIEW_UDA) or "")


def test_plan_phase_done_non_suspect_task_is_unaffected(task_repo):
    handle = _plan_task_with_accepted_child()
    ops.claim(handle)

    output = ops.done(handle, validation=["plan board populated"])
    row = identity.resolve(handle)

    assert f"advanced {handle} -> todo" in output
    assert row["phase"] == "todo"
    assert row["validation"] == "plan board populated"
    assert not str(row.get(config.TASK_WORDING_REVIEW_UDA) or "")


def test_reword_rejects_non_owner(task_repo, monkeypatch):
    handle = _suspect_plan_task_with_accepted_child()
    ops.claim(handle)
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)

    with pytest.raises(SpiceError, match=f"task claimed by {ACTOR_A}"):
        wordingreview.reword(handle, reason="peer cannot clear marker")

    row = identity.resolve(handle)
    annotations = [
        str(item.get("description") or "") for item in row.get("annotations") or []
    ]
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"
    assert not any(item.startswith("wording review resolved:") for item in annotations)


def test_reword_rejects_inactive_task(task_repo):
    handle = _suspect_plan_task_with_accepted_child()

    with pytest.raises(SpiceError, match="reword requires a claim"):
        wordingreview.reword(
            handle,
            reason="inactive task cannot clear marker",
        )

    row = identity.resolve(handle)
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"


def test_reword_from_claimed_plan_parent_clears_child_marker(task_repo):
    parent = _clean_plan_parent()
    child = _suspect_unclaimed_child("Adopting connected child")
    ops.depends(parent, [child])
    ops.claim(parent)

    output = wordingreview.reword(
        child,
        reason="child wording rewritten during parent planning",
    )

    child_row = identity.resolve(child)
    parent_row = identity.resolve(parent)
    annotations = [
        str(item.get("description") or "")
        for item in child_row.get("annotations") or []
    ]
    assert output == f"reworded {child}"
    assert str(child_row.get(config.TASK_WORDING_REVIEW_UDA) or "") == ""
    assert (
        "wording review resolved: child wording rewritten during parent planning"
        in annotations
    )
    assert parent_row["claim_by"] == ACTOR_A
    assert str(parent_row.get("start") or "") != ""
    assert parent_row["phase"] == "plan"
    assert str(child_row.get("claim_by") or "") == ""
    assert str(child_row.get("start") or "") == ""


def test_task_edit_acceptance_suspect_wording_sets_review_marker(task_repo):
    handle = create.add(
        "Edit gains suspect acceptance",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["clean criterion"],
    )

    ops.edit(handle, acceptance=["adopting the legacy rows"])

    row = identity.resolve(handle)
    assert row["acceptance"] == "adopting the legacy rows"
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"
    annotations = [
        str(entry.get("description") or "") for entry in row.get("annotations") or []
    ]
    assert any("self-correction required" in note for note in annotations)


def _suspect_plan_task_with_accepted_child() -> str:
    handle = create.add(
        "Adopting plan parent",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["parent bookend acceptance exists"],
    )
    child = create.add(
        "Concrete child",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["child node has acceptance"],
    )
    ops.depends(handle, [child])
    return handle


def _plan_task_with_accepted_child() -> str:
    handle = create.add(
        "Plan wording review parent",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        flow=["plan", "todo", "review"],
        acceptance=["parent bookend acceptance exists"],
    )
    child = create.add(
        "Plan wording review child",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["child node has acceptance"],
    )
    ops.depends(handle, [child])
    return handle


def _clean_plan_parent() -> str:
    return create.add(
        "Plan parent bookend",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        flow=["plan", "todo", "review"],
        acceptance=["parent bookend acceptance exists"],
    )


def _suspect_unclaimed_child(title: str) -> str:
    child = create.add(
        title,
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["child node has acceptance"],
    )
    row = identity.resolve(child)
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"
    assert str(row.get("claim_by") or "") == ""
    return child


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
