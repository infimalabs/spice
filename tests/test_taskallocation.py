"""Task allocation, stale takeover, and claim-lease behavior."""

from __future__ import annotations

import shutil
from datetime import datetime
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.mail.inbox import collect_inbox_items, inbox_request_body
from spice.tasks import alloc, claimstate, config, create, identity, ops, render, tw
from tests.test_tasks import ACTOR_A, PEER_ACTOR, task_repo

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

__all__ = ["task_repo"]


def test_do_claim_records_explicit_cross_worktree_site(task_repo):
    handle = create.add(
        "Claim work in a different lane",
        project="task.unit",
        origin="ack:1jN54zJJ",
        acceptance=["claim metadata identifies the target lane"],
    )
    row = identity.resolve(handle)
    target_site = claimstate.ClaimSite(
        task_repo.parent / "worktree-b",
        "main-b",
        "target-head-b",
    )

    claimed = claimstate.do_claim(
        identity.uuid_of(row),
        ACTOR_A,
        site=target_site,
        context_thread=ACTOR_A,
        lease_seconds=60.0,
    )
    fresh = identity.resolve(handle)
    next_row = alloc.next_task()

    assert claimed is True
    assert identity.render_handle(next_row) == handle
    assert config.repo_root() == task_repo
    assert Path(fresh["claim_worktree"]) == target_site.worktree
    assert fresh["claim_branch"] == target_site.branch
    assert fresh["claim_head"] == target_site.head
    assert fresh["claim_thread"] == ACTOR_A
    assert fresh["claim_lease_seconds"] == "60"


@pytest.mark.parametrize(
    (
        "initial_lease",
        "renewal_requests",
        "expected_leases",
    ),
    [
        (60.0, (600.0, 60.0), (600.0, 600.0)),
        (600.0, (60.0, 600.0), (600.0, 600.0)),
    ],
)
def test_renew_claim_uses_longest_requested_lease_in_either_order(
    task_repo,
    monkeypatch,
    initial_lease,
    renewal_requests,
    expected_leases,
):
    handle = create.add(
        "Keep the longest requested claim lease",
        project="task.unit",
        origin="ack:1jN54zJJ",
        acceptance=["renewal preserves the longest requested lease policy"],
    )
    row = identity.resolve(handle)
    site = claimstate.current_claim_site()
    claimstate.do_claim(
        identity.uuid_of(row),
        ACTOR_A,
        site=site,
        context_thread=ACTOR_A,
        lease_seconds=initial_lease,
    )
    monkeypatch.setattr(claimstate, "current_claim_site", lambda: site)

    results = []
    observed_leases = []
    for requested_lease in renewal_requests:
        results.append(
            claimstate.renew_claim(
                handle,
                actor=ACTOR_A,
                lease_seconds=requested_lease,
            )
        )
        observed_leases.append(identity.resolve(handle)["claim_lease_seconds"])
    expected_lease = expected_leases[-1]
    renewed = identity.resolve(handle)
    until = datetime.fromisoformat(renewed["claim_until"].replace("Z", "+00:00"))
    context_end = datetime.fromisoformat(
        renewed["claim_context_end"].replace("Z", "+00:00")
    )

    assert all(result.renewed for result in results)
    assert observed_leases == [f"{lease:g}" for lease in expected_leases]
    assert renewed["claim_lease_seconds"] == f"{expected_lease:g}"
    assert (context_end - until).total_seconds() == (
        config.CLAIM_CONTEXT_SECONDS - expected_lease
    )


