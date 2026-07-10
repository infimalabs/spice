"""Task claim-flow lifecycle: capture over loose commits, done/advance,
plan-phase gates, and claim renewal."""

from __future__ import annotations

import json
import shutil
from pathlib import Path
from types import SimpleNamespace

import pytest

from spice.cli.parser import build_parser
from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.hooks import precommit
from spice.sessions import learnings
from spice.tasks import alloc, claimstate, create, gitsync, identity, ops, render, tw
from tests.test_tasks import (
    ACTOR_A,
    _configure_git_identity,
    PEER_ACTOR,
    _git,
    _make_loose_commit,
    _run,
    remote_task_repo,
    task_repo,
)

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

__all__ = ["remote_task_repo", "task_repo"]


def test_task_capture_mints_task_over_loose_then_done_captures_it(remote_task_repo):
    loose = _make_loose_commit(remote_task_repo, subject="loose fix worth keeping")
    assert gitsync.commits_ahead_of_baseline(remote_task_repo) == 1

    output = ops.capture(project="task.unit", origin="ack:20260101T000000000000Z")
    handle = output.splitlines()[0].split()[-1]
    row = identity.resolve(handle)

    assert "captured 1 loose commit into" in output
    assert f"next: spice task done {handle}" in output
    assert row["claim_by"] == ACTOR_A
    assert bool(row["start"])
    assert row["description"] == "loose fix worth keeping"
    # The loose commit was preserved, not fast-forwarded away.
    assert _git(remote_task_repo, "rev-parse", "HEAD") == loose

    done_output = ops.done(handle, validation=["loose commit captured"])
    review_row = identity.resolve(handle)

    assert f"advanced {handle} -> review" in done_output
    assert review_row["done_head"] == loose
    assert (
        _git(remote_task_repo, "ls-remote", "origin", "refs/heads/main").split()[0]
        == review_row["done_merge_head"]
    )


def test_task_capture_can_complete_loose_in_one_shot(remote_task_repo):
    loose = _make_loose_commit(remote_task_repo, subject="one shot loose")

    output = ops.capture(
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        complete=True,
        validation=["one-shot validation"],
    )
    handle = output.splitlines()[0].split()[-1]
    review_row = identity.resolve(handle)

    assert "captured 1 loose commit into" in output
    assert f"advanced {handle} -> review" in output
    assert review_row["validation"] == "one-shot validation"
    assert review_row["done_head"] == loose


def test_task_capture_claims_existing_handle_over_loose(remote_task_repo):
    handle = create.add(
        "Pre-filed task awaiting its commit",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["loose commit is folded into this task"],
    )
    loose = _make_loose_commit(remote_task_repo)

    output = ops.capture(handle)
    row = identity.resolve(handle)

    assert f"captured 1 loose commit into {handle}" in output
    assert row["claim_by"] == ACTOR_A
    assert bool(row["start"])
    assert _git(remote_task_repo, "rev-parse", "HEAD") == loose


def test_task_capture_claims_existing_active_handle_over_loose(remote_task_repo):
    handle = create.add(
        "Active task awaiting loose commit",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["loose commit is folded into the active task"],
        claim=True,
    )
    loose = _make_loose_commit(remote_task_repo)

    output = ops.capture(handle)
    row = identity.resolve(handle)

    assert f"captured 1 loose commit into {handle}" in output
    assert row["claim_by"] == ACTOR_A
    assert bool(row["start"])
    assert row["claim_head"] == loose


def test_task_capture_deleted_handle_points_to_new_capture_task(remote_task_repo):
    handle = create.add(
        "Deleted task with loose work",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["deleted task recovery is explicit"],
    )
    ops.delete(handle, reason="duplicate")
    _make_loose_commit(remote_task_repo)

    with pytest.raises(SpiceError) as exc_info:
        ops.capture(handle)

    message = str(exc_info.value)
    assert f"cannot capture a deleted task: {handle}" in message
    assert "discard local work" in message
    assert "hand off" in message
    assert "do not capture the deleted handle" in message
    assert f"spice task capture --project task.unit --origin task:{handle}" in message


