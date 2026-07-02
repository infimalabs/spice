"""Team/lane routing: auto-subscription, filter GC, and lifetime visibility."""

from __future__ import annotations


from spice.agent.driver import DRIVER
from spice.serve.team.store import (
    TASK_FILTER_SOURCE_AUTO_CLAIM,
    TASK_FILTER_SOURCE_AUTO_CREATE,
    TASK_FILTER_SOURCE_MANUAL,
    ServeTeamStore,
    TeamConfig,
)
from spice.tasks import alloc, config, create, identity, lanes, ops, render

from tests.test_tasks import (
    ACTOR_A,
    ACTOR_A_MEMBER,
    PEER_ACTOR,
    PEER_ACTOR_MEMBER,
    task_repo,
)

__all__ = ["task_repo"]


def test_manual_claim_subscribes_project_and_routes_review_to_teammate(
    task_repo, monkeypatch
):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER, PEER_ACTOR_MEMBER], config=TeamConfig(lifetime="Steer")
    )
    handle = create.add(
        "Manual claim out of lane",
        project="task.unit",
        priority="medium",
        acceptance=["manual claim subscribes the project"],
    )

    claimed = ops.claim(handle)
    after_claim = store.team_config(team.team_id)

    assert handle in claimed.splitlines()
    assert after_claim.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in after_claim.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CLAIM}
    ]

    ops.done(handle, validation=["claim subscription routed review"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert assigned["claim_by"] == PEER_ACTOR


def test_task_next_auto_claim_does_not_rewrite_team_filters(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER],
        config=TeamConfig(lifetime="Steer", task_filters=("task.unit",)),
    )
    handle = create.add(
        "Auto claim in lane",
        project="task.unit",
        priority="medium",
        acceptance=["auto claim leaves filter store unchanged"],
    )
    before = store.global_revision()

    assigned = alloc.next_task()
    after = store.global_revision()
    entries = store.team_config(team.team_id).task_filter_entries

    assert identity.render_handle(assigned or {}) == handle
    assert after == before
    assert [entry.to_payload() for entry in entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_MANUAL}
    ]


def test_manual_claim_skips_private_project_subscription(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Steer")
    )
    handle = create.add(
        "Private manual claim",
        priority="medium",
        acceptance=["private claims do not touch team filters"],
    )
    before = store.global_revision()

    claimed = ops.claim(handle)

    assert handle in claimed.splitlines()
    assert store.global_revision() == before
    assert store.team_config(team.team_id).task_filters == ()


def test_manual_claim_skips_subscription_for_teamless_actor(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    handle = create.add(
        "Teamless manual claim",
        project="task.unit",
        priority="medium",
        acceptance=["teamless claims do not create subscriptions"],
    )
    before = store.global_revision()

    claimed = ops.claim(handle)

    assert handle in claimed.splitlines()
    assert store.global_revision() == before


def test_manual_claim_skips_oops_subscription(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Steer")
    )
    created = ops.oops("Manual oops claim target", description="triage only")
    handle = created.split()[1]
    before = store.global_revision()

    claimed = ops.claim(handle)

    assert handle in claimed.splitlines()
    assert store.global_revision() == before
    assert store.team_config(team.team_id).task_filters == ()


def test_final_review_completion_gcs_auto_claim_filter(task_repo, monkeypatch):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A_MEMBER, PEER_ACTOR_MEMBER])
    handle = create.add(
        "Review keeps project subscribed",
        project="task.unit",
        priority="medium",
        acceptance=["review keeps auto filter until complete"],
    )
    ops.claim(handle)

    ops.done(handle, validation=["implementation leaves review pending"])
    review_config = store.team_config(team.team_id)

    assert review_config.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in review_config.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CLAIM},
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE},
    ]

    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)
    ops.review(handle, finding="clean", note="review complete")
    final_config = store.team_config(team.team_id)

    assert final_config.task_filters == ()
    assert final_config.task_filter_entries == ()


def test_empty_project_gc_removes_auto_sources_but_preserves_manual(
    task_repo, monkeypatch
):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER, PEER_ACTOR_MEMBER],
        config=TeamConfig(task_filters=("task.unit",)),
    )
    handle = create.add(
        "Manual filter survives auto gc",
        project="task.unit",
        priority="medium",
        acceptance=["manual task filter survives empty-project gc"],
    )
    ops.claim(handle)
    with_auto = store.team_config(team.team_id)

    assert [entry.to_payload() for entry in with_auto.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CLAIM},
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE},
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_MANUAL},
    ]

    ops.done(handle, validation=["implementation complete"])
    # Keep the manual source through the final review path while reclaiming auto.
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)
    ops.review(handle, finding="clean", note="manual survives")

    final_config = store.team_config(team.team_id)

    assert final_config.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in final_config.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_MANUAL}
    ]