def test_renew_claim_keeps_title_when_shared_taskrc_lacks_lease_uda(
    task_repo, monkeypatch
):
    title = "Renew without rewriting the task description"
    handle = create.add(
        title,
        project="task.unit",
        origin="ack:1jN54zJJ",
        acceptance=["claim renewal keeps the task title intact"],
    )
    ops.claim(handle)
    configured_taskrc = config.taskrc_path()
    downgraded_taskrc = configured_taskrc.with_name("taskrc-without-lease-uda")
    downgraded_taskrc.write_text(
        "\n".join(
            line
            for line in configured_taskrc.read_text(encoding="utf-8").splitlines()
            if not line.startswith("uda.claim_lease_seconds.")
        )
        + "\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(config, "bootstrap", lambda: downgraded_taskrc)

    result = claimstate.renew_claim(handle, actor=ACTOR_A, lease_seconds=3600.0)

    monkeypatch.setattr(config, "bootstrap", lambda: configured_taskrc)
    fresh = identity.resolve(handle)
    assert result.renewed is True
    assert fresh["description"] == title
    assert fresh["claim_lease_seconds"] == "3600"


def test_task_next_takes_over_stale_peer_claim(task_repo, monkeypatch):
    handle = create.add(
        "Stale takeover",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
    )
    uuid = identity.uuid_of(identity.resolve(handle))
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )

    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)

    # A fresh peer claim is untouchable: no READY work means no assignment.
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    assert alloc.next_task() is None

    # Once the deadline elapses the allocator reassigns without --steal.
    tw.run([uuid, "modify", "claim_until:2020-01-01T00:00:00.000000Z"])
    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert assigned["claim_by"] == ACTOR_A
    annotations = [
        str(entry.get("description") or "")
        for entry in tw.export([uuid])[0].get("annotations") or []
    ]
    assert any(
        f"stale claim reassigned: {PEER_ACTOR} -> {ACTOR_A}" in note
        for note in annotations
    )

    # The original owner returning late is refused cleanly.
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    with pytest.raises(SpiceError, match="not yours"):
        ops.done(handle, validation=["late owner attempt"])


def test_automatic_peer_stale_snapshot_preserves_winner_lease_and_notifies_old_lane(
    task_repo, tmp_path, monkeypatch
):
    handle = create.add(
        "Concurrent stale takeover",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
    )
    uuid = identity.uuid_of(identity.resolve(handle))
    displaced_lane = tmp_path / "displaced-lane"
    displaced_lane.mkdir()
    second_allocator = "cccccccccccccccccccccccccccccccc"

    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)
    tw.run(
        [
            uuid,
            "modify",
            "claim_until:2020-01-01T00:00:00.000000Z",
            f"claim_worktree:{displaced_lane}",
        ]
    )
    stale_snapshot = tw.export([uuid])[0]
    stale_until = str(stale_snapshot["claim_until"])

    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    first = alloc._take_over_stale([stale_snapshot], ACTOR_A, [stale_snapshot])
    # An automatically started peer selected the same row before the first
    # allocator committed, then reaches takeover with that cached snapshot.
    monkeypatch.setenv(DRIVER.thread_id_env, second_allocator)
    alloc._take_over_stale([stale_snapshot], second_allocator, [stale_snapshot])

    fresh = tw.export([uuid])[0]
    takeover_notes = [
        str(entry.get("description") or "")
        for entry in fresh.get("annotations") or []
        if str(entry.get("description") or "").startswith("stale claim reassigned:")
    ]
    notice_notes = [
        str(entry.get("description") or "")
        for entry in fresh.get("annotations") or []
        if str(entry.get("description") or "").startswith(
            "stale claim reassignment notice:"
        )
    ]
    inbox_items = collect_inbox_items(displaced_lane)

    assert identity.render_handle(first or {}) == handle
    assert fresh["claim_by"] == ACTOR_A
    assert fresh["claim_until"] > tw.now_iso()
    assert takeover_notes == [f"stale claim reassigned: {PEER_ACTOR} -> {ACTOR_A}"]
    assert len(notice_notes) == 1
    assert "delivered key=" in notice_notes[0]
    assert len(inbox_items) == 1
    assert inbox_request_body(inbox_items[0].text) == (
        f"[CLAIM] {handle} was reassigned from your lane ({PEER_ACTOR}) to "
        f"{ACTOR_A} after its recorded lease expired at "
        f"{stale_until}. Stop editing that task; capture any work that must "
        "continue before attempting to land it."
    )


