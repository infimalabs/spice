"""Focused task output guidance for continuing allocator work."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.tasks import config, identity, ops, render

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
KEEP_DRAINING = (
    "keep working until no allocator-selected work remains or a real blocker exists"
)


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


def test_task_done_and_review_outputs_keep_draining_guidance(task_repo, monkeypatch):
    assert task_repo.is_dir()
    handle = ops.add(
        "Exercise task next guidance",
        project="task.guidance",
        priority="medium",
        acceptance=["post-boundary guidance is explicit"],
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

    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.guidance"], "lifetime": "Drive"},
    )
    assigned = ops.next_task()
    assert identity.render_handle(assigned or {}) == handle

    review_output = ops.review(handle, finding="clean", note="description current")

    assert (
        f"next: YOU ARE NOT DONE. Run spice task next; {KEEP_DRAINING}" in review_output
    )


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
