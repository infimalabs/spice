"""Task control-plane lifecycle, allocator, and git publication behavior."""

from __future__ import annotations

import shutil
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.paths import shared_attachment_root
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.store import (
    TASK_FILTER_SOURCE_AUTO_CREATE,
    ServeTeamStore,
    TeamConfig,
)
from spice.tasks import (
    alloc,
    claimstate,
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
from tests.test_reposcaffolding import (
    run as _run,
)
from tests.test_teamstorehelpers import store_global_revision

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
PEER_ACTOR = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
ACTOR_A_MEMBER = thread_actor_id(ACTOR_A)
PEER_ACTOR_MEMBER = thread_actor_id(PEER_ACTOR)


task_repo = make_task_repo_fixture(lambda path: _init_repo(path), actor=ACTOR_A)


@pytest.fixture
def remote_task_repo(tmp_path, monkeypatch):
    """A task-wired worktree with a real upstream baseline (origin/main)."""
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "repo")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-a")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def _make_loose_commit(
    repo: Path, name: str = "loose.txt", subject: str = "loose work"
) -> str:
    (repo / name).write_text(f"{subject}\n", encoding="utf-8")
    _run(repo, "git", "add", name)
    _run(repo, "git", "commit", "-m", subject)
    return _git(repo, "rev-parse", "HEAD")


def _ready_handles() -> set[str]:
    rows = tw.export(["status:pending", "+READY", "-ACTIVE"])
    return {
        identity.render_handle(row)
        for row in rows
        if not alloc.is_hidden(row) and not str(row.get("claim_by") or "")
    }


def test_task_modify_changes_priority_in_place(task_repo):
    handle = create.add(
        "Bump me",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="low",
    )
    assert identity.resolve(handle)["priority"] == "L"

    ops.modify(handle, priority="high")

    assert identity.resolve(handle)["priority"] == "H"


def test_task_modify_reassigns_project_in_place(task_repo):
    handle = create.add(
        "Move me",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
    )

    ops.modify(handle, project="task.moved")

    assert identity.resolve(handle)["project"] == "task.moved"


def test_task_modify_requires_at_least_one_field(task_repo):
    handle = create.add(
        "Leave me",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
    )
    with pytest.raises(SpiceError):
        ops.modify(handle)


def test_task_modify_replaces_acceptance_in_place(task_repo):
    handle = create.add(
        "Reword my acceptance",
        project="task.unit",
        origin="ack:1jN54zJJ",
        acceptance=["original criterion"],
    )

    ops.modify(handle, acceptance=["first bookend", "second bookend"])

    assert identity.resolve(handle)["acceptance"] == "first bookend | second bookend"


def test_task_modify_acceptance_unsticks_plan_phase(task_repo):
    handle = create.add(
        "Plan gains bookend acceptance via modify",
        project="task.unit",
        origin="ack:1jN54zJJ",
        flow=["plan", "todo", "review"],
    )
    child = create.add(
        "Unaccepted child for modified plan",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )
    ops.depends(handle, [child])
    ops.claim(handle)
    with pytest.raises(SpiceError, match="current task or connect at least one"):
        ops.done(handle, validation=["plan attempted without acceptance"])

    ops.modify(handle, acceptance=["plan bookend added via modify"])
    output = ops.done(handle, validation=["plan board populated"])

    row = identity.resolve(handle)
    assert f"advanced {handle} -> todo" in output
    assert row["phase"] == "todo"
    assert row["acceptance"] == "plan bookend added via modify"


def test_task_modify_acceptance_rejects_completed_task(task_repo):
    handle = create.add(
        "Complete before acceptance modify",
        project="task.unit",
        origin="ack:1jN54zJJ",
        flow=["todo"],
        acceptance=["complete once"],
    )
    ops.claim(handle)
    ops.done(handle, validation=["completed ahead of the modify attempt"])

    with pytest.raises(SpiceError, match="cannot modify acceptance for a completed"):
        ops.modify(handle, acceptance=["late criterion"])


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


