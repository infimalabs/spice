"""Shared coordination state across primary and linked worktrees."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.agent.maximmetrics import (
    MAXIM_EVENT_FIRE,
    MaximMetricEventWrite,
    maxim_metric_records,
    maxim_metrics_database_path,
    record_maxim_metric_events,
)
from spice.mail.ackstate import (
    AckStateWrite,
    ack_state_database_path,
    ack_state_records,
    record_acked_inbox_items,
)
from spice.paths import git_common_dir
from spice.serve.team.schema import TEAM_DATABASE_FILENAME
from spice.serve.team.store import ServeTeamStore, team_database_path
from spice.tasks import config, create, tw

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ORIGIN = "ack:1kCxkWCN"


@pytest.fixture(autouse=True)
def _reset_task_backend():
    config.set_backend(None)
    yield
    config.set_backend(None)


def test_coordination_state_reopens_from_linked_worktree(tmp_path, monkeypatch):
    repo, linked = _repo_with_linked_worktree(tmp_path)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-shared-coordination")
    monkeypatch.delenv(config.TASK_BACKEND_ENV, raising=False)

    monkeypatch.chdir(repo)
    config.bootstrap()
    create.add(
        "shared coordination task",
        project="task.unit",
        origin=ORIGIN,
        acceptance=["visible from every worktree"],
    )
    primary_store = ServeTeamStore()
    primary_store.create_team(team_id="team-shared", members=["thread:agent-a"])
    record_acked_inbox_items(
        repo,
        [
            AckStateWrite(
                key="1kCxkWZc",
                inbox_name="shared.txt",
                text="shared ACK state",
            )
        ],
        now=100.0,
    )
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                bag_name="shared",
                driver_name="codex",
                thread_id=ACTOR,
            )
        ],
        now=101.0,
    )
    primary_paths = _coordination_paths(repo)

    monkeypatch.chdir(linked)
    linked_paths = _coordination_paths(linked)
    task_rows = tw.export(["status:pending"])
    linked_store = ServeTeamStore()
    linked_team = linked_store.team_state("team-shared")
    linked_acks = ack_state_records(linked)
    linked_maxims = maxim_metric_records(linked)
    with linked_store.connect() as connection:
        journal_mode = connection.execute("PRAGMA journal_mode").fetchone()[0]

    assert linked_paths == primary_paths
    assert primary_paths == (
        git_common_dir(repo) / ".spice",
        git_common_dir(repo) / ".spice" / "data",
        git_common_dir(repo) / ".spice" / "taskrc",
        git_common_dir(repo) / ".spice" / config.TASK_EVENT_FILENAME,
        git_common_dir(repo) / ".spice" / ".bootstrap.lock",
        git_common_dir(repo) / ".spice" / "data" / TEAM_DATABASE_FILENAME,
        git_common_dir(repo) / ".spice" / "data" / "spiceacks.sqlite3",
        git_common_dir(repo) / ".spice" / "data" / "spicemaxims.sqlite3",
    )
    assert [row["description"] for row in task_rows] == ["shared coordination task"]
    assert [member.agent_id for member in linked_team.members] == ["thread:agent-a"]
    assert [record.text for record in linked_acks] == ["shared ACK state"]
    assert [record.bag_name for record in linked_maxims] == ["shared"]
    assert str(journal_mode).lower() == "wal"


def test_task_backend_override_preserves_repository_owned_state_boundary(
    tmp_path, monkeypatch
):
    repo, linked = _repo_with_linked_worktree(tmp_path)
    backend = tmp_path / "task-backend"
    config.set_backend(str(backend))
    monkeypatch.chdir(repo)
    primary_paths = _coordination_paths(repo)
    monkeypatch.chdir(linked)
    linked_paths = _coordination_paths(linked)

    assert linked_paths == primary_paths
    assert primary_paths == (
        backend,
        backend / "data",
        backend / "taskrc",
        backend / config.TASK_EVENT_FILENAME,
        backend / ".bootstrap.lock",
        backend / "data" / TEAM_DATABASE_FILENAME,
        git_common_dir(repo) / ".spice" / "data" / "spiceacks.sqlite3",
        git_common_dir(repo) / ".spice" / "data" / "spicemaxims.sqlite3",
    )


def _coordination_paths(repo: Path) -> tuple[Path, ...]:
    return (
        config.backend_root(),
        config.data_dir(),
        config.taskrc_path(),
        config.task_event_path(),
        config.bootstrap_lock_path(),
        team_database_path(),
        ack_state_database_path(repo),
        maxim_metrics_database_path(repo),
    )


def _repo_with_linked_worktree(tmp_path: Path) -> tuple[Path, Path]:
    repo = tmp_path / "repo"
    linked = tmp_path / "linked"
    repo.mkdir()
    _run(repo, "git", "init", "-q", "-b", "main")
    _run(repo, "git", "config", "user.email", "spice@example.test")
    _run(repo, "git", "config", "user.name", "Spice Tests")
    (repo / "README.md").write_text("initial\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-qm", "initial")
    _run(repo, "git", "worktree", "add", "-q", "-b", "linked", str(linked))
    return repo, linked


def _run(cwd: Path, *args: str) -> None:
    subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
