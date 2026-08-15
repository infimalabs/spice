"""Task dependency, readiness, deletion, and wake mutation behavior."""

from __future__ import annotations

import shutil

import pytest

from spice.errors import SpiceError
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.store import (
    TASK_FILTER_SOURCE_AUTO_CREATE,
    ServeTeamStore,
    TeamConfig,
)
from spice.tasks import (
    alloc,
    config,
    create,
    identity,
    ops,
    readiness,
    render,
    tw,
)
from tests.test_reposcaffolding import (
    init_committed_repo as _init_repo,
)
from tests.test_reposcaffolding import (
    make_task_repo_fixture,
)
from tests.test_teamstorehelpers import store_global_revision

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ACTOR_A_MEMBER = thread_actor_id(ACTOR_A)


task_repo = make_task_repo_fixture(lambda path: _init_repo(path), actor=ACTOR_A)


def _ready_handles() -> set[str]:
    rows = tw.export(["status:pending", "+READY", "-ACTIVE"])
    return {
        identity.render_handle(row)
        for row in rows
        if not alloc.is_hidden(row) and not str(row.get("claim_by") or "")
    }


def _rendered_dependency_handles(handle: str) -> list[str]:
    return sorted(
        line.split()[1]
        for line in render.render_show(handle).splitlines()
        if line.startswith("  after ")
    )


