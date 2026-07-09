"""Suspect task wording review clearance."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.tasks import config, create, identity, ops, wordingreview

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
    assert "spice task resolve-wording" in message
    assert row["phase"] == "plan"
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"
    assert not str(row.get("validation") or "")


def test_resolve_wording_review_clears_marker_and_allows_plan_done(task_repo):
    handle = _suspect_plan_task_with_accepted_child()
    ops.claim(handle)

    output = wordingreview.resolve_wording_review(
        handle,
        reason="split into accepted child tasks",
    )
    resolved = identity.resolve(handle)
    annotations = [
        str(item.get("description") or "") for item in resolved.get("annotations") or []
    ]

    assert output == f"resolved wording review for {handle}"
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


def test_resolve_wording_review_rejects_non_owner(task_repo, monkeypatch):
    handle = _suspect_plan_task_with_accepted_child()
    ops.claim(handle)
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)

    with pytest.raises(SpiceError, match=f"task claimed by {ACTOR_A}"):
        wordingreview.resolve_wording_review(handle, reason="peer cannot clear marker")

    row = identity.resolve(handle)
    annotations = [
        str(item.get("description") or "") for item in row.get("annotations") or []
    ]
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"
    assert not any(item.startswith("wording review resolved:") for item in annotations)


def test_resolve_wording_review_rejects_inactive_task(task_repo):
    handle = _suspect_plan_task_with_accepted_child()

    with pytest.raises(SpiceError, match="resolve wording review requires a claim"):
        wordingreview.resolve_wording_review(
            handle,
            reason="inactive task cannot clear marker",
        )

    row = identity.resolve(handle)
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"


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
