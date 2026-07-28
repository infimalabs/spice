"""Drain allocator regression coverage."""

from __future__ import annotations

import shutil
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.store import ServeTeamStore, TeamConfig
from spice.tasks import alloc, config, create, identity, ops
from tests.test_reposcaffolding import init_committed_repo as _init_repo
from tests.test_reposcaffolding import run as _run

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
    monkeypatch.setenv("CODEX_TURN_ID", "turn-drain")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def test_drain_phase_boundary_sees_configured_assignable_stem(task_repo, monkeypatch):
    (task_repo / "spice.toml").write_text(
        '[tasks]\nstems = ["paintball"]\n', encoding="utf-8"
    )
    _run(task_repo, "git", "add", "spice.toml")
    _run(task_repo, "git", "commit", "-m", "configure paintball stem")
    ServeTeamStore().create_team(
        members=[ACTOR_A_MEMBER, PEER_ACTOR_MEMBER],
        config=TeamConfig(lifetime="Drain"),
    )
    handle = create.add(
        "Drain sees configured stem",
        project="paintball.docs",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["drain sees repo-defined assignable stems"],
    )

    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert assigned["project"] == "paintball.docs"

    ops.done(handle, validation=["implementation complete"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    review = alloc.next_task()

    assert identity.render_handle(review or {}) == handle
    assert review["phase"] == "review"
    assert review["project"] == "paintball.docs"


def _configure_git_identity(repo: Path) -> None:
    _run(repo, "git", "config", "user.email", "spice@example.test")
    _run(repo, "git", "config", "user.name", "Spice Tests")