def test_claimed_oops_is_plan_parent_for_public_child(task_repo):
    created = ops.oops(
        "In-place oops plan parent",
        description="triage builds a public child",
        origin="ack:1jN54zJJ",
    )
    handle = created.split()[1]

    claimed = ops.claim(handle)
    child = create.add(
        "Public oops implementation child",
        project="task.unit",
        acceptance=["implementation child has an execution contract"],
    )
    ops.depends(handle, [child])

    parent_row = identity.resolve(handle)
    child_row = identity.resolve(child)
    assert handle in claimed.splitlines()
    assert parent_row["phase"] == "plan"
    assert child_row["origin"] == f"task:{handle}"
    assert identity.uuid_of(child_row) in parent_row["depends"]


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


def test_task_add_stores_description_and_caps_title(task_repo):
    overlong = "A" * (create.TASK_TITLE_LIMIT + 1)
    with pytest.raises(SpiceError, match="move detail into --description"):
        create.add(
            overlong,
            project="task.unit",
            origin="ack:1jN54zJJ",
            priority="medium",
            acceptance=["title cap is enforced"],
        )

    body = "Longer context reviewers should keep current."
    handle = create.add(
        "Short subject",
        project="task.unit",
        origin="ack:1jN54zJJ",
        description=body,
        priority="medium",
        acceptance=["description is stored"],
    )
    row = identity.resolve(handle)

    assert row["description"] == "Short subject"
    assert row["task_description"] == body
    shown = render.render_show(handle)
    assert "title Short subject" in shown
    assert f"description {body}" in shown


@pytest.mark.parametrize(
    "title",
    ["Keep due:eom in the title", "Ordinary title remains unchanged"],
)
def test_task_add_treats_title_as_literal_text(task_repo, title):
    handle = create.add(
        title,
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="none",
    )
    row = identity.resolve(handle)

    assert row["description"] == title
    assert row.get("due", "") == ""


def test_task_add_preserves_shared_attachment_refs(task_repo):
    shared = shared_attachment_root(task_repo) / "digest" / "01-image.png"
    shared.parent.mkdir(parents=True, exist_ok=True)
    shared.write_bytes(b"shared-image")
    shared_ref = shared.as_posix()

    handle = create.add(
        "Preserve shared attachment references",
        project="task.unit",
        origin="ack:1jN54zJJ",
        description=f"Screenshot/reference attachment: {shared_ref}.",
        priority="medium",
        acceptance=[f"Open {shared_ref}."],
    )
    row = identity.resolve(handle)

    assert row["task_description"] == f"Screenshot/reference attachment: {shared_ref}."
    assert row["acceptance"] == f"Open {shared_ref}."
    assert shared.is_file()


def test_task_note_preserves_shared_attachment_refs(task_repo):
    handle = create.add(
        "Track attachment note",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["notes are normalized"],
    )
    shared = shared_attachment_root(task_repo) / "digest" / "02-image.png"
    shared.parent.mkdir(parents=True, exist_ok=True)
    shared.write_bytes(b"note-image")
    shared_ref = shared.as_posix()

    ops.note(
        handle,
        f"Screenshot reference: {shared_ref}",
    )
    shown = render.render_show(handle)

    assert f"Screenshot reference: {shared_ref}" in shown
    assert shared.is_file()


def test_repo_configured_per_stem_default_flow_feeds_task_add(task_repo):
    (task_repo / "pyproject.toml").write_text(
        "[tool.spice.tasks]\n"
        'stems = ["qa"]\n'
        "\n"
        "[tool.spice.tasks.flows]\n"
        'qa = ["todo", "verify", "review"]\n',
        encoding="utf-8",
    )

    handle = create.add(
        "Exercise configured flow",
        project="qa.pipeline",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["configured flow is applied"],
    )
    row = identity.resolve(handle)
    catalog = config.task_project_validation_catalog()

    assert config.resolve_flow(None, "qa.pipeline") == ["todo", "verify", "review"]
    assert claimstate.phases_of(row) == ["todo", "verify", "review"]
    assert catalog["perStemFlows"]["qa"] == ["todo", "verify", "review"]


