"""Task filter provenance lifecycle regressions."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.serve.teamids import thread_actor_id
from spice.serve.teams import TASK_FILTER_SOURCE_AUTO_CREATE, ServeTeamStore, TeamConfig
from spice.tasks import config, identity, ops

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
PEER_ACTOR = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
ACTOR_A_MEMBER = thread_actor_id(ACTOR_A)
PEER_ACTOR_MEMBER = thread_actor_id(PEER_ACTOR)


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


def test_drive_replace_path_preserves_auto_create_filter_for_gc(task_repo, monkeypatch):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER, PEER_ACTOR_MEMBER],
        config=TeamConfig(lifetime="Drive"),
    )
    handle = ops.add(
        "Drive replace preserves provenance",
        project="task.unit",
        priority="medium",
        acceptance=["replace path preserves auto source for empty-project gc"],
    )

    store.update_team_config(
        team.team_id,
        TeamConfig(lifetime="Drive", task_filters=("task.unit",)),
        replace_task_filters=True,
    )
    after_replace = store.team_config(team.team_id)

    assert [entry.to_payload() for entry in after_replace.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE}
    ]

    assigned = ops.next_task()
    assert identity.render_handle(assigned or {}) == handle
    ops.done(handle, validation=["implementation complete"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    review = ops.next_task()
    assert identity.render_handle(review or {}) == handle
    ops.review(handle, finding="clean", note="review complete")
    after_review = store.team_config(team.team_id)

    assert after_review.task_filters == ()
    assert after_review.task_filter_entries == ()


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
    return subprocess.run(
        args,
        cwd=cwd,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
