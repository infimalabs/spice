"""Task phase effort window ledger behavior."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.store import ServeTeamStore
from spice.tasks import config, create, effort, identity, ops

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ACTOR_B = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
ACTOR_A_MEMBER = thread_actor_id(ACTOR_A)
ACTOR_B_MEMBER = thread_actor_id(ACTOR_B)


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


def test_phase_effort_windows_split_real_task_lifecycle_phases(task_repo, monkeypatch):
    clock = {"now": 100.0}
    monkeypatch.setattr("spice.serve.team.metrics.time.time", lambda: clock["now"])
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A_MEMBER])
    _record_identity(
        store,
        ACTOR_A_MEMBER,
        thread_id=ACTOR_A,
        driver="codex",
        model="gpt-5.5",
        effort_value="xhigh",
    )
    handle = create.add(
        "Measure two effort phases",
        project="task.unit",
        flow=["todo", "verify", "review"],
        acceptance=["phase effort windows split phases"],
    )

    clock["now"] = 100.0
    ops.claim(handle)
    clock["now"] = 145.0
    ops.done(handle, validation=["todo complete"])
    clock["now"] = 160.0
    ops.claim(handle)
    clock["now"] = 220.0
    ops.done(handle, validation=["verify complete"])

    row = identity.resolve(handle)
    windows = store.task_phase_effort_windows([row])

    assert [
        (
            window.handle,
            window.title,
            window.phase,
            window.phase_index,
            window.actor_id,
            window.thread_id,
            window.team_id,
            window.driver,
            window.model,
            window.effort,
            window.started_at,
            window.ended_at,
            window.wall_seconds,
            window.partial_markers,
        )
        for window in windows
    ] == [
        (
            handle,
            "Measure two effort phases",
            "todo",
            0,
            ACTOR_A_MEMBER,
            ACTOR_A,
            team.team_id,
            "codex",
            "gpt-5.5",
            "xhigh",
            100.0,
            145.0,
            45.0,
            (),
        ),
        (
            handle,
            "Measure two effort phases",
            "verify",
            1,
            ACTOR_A_MEMBER,
            ACTOR_A,
            team.team_id,
            "codex",
            "gpt-5.5",
            "xhigh",
            160.0,
            220.0,
            60.0,
            (),
        ),
    ]


def test_phase_effort_windows_mark_partial_lifecycle_segments(task_repo):
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A_MEMBER, ACTOR_B_MEMBER])
    _record_identity(
        store,
        ACTOR_A_MEMBER,
        thread_id=ACTOR_A,
        driver="codex",
        model="gpt-5.5",
        effort_value="xhigh",
    )
    _record_identity(
        store,
        ACTOR_B_MEMBER,
        thread_id=ACTOR_B,
        driver="claude",
        model="claude-sonnet",
        effort_value="medium",
    )
    handle = create.add(
        "Mark partial effort phases",
        project="task.unit",
        flow=["todo", "verify", "review"],
        acceptance=["partial effort windows are marked"],
    )
    task_id = identity.uuid_of(identity.resolve(handle))
    store.record_task_lifecycle_event(
        "phaseAdvance",
        task_id=task_id,
        agent_id=ACTOR_A_MEMBER,
        team_id=team.team_id,
        ts=20.0,
    )
    store.record_task_lifecycle_event(
        "claim",
        task_id=task_id,
        agent_id=ACTOR_A_MEMBER,
        team_id=team.team_id,
        ts=30.0,
    )
    store.record_task_lifecycle_event(
        "claim",
        task_id=task_id,
        agent_id=ACTOR_B_MEMBER,
        team_id=team.team_id,
        ts=45.0,
    )

    windows = store.task_phase_effort_windows([identity.resolve(handle)])

    assert [
        (
            window.phase,
            window.actor_id,
            window.driver,
            window.model,
            window.effort,
            window.started_at,
            window.ended_at,
            window.wall_seconds,
            window.partial_markers,
        )
        for window in windows
    ] == [
        (
            "todo",
            ACTOR_A_MEMBER,
            "codex",
            "gpt-5.5",
            "xhigh",
            None,
            20.0,
            None,
            (effort.PARTIAL_MISSING_START,),
        ),
        (
            "verify",
            ACTOR_A_MEMBER,
            "codex",
            "gpt-5.5",
            "xhigh",
            30.0,
            45.0,
            15.0,
            (effort.PARTIAL_HANDOFF,),
        ),
        (
            "verify",
            ACTOR_B_MEMBER,
            "claude",
            "claude-sonnet",
            "medium",
            45.0,
            None,
            None,
            (effort.PARTIAL_MISSING_END,),
        ),
    ]


def _record_identity(
    store: ServeTeamStore,
    actor_id: str,
    *,
    thread_id: str,
    driver: str,
    model: str,
    effort_value: str,
) -> None:
    store.record_agent_identity(
        actor_id=actor_id,
        target_id="wt-a",
        thread_id=thread_id,
        actual_driver=driver,
        actual_model=model,
        actual_effort=effort_value,
        desired_driver=driver,
        desired_model=model,
        desired_effort=effort_value,
        transcript_owner=driver,
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