def test_repo_configured_per_stem_default_flow_rejects_unknown_phase(task_repo):
    (task_repo / "pyproject.toml").write_text(
        "[tool.spice.tasks]\n"
        'stems = ["qa"]\n'
        "\n"
        "[tool.spice.tasks.flows]\n"
        'qa = ["todo", "ship", "review"]\n',
        encoding="utf-8",
    )

    with pytest.raises(SpiceError, match="phase 'ship' is not approved"):
        config.resolve_flow(None, "qa.pipeline")


def test_allocator_spreads_from_peer_cell_then_sticks_to_last_cell():
    ready = [
        _row("same-crowded", project="task.alpha", phase="todo", urgency=10),
        _row("different", project="task.beta", phase="todo", urgency=9),
        _row("same-project", project="task.alpha", phase="review", urgency=8),
        _row("outside-band", project="task.gamma", phase="todo", urgency=1),
    ]
    claimed = [
        _row(
            "last",
            project="task.alpha",
            phase="todo",
            urgency=1,
            claim_at="2026-01-01T00:00:00Z",
            claim_by=ACTOR_A,
        )
    ]
    active = [
        _row(
            "peer",
            project="task.alpha",
            phase="todo",
            urgency=1,
            claim_by=PEER_ACTOR,
        )
    ]

    ordered = alloc.order(ready, ACTOR_A, claimed, active)

    assert [row["description"] for row in ordered] == [
        "different",
        "same-project",
        "same-crowded",
        "outside-band",
    ]


def test_allocator_effective_priority_outranks_every_actor_history():
    prerequisite = _row(
        "low prerequisite",
        uuid="prerequisite",
        project="task.alpha",
        phase="todo",
        priority="L",
        urgency=1,
    )
    middle = _row(
        "middle",
        uuid="middle",
        project="task.alpha",
        phase="todo",
        priority="M",
        urgency=4,
        depends=["prerequisite"],
    )
    critical = _row(
        "critical descendant",
        uuid="critical",
        project="task.alpha",
        phase="todo",
        priority="C",
        urgency=8,
        depends=["middle"],
    )
    direct_high = _row(
        "direct high",
        uuid="direct-high",
        project="task.beta",
        phase="review",
        priority="H",
        urgency=100,
    )
    histories = [
        ([], []),
        (
            [
                _row(
                    "last",
                    project="task.beta",
                    phase="review",
                    urgency=1,
                    claim_at="2026-01-01T00:00:00Z",
                    claim_by=ACTOR_A,
                )
            ],
            [],
        ),
        (
            [
                _row(
                    "last",
                    project="task.beta",
                    phase="review",
                    urgency=1,
                    claim_at="2026-01-01T00:00:00Z",
                    claim_by=ACTOR_A,
                )
            ],
            [
                _row(
                    "peer",
                    project="task.alpha",
                    phase="todo",
                    urgency=1,
                    claim_by=PEER_ACTOR,
                )
            ],
        ),
    ]

    for claimed, active in histories:
        ordered = alloc.order(
            [direct_high, prerequisite],
            ACTOR_A,
            claimed,
            active,
            graph_rows=[prerequisite, middle, critical, direct_high],
        )
        assert [row["description"] for row in ordered] == [
            "low prerequisite",
            "direct high",
        ]


def test_allocator_downstream_weight_outranks_tags_urgency_and_locality():
    wide = _row(
        "wide",
        uuid="wide",
        project="task.alpha",
        phase="todo",
        priority="H",
        urgency=1,
        tags=["one"],
    )
    wide_child = _row(
        "wide child",
        uuid="wide-child",
        project="task.alpha",
        phase="todo",
        priority="L",
        urgency=1,
        depends=["wide"],
    )
    wide_grandchild = _row(
        "wide grandchild",
        uuid="wide-grandchild",
        project="task.alpha",
        phase="todo",
        priority="L",
        urgency=1,
        depends=["wide-child"],
    )
    narrow = _row(
        "narrow",
        uuid="narrow",
        project="task.beta",
        phase="review",
        priority="H",
        urgency=100,
        tags=["one", "two", "three", "four"],
    )
    claimed = [
        _row(
            "last",
            project="task.beta",
            phase="review",
            urgency=1,
            claim_at="2026-01-01T00:00:00Z",
            claim_by=ACTOR_A,
        )
    ]
    active = [
        _row(
            "peer",
            project="task.alpha",
            phase="todo",
            urgency=1,
            claim_by=PEER_ACTOR,
        )
    ]

    ordered = alloc.order(
        [narrow, wide],
        ACTOR_A,
        claimed,
        active,
        graph_rows=[wide, wide_child, wide_grandchild, narrow],
    )

    assert [row["description"] for row in ordered] == ["wide", "narrow"]