def test_task_capture_other_claimed_handle_points_to_new_capture_task(
    remote_task_repo, monkeypatch
):
    handle = create.add(
        "Peer claimed task with loose work",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["peer claimed task recovery is explicit"],
    )
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    _make_loose_commit(remote_task_repo)

    with pytest.raises(SpiceError) as exc_info:
        ops.capture(handle)

    message = str(exc_info.value)
    assert f"cannot capture {handle}: task already claimed by {PEER_ACTOR}" in message
    assert "discard local work" in message
    assert "hand off" in message
    assert f"spice task capture --project task.unit --origin task:{handle}" in message


def test_task_done_deleted_claim_points_to_recovery_paths(remote_task_repo):
    handle = create.add(
        "Deleted claimed task with loose work",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["deleted task done recovery is explicit"],
        claim=True,
    )
    ops.delete(handle, reason="duplicate", force_claimed=True)
    _make_loose_commit(remote_task_repo)

    with pytest.raises(SpiceError) as exc_info:
        ops.done(handle, validation=["loose work validated"])

    message = str(exc_info.value)
    assert f"cannot complete a deleted task: {handle}" in message
    assert "discard local work" in message
    assert "hand off" in message
    assert f"spice task capture --project task.unit --origin task:{handle}" in message


def test_task_capture_refuses_when_no_loose_commit(remote_task_repo):
    with pytest.raises(SpiceError, match="nothing to capture"):
        ops.capture(project="task.unit")


def test_task_add_claim_refuses_dirty_tree_without_creating_task(remote_task_repo):
    (remote_task_repo / "README.md").write_text("dirty\n", encoding="utf-8")

    with pytest.raises(SpiceError, match="commit or clear the working tree first"):
        create.add(
            "Dirty claim should not leak",
            project="task.unit",
            origin="ack:20260101T000000000000Z",
            claim=True,
        )

    rows = tw.export(["status:pending"])
    assert [
        row for row in rows if row.get("description") == "Dirty claim should not leak"
    ] == []


def test_task_add_claim_creates_and_claims_clean_task(task_repo):
    handle = create.add(
        "Clean claim lands",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        claim=True,
    )
    row = identity.resolve(handle)

    assert row["claim_by"] == ACTOR_A
    assert bool(row["start"])


def test_task_capture_rejects_handle_with_new_task_fields(remote_task_repo):
    handle = create.add(
        "Existing task",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["x"],
    )
    _make_loose_commit(remote_task_repo)
    with pytest.raises(SpiceError, match="either an existing <handle> or new-task"):
        ops.capture(handle, project="task.unit")


def test_task_capture_parser_accepts_done_with_validation():
    args = build_parser().parse_args(
        ["task", "capture", "--done", "--validation", "tests passed"]
    )

    assert args.task_action == "capture"
    assert args.done is True
    assert args.validation == ["tests passed"]


def test_task_done_review_flow_and_author_claim_separation(task_repo, monkeypatch):
    handle = create.add(
        "Exercise task phase flow",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["phase flow is covered"],
    )
    claimed = ops.claim(handle)
    head = _git(task_repo, "rev-parse", "HEAD")
    claimed_row = identity.resolve(handle)

    assert handle in claimed.splitlines()
    assert claimed_row["claim_by"] == ACTOR_A
    assert claimed_row["claim_head"] == head

    done_output = ops.done(handle, validation=["pytest task flow passed"])
    review_row = identity.resolve(handle)
    uuid = identity.uuid_of(review_row)

    assert f"advanced {handle} -> review" in done_output
    assert review_row["phase"] == "review"
    assert str(review_row["phase_i"]) == "1"
    assert review_row["review_author"] == ACTOR_A
    assert review_row["validation"] == "pytest task flow passed"
    assert review_row["done_head"] == head
    assert review_row["done_merge_head"] == head
    assert review_row["done_ref"] == head

    with pytest.raises(SpiceError, match="authored the review"):
        ops.claim(handle)

    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )
    assigned = alloc.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert assigned["claim_by"] == ACTOR_A

    review_output = ops.review(handle, finding="clean", note="review passed")
    completed_row = tw.export([uuid])[0]

    assert f"reviewed {handle} clean; completed {handle}" in review_output
    assert completed_row["status"] == "completed"
    assert completed_row["review_by"] == ACTOR_A
    assert completed_row["review_finding"] == "clean"
    assert completed_row["review_note"] == "review passed"


