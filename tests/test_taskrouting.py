"""Team/lane routing: auto-subscription, filter GC, and lifetime visibility."""

from __future__ import annotations

import subprocess

from spice import paths
from spice.agent.driver import DRIVER
from spice.config import layers
from spice.serve.team.lifecycle import team_task_transitions
from spice.serve.team.store import (
    TASK_FILTER_SOURCE_AUTO_CLAIM,
    TASK_FILTER_SOURCE_AUTO_CREATE,
    TASK_FILTER_SOURCE_MANUAL,
    ServeTeamStore,
    TeamConfig,
)
from spice.tasks import alloc, config, create, identity, lanes, ops, projectsubs, render
from tests.test_teamstorehelpers import store_global_revision

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
        members=[ACTOR_A_MEMBER, PEER_ACTOR_MEMBER], config=TeamConfig(lifetime="Drive")
    )
    handle = create.add(
        "Manual claim out of lane",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["manual claim subscribes the project"],
    )

    claimed = ops.claim(handle)
    after_claim = store.team_config(team.team_id)

    assert handle in claimed.splitlines()
    assert after_claim.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in after_claim.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CLAIM},
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE},
    ]

    ops.done(handle, validation=["claim subscription routed review"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert assigned["claim_by"] == PEER_ACTOR


def test_lifetime_lens_reinterprets_same_stored_filters_without_writes(task_repo):
    """The slider is virtual: the same filter rows read differently per
    lifetime, and flipping lifetime never mutates the filter store."""
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER],
        config=TeamConfig(lifetime="Drive", task_filters=("serve.ui",)),
    )
    store.add_task_filter(
        team.team_id, "task.unit", source=TASK_FILTER_SOURCE_AUTO_CLAIM
    )

    def entries():
        return [
            entry.to_payload()
            for entry in store.team_config(team.team_id).task_filter_entries
        ]

    stored_entries = entries()
    drive_route = lanes.team_route_for_actor(ACTOR_A)
    assert drive_route is not None
    assert lanes.effective_filter_terms(drive_route) == [
        "project:serve.ui",
        "project:task.unit",
    ]

    store.update_team_config(
        team.team_id,
        TeamConfig(lifetime="Steer"),
        replace_task_filters=False,
    )
    steer_route = lanes.team_route_for_actor(ACTOR_A)
    assert steer_route is not None
    assert lanes.effective_filter_terms(steer_route) == ["project:serve.ui"]
    assert entries() == stored_entries

    store.update_team_config(
        team.team_id,
        TeamConfig(lifetime="Drain"),
        replace_task_filters=False,
    )
    drain_route = lanes.team_route_for_actor(ACTOR_A)
    assert drain_route is not None
    assert lanes.effective_filter_terms(drain_route) == [
        "project:serve",
        "project:task",
    ]
    assert entries() == stored_entries


def test_effective_filter_helpers_track_the_actor_route_per_lifetime(task_repo):
    """The team-driven helpers the UI payload uses reproduce the actor route's
    lifetime lens exactly, and expose bare project names for display."""
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER],
        config=TeamConfig(lifetime="Drive", task_filters=("serve.ui",)),
    )
    store.add_task_filter(
        team.team_id, "task.unit", source=TASK_FILTER_SOURCE_AUTO_CLAIM
    )

    def check(lifetime: str, terms: list[str], projects: list[str]) -> None:
        store.update_team_config(
            team.team_id, TeamConfig(lifetime=lifetime), replace_task_filters=False
        )
        state = store.team_state(team.team_id)
        route = lanes.team_route_for_actor(ACTOR_A)
        assert route is not None
        assert lanes.effective_filter_terms_for_team(state) == terms
        assert lanes.effective_filter_terms_for_team(state) == (
            lanes.effective_filter_terms(route)
        )
        assert lanes.effective_filter_projects_for_team(state) == projects

    # Drive uses the durable pin plus the auto-claim subscription.
    check("Drive", ["project:serve.ui", "project:task.unit"], ["serve.ui", "task.unit"])
    # Steer narrows to the manual pin only, dropping the auto subscription.
    check("Steer", ["project:serve.ui"], ["serve.ui"])
    # Drain dissolves the boundary to every assignable stem.
    check("Drain", ["project:serve", "project:task"], ["serve", "task"])


