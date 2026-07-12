"""Ingest resolves origin and project from flags or the active claim.

Origin reuses the creation-path resolver; project inheritance from the active
claim is new ingest surface. Both resolve before any board write, so a missing
reference refuses instead of half-applying a document.
"""

from __future__ import annotations

import pytest

from spice.errors import SpiceError
from spice.tasks import ops, tw
from spice.tasks.markdown import apply

from tests.test_tasks import task_repo
from tests.test_taskorigin import ACK_KEY, _seed_task

__all__ = ["task_repo"]


def _actor() -> str:
    return tw.canonical_actor(tw.current_actor())


def test_explicit_flags_resolve_origin_and_project(task_repo):
    assert task_repo.is_dir()

    project, origin = apply.resolve_ingest_target(
        _actor(), project="task.unit", origin=f"ack:{ACK_KEY}"
    )

    assert project == "task.unit"
    assert origin == f"ack:{ACK_KEY}"


def test_active_claim_supplies_origin_and_project(task_repo):
    assert task_repo.is_dir()
    root = _seed_task("Ingest inherits the active claim")
    ops.claim(root)

    project, origin = apply.resolve_ingest_target(_actor(), project=None, origin=None)

    assert origin == f"task:{root}"
    assert project == "task.unit"


def test_explicit_project_overrides_the_active_claim_project(task_repo):
    assert task_repo.is_dir()
    root = _seed_task("Claim on one project, ingest onto another")
    ops.claim(root)

    project, origin = apply.resolve_ingest_target(
        _actor(), project="task.plan", origin=None
    )

    # Origin still inherits the claim; the explicit project wins over it.
    assert origin == f"task:{root}"
    assert project == "task.plan"


def test_missing_origin_without_claim_refuses_before_any_write(task_repo):
    assert task_repo.is_dir()

    with pytest.raises(SpiceError, match="requires an origin"):
        apply.ingest_path("never-read.md", project="task.unit", origin=None)

    assert tw.export(["status:pending"]) == []


def test_missing_project_without_claim_refuses_before_any_write(task_repo):
    assert task_repo.is_dir()

    with pytest.raises(SpiceError, match="requires a project"):
        apply.ingest_path("never-read.md", project=None, origin=f"ack:{ACK_KEY}")

    assert tw.export(["status:pending"]) == []