def test_task_next_takes_over_stale_peer_claim(task_repo, monkeypatch):
    handle = create.add(
        "Stale takeover",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
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


def test_task_next_prefers_ready_work_over_stale_takeover(task_repo, monkeypatch):
    stale_handle = create.add(
        "Stale claim",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
    )
    ready_handle = create.add(
        "Fresh work",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
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


def test_task_done_distills_and_reconfirms_project_stem_learning(
    task_repo, tmp_path, monkeypatch
):
    codex_home = tmp_path / "codex-home"
    monkeypatch.setenv("CODEX_HOME", str(codex_home))
    monkeypatch.setattr(
        learnings,
        "evaluate_maxim",
        lambda *_args, **_kwargs: SimpleNamespace(agrees=True),
    )

    first = _done_learning_task(task_repo, codex_home, "first-task")
    second = _done_learning_task(task_repo, codex_home, "second-task")
    records = learnings.load_learning_records(task_repo, "task")

    assert "learnings: stored 1 accepted from 1 candidate(s)" in first
    assert "learnings: stored 1 accepted from 1 candidate(s)" in second
    assert len(records) == 1
    assert records[0].statement == "Use spice task next after phase boundaries"
    assert records[0].project_stem == "task"
    assert records[0].confirmation_count == 2
    assert _git(task_repo, "status", "--porcelain") == ""


def test_task_done_advances_when_learning_transcript_is_missing(
    task_repo, tmp_path, monkeypatch
):
    monkeypatch.setenv("CODEX_HOME", str(tmp_path / "missing-codex-home"))
    handle = create.add(
        "Complete without transcript",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["task done remains non-fragile"],
    )
    ops.claim(handle)

    output = ops.done(handle, validation=["validated without transcript"])
    row = identity.resolve(handle)

    assert f"advanced {handle} -> review" in output
    assert "learnings: skipped missing_transcript" in output
    assert row["phase"] == "review"


def test_task_done_advances_when_learning_judge_is_unavailable(
    task_repo, tmp_path, monkeypatch
):
    codex_home = tmp_path / "codex-home"
    monkeypatch.setenv("CODEX_HOME", str(codex_home))

    def unavailable_judge(*_args, **_kwargs):
        raise SpiceError("could not launch 'afm-cli': missing")

    monkeypatch.setattr(learnings, "evaluate_maxim", unavailable_judge)
    handle = create.add(
        "Complete with unavailable learning judge",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["judge skip remains non-fragile"],
    )
    ops.claim(handle)
    claimed = identity.resolve(handle)
    _write_learning_transcript(
        codex_home,
        thread_id=ACTOR_A,
        turn_id="turn-judge-unavailable",
        timestamp=str(claimed["claim_at"]),
    )

    output = ops.done(handle, validation=["validated with unavailable judge"])
    row = identity.resolve(handle)

    assert f"advanced {handle} -> review" in output
    assert "learnings: skipped unavailable" in output
    assert row["phase"] == "review"


def test_plan_phase_show_injects_board_generation_guidance(task_repo):
    handle = create.add(
        "Plan a task arc",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        flow=["plan", "todo", "review"],
        acceptance=["plan bookend acceptance exists"],
    )

    shown = render.render_show(handle)

    assert "phase_guidance:" in shown
    assert "phase:plan decomposes the goal into connected child tasks" in shown
    assert "Add bookend acceptance on this plan task" in shown
    assert f'spice task done {handle} --validation "..."' in shown


def test_design_phase_show_injects_artifact_boundary_guidance(task_repo):
    handle = create.add(
        "Design a task arc",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        flow=["design", "plan", "todo", "review"],
        acceptance=["design surveys environment"],
    )

    shown = render.render_show(handle)

    assert "phase_guidance:" in shown
    assert "phase:design surveys the environment" in shown
    assert "docs/design/accepted/ or docs/design/experimental/" in shown
    assert "only phase that legitimizes committing design records" in shown
    assert "plan and other phases keep non-code reasoning on the board" in shown
    assert f'spice task done {handle} --validation "..."' in shown


def test_plan_phase_done_requires_connected_child_board(task_repo):
    handle = create.add(
        "Plan needs children",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        flow=["plan", "todo", "review"],
        acceptance=["parent bookend acceptance exists"],
    )
    ops.claim(handle)

    with pytest.raises(SpiceError, match="populate the board"):
        ops.done(handle, validation=["plan attempted without children"])

    row = identity.resolve(handle)
    assert row["phase"] == "plan"
    assert not str(row.get("validation") or "")


def test_plan_phase_done_requires_child_acceptance(task_repo):
    handle = create.add(
        "Plan needs accepted children",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        flow=["plan", "todo", "review"],
        acceptance=["parent bookend acceptance exists"],
    )
    child = create.add(
        "Unaccepted child",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
    )
    ops.depends(handle, [child])
    ops.claim(handle)

    with pytest.raises(SpiceError, match="child tasks missing acceptance"):
        ops.done(handle, validation=["plan attempted with incomplete child"])

    row = identity.resolve(handle)
    assert row["phase"] == "plan"
    assert not str(row.get("validation") or "")


def test_plan_phase_done_requires_bookend_acceptance(task_repo):
    handle = create.add(
        "Plan needs bookend acceptance",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        flow=["plan", "todo", "review"],
    )
    child = create.add(
        "Accepted child for unaccepted plan",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["child node has acceptance"],
    )
    ops.depends(handle, [child])
    ops.claim(handle)

    with pytest.raises(SpiceError, match="bookend acceptance"):
        ops.done(handle, validation=["plan attempted without bookend"])

    row = identity.resolve(handle)
    assert row["phase"] == "plan"
    assert not str(row.get("validation") or "")


def test_plan_phase_done_advances_after_board_population(task_repo):
    handle = create.add(
        "Plan has children",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        flow=["plan", "todo", "review"],
        acceptance=["parent bookend acceptance exists"],
    )
    child = create.add(
        "Accepted child",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["child node has acceptance"],
    )
    ops.depends(handle, [child])
    ops.claim(handle)

    output = ops.done(handle, validation=["plan board populated"])
    row = identity.resolve(handle)

    assert f"advanced {handle} -> todo" in output
    assert row["phase"] == "todo"
    assert str(row["phase_i"]) == "1"
    assert row["validation"] == "plan board populated"
    assert identity.uuid_of(identity.resolve(child)) in row["depends"]


def test_pre_commit_blocks_active_plan_phase_claim(task_repo):
    handle = _plan_task_with_accepted_child()
    ops.claim(handle)

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_plan_phase_mutation_guard(task_repo)

    message = str(exc_info.value)
    assert "git commit blocked" in message
    assert f"{handle} is in plan phase" in message
    assert "Plan phase output is board state" in message


def test_task_capture_blocks_plan_phase_loose_commit(remote_task_repo):
    handle = _plan_task_with_accepted_child()
    ops.claim(handle)
    _make_loose_commit(remote_task_repo, subject="plan implementation commit")

    with pytest.raises(SpiceError) as exc_info:
        ops.capture(project="task.unit", origin="ack:20260101T000000000000Z")

    message = str(exc_info.value)
    assert "task capture blocked" in message
    assert f"{handle} is in plan phase" in message
    assert "Claim an implementation child task" in message


def test_task_done_blocks_plan_phase_local_commits(remote_task_repo):
    handle = _plan_task_with_accepted_child()
    ops.claim(handle)
    _make_loose_commit(remote_task_repo, subject="plan implementation commit")

    with pytest.raises(SpiceError) as exc_info:
        ops.done(handle, validation=["plan board populated"])

    row = identity.resolve(handle)
    message = str(exc_info.value)
    assert "task done blocked" in message
    assert "Found 1 local commit ahead of the task baseline" in message
    assert row["phase"] == "plan"
    assert not str(row.get("validation") or "")


def test_plan_phase_done_allows_clean_baseline_fast_forward(remote_task_repo, tmp_path):
    handle = _plan_task_with_accepted_child()
    ops.claim(handle)
    remote_url = _git(remote_task_repo, "remote", "get-url", "origin")
    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", remote_url, str(peer))
    _configure_git_identity(peer)
    (peer / "baseline.txt").write_text("baseline work\n", encoding="utf-8")
    _run(peer, "git", "add", "baseline.txt")
    _run(peer, "git", "commit", "-m", "baseline work")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")

    output = ops.done(handle, validation=["plan board populated"])
    row = identity.resolve(handle)

    assert f"advanced {handle} -> todo" in output
    assert row["phase"] == "todo"
    assert row["validation"] == "plan board populated"
    assert row["done_merge_head"] == upstream_head
    assert _git(remote_task_repo, "rev-parse", "HEAD") == upstream_head


def test_task_next_repairs_active_claim_missing_owner(task_repo, monkeypatch):
    handle = create.add(
        "Repair partial active claim",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
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
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["active_claim_phase reflects the claimed task's phase"],
    )
    ops.claim(handle)

    assert claimstate.active_claim_phase(ACTOR_A) == "todo"
    assert claimstate.active_claim_phase(PEER_ACTOR) == ""
    assert claimstate.active_claim_phase("") == ""


def test_renew_claim_refreshes_stale_own_active_claim(task_repo):
    handle = create.add(
        "Renew my active claim",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
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
    assert fresh["claim_context_turn"] == "turn-a"
    assert fresh["claim_context_link"].startswith(f"spice-session://{ACTOR_A}?")


def test_renew_claim_without_active_claim_reports_no_active_claim(task_repo):
    handle = create.add(
        "Leave renewal unclaimed",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
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
        origin="ack:20260101T000000000000Z",
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
        origin="ack:20260101T000000000000Z",
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
        origin="ack:20260101T000000000000Z",
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
        origin="ack:20260101T000000000000Z",
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
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["deleted rows report a renewal reason"],
    )
    ops.delete(deleted, "obsolete")
    deleted_result = claimstate.renew_claim(deleted)
    assert deleted_result.reason == "deleted"

    completed = create.add(
        "Completed renewal row",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
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


def _plan_task_with_accepted_child() -> str:
    handle = create.add(
        "Plan mutation gate parent",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        flow=["plan", "todo", "review"],
        acceptance=["parent bookend acceptance exists"],
    )
    child = create.add(
        "Plan mutation gate child",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["child node has acceptance"],
    )
    ops.depends(handle, [child])
    return handle


def _done_learning_task(task_repo: Path, codex_home: Path, turn_id: str) -> str:
    handle = create.add(
        f"Distill learning {turn_id}",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["learning distillation is captured"],
    )
    ops.claim(handle)
    claimed = identity.resolve(handle)
    _write_learning_transcript(
        codex_home,
        thread_id=ACTOR_A,
        turn_id=turn_id,
        timestamp=str(claimed["claim_at"]),
    )
    output = ops.done(handle, validation=[f"validated {turn_id}"])
    assert _git(task_repo, "status", "--porcelain") == ""
    return output


def _write_learning_transcript(
    codex_home: Path,
    *,
    thread_id: str,
    turn_id: str,
    timestamp: str,
) -> Path:
    transcript = codex_home / "sessions" / f"rollout-{thread_id}.jsonl"
    transcript.parent.mkdir(parents=True, exist_ok=True)
    events: list[dict[str, object]] = [
        {
            "timestamp": timestamp,
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": turn_id},
        },
        {
            "timestamp": timestamp,
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [
                    {"text": ("Lesson: Use spice task next after phase boundaries.")}
                ],
            },
        },
        {
            "timestamp": timestamp,
            "type": "event_msg",
            "payload": {"type": "task_complete"},
        },
    ]
    transcript.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events),
        encoding="utf-8",
    )
    return transcript
