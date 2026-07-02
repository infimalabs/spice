"""Private task creation policy: Steer-only, and adopt's --project requirement."""

from __future__ import annotations

import pytest

from spice.errors import SpiceError
from spice.serve.team.store import ServeTeamStore, TeamConfig
from spice.tasks import config, create, identity, ops

from tests.test_tasks import (
    ACTOR_A,
    ACTOR_A_MEMBER,
    _make_orphan_commit,
    remote_task_repo,
    task_repo,
)

__all__ = ["remote_task_repo", "task_repo"]


def test_private_task_creation_allowed_in_steer_lifetime(task_repo):
    ServeTeamStore().create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Steer")
    )

    handle = create.add(
        "Steer scratch task",
        acceptance=["private creation is allowed in Steer"],
    )
    row = identity.resolve(handle)

    assert row["project"] == config.private_project(ACTOR_A)


def test_private_task_creation_blocked_for_teamless_actor(task_repo):
    with pytest.raises(SpiceError, match="requires Steer lifetime"):
        create.add(
            "Teamless scratch task",
            acceptance=["private creation needs Steer"],
        )


def test_private_task_creation_blocked_outside_steer_lifetime(task_repo):
    ServeTeamStore().create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drain")
    )

    with pytest.raises(SpiceError, match="requires Steer lifetime"):
        create.add(
            "Drain scratch task",
            acceptance=["private creation is blocked outside Steer"],
        )


def test_task_adopt_requires_project_when_minting_new_task(remote_task_repo):
    _make_orphan_commit(remote_task_repo, subject="orphan needing a project")

    with pytest.raises(SpiceError, match="adopt requires --project"):
        ops.adopt()