def test_alloc_classifies_oops_and_hidden_by_project_stem_alone():
    # Rows carry a project and nothing else -- no oops/hidden tags, no UDA.
    # Identity must ride the project stem alone.
    oops_kind = _row(
        "triage kind", project=".oops.correctness", phase="plan", urgency=1
    )
    maxim = _row(
        "proposal", project=config.MAXIM_PROPOSAL_PROJECT, phase="todo", urgency=1
    )
    public = _row("ordinary", project="task.alpha", phase="todo", urgency=1)

    # A .oops.<kind> descendant classifies as oops, and therefore hidden.
    assert alloc.is_oops(oops_kind)
    assert alloc.is_hidden(oops_kind)
    # .maxim_proposal is hidden but a distinct stem: its oops verdict differs.
    assert alloc.is_hidden(maxim)
    assert alloc.is_oops(maxim) != alloc.is_oops(oops_kind)
    # An ordinary public row differs from the hidden ones on both axes.
    assert alloc.is_hidden(public) != alloc.is_hidden(oops_kind)
    assert alloc.is_oops(public) != alloc.is_oops(oops_kind)


def test_oops_rows_returns_the_oops_project_hierarchy_against_a_real_backend(
    task_repo,
):
    # oops_rows fetches by the .oops project stem alone: the deferred triage
    # root and any .oops.<kind> descendant belong to it, while an ordinary
    # public task in its own project is a distinct, separately-resolved row.
    ops.oops("Root triage", origin="ack:1jN54zJJ")
    ops.oops("Kind triage", kind="Tooling", origin="ack:1jN54zJJ")
    public = create.add(
        "Ordinary work",
        project="task.unit",
        origin="ack:1jN54zJJ",
    )

    oops_projects = sorted(row["project"] for row in alloc.oops_rows())

    assert oops_projects == [
        config.OOPS_PROJECT,
        f"{config.OOPS_PROJECT}.tooling",
    ]
    assert identity.resolve(public)["project"] == "task.unit"


def test_task_review_help_requires_description_check(capsys):
    parser = build_parser()

    with pytest.raises(SystemExit):
        parser.parse_args(["task", "review", "--help"])

    help_text = capsys.readouterr().out
    assert "verify the task description is current" in help_text
    assert "description=..." in help_text
    assert (
        "Findings other than clean require durable follow-up tracking through "
        "either --then or --followup" in help_text
    )
    assert "adds the reviewed task as its dependency" in help_text


def test_unclean_review_requires_followup_tracking(task_repo, monkeypatch):
    handle = _review_claim(task_repo, monkeypatch)

    with pytest.raises(SpiceError, match="requires follow-up tracking"):
        ops.review(handle, finding="changes", note="needs work")

    row = identity.resolve(handle)
    assert row["phase"] == "review"
    assert str(row.get("review_by") or "") == ""


def test_unclean_review_spawns_dependent_followup(task_repo, monkeypatch):
    handle = _review_claim(task_repo, monkeypatch)
    reviewed_uuid = identity.uuid_of(identity.resolve(handle))

    output = ops.review(
        handle,
        finding="changes",
        note="needs coverage",
        then=[
            "title=Add review coverage | project=task.unit | "
            "acceptance=Regression covers the requested review change"
        ],
    )
    spawned = next(
        line.split()[1] for line in output.splitlines() if line.startswith("spawned ")
    )
    followup = identity.resolve(spawned)
    reviewed = tw.export([reviewed_uuid])[0]

    assert f"reviewed {handle} changes; completed {handle}" in output
    assert followup["description"] == "Add review coverage"
    assert reviewed_uuid in followup.get("depends", [])
    assert reviewed["status"] == "completed"
    assert reviewed["review_finding"] == "changes"


