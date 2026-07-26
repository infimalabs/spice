"""Task filter provenance lifecycle regressions."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.store import (
    TASK_FILTER_SOURCE_AUTO_CREATE,
    TASK_FILTER_SOURCE_MANUAL,
    ServeTeamStore,
    TeamConfig,
)
from spice.tasks import alloc, create, identity, ops
from tests.test_reposcaffolding import (
    init_committed_repo as _init_repo,
)
from tests.test_reposcaffolding import (
    make_task_repo_fixture,
)

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
PEER_ACTOR = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
ACTOR_A_MEMBER = thread_actor_id(ACTOR_A)
PEER_ACTOR_MEMBER = thread_actor_id(PEER_ACTOR)


task_repo = make_task_repo_fixture(lambda path: _init_repo(path), actor=ACTOR_A)


def test_drive_replace_path_preserves_auto_create_filter_for_gc(task_repo, monkeypatch):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER, PEER_ACTOR_MEMBER],
        config=TeamConfig(lifetime="Drive"),
    )
    handle = create.add(
        "Drive replace preserves provenance",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["replace path preserves auto source for empty-project gc"],
    )

    store.update_team_config(
        team.team_id,
        TeamConfig(lifetime="Drive", task_filters=("task.unit",)),
        replace_task_filters=True,
    )
    after_replace = store.team_config(team.team_id)

    # The replace list is the manual pin layer; the auto:create subscription
    # from task creation coexists with the new pin.
    assert [entry.to_payload() for entry in after_replace.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE},
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_MANUAL},
    ]

    assigned = alloc.next_task()
    assert identity.render_handle(assigned or {}) == handle
    ops.done(handle, validation=["implementation complete"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    review = alloc.next_task()
    assert identity.render_handle(review or {}) == handle
    ops.review(handle, finding="clean", note="review complete")
    after_review = store.team_config(team.team_id)

    # Empty-project GC reclaims the auto subscription; the manual pin is
    # sticky and survives.
    assert after_review.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in after_review.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_MANUAL}
    ]


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=cwd,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