def test_steer_next_task_ignores_auto_subscriptions_but_honors_pins(
    task_repo,
):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER],
        config=TeamConfig(lifetime="Steer", task_filters=("serve.ui",)),
    )
    store.add_task_filter(
        team.team_id, "task.unit", source=TASK_FILTER_SOURCE_AUTO_CLAIM
    )
    create.add(
        "Auto-subscribed project task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="high",
        acceptance=["steer must not allocate through auto subscriptions"],
    )
    pinned = create.add(
        "Pinned project task",
        project="serve.ui",
        origin="ack:1jN54zJJ",
        priority="low",
        acceptance=["steer allocates through manual pins"],
    )

    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == pinned
    assert assigned["project"] == "serve.ui"


def test_steer_manual_claim_never_subscribes(task_repo):
    """Steer never auto-subscribes: manual claims stay claimable but must not
    widen the team filter set (the auto:claim ratchet)."""
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER, PEER_ACTOR_MEMBER], config=TeamConfig(lifetime="Steer")
    )
    handle = create.add(
        "Steer manual claim out of lane",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["steer manual claim leaves team filters untouched"],
    )
    before = store_global_revision(store)

    claimed = ops.claim(handle)
    after_claim = store.team_config(team.team_id)

    assert handle in claimed.splitlines()
    assert store_global_revision(store) == before
    assert after_claim.task_filters == ()
    assert after_claim.task_filter_entries == ()


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
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["auto claim leaves filter store unchanged"],
    )
    before = store_global_revision(store)

    assigned = alloc.next_task()
    after = store_global_revision(store)
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
        origin="ack:1jN54zJJ",
    )
    before = store_global_revision(store)

    claimed = ops.claim(handle)

    assert handle in claimed.splitlines()
    assert store_global_revision(store) == before
    assert store.team_config(team.team_id).task_filters == ()


def test_manual_claim_skips_subscription_for_teamless_actor(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    handle = create.add(
        "Teamless manual claim",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["teamless claims do not create subscriptions"],
    )
    before = store_global_revision(store)

    claimed = ops.claim(handle)

    assert handle in claimed.splitlines()
    assert store_global_revision(store) == before


def test_manual_claim_skips_oops_subscription(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Steer")
    )
    created = ops.oops(
        "Manual oops claim target",
        description="triage only",
        origin="ack:1jN54zJJ",
    )
    handle = created.split()[1]
    before = store_global_revision(store)

    claimed = ops.claim(handle)

    assert handle in claimed.splitlines()
    assert store_global_revision(store) == before
    assert store.team_config(team.team_id).task_filters == ()


def test_final_review_completion_gcs_auto_claim_filter(task_repo, monkeypatch):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A_MEMBER, PEER_ACTOR_MEMBER])
    handle = create.add(
        "Review keeps project subscribed",
        project="task.unit",
        origin="ack:1jN54zJJ",
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
        origin="ack:1jN54zJJ",
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
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["parent deletion keeps filter while child pending"],
    )
    child = create.add(
        "Child task",
        project="task.unit.child",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["child deletion empties parent subtree"],
    )

    ops.delete(parent, "parent abandoned")
    still_live = store.team_config(team.team_id)

    assert still_live.task_filters == ("task.unit", "task.unit.child")

    ops.delete(child, "child abandoned")
    emptied = store.team_config(team.team_id)
    after_empty_revision = store_global_revision(store)
    projectsubs._gc_empty_project_task_filters("task.unit")

    assert emptied.task_filters == ()
    assert emptied.task_filter_entries == ()
    assert store_global_revision(store) == after_empty_revision


def test_empty_project_gc_counts_waiting_tasks(task_repo):
    """Deferred work keeps a project subscribed: GC must not strip auto
    filters while waiting tasks will wake back into the project."""
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drive")
    )
    doomed = create.add(
        "Deleted while sibling waits",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["gc keeps filter while a waiting task remains"],
    )
    sleeper = create.add(
        "Wakes back into the project",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["waiting task holds the subscription"],
        deferred=True,
    )

    ops.delete(doomed, "abandoned while sibling defers")
    still_subscribed = store.team_config(team.team_id)

    assert still_subscribed.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in still_subscribed.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE}
    ]

    ops.wake([sleeper])
    ops.delete(sleeper, "abandoned after wake")
    emptied = store.team_config(team.team_id)

    assert emptied.task_filters == ()
    assert emptied.task_filter_entries == ()