def test_task_depends_not_after_drops_one_edge_and_leaves_the_other(task_repo):
    handle = create.add(
        "Plan with two dependency edges",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    keep = create.add(
        "Edge that stays",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    drop = create.add(
        "Edge that goes",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    ops.depends(handle, [keep, drop])
    keep_uuid = identity.uuid_of(identity.resolve(keep))

    result = ops.depends(handle, [], not_after=[drop])

    row = identity.resolve(handle)
    assert result == handle
    assert row["depends"] == [keep_uuid]
    assert _rendered_dependency_handles(handle) == [keep]


def test_task_depends_not_after_clears_dangling_edge_after_dependency_deleted(
    task_repo,
):
    handle = create.add(
        "Plan pointing at a doomed dependency",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    doomed = create.add(
        "Dependency about to be deleted",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    ops.depends(handle, [doomed])
    doomed_uuid = identity.uuid_of(identity.resolve(doomed))
    ops.delete(doomed, "no longer needed")
    assert identity.resolve(handle)["depends"] == [doomed_uuid]

    result = ops.depends(handle, [], not_after=[doomed])

    row = identity.resolve(handle)
    assert result == handle
    assert row.get("depends", []) == []


def test_task_depends_not_after_rejects_an_absent_edge(task_repo):
    handle = create.add(
        "Plan with no such edge",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    other = create.add(
        "Unrelated task",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )

    with pytest.raises(SpiceError, match="does not depend on"):
        ops.depends(handle, [], not_after=[other])


def test_task_depends_repoints_an_edge_in_one_invocation(task_repo):
    handle = create.add(
        "Plan whose edge moves",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    old = create.add(
        "Superseded dependency",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    new = create.add(
        "Replacement dependency",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    ops.depends(handle, [old])
    new_uuid = identity.uuid_of(identity.resolve(new))

    result = ops.depends(handle, [new], not_after=[old])

    row = identity.resolve(handle)
    assert result == handle
    assert row["depends"] == [new_uuid]
    assert _rendered_dependency_handles(handle) == [new]


def test_task_depends_keeps_an_edge_dropped_and_readded_in_one_call(task_repo):
    handle = create.add(
        "Plan whose edge is dropped and re-added at once",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    dep = create.add(
        "Dependency removed and restored in one call",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    ops.depends(handle, [dep])
    dep_uuid = identity.uuid_of(identity.resolve(dep))

    result = ops.depends(handle, [dep], not_after=[dep])

    row = identity.resolve(handle)
    assert result == handle
    assert row["depends"] == [dep_uuid]


def test_task_depends_repoints_multiple_edges_in_one_invocation(task_repo):
    handle = create.add(
        "Plan whose two edges move together",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    old_edges = [
        create.add(
            f"Superseded dependency {index}",
            project="task.unit",
            origin="ack:1jN54zJJ",
        )
        for index in range(2)
    ]
    new_edges = [
        create.add(
            f"Replacement dependency {index}",
            project="task.unit",
            origin="ack:1jN54zJJ",
        )
        for index in range(2)
    ]
    ops.depends(handle, old_edges)
    new_uuids = sorted(identity.uuid_of(identity.resolve(edge)) for edge in new_edges)

    result = ops.depends(handle, new_edges, not_after=old_edges)

    row = identity.resolve(handle)
    assert result == handle
    assert sorted(row["depends"]) == new_uuids
    assert _rendered_dependency_handles(handle) == sorted(new_edges)


def test_task_depends_repeated_after_handle_lands_one_native_edge(task_repo):
    handle = create.add(
        "Plan given the same dependency twice",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    dep = create.add(
        "Dependency named twice in one call",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    dep_uuid = identity.uuid_of(identity.resolve(dep))

    result = ops.depends(handle, [dep, dep])

    row = identity.resolve(handle)
    assert result == handle
    assert row["depends"] == [dep_uuid]


def test_task_depends_rejects_every_oops_endpoint_before_mixed_batch_mutation(
    task_repo,
):
    parent = create.add(
        "Public parent keeps its original dependency",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    original = create.add(
        "Original dependency survives refused re-point",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    public = create.add(
        "Public replacement in refused mixed batch",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    plain_oops = ops.oops(
        "Hidden prerequisite one",
        description="first hidden endpoint",
        origin="ack:1jN54zJJ",
    ).split()[1]
    kind_oops = ops.oops(
        "Hidden prerequisite two",
        description="second hidden endpoint",
        kind="tooling",
        origin="ack:1jN54zJJ",
    ).split()[1]
    ops.depends(parent, [original])
    original_uuid = identity.uuid_of(identity.resolve(original))

    with pytest.raises(SpiceError) as exc_info:
        ops.depends(
            parent,
            [public, plain_oops, kind_oops],
            not_after=[original],
        )

    message = str(exc_info.value)
    assert f"{plain_oops} (project={config.OOPS_PROJECT}, state=waiting)" in message
    assert (
        f"{kind_oops} (project={config.OOPS_PROJECT}.tooling, state=waiting)" in message
    )
    assert f"spice task wake {plain_oops} --into PUBLIC_PROJECT" in message
    assert f"spice task wake {kind_oops} --into PUBLIC_PROJECT" in message
    assert identity.resolve(parent)["depends"] == [original_uuid]


def test_dependency_completion_stamps_queue_age_at_ready_transition(
    task_repo, monkeypatch
):
    blocker = create.add(
        "Dependency completed much later",
        project="task.unit",
        origin="ack:1jN54zJJ",
        flow=["todo"],
    )
    planned = create.add(
        "Long-planned work enters the queue later",
        project="task.unit",
        origin="ack:1jN54zJJ",
        after=[blocker],
    )
    before = identity.resolve(planned)
    transition = "2099-01-02T03:04:05.000000Z"
    monkeypatch.setattr(tw, "now_iso", lambda: transition)

    completed = ops._advance(identity.resolve(blocker))

    ready = identity.resolve(planned)
    assert completed == f"completed {blocker}"
    assert str(before.get(config.TASK_READY_AT_UDA) or "") == ""
    assert ready[config.TASK_READY_AT_UDA] == transition
    assert (
        readiness.queue_ready_epoch(ready)
        > identity.incepted_datetime(str(ready["incepted"])).timestamp()
    )
    assert planned in _ready_handles()


def test_queue_age_refreshes_each_time_task_reenters_ready(task_repo, monkeypatch):
    task = create.add(
        "Task that leaves and reenters the queue",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    blocker = create.add(
        "Temporary queue blocker",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    ops.depends(task, [blocker])
    dependency_release = "2099-02-03T04:05:06.000000Z"
    monkeypatch.setattr(tw, "now_iso", lambda: dependency_release)

    ops.depends(task, [], not_after=[blocker])

    first_ready = identity.resolve(task)
    assert first_ready[config.TASK_READY_AT_UDA] == dependency_release
    ops.claim(task)
    claimed = identity.resolve(task)
    assert str(claimed.get(config.TASK_READY_AT_UDA) or "") == ""

    claim_release = "2099-03-04T05:06:07.000000Z"
    monkeypatch.setattr(tw, "now_iso", lambda: claim_release)
    ops.unclaim(task)

    second_ready = identity.resolve(task)
    assert second_ready[config.TASK_READY_AT_UDA] == claim_release


def test_dependency_drop_publishes_ready_age_before_first_watcher_read(
    task_repo, monkeypatch
):
    task = create.add(
        "Long-planned dependency edit",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    blocker = create.add(
        "Dependency removed at queue admission",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    ops.depends(task, [blocker])
    task_uuid = identity.uuid_of(identity.resolve(task))
    blocker_uuid = identity.uuid_of(identity.resolve(blocker))
    transition = "2099-04-05T06:07:08.000000Z"
    observed: dict[str, object] = {}
    real_run = tw.run

    def observe_first_backend_wake(args, **kwargs):
        result = real_run(args, **kwargs)
        if f"depends:-{blocker_uuid}" in args:
            row = identity.resolve(task)
            observed.update(
                ready=readiness.is_ready(task_uuid),
                ready_at=row.get(config.TASK_READY_AT_UDA),
                queue_epoch=readiness.queue_ready_epoch(row),
            )
        return result

    monkeypatch.setattr(tw, "now_iso", lambda: transition)
    monkeypatch.setattr(tw, "run", observe_first_backend_wake)

    ops.depends(task, [], not_after=[blocker])

    assert observed == {
        "ready": True,
        "ready_at": transition,
        "queue_epoch": readiness.queue_ready_epoch(
            {config.TASK_READY_AT_UDA: transition}
        ),
    }


def test_task_delete_allows_unclaimed_task(task_repo):
    handle = create.add(
        "Delete unclaimed task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
    )
    uuid = identity.uuid_of(identity.resolve(handle))

    output = ops.delete(handle, "duplicate")
    row = tw.export([uuid])[0]

    assert output == handle
    assert row["status"] == "deleted"
    assert row["delete_reason"] == "duplicate"


def test_task_delete_refuses_live_claim_without_override(task_repo):
    handle = create.add(
        "Delete claimed task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
    )
    ops.claim(handle)

    with pytest.raises(SpiceError, match=f"cannot delete {handle}: live claim held"):
        ops.delete(handle, "duplicate")

    row = identity.resolve(handle)
    assert row["status"] == "pending"
    assert row["claim_by"] == ACTOR_A


def test_task_delete_force_claimed_logs_holder(task_repo):
    handle = create.add(
        "Force delete claimed task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
    )
    ops.claim(handle)
    claimed = identity.resolve(handle)
    uuid = identity.uuid_of(claimed)
    holder = (
        f"claim_by={ACTOR_A} claim_thread={ACTOR_A} "
        f"claim_until={claimed['claim_until']}"
    )

    output = ops.delete(handle, "duplicate", force_claimed=True)
    row = tw.export([uuid])[0]
    annotations = [str(item.get("description") or "") for item in row["annotations"]]

    assert output == f"warning: deleted {handle} despite live claim {holder}\n{handle}"
    assert row["status"] == "deleted"
    assert f"forced delete of live claim: {holder}" in annotations
    assert "deleted: duplicate" in annotations


def test_task_wake_clears_multiple_waits_and_makes_tasks_current(task_repo):
    handles = [
        create.add(
            f"Wake delayed task {index}",
            project="task.unit",
            origin="ack:1jN54zJJ",
            priority="medium",
            wait=config.DEFERRED_WAIT,
        )
        for index in range(4)
    ]
    delayed = [identity.resolve(handle) for handle in handles]

    assert all(row.get("wait") for row in delayed)
    assert not set(handles) & _ready_handles()

    output = ops.wake(handles)
    rows = [identity.resolve(handle) for handle in handles]

    for handle in handles:
        assert f"woke {handle}: wait:" in output
    assert "next: spice task next" in output
    assert all(not str(row.get("wait") or "") for row in rows)
    assert set(handles) <= _ready_handles()
    assert all(not str(row.get("claim_by") or "") for row in rows)
    assert all(not row.get("start") for row in rows)


def test_task_wake_rejects_batch_without_partial_clear(task_repo):
    delayed = create.add(
        "Wake batch delayed task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        wait=config.DEFERRED_WAIT,
    )
    claimed = create.add(
        "Wake batch claimed task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        claim=True,
    )

    with pytest.raises(SpiceError, match="active or claimed"):
        ops.wake([delayed, claimed])

    row = identity.resolve(delayed)
    assert row.get("wait")
    assert delayed not in _ready_handles()
    assert not str(row.get("claim_by") or "")
    assert not row.get("start")


def test_task_wake_refuses_deferred_oops_triage(task_repo):
    created = ops.oops(
        "Delayed oops remains triage",
        description="triage only",
        origin="ack:1jN54zJJ",
    )
    handle = created.split()[1]

    with pytest.raises(SpiceError, match="oops triage") as exc:
        ops.wake([handle])

    assert f"spice task claim {handle}" in str(exc.value)
    assert "already in plan mode" in str(exc.value)


def test_claimed_oops_must_be_promoted_before_it_can_depend_on_public_child(
    task_repo,
):
    created = ops.oops(
        "Claimed oops plan parent",
        description="triage promotes before wiring a public child",
        origin="ack:1jN54zJJ",
    )
    handle = created.split()[1]
    parent_uuid = identity.uuid_of(identity.resolve(handle))

    claimed = ops.claim(handle)
    child = create.add(
        "Public oops implementation child",
        project="task.unit",
        acceptance=["implementation child has an execution contract"],
    )

    with pytest.raises(SpiceError) as exc_info:
        ops.depends(handle, [child])

    refusal = str(exc_info.value)
    assert f"{handle} (project={config.OOPS_PROJECT}, state=claimed)" in refusal
    assert f"spice task wake {handle} --into PUBLIC_PROJECT" in refusal
    assert identity.resolve(handle).get("depends", []) == []

    promoted = ops.wake([handle], into="task.unit")
    parent_row = tw.export([parent_uuid])[0]
    public_handle = identity.render_handle(parent_row)
    ops.depends(public_handle, [child])

    child_row = identity.resolve(child)
    assert handle in claimed.splitlines()
    assert f"promoted {handle} -> {public_handle}" in promoted
    assert parent_row["project"] == "task.unit"
    assert parent_row["phase"] == "plan"
    assert parent_row["claim_by"] == ACTOR_A
    assert parent_row.get("start")
    assert not str(parent_row.get(config.TASK_READY_AT_UDA) or "")
    assert child_row["origin"] == f"task:{handle}"
    assert identity.uuid_of(child_row) in identity.resolve(public_handle)["depends"]


def test_task_oops_kind_routes_to_child_board_with_caller_tags_only(task_repo):
    created = ops.oops(
        "Kind routes to a child board",
        description="triage only",
        kind="Tooling",
        tags=["repro"],
        origin="ack:1jN54zJJ",
    )
    handle = created.split()[1]
    row = identity.resolve(handle)

    assert row["project"] == f"{config.OOPS_PROJECT}.tooling"
    assert row["phase"] == "plan"
    assert handle.startswith("TOOLING-")
    assert row.get("tags") == ["repro"]
    assert row.get("wait")


def test_task_wake_into_promotes_deferred_oops_into_public_project(task_repo):
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drive")
    )
    created = ops.oops(
        "Promote this oops into the queue",
        description="promotion candidate",
        origin="ack:1jN54zJJ",
    )
    handle = created.split()[1]
    assert identity.resolve(handle).get("wait")

    output = ops.wake([handle], into="task.unit")
    row = identity.resolve(handle)
    fresh = identity.render_handle(row)
    team_config = store.team_config(team.team_id)

    assert row["project"] == "task.unit"
    assert not str(row.get("wait") or "")
    assert row.get("tags", []) == []
    assert fresh != handle
    assert f"promoted {handle} -> {fresh}: wait:" in output
    assert "project:task.unit" in output
    assert "route_filter=added:task.unit:auto:create" in output
    assert team_config.task_filters == ("task.unit",)
    assert fresh in _ready_handles()


def test_task_wake_into_rejects_hidden_or_malformed_target_and_keeps_wait(task_repo):
    created = ops.oops(
        "Hidden target keeps deferral",
        description="triage only",
        origin="ack:1jN54zJJ",
    )
    handle = created.split()[1]
    project_before = str(identity.resolve(handle)["project"])

    with pytest.raises(SpiceError, match="hidden project stem"):
        ops.wake([handle], into=".oops.triage")
    with pytest.raises(SpiceError, match="at least"):
        ops.wake([handle], into="task")

    row = identity.resolve(handle)
    assert row.get("wait")
    assert str(row["project"]) == project_before


def test_task_wake_into_still_refuses_active_or_claimed(task_repo):
    claimed = create.add(
        "Promotion refuses claimed task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        claim=True,
    )

    with pytest.raises(SpiceError, match="active or claimed"):
        ops.wake([claimed], into="task.unit")


def test_drive_wake_auto_subscribes_woken_project(task_repo):
    handle = create.add(
        "Drive wakes delayed task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        wait=config.DEFERRED_WAIT,
        acceptance=["drive wake subscribes delayed work"],
    )
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drive")
    )
    before = store_global_revision(store)

    output = ops.wake([handle])
    team_config = store.team_config(team.team_id)

    assert store_global_revision(store) > before
    assert f"woke {handle}: wait:" in output
    assert "route_filter=added:task.unit:auto:create" in output
    assert team_config.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in team_config.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE}
    ]


def test_drain_wake_auto_subscribes_woken_project(task_repo):
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drain")
    )
    handle = create.add(
        "Drain wakes delayed task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        wait=config.DEFERRED_WAIT,
        acceptance=["drain wake subscribes delayed work"],
    )
    assert store.team_config(team.team_id).task_filters == ()
    before = store_global_revision(store)

    output = ops.wake([handle])
    team_config = store.team_config(team.team_id)

    assert store_global_revision(store) > before
    assert "route_filter=added:task.unit:auto:create" in output
    assert team_config.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in team_config.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE}
    ]


def test_steer_wake_keeps_preparation_only_boundary(task_repo):
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Steer")
    )
    handle = create.add(
        "Steer wakes delayed task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        wait=config.DEFERRED_WAIT,
        acceptance=["steer wake remains preparation only"],
    )
    before = store_global_revision(store)

    output = ops.wake([handle])

    assert store_global_revision(store) == before
    assert "route_filter=skipped:task.unit:lifetime:Steer" in output
    assert store.team_config(team.team_id).task_filters == ()