def test_delete_gcs_empty_auto_create_filter_after_project_subtree_empties(
    task_repo,
):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A_MEMBER])
    store.add_task_filter(
        team.team_id, "task.unit", source=TASK_FILTER_SOURCE_AUTO_CREATE
    )
    parent = create.add(
        "Parent task",
        project="task.unit",
        priority="medium",
        acceptance=["parent deletion keeps filter while child pending"],
    )
    child = create.add(
        "Child task",
        project="task.unit.child",
        priority="medium",
        acceptance=["child deletion empties parent subtree"],
    )

    ops.delete(parent, "parent abandoned")
    still_live = store.team_config(team.team_id)

    assert still_live.task_filters == ("task.unit", "task.unit.child")

    ops.delete(child, "child abandoned")
    emptied = store.team_config(team.team_id)
    after_empty_revision = store.global_revision()
    ops._gc_empty_project_task_filters("task.unit")

    assert emptied.task_filters == ()
    assert emptied.task_filter_entries == ()
    assert store.global_revision() == after_empty_revision


def test_drive_task_creation_subscribes_project_idempotently(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drive")
    )

    first = create.add(
        "Drive creates first task",
        project="task.unit",
        priority="medium",
        acceptance=["drive creation subscribes"],
    )
    after_first = store.global_revision()
    after_first_config = store.team_config(team.team_id)
    second = create.add(
        "Drive creates second task",
        project="task.unit",
        priority="medium",
        acceptance=["duplicate drive creation is idempotent"],
    )
    after_second = store.global_revision()

    assert first != second
    assert after_first_config.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in after_first_config.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE}
    ]
    assert after_second == after_first


def test_steer_task_creation_keeps_manual_subscription_boundary(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Steer")
    )
    before = store.global_revision()

    handle = create.add(
        "Steer creates task",
        project="task.unit",
        priority="medium",
        acceptance=["steer creation does not auto-subscribe"],
    )

    assert identity.resolve(handle)["project"] == "task.unit"
    assert store.global_revision() == before
    assert store.team_config(team.team_id).task_filters == ()


def test_drain_task_creation_uses_effective_visibility_not_stored_filter(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drain")
    )
    before = store.global_revision()

    handle = create.add(
        "Drain creates task",
        project="task.unit",
        priority="medium",
        acceptance=["drain creation relies on computed visibility"],
    )

    assert identity.resolve(handle)["project"] == "task.unit"
    assert store.global_revision() == before
    assert store.team_config(team.team_id).task_filters == ()


def test_teamless_task_creation_routes_creator_without_team_subscription(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    before = store.global_revision()

    handle = create.add(
        "Teamless creates task",
        project="task.unit",
        priority="medium",
        acceptance=["teamless creation has no team subscription"],
    )

    assert identity.resolve(handle)["project"] == "task.unit"
    assert store.global_revision() == before
    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert store.current_team_for_agent(ACTOR_A) is None
    assert store.open_task_filter_projects() == ()


def test_teamless_creator_scope_does_not_route_peer_public_tasks(
    task_repo, monkeypatch
):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    handle = create.add(
        "Peer teamless public task",
        project="task.unit",
        priority="medium",
        acceptance=["origin scope is not global public visibility"],
    )

    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    assigned = alloc.next_task()

    assert identity.resolve(handle)["origin_thread"] == PEER_ACTOR
    assert assigned is None
    assert store.current_team_for_agent(ACTOR_A) is None
    assert store.current_team_for_agent(PEER_ACTOR) is None


def test_explicit_thread_membership_routes_peer_review_through_status_and_next(
    task_repo, monkeypatch
):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    store.create_team(
        members=[ACTOR_A_MEMBER],
        config=TeamConfig(lifetime="Drive", task_filters=("serve.ui",)),
    )
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    handle = create.add(
        "Peer serve review",
        project="serve.ui",
        priority="medium",
        acceptance=["explicit thread membership routes serve reviews"],
    )
    ops.claim(handle)
    ops.done(handle, validation=["implementation complete"])

    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    status = render.render_status()
    assigned = alloc.next_task()

    assert f"project:{config.private_project(ACTOR_A)}" in status
    assert f"origin_thread.is:{ACTOR_A}" in status
    assert "project:serve.ui" in status
    assert identity.render_handle(assigned or {}) == handle
    assert assigned["phase"] == "review"
    assert assigned["review_author"] == PEER_ACTOR
    assert assigned["claim_by"] == ACTOR_A


def test_explicit_thread_route_keeps_private_fallback_without_membership(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    store.create_team(
        members=[PEER_ACTOR_MEMBER],
        config=TeamConfig(lifetime="Drive", task_filters=("serve.ui",)),
    )
    private = f"project:{config.private_project(ACTOR_A)}"

    route = lanes.team_route_for_actor(ACTOR_A)
    status = render.render_status()
    filter_line = next(
        line for line in status.splitlines() if line.startswith("filter ")
    )

    assert alloc.effective_route_filter_args(ACTOR_A, route) == [
        "(",
        private,
        "or",
        f"origin_thread.is:{ACTOR_A}",
        ")",
    ]
    assert filter_line == f"filter ( {private} or origin_thread.is:{ACTOR_A} )"


def test_drive_oops_creation_skips_subscription(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drive")
    )
    before = store.global_revision()

    created = ops.oops("Drive oops creation", description="triage only")
    handle = created.split()[1]
    row = identity.resolve(handle)

    assert row["project"] == config.OOPS_PROJECT
    assert row["phase"] == "todo"
    assert row[config.PROJECT_HIDDEN_UDA] == "1"
    assert config.HIDDEN_TASK_TAG in row["tags"]
    assert store.global_revision() == before
    assert store.team_config(team.team_id).task_filters == ()


def test_drive_create_allocate_review_and_gc_capstone(task_repo, monkeypatch):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER, PEER_ACTOR_MEMBER], config=TeamConfig(lifetime="Drive")
    )
    handle = create.add(
        "Drive capstone task",
        project="task.unit",
        priority="medium",
        acceptance=["drive lifecycle capstone"],
    )
    after_create = store.team_config(team.team_id)

    assert after_create.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in after_create.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE}
    ]

    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == handle

    ops.done(handle, validation=["implementation complete"])
    review_pending = store.team_config(team.team_id)

    assert review_pending.task_filters == ("task.unit",)

    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    review = alloc.next_task()

    assert identity.render_handle(review or {}) == handle

    ops.review(handle, finding="clean", note="capstone review complete")
    after_review = store.team_config(team.team_id)

    assert after_review.task_filters == ()
    assert after_review.task_filter_entries == ()