def test_allocator_open_statuses_are_pending_and_waiting():
    statuses = ("pending", "waiting", "deleted", "completed")

    open_statuses = [
        status for status in statuses if alloc._is_open_task({"status": status})
    ]

    assert open_statuses == ["pending", "waiting"]


def test_task_next_routes_to_live_work_after_deleted_current_claim(
    task_repo, monkeypatch
):
    deleted_handle = create.add(
        "Deleted current claim",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
    )
    ready_handle = create.add(
        "Live work after deleted claim",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
    )
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )

    ops.claim(deleted_handle)
    ops.delete(deleted_handle, reason="duplicate", force_claimed=True)

    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == ready_handle
    assert str((assigned or {}).get("status") or "") == "pending"


def test_task_next_routes_around_deleted_unowned_active_row(task_repo, monkeypatch):
    handle = create.add(
        "Deleted unowned active row",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="high",
    )
    ready_handle = create.add(
        "Live work after deleted repair candidate",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="low",
    )
    uuid = identity.uuid_of(identity.resolve(handle))
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )

    ops.claim(handle)
    ops.delete(handle, reason="duplicate", force_claimed=True)
    tw.run([uuid, "modify", "claim_by:"])

    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == ready_handle
    deleted = tw.export([uuid])[0]
    assert deleted["status"] == "deleted"
    assert str(deleted.get("claim_by") or "") == ""


def test_task_next_takes_over_open_stale_claim_ahead_of_deleted_history(
    task_repo, monkeypatch
):
    deleted_handle = create.add(
        "Deleted stale peer claim",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="high",
    )
    open_handle = create.add(
        "Open stale peer claim",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="low",
    )
    deleted_uuid = identity.uuid_of(identity.resolve(deleted_handle))
    open_uuid = identity.uuid_of(identity.resolve(open_handle))
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )

    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(deleted_handle)
    tw.run([deleted_uuid, "modify", "claim_until:2020-01-01T00:00:00.000000Z"])
    ops.delete(deleted_handle, reason="duplicate", force_claimed=True)

    open_peer = "cccccccccccccccccccccccccccccccc"
    monkeypatch.setenv(DRIVER.thread_id_env, open_peer)
    ops.claim(open_handle)
    tw.run([open_uuid, "modify", "claim_until:2020-01-01T00:00:00.000000Z"])

    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == open_handle
    assert (assigned or {}).get("claim_by") == ACTOR_A
    deleted = tw.export([deleted_uuid])[0]
    assert deleted["status"] == "deleted"
    assert deleted["claim_by"] == PEER_ACTOR


def test_task_next_prefers_ready_work_over_stale_takeover(task_repo, monkeypatch):
    stale_handle = create.add(
        "Stale claim",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
    )
    ready_handle = create.add(
        "Fresh work",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
    )
    stale_uuid = identity.uuid_of(identity.resolve(stale_handle))
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )

    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(stale_handle)
    tw.run([stale_uuid, "modify", "claim_until:2020-01-01T00:00:00.000000Z"])

    # The TTL is never refreshed mid-task, so a slow lane looks dead; fresh
    # READY work must win before any takeover happens.
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == ready_handle
    assert tw.export([stale_uuid])[0]["claim_by"] == PEER_ACTOR


def test_task_next_repairs_active_claim_missing_owner(task_repo, monkeypatch):
    handle = create.add(
        "Repair partial active claim",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["active missing-owner claims are repaired"],
    )
    row = identity.resolve(handle)
    uuid = identity.uuid_of(row)
    tw.run([uuid, "modify", "start:now", *claimstate.CLAIM_CLEAR])
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )

    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert assigned["claim_by"] == ACTOR_A
    assert assigned["start"]