def test_unclean_review_passes_followups_to_feedback_bridge(task_repo, monkeypatch):
    handle = _review_claim(task_repo, monkeypatch)
    calls: list[dict[str, object]] = []

    def fake_feedback(row, *, finding, note, followups, reviewer, reviewed_at):
        calls.append(
            {
                "handle": identity.render_handle(row),
                "finding": finding,
                "note": note,
                "followups": list(followups),
                "reviewer": reviewer,
                "reviewed_at": reviewed_at,
            }
        )
        return ops.reviewfeedback.ReviewFeedbackResult(
            "delivered",
            "source=task-review",
            key="1jNJvRyn",
            target_repo_root=str(task_repo),
        )

    monkeypatch.setattr(ops.reviewfeedback, "emit_review_feedback", fake_feedback)

    output = ops.review(
        handle,
        finding="changes",
        note="needs coverage",
        then=[
            "title=Add review coverage | project=task.unit | "
            "acceptance=Regression covers the requested review change"
        ],
    )
    spawned = next(
        line.split()[1] for line in output.splitlines() if line.startswith("spawned ")
    )

    assert calls == [
        {
            "handle": handle,
            "finding": "changes",
            "note": "needs coverage",
            "followups": [spawned],
            "reviewer": PEER_ACTOR,
            "reviewed_at": calls[0]["reviewed_at"],
        }
    ]
    assert str(calls[0]["reviewed_at"])
    assert (
        "review-feedback delivered; key=1jNJvRyn; "
        f"target={task_repo}; source=task-review"
    ) in output


def test_review_rejects_a_bad_followup_record_before_creating_any(
    task_repo, monkeypatch
):
    """One malformed record leaves the board exactly as it found it."""
    handle = _review_claim(task_repo, monkeypatch)
    before = sorted(str(row.get("uuid") or "") for row in tw.export())

    with pytest.raises(SpiceError) as rejected:
        ops.review(
            handle,
            finding="changes",
            note="needs coverage",
            then=[
                "title=Well formed record | project=task.unit | "
                "acceptance=The first record carries everything it needs",
                "acceptance=The second record carries no title at all",
            ],
        )

    row = identity.resolve(handle)
    assert sorted(str(r.get("uuid") or "") for r in tw.export()) == before
    assert row["phase"] == "review"
    assert str(row.get("review_by") or "") == ""
    assert str(row.get("review_finding") or "") == ""
    assert "line 2: missing required field 'title'" in str(rejected.value)


def test_review_rejects_a_parser_clean_followup_record_before_creating_any(
    task_repo, monkeypatch
):
    """A record the parser accepts still writes nothing when creation rejects it.

    Review follow-ups parse with require_project false, so an omitted project
    survives the parse pass and is only refused deeper in, where a private
    project needs Steer lifetime. That rejection is context-dependent and the
    parser cannot see it, so preparation has to resolve every record before the
    review mutation or a well-formed earlier record lands beside a failure.
    """
    handle = _review_claim(task_repo, monkeypatch)
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drain"},
    )
    before = sorted(str(row.get("uuid") or "") for row in tw.export())

    with pytest.raises(SpiceError) as rejected:
        ops.review(
            handle,
            finding="changes",
            note="needs coverage",
            then=[
                "title=Well formed record | project=task.unit | "
                "acceptance=The first record carries everything it needs",
                "title=Parser-clean record | "
                "acceptance=The second record omits the project it needs",
            ],
        )

    row = identity.resolve(handle)
    assert sorted(str(r.get("uuid") or "") for r in tw.export()) == before
    assert row["phase"] == "review"
    assert str(row.get("review_by") or "") == ""
    assert str(row.get("review_finding") or "") == ""
    assert "requires Steer lifetime (got Drain)" in str(rejected.value)


