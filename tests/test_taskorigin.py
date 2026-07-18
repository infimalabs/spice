"""Task origin provenance: ack:/task: realms, requirement, and defaults."""

from __future__ import annotations


import pytest

from spice.errors import SpiceError
from spice.tasks import config, create, identity, ops, tw

from tests.test_tasks import ACTOR_A, task_repo

__all__ = ["task_repo"]

ACK_KEY = "1jNmXPHm"


def _seed_task(title: str = "Provenance root") -> str:
    return create.add(
        title,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
        priority="medium",
        acceptance=["origin seed"],
    )


def test_origin_accepts_ack_task_and_bare_forms(task_repo):
    assert task_repo.is_dir()
    root = _seed_task()

    explicit_ack = create.add(
        "Explicit ack origin",
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
        priority="medium",
        acceptance=["ack realm"],
    )
    explicit_task = create.add(
        "Explicit task origin",
        project="task.unit",
        origin=f"task:{root}",
        priority="medium",
        acceptance=["task realm"],
    )
    bare_ack = create.add(
        "Bare ack key auto-realms",
        project="task.unit",
        # A bare inbox-key-shaped value lands in the ack realm without a prefix.
        origin=ACK_KEY,
        priority="medium",
        acceptance=["bare ack"],
    )
    bare_task = create.add(
        "Bare handle auto-realms",
        project="task.unit",
        origin=root,
        priority="medium",
        acceptance=["bare task"],
    )

    assert identity.resolve(explicit_ack)["origin"] == f"ack:{ACK_KEY}"
    assert identity.resolve(explicit_task)["origin"] == f"task:{root}"
    assert identity.resolve(bare_ack)["origin"] == f"ack:{ACK_KEY}"
    assert identity.resolve(bare_task)["origin"] == f"task:{root}"


def test_assignable_creation_without_origin_or_claim_fails(task_repo):
    assert task_repo.is_dir()
    with pytest.raises(SpiceError, match="task creation requires an origin"):
        create.add(
            "No provenance",
            project="task.unit",
            priority="medium",
            acceptance=["should not exist"],
        )


def test_active_claim_supplies_default_origin(task_repo):
    assert task_repo.is_dir()
    root = _seed_task()
    ops.claim(root)

    spawned = create.add(
        "Created mid-claim",
        project="task.unit",
        priority="medium",
        acceptance=["inherits the active claim"],
    )

    assert identity.resolve(spawned)["origin"] == f"task:{root}"


def test_origin_is_universal_across_private_and_hidden_projects(task_repo):
    """Every single task carries an origin: private scratch and hidden triage
    included. Claim-less creation without a citation is refused everywhere."""
    from spice.serve.team.store import ServeTeamStore, TeamConfig

    from tests.test_tasks import ACTOR_A_MEMBER

    assert task_repo.is_dir()
    ServeTeamStore().create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Steer")
    )
    with pytest.raises(SpiceError, match="task creation requires an origin"):
        create.add(
            "Private scratch without provenance",
            priority="medium",
            acceptance=["refused"],
        )
    with pytest.raises(SpiceError, match="task creation requires an origin"):
        ops.oops("Triage without provenance", description="refused")
    private = create.add(
        "Private scratch citing its ack",
        priority="medium",
        acceptance=["private cites the ack"],
        origin=f"ack:{ACK_KEY}",
    )
    oops_line = ops.oops(
        "Triage citing its ack",
        description="origin recorded",
        origin=f"ack:{ACK_KEY}",
    )
    oops_handle = oops_line.split()[1]

    private_row = identity.resolve(private)
    oops_row = identity.resolve(oops_handle)

    assert private_row["project"] == config.private_project(ACTOR_A)
    assert private_row["origin"] == f"ack:{ACK_KEY}"
    assert oops_row["origin"] == f"ack:{ACK_KEY}"
    assert all(
        not str(note.get("description") or "").startswith("origin:")
        for note in oops_row.get("annotations") or []
    )