def test_active_claim_phase_reports_claimed_task_phase(task_repo):
    handle = create.add(
        "Report phase of an active claim",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["active_claim_phase reflects the claimed task's phase"],
    )
    ops.claim(handle)

    assert claimstate.active_claim_phase(ACTOR_A) == "todo"
    assert claimstate.active_claim_phase(PEER_ACTOR) == ""
    assert claimstate.active_claim_phase("") == ""


def test_renew_claim_refreshes_stale_own_active_claim(task_repo, monkeypatch):
    handle = create.add(
        "Renew my active claim",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["same-actor renewal refreshes the claim deadline and context"],
    )
    ops.claim(handle)
    claimed = identity.resolve(handle)
    uuid = identity.uuid_of(claimed)
    tw.run(
        [
            uuid,
            "modify",
            "claim_until:2020-01-01T00:00:00.000000Z",
            "claim_context_start:2020-01-01T00:00:00.000000Z",
            "claim_context_end:2020-01-01T00:00:00.000000Z",
            "claim_context_link:stale",
            "claim_context_turn:stale",
        ]
    )
    stale = identity.resolve(handle)
    renewal_site = claimstate.ClaimSite(
        task_repo,
        "renewal-branch",
        "renewal-head",
    )
    monkeypatch.setattr(claimstate, "current_claim_site", lambda: renewal_site)

    result = claimstate.renew_claim(handle)
    fresh = identity.resolve(handle)

    assert result.renewed is True
    assert result.reason == "renewed"
    assert result.handle == handle
    assert result.claim_until == fresh["claim_until"]
    assert fresh["claim_until"] > stale["claim_until"]
    assert fresh["claim_by"] == ACTOR_A
    assert fresh["claim_at"] == claimed["claim_at"]
    assert fresh["phase"] == claimed["phase"]
    assert Path(fresh["claim_worktree"]) == renewal_site.worktree
    assert fresh["claim_branch"] == renewal_site.branch
    assert fresh["claim_head"] == renewal_site.head
    assert fresh["claim_context_turn"] == "turn-a"
    assert fresh["claim_context_link"].startswith(f"spice-session://{ACTOR_A}?")


def test_renew_claim_without_active_claim_reports_no_active_claim(task_repo):
    handle = create.add(
        "Leave renewal unclaimed",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["renewal without an active claim is a no-op"],
    )

    result = claimstate.renew_claim()
    row = identity.resolve(handle)

    assert result == claimstate.ClaimRenewalResult(False, "no_active_claim")
    assert str(row.get("claim_by") or "") == ""
    assert str(row.get("claim_until") or "") == ""


def test_task_next_renewal_does_not_touch_peer_claim(task_repo, monkeypatch):
    peer_handle = create.add(
        "Peer claim stays untouched",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["task next renewal does not refresh another actor"],
    )
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(peer_handle)
    peer_before = identity.resolve(peer_handle)

    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    candidate = create.add(
        "Candidate for current actor",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["current actor can still claim ready work"],
    )

    output = render.render_next()
    peer_after = identity.resolve(peer_handle)
    candidate_after = identity.resolve(candidate)

    assert output.startswith("claim_renewal=skipped no_active_claim\n")
    assert peer_after["claim_by"] == PEER_ACTOR
    assert peer_after["claim_until"] == peer_before["claim_until"]
    assert candidate_after["claim_by"] == ACTOR_A


def test_renew_claim_refuses_stale_peer_claim_without_stealing(task_repo, monkeypatch):
    handle = create.add(
        "Do not renew peer claim",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["renewal refuses peer claims even when stale"],
    )
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)
    claimed = identity.resolve(handle)
    uuid = identity.uuid_of(claimed)
    tw.run([uuid, "modify", "claim_until:2020-01-01T00:00:00.000000Z"])

    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    result = claimstate.renew_claim(handle)
    fresh = identity.resolve(handle)

    assert result.reason == "claimed_by_other"
    assert result.detail == PEER_ACTOR
    assert fresh["claim_by"] == PEER_ACTOR
    assert fresh["claim_until"] == "2020-01-01T00:00:00.000000Z"