def test_task_lifecycle_events_are_emitted_for_scripted_task_lifecycle(
    task_repo, monkeypatch
):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER, PEER_ACTOR_MEMBER], config=TeamConfig(lifetime="Drive")
    )
    handle = create.add(
        "Lifecycle metric task",
        project="task.unit",
        priority="medium",
        acceptance=["task lifecycle emits metric facts"],
    )

    assigned = alloc.next_task()
    task_uuid = identity.uuid_of(assigned or {})
    ops.done(handle, validation=["implementation complete"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    review = alloc.next_task()
    ops.review(handle, finding="clean", note="review complete")

    series = store.task_lifecycle_series(
        team_ids=[team.team_id], start=0, end=4_102_444_800
    )
    with store.connect() as connection:
        rows = connection.execute(
            "SELECT kind, task_id, agent_id, team_id FROM task_events "
            "WHERE task_id = ? ORDER BY rowid",
            (task_uuid,),
        ).fetchall()

    assert identity.render_handle(assigned or {}) == handle
    assert identity.render_handle(review or {}) == handle
    assert [str(row["kind"]) for row in rows] == [
        "claim",
        "phaseAdvance",
        "claim",
        "review",
        "complete",
        "drain",
    ]
    assert {str(row["agent_id"]) for row in rows} == {ACTOR_A_MEMBER, PEER_ACTOR_MEMBER}
    assert {str(row["team_id"]) for row in rows} == {team.team_id}
    assert (
        sum(point.claimed for point in series),
        sum(point.active for point in series),
        sum(point.completed for point in series),
        sum(point.drained for point in series),
    ) == (2, 2, 1, 1)


def test_drain_visibility_and_empty_steer_private_fail_closed(task_repo, monkeypatch):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    store.create_team(members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drain"))
    store.create_team(members=[PEER_ACTOR_MEMBER], config=TeamConfig(lifetime="Steer"))
    public = create.add(
        "Drain-visible public task",
        project="serve.ui",
        priority="medium",
        acceptance=["drain sees assignable public work"],
    )
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    private = create.add(
        "Peer private task",
        priority="medium",
        acceptance=["empty steer sees own private work"],
    )

    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    drain_assigned = alloc.next_task()

    assert identity.render_handle(drain_assigned or {}) == public
    assert drain_assigned["project"] == "serve.ui"

    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    steer_assigned = alloc.next_task()

    assert identity.render_handle(steer_assigned or {}) == private
    assert steer_assigned["project"] == config.private_project(PEER_ACTOR)


def test_lifetime_filter_args_use_single_visibility_contract(task_repo):
    assert task_repo.is_dir()
    stored = ["project:task.unit"]
    private = f"project:{config.private_project(ACTOR_A)}"

    assert lanes.filter_args({"filter": stored, "lifetime": "Steer"}) == stored
    assert lanes.filter_args({"filter": stored, "lifetime": "Drive"}) == stored
    assert lanes.filter_args({"filter": stored, "lifetime": "Drain"}) == [
        "(",
        "project:serve",
        "or",
        "project:task",
        ")",
    ]
    assert lanes.filter_args({"filter": [], "lifetime": "Steer"}) == []
    assert alloc.effective_route_filter_args(ACTOR_A, None) == [
        "(",
        private,
        "or",
        f"origin_thread.is:{ACTOR_A}",
        ")",
    ]
    assert alloc.effective_route_filter_args(
        ACTOR_A, {"filter": [], "lifetime": "Steer"}
    ) == [private]
    assert alloc.effective_route_filter_args(
        ACTOR_A, {"filter": stored, "lifetime": "Drain"}
    ) == [
        "(",
        private,
        "or",
        f"origin_thread.is:{ACTOR_A}",
        "or",
        "(",
        "project:serve",
        "or",
        "project:task",
        ")",
        ")",
    ]