def test_drive_task_creation_subscribes_project_idempotently(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drive")
    )

    first = create.add(
        "Drive creates first task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["drive creation subscribes"],
    )
    after_first = store_global_revision(store)
    after_first_config = store.team_config(team.team_id)
    second = create.add(
        "Drive creates second task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["duplicate drive creation is idempotent"],
    )
    after_second = store_global_revision(store)

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
    before = store_global_revision(store)

    handle = create.add(
        "Steer creates task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["steer creation does not auto-subscribe"],
    )

    assert identity.resolve(handle)["project"] == "task.unit"
    assert store_global_revision(store) == before
    assert store.team_config(team.team_id).task_filters == ()


def test_drain_task_creation_uses_effective_visibility_not_stored_filter(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drain")
    )
    before = store_global_revision(store)

    handle = create.add(
        "Drain creates task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["drain creation relies on computed visibility"],
    )

    assert identity.resolve(handle)["project"] == "task.unit"
    assert store_global_revision(store) == before
    assert store.team_config(team.team_id).task_filters == ()


def test_teamless_task_creation_routes_creator_without_team_subscription(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    before = store_global_revision(store)

    handle = create.add(
        "Teamless creates task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["teamless creation has no team subscription"],
    )

    assert identity.resolve(handle)["project"] == "task.unit"
    assert store_global_revision(store) == before
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
        origin="ack:1jN54zJJ",
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
        origin="ack:1jN54zJJ",
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


def test_status_resolves_one_route_scope_for_every_scoped_category(monkeypatch):
    route = {"lifetime": "Drive"}
    scope = ["(", "project:task.unit", "or", "origin_thread.is:actor-a", ")"]
    resolutions: list[tuple[str, object]] = []
    scoped_queries: list[tuple[tuple[str, ...], object]] = []

    def resolve_route(actor):
        resolutions.append(("route", actor))
        return route

    def resolve_scope(actor, resolved_route):
        resolutions.append(("scope", (actor, resolved_route)))
        return scope

    def active_rows(actor, *, scope):
        scoped_queries.append((("active", actor), scope))
        return []

    def ready_rows(actor, *, scope):
        scoped_queries.append((("ready", actor), scope))
        return [{"phase": "todo"}, {"phase": "review"}]

    def rows_in_scope(filters, resolved_scope):
        scoped_queries.append((tuple(filters), resolved_scope))
        return [{"status": "waiting"}]

    monkeypatch.setattr(render.tw, "current_actor", lambda: "actor-a")
    monkeypatch.setattr(render.tw, "now_iso", lambda: "2026-07-23T00:00:00Z")
    monkeypatch.setattr(render.lanes, "team_route_for_actor", resolve_route)
    monkeypatch.setattr(render.alloc, "effective_route_filter_args", resolve_scope)
    monkeypatch.setattr(render.alloc, "visible_active_rows", active_rows)
    monkeypatch.setattr(render.alloc, "visible_ready_rows", ready_rows)
    monkeypatch.setattr(render.alloc, "visible_rows_in_scope", rows_in_scope)
    monkeypatch.setattr(render.alloc, "is_hidden", lambda _row: False)
    monkeypatch.setattr(render.alloc, "oops_rows", lambda: [{}, {}])
    monkeypatch.setattr(
        render, "public_task_project_depth_label", lambda: "public depth"
    )

    status = render.render_status()

    assert resolutions == [
        ("route", "actor-a"),
        ("scope", ("actor-a", route)),
    ]
    assert scoped_queries == [
        (("active", "actor-a"), scope),
        (("ready", "actor-a"), scope),
        (("status:pending", "+BLOCKED"), scope),
        (("status:waiting", "-ACTIVE"), scope),
    ]
    assert status.splitlines() == [
        "claim -",
        "actor actor-a",
        "filter ( project:task.unit or origin_thread.is:actor-a )",
        "public depth",
        "active 0",
        "ready 1",
        "review 1",
        "blocked 1",
        "waiting 1",
        "stale 0",
        "oops 2",
    ]


def test_many_row_status_keeps_repo_and_config_resolution_constant(
    tmp_path, monkeypatch
):
    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    monkeypatch.chdir(tmp_path)
    actor = "actor-many-rows"
    route = {"lifetime": "Drain", "filter": [], "manual": []}
    waiting_rows = [{"project": ".oops", "status": "waiting"} for _ in range(128)]
    git_probes: list[list[str]] = []
    parsed: list[tuple[str, object]] = []
    run_git_command = paths.run_git_command
    read_toml = layers._read_toml

    def track_git(command, **kwargs):
        git_probes.append(list(command))
        return run_git_command(command, **kwargs)

    def track_parse(path, source_name):
        parsed.append((source_name, path))
        return read_toml(path, source_name)

    def scoped_rows(filters, _scope):
        if filters == ["status:waiting", "-ACTIVE"]:
            return waiting_rows
        return []

    monkeypatch.setattr(paths, "run_git_command", track_git)
    monkeypatch.setattr(layers, "_read_toml", track_parse)
    monkeypatch.setattr(render.tw, "current_actor", lambda: actor)
    monkeypatch.setattr(render.tw, "now_iso", lambda: "2026-07-23T00:00:00Z")
    monkeypatch.setattr(render.lanes, "team_route_for_actor", lambda _actor: route)
    monkeypatch.setattr(
        render.alloc, "visible_active_rows", lambda _actor, *, scope: []
    )
    monkeypatch.setattr(render.alloc, "visible_ready_rows", lambda _actor, *, scope: [])
    monkeypatch.setattr(render.alloc, "visible_rows_in_scope", scoped_rows)
    monkeypatch.setattr(render.alloc, "oops_rows", lambda: [])
    monkeypatch.setattr(
        render, "public_task_project_depth_label", lambda: "public depth"
    )

    status = render.render_status()

    assert "waiting 0" in status.splitlines()
    # One probe resolves the repository root and one resolves this worktree's
    # Git directory for its private configuration layer. Row count adds none.
    assert len(git_probes) == 2
    assert [source for source, _path in parsed] == [
        layers.SYSTEM_SOURCE,
        layers.REPOSITORY_SOURCE,
        layers.WORKTREE_SOURCE,
    ]


def test_drive_oops_creation_skips_subscription(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drive")
    )
    before = store_global_revision(store)

    created = ops.oops(
        "Drive oops creation",
        description="triage only",
        origin="ack:1jN54zJJ",
    )
    handle = created.split()[1]
    row = identity.resolve(handle)

    assert row["project"] == config.OOPS_PROJECT
    assert row["phase"] == "plan"
    assert row.get("tags", []) == []
    assert store_global_revision(store) == before
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
        origin="ack:1jN54zJJ",
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
        origin="ack:1jN54zJJ",
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
        facts = [
            transition
            for transition in team_task_transitions(connection, end_time=4_102_444_800)
            if transition.task_id == task_uuid
        ]

    assert identity.render_handle(assigned or {}) == handle
    assert identity.render_handle(review or {}) == handle
    # Completing the last phase is the movement that drains the task, so the
    # scripted lifecycle reads back as five movements and still counts one
    # completion and one drain.
    assert [str(fact.kind) for fact in facts] == [
        "claim",
        "phaseAdvance",
        "claim",
        "review",
        "complete",
    ]
    assert {fact.agent_id for fact in facts} == {ACTOR_A_MEMBER, PEER_ACTOR_MEMBER}
    assert {fact.team_id for fact in facts} == {team.team_id}
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
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["drain sees assignable public work"],
    )
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    private = create.add(
        "Peer private task",
        priority="medium",
        acceptance=["empty steer sees own private work"],
        origin="ack:1jN54zJJ",
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
    pinned = ["project:serve.ui"]
    private = f"project:{config.private_project(ACTOR_A)}"

    # Steer reads only the manual-pin layer of the same stored route.
    assert lanes.filter_args({"filter": stored, "lifetime": "Steer"}) == []
    assert (
        lanes.filter_args(
            {"filter": stored + pinned, "manual": pinned, "lifetime": "Steer"}
        )
        == pinned
    )
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