def test_review_spawns_every_record_when_all_of_them_parse(task_repo, monkeypatch):
    """The rejection guard leaves multi-record creation working."""
    handle = _review_claim(task_repo, monkeypatch)

    output = ops.review(
        handle,
        finding="changes",
        note="needs coverage",
        then=[
            "title=First follow-up | project=task.unit | "
            "acceptance=The first record lands",
            "title=Second follow-up | project=task.unit | "
            "acceptance=The second record lands",
        ],
    )
    spawned = [
        line.split()[1] for line in output.splitlines() if line.startswith("spawned ")
    ]
    titles = [identity.resolve(each)["description"] for each in spawned]

    assert titles == ["First follow-up", "Second follow-up"]
    assert spawned[0] != spawned[1]


def test_unclean_review_links_existing_followup(task_repo, monkeypatch):
    handle = _review_claim(task_repo, monkeypatch)
    reviewed_uuid = identity.uuid_of(identity.resolve(handle))
    existing = create.add(
        "Existing review follow-up",
        project="task.unit",
        origin="ack:1jN54zJJ",
        acceptance=["Tracks the requested review change"],
    )

    output = ops.review(
        handle,
        finding="changes",
        note="use existing task",
        followup=[existing],
    )
    followup = identity.resolve(existing)
    reviewed = tw.export([reviewed_uuid])[0]

    assert f"reviewed {handle} changes; completed {handle}" in output
    assert f"linked {existing}" in output
    assert reviewed_uuid in followup.get("depends", [])
    assert reviewed["status"] == "completed"
    assert reviewed["review_finding"] == "changes"


def _row(
    description: str,
    *,
    project: str,
    phase: str,
    urgency: float,
    uuid: str = "",
    priority: str = "",
    depends: list[str] | None = None,
    tags: list[str] | None = None,
    claim_at: str = "",
    claim_by: str = "",
) -> dict[str, object]:
    return {
        "description": description,
        "uuid": uuid,
        "project": project,
        "phase": phase,
        "priority": priority,
        "depends": depends or [],
        "tags": tags or [],
        "urgency": urgency,
        "claim_at": claim_at,
        "claim_by": claim_by,
    }


def _configure_git_identity(repo: Path) -> None:
    _run(repo, "git", "config", "user.email", "spice@example.test")
    _run(repo, "git", "config", "user.name", "Spice Tests")


def _review_claim(task_repo: Path, monkeypatch) -> str:
    assert task_repo.is_dir()
    handle = create.add(
        "Review follow-up invariant",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["review follow-up tracking is enforced"],
    )
    ops.claim(handle)
    ops.done(handle, validation=["implementation validated"])
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    assigned = alloc.next_task()
    assert identity.render_handle(assigned or {}) == handle
    return handle


def test_bound_quality_gate_blocks_completion_while_metric_nonzero(
    tmp_path, monkeypatch
):
    repo = tmp_path / "repo"
    (repo / "spice").mkdir(parents=True)
    (repo / "tests").mkdir()
    (repo / "spice" / "foo.py").write_text("_secret = 1\n", encoding="utf-8")
    (repo / "tests" / "test_foo.py").write_text(
        "from spice.foo import _secret\n\n"
        "def test_secret():\n    assert _secret == 1\n",
        encoding="utf-8",
    )
    monkeypatch.setattr("spice.tasks.ops.repo_root_from_cwd", lambda: repo)

    # A task bound to the coupling gate cannot complete while the gate is dirty.
    with pytest.raises(SpiceError, match="bound to a quality gate"):
        ops._require_bound_quality_gates_clean({"tags": ["gate:coupling"]})

    # An untagged task is unaffected even when the same repo is dirty.
    ops._require_bound_quality_gates_clean({"tags": ["plain"]})

    # Once the coupling is gone, the bound gate passes.
    (repo / "tests" / "test_foo.py").write_text(
        "def test_nothing():\n    assert True\n", encoding="utf-8"
    )
    ops._require_bound_quality_gates_clean({"tags": ["gate:coupling"]})


def _git(repo: Path, *args: str) -> str:
    return _run(repo, "git", *args).stdout.strip()