def test_renew_claim_refuses_same_actor_different_worktree(task_repo):
    handle = create.add(
        "Do not renew another worktree",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["renewal requires matching actor and worktree"],
    )
    ops.claim(handle)
    claimed = identity.resolve(handle)
    uuid = identity.uuid_of(claimed)
    tw.run([uuid, "modify", "claim_worktree:/tmp/spice-other-worktree"])

    result = claimstate.renew_claim(handle)
    fresh = identity.resolve(handle)

    assert result.reason == "different_worktree"
    assert fresh["claim_by"] == ACTOR_A
    assert fresh["claim_worktree"] == "/tmp/spice-other-worktree"
    assert fresh["claim_until"] == claimed["claim_until"]


def test_renew_claim_reports_missing_and_terminal_rows(task_repo):
    missing = claimstate.renew_claim("TASK-00000000")
    assert missing.reason == "missing"

    deleted = create.add(
        "Deleted renewal row",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["deleted rows report a renewal reason"],
    )
    ops.delete(deleted, "obsolete")
    deleted_result = claimstate.renew_claim(deleted)
    assert deleted_result.reason == "deleted"

    completed = create.add(
        "Completed renewal row",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        acceptance=["completed rows report a renewal reason"],
    )
    completed_uuid = identity.uuid_of(identity.resolve(completed))
    tw.run([completed_uuid, "done"])
    completed_result = claimstate.renew_claim(completed)
    assert completed_result.reason == "completed"


def test_renew_claim_reports_backend_failure(monkeypatch):
    def fail_export(*_args, **_kwargs):
        raise SpiceError("backend offline")

    monkeypatch.setattr(ops.tw, "export", fail_export)

    result = claimstate.renew_claim("TASK-00000000")

    assert result.reason == "backend_error"
    assert result.detail == "backend offline"


def _cross_lane_review(monkeypatch) -> str:
    """File a task as one lane, work its todo phase as another.

    The lanes disagree on purpose: `origin_thread` (rendered as
    creator_context) stays with the filing lane while `review_author` records
    the lane that produced the work. A guard that compares the creator passes
    this case, so it is the shape the refusal has to be proven against.
    """
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    handle = create.add(
        "Cross-lane handoff",
        project="task.unit",
        origin="ack:1jN54zJJ",
        priority="medium",
        flow=["todo", "review"],
    )
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    assert identity.render_handle(alloc.next_task() or {}) == handle
    ops.done(handle, validation=["todo phase complete"])
    return handle


def test_task_next_refuses_a_review_the_asking_actor_authored(task_repo, monkeypatch):
    handle = _cross_lane_review(monkeypatch)
    reviewable = identity.resolve(handle)

    refused = alloc.next_task()
    unclaimed = identity.resolve(handle)
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    reviewer_assignment = alloc.next_task()

    assert (
        reviewable["phase"],
        reviewable["review_author"],
        reviewable["origin_thread"],
    ) == ("review", ACTOR_A, PEER_ACTOR)
    assert refused is None
    assert str(unclaimed.get("claim_by") or "") == ""
    assert identity.render_handle(reviewer_assignment or {}) == handle
    assert reviewer_assignment["claim_by"] == PEER_ACTOR


def test_ready_surface_hides_a_review_from_its_author_and_keeps_it_for_peers(
    task_repo, monkeypatch
):
    handle = _cross_lane_review(monkeypatch)

    author_ready = [
        identity.render_handle(row) for row in alloc.visible_ready_rows(ACTOR_A)
    ]
    peer_ready = [
        identity.render_handle(row) for row in alloc.visible_ready_rows(PEER_ACTOR)
    ]

    assert author_ready == []
    assert peer_ready == [handle]
    assert author_ready != peer_ready