def test_exempt_projects_capture_origin_opportunistically(task_repo):
    """Private scratch and oops never REQUIRE an origin, but they link back
    to the active claim automatically when one exists."""
    from spice.serve.team.store import ServeTeamStore, TeamConfig

    from tests.test_tasks import ACTOR_A_MEMBER

    assert task_repo.is_dir()
    ServeTeamStore().create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Steer")
    )
    root = _seed_task("Claimed work surfaces side captures")
    ops.claim(root)

    oops_line = ops.oops("Failure observed mid-claim", description="links back")
    oops_handle = oops_line.split()[1]
    private = create.add(
        "Private scratch mid-claim",
        priority="medium",
        acceptance=["private scratch links back"],
    )

    assert identity.resolve(oops_handle)["origin"] == f"task:{root}"
    assert identity.resolve(private)["origin"] == f"task:{root}"


def test_origin_rejects_unresolvable_handles_and_malformed_keys(task_repo):
    assert task_repo.is_dir()
    with pytest.raises(SpiceError, match="task origin"):
        create.validated_task_origin("task:NOPE-404")
    with pytest.raises(SpiceError, match="task origin ack key"):
        create.validated_task_origin("ack:not-a-key")
    with pytest.raises(SpiceError, match="task origin"):
        create.validated_task_origin("gibberish without realm")


def test_batch_origin_field_round_trips(task_repo):
    assert task_repo.is_dir()
    handles = create.add_batch(
        [
            f"title=Batch with origin | project=task.unit | acceptance=ok | "
            f"origin=ack:{ACK_KEY}"
        ]
    )

    assert len(handles) == 1
    assert identity.resolve(handles[0])["origin"] == f"ack:{ACK_KEY}"


def test_batch_rejects_invalid_origin_before_creating_anything(task_repo):
    assert task_repo.is_dir()
    with pytest.raises(SpiceError, match="task origin"):
        create.add_batch(
            [
                "title=Bad origin | project=task.unit | acceptance=ok | "
                "origin=task:NOPE-404"
            ]
        )
    assert tw.export(["status:pending"]) == []


def test_review_then_followup_inherits_reviewed_task_origin(task_repo, monkeypatch):
    from spice.agent.driver import DRIVER

    from tests.test_tasks import PEER_ACTOR

    assert task_repo.is_dir()
    handle = _seed_task("Reviewed work spawns follow-up")
    ops.claim(handle)
    ops.done(handle, validation=["implementation complete"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)

    ops.review(
        handle,
        finding="unclean",
        note="needs a follow-up",
        then=[
            "title=Follow the review | project=task.unit | "
            "acceptance=Review feedback addressed"
        ],
    )

    followups = [
        row
        for row in tw.export(["status:pending"])
        if row.get("description") == "Follow the review"
    ]
    assert len(followups) == 1
    assert followups[0]["origin"] == f"task:{handle}"


def test_review_then_explicit_origin_overrides_reviewed_task(task_repo, monkeypatch):
    from spice.agent.driver import DRIVER

    from tests.test_tasks import PEER_ACTOR

    assert task_repo.is_dir()
    handle = _seed_task("Reviewed work with redirected follow-up")
    ops.claim(handle)
    ops.done(handle, validation=["implementation complete"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)

    ops.review(
        handle,
        finding="unclean",
        note="follow-up traces to the steering ack",
        then=[
            "title=Redirected follow-up | project=task.unit | "
            f"acceptance=Explicit origin wins | origin=ack:{ACK_KEY}"
        ],
    )

    followups = [
        row
        for row in tw.export(["status:pending"])
        if row.get("description") == "Redirected follow-up"
    ]
    assert len(followups) == 1
    assert followups[0]["origin"] == f"ack:{ACK_KEY}"


def test_capture_mint_new_records_origin(task_repo, monkeypatch):
    from spice.tasks import gitsync

    assert task_repo.is_dir()
    monkeypatch.setattr(gitsync, "commits_ahead_of_baseline", lambda *_a: 1)
    monkeypatch.setattr(tw, "require_clean_worktree", lambda *_a, **_k: None)

    output = ops.capture(project="task.unit", origin=f"ack:{ACK_KEY}")
    handle = output.splitlines()[0].split()[-1]

    assert identity.resolve(handle)["origin"] == f"ack:{ACK_KEY}"
