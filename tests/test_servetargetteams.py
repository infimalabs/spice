"""Serve team identity contracts for unstarted worktree targets."""

from __future__ import annotations

from http import HTTPStatus
from pathlib import Path
import subprocess
from types import SimpleNamespace

import pytest

from spice.mail.inbox import compose_inbox_text, write_inbox_item
from spice.serve import agentapi, app, lifecycle, workroutes
from spice.serve.worktree import inventory
from spice.serve.payload import identity, lane, message
from spice.serve.app import ServeState
from spice.serve.lifecycle import start_lifecycle_reconciler
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.team.store import ServeTeamStore, TeamConfig
from spice.serve.workroutes import (
    work_tree_send_response_payload,
    work_tree_task_drain_response_payload,
)
from spice.serve.worktree import target
from spice.serve.worktree.target import WorktreeTarget
from spice.tasks import claimstate, config
from spice.worktrees import WorktreeRecord

THREAD_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ACTOR_A = f"thread:{THREAD_A}"


@pytest.mark.parametrize(
    ("lifetime", "expected_ensure"),
    [
        ("Drain", {"ok": True, "trigger": "available-work"}),
        ("Drive", None),
        ("Steer", None),
    ],
)
def test_available_work_expansion_is_scoped_to_drain_lifetime(
    tmp_path, monkeypatch, lifetime, expected_ensure
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    state.team_store.create_team(
        config=TeamConfig(lifetime=lifetime),
        members=[ACTOR_A],
    )
    _patch_payload_dependencies(monkeypatch, thread_id=THREAD_A, running=False)
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_available_work",
        lambda *_args, **_kwargs: {"ok": True, "trigger": "available-work"},
    )

    result = lifecycle.lifecycle_decision_authority(state).evaluate_target(
        target, thread_id=THREAD_A
    )

    assert result.agent_ensure == expected_ensure


def test_drain_expansion_passes_ready_backlog_policy_without_lane_capacity(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    state.team_store.create_team(
        config=TeamConfig(lifetime="Drain"),
        members=[ACTOR_A],
    )
    _patch_payload_dependencies(monkeypatch, thread_id=THREAD_A, running=False)
    observed: list[dict[str, object]] = []

    def capture_available_work(_target, **kwargs):
        observed.append(kwargs)
        return {"ok": True, "trigger": "available-work"}

    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_available_work",
        capture_available_work,
    )

    result = lifecycle.lifecycle_decision_authority(state).evaluate_target(
        target, thread_id=THREAD_A
    )

    assert result.agent_ensure == {"ok": True, "trigger": "available-work"}
    retry_due = observed[0]["retry_due"]
    assert callable(retry_due)
    assert observed == [
        {
            "thread_id": THREAD_A,
            "retry_due": retry_due,
            "fast_mode": False,
            "force_new": False,
        }
    ]


def test_operator_wake_bypasses_steer_available_work_gate(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    state.team_store.create_team(
        config=TeamConfig(lifetime="Steer"),
        members=[ACTOR_A],
    )
    _patch_payload_dependencies(monkeypatch, thread_id=THREAD_A, running=False)
    operator_wake = {
        "ok": True,
        "trigger": "pending-inbox",
        "threadId": THREAD_A,
    }
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: operator_wake,
    )

    result = lifecycle.lifecycle_decision_authority(state).evaluate_target(
        target, thread_id=THREAD_A
    )

    assert result.agent_ensure == operator_wake


def test_unstarted_target_id_membership_is_visible_in_target_payload(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(
        config=TeamConfig(lifetime="Drain", task_filters=("serve.ui",)),
        members=[f"target:{target.id}"],
    )
    _patch_payload_dependencies(monkeypatch, thread_id="", running=False)

    result = inventory.work_trees_payload(state)

    work_tree = result["workTrees"][0]
    assert work_tree["targetIdentity"]["thread"] == {"state": "unbound"}
    assert work_tree["teamIdentity"]["teamId"] == created.team_id
    assert work_tree["lifetime"] == "Drain"
    assert work_tree["taskFilters"] == ["serve.ui"]
    # Drain dissolves the boundary: the UI-facing effective set is every
    # assignable stem, even though the durable pin is only serve.ui.
    assert work_tree["effectiveTaskFilters"] == sorted(config.assignable_stems())
    assert [
        member.agent_id
        for member in state.team_store.team_state(created.team_id).members
    ] == [f"target:{target.id}"]


def test_unstarted_target_id_membership_is_visible_in_lane_payload(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(
        config=TeamConfig(lifetime="Drain", task_filters=("serve.ui",)),
        members=[f"target:{target.id}"],
    )
    _patch_payload_dependencies(monkeypatch, thread_id="", running=False)

    result = message.messages_payload_for_worktree(state, target, limit=5)
    signature = app.lane_signature_for_target(state, target, "", None)

    assert result["targetIdentity"]["thread"] == {"state": "unbound"}
    assert result["teamIdentity"]["teamId"] == created.team_id
    assert result["lifetime"] == "Drain"
    assert result["taskFilters"] == ["serve.ui"]
    assert result["effectiveTaskFilters"] == sorted(config.assignable_stems())
    assert signature.other[0] == created.team_id


def test_lifecycle_wake_rewrites_placeholder_membership_and_renewal_atomically(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[f"target:{target.id}"])
    _record_target_identity(state, target)
    state.team_store.set_agent_renewal_request(f"target:{target.id}", requested=True)
    ensure_calls: list[dict[str, object]] = []
    _patch_payload_dependencies(
        monkeypatch, thread_id=THREAD_A, running=False, ensure_calls=ensure_calls
    )

    lifecycle.submit_inbox_wake(state, target, "test-membership-revision").result()
    result = inventory.work_trees_payload(state)

    work_tree = result["workTrees"][0]
    members = state.team_store.team_state(created.team_id).members
    snapshot_members = state.team_store.team_snapshot().teams[0].members
    assert work_tree["targetIdentity"]["thread"] == {
        "state": "bound",
        "threadId": THREAD_A,
    }
    assert work_tree["teamIdentity"]["teamId"] == created.team_id
    assert work_tree["renewalIntent"]["agentId"] == ACTOR_A
    assert work_tree["renewalIntent"]["requested"] is True
    assert [member.agent_id for member in members] == [ACTOR_A]
    assert [member.agent_id for member in snapshot_members] == [ACTOR_A]
    renewal = snapshot_members[0].renewal
    assert renewal.agent_id == ACTOR_A
    retry_due = ensure_calls[0]["retry_due"]
    assert callable(retry_due)
    assert ensure_calls == [
        {
            "target": target,
            "retry_due": retry_due,
            "fast_mode": False,
            "force_new": True,
        }
    ]


def test_inventory_message_and_history_builders_are_pure_lifecycle_projections(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(
        config=TeamConfig(lifetime="Drain"),
        members=[ACTOR_A],
    )
    state.team_store.record_agent_identity(
        actor_id=ACTOR_A,
        target_id=target.id,
        thread_id=THREAD_A,
        actual_driver="codex",
        actual_model="gpt-test",
        actual_effort="low",
        actual_service_tier="fast",
        desired_driver="codex",
        desired_model="gpt-next",
        desired_effort="high",
        transcript_owner="codex",
    )
    state.team_store.set_agent_renewal_request(ACTOR_A, requested=True)
    write_inbox_item(
        repo,
        "1jN54zJK.txt",
        compose_inbox_text(body="pending operator steering", priority=None, stop=False),
    )
    _patch_payload_dependencies(monkeypatch, thread_id=THREAD_A, running=False)
    monkeypatch.setattr(
        inventory, "pending_inbox_identity_payload", pending_inbox_identity_payload
    )
    monkeypatch.setattr(
        message, "pending_inbox_identity_payload", pending_inbox_identity_payload
    )

    mutations: list[str] = []

    def reject(label):
        def rejected(*_args, **_kwargs):
            mutations.append(label)
            raise AssertionError(f"projection attempted lifecycle mutation: {label}")

        return rejected

    monkeypatch.setattr(
        lifecycle, "ensure_agent_for_pending_inbox", reject("pending ensure")
    )
    monkeypatch.setattr(
        lifecycle, "ensure_agent_for_available_work", reject("work ensure")
    )
    monkeypatch.setattr(agentapi, "deadletter_inbox_item", reject("inbox deadletter"))
    monkeypatch.setattr(
        lifecycle.LifecycleDecisionAuthority,
        "evaluate_target",
        reject("lifecycle evaluation"),
    )
    monkeypatch.setattr(claimstate, "do_claim", reject("task claim"))
    monkeypatch.setattr(claimstate, "release_claim", reject("task release"))
    monkeypatch.setattr(state.team_store, "assign_agent", reject("membership write"))
    monkeypatch.setattr(
        state.team_store, "record_agent_identity", reject("identity write")
    )
    monkeypatch.setattr(
        state.team_store, "record_pending_renewal", reject("pending renewal write")
    )
    monkeypatch.setattr(
        state.team_store, "record_started_renewal", reject("started renewal write")
    )

    inventories = [inventory.work_trees_payload(state) for _ in range(2)]
    messages = [
        message.messages_payload_for_worktree(state, target, limit=5) for _ in range(2)
    ]
    history = message.messages_payload_for_worktree(
        state,
        target,
        limit=5,
        before="2026-01-01T00:00:00.000000Z#0",
        expected_thread_id=THREAD_A,
    )

    lanes = [payload["workTrees"][0] for payload in inventories]
    assert mutations == []
    assert not hasattr(inventory, "ensure_work_tree_agent")
    assert [lane["agentEnsure"] for lane in lanes] == [{}, {}]
    assert [payload["agentEnsure"] for payload in messages] == [{}, {}]
    assert history["agentEnsure"] == {}
    assert all(lane["lifetime"] == "Drain" for lane in lanes)
    assert all(lane["pendingInboxCount"] == 1 for lane in lanes)
    assert all(payload["pendingInboxCount"] == 1 for payload in messages)
    assert all(
        lane["renewalIntent"]["agentId"] == ACTOR_A
        and lane["renewalIntent"]["requested"] is True
        for lane in lanes
    )
    assert [
        member.agent_id
        for member in state.team_store.team_state(created.team_id).members
    ] == [ACTOR_A]


def test_unstarted_target_ignores_stale_cached_thread_when_already_imported(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[ACTOR_A, f"target:{target.id}"])
    state.cached_thread_ids[target.id] = THREAD_A
    _patch_payload_dependencies(monkeypatch, thread_id="", running=False)

    result = inventory.work_trees_payload(state)

    work_tree = result["workTrees"][0]
    members = state.team_store.team_state(created.team_id).members
    assert work_tree["targetIdentity"]["thread"] == {"state": "unbound"}
    assert work_tree["teamIdentity"]["teamId"] == created.team_id
    assert [member.agent_id for member in members] == [ACTOR_A, f"target:{target.id}"]
    assert state.cached_thread_ids == {}


def test_task_drain_uses_unstarted_target_actor_without_binding_thread(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_payload_dependencies(monkeypatch, thread_id="", running=False)

    result, status = work_tree_task_drain_response_payload(
        state,
        target,
        {
            "replaceTaskFilters": True,
            "taskFilters": ["serve.ui", ""],
            "lifetime": "Drive",
        },
    )

    target_actor = f"target:{target.id}"
    team_id = state.team_store.current_team_for_agent(target_actor)
    assert status == HTTPStatus.OK
    assert result["route"]["actor"] == target_actor
    assert result["route"]["targetIdentity"]["thread"] == {"state": "unbound"}
    assert result["route"]["teamIdentity"]["teamId"] == team_id
    assert result["route"]["taskFilters"] == ["serve.ui"]
    assert [
        member.agent_id for member in state.team_store.team_state(team_id).members
    ] == [target_actor]


def test_unstarted_send_rewrites_placeholder_membership_to_ensured_thread(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(
        config=TeamConfig(lifetime="Drain", task_filters=("serve.ui",)),
        members=[f"target:{target.id}"],
    )
    _patch_payload_dependencies(monkeypatch, thread_id="", running=False)
    # The send's own launch is the subject here, so the explicit decision runs for
    # real against the ensure stub below; the fixture only holds automatic wakes.
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_pending_inbox",
        agentapi.ensure_agent_for_pending_inbox,
    )

    def fake_ensure(ensured_target, **kwargs):
        return {"ok": True, "threadId": THREAD_A}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_pending_inbox",
        agentapi.ensure_agent_for_pending_inbox,
    )

    result, status = work_tree_send_response_payload(
        state,
        target,
        {"text": "start this lane"},
    )

    members = state.team_store.team_state(created.team_id).members
    assert status == HTTPStatus.OK
    assert result["agentEnsure"]["threadId"] == THREAD_A
    assert result["route"]["actor"] == ACTOR_A
    assert result["route"]["targetIdentity"]["thread"] == {
        "state": "bound",
        "threadId": THREAD_A,
    }
    assert result["route"]["teamIdentity"]["teamId"] == created.team_id
    assert result["route"]["taskFilters"] == ["serve.ui"]
    assert result["route"]["lifetime"] == "Drain"
    assert [member.agent_id for member in members] == [ACTOR_A]


def test_worktree_discovery_failure_keeps_prior_targets_and_reports_it(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    state = ServeState(
        anchor_root=tmp_path,
        team_store=ServeTeamStore(path=tmp_path / "teams.sqlite3"),
    )
    _patch_payload_dependencies(monkeypatch, thread_id="", running=False)
    monkeypatch.setattr(
        target,
        "list_worktrees",
        lambda **_kwargs: [WorktreeRecord(path=repo, branch="refs/heads/main")],
    )

    healthy = inventory.work_trees_payload(state)

    def refuse_to_list(**_kwargs):
        raise RuntimeError("could not list git worktrees from repo")

    monkeypatch.setattr(target, "list_worktrees", refuse_to_list)
    # The live-bus targets push invalidates before every build, so the failure
    # lands with the per-build cache already cleared -- exactly the window in
    # which discovery used to collapse to [] and the client closed every lane.
    state.invalidate_targets()
    degraded = inventory.work_trees_payload(state)
    degraded_again = inventory.work_trees_payload(state)

    healthy_ids = [tree["id"] for tree in healthy["workTrees"]]
    assert healthy_ids == sorted(healthy_ids)
    assert len(healthy_ids) == 2
    assert healthy.get("targetsDiscoveryErrors", []) == []
    # The failed listing keeps the worktrees it last observed, so the payload
    # stays the same size and the client is told why rather than shown a gap.
    assert [tree["id"] for tree in degraded["workTrees"]] == healthy_ids
    assert degraded["defaultTargetId"] == healthy["defaultTargetId"]
    assert degraded["targetsDiscoveryErrors"] == [
        "could not list git worktrees from repo"
    ]
    assert degraded_again == degraded

    # Recovery is immediate: the next successful listing resumes enumeration
    # and clears the reported failure.
    monkeypatch.setattr(
        target,
        "list_worktrees",
        lambda **_kwargs: [WorktreeRecord(path=repo, branch="refs/heads/main")],
    )
    state.invalidate_targets()
    recovered = inventory.work_trees_payload(state)

    assert [tree["id"] for tree in recovered["workTrees"]] == healthy_ids
    assert recovered.get("targetsDiscoveryErrors", []) == []


def _repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir(exist_ok=True)
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    return repo


def _target(repo: Path) -> WorktreeTarget:
    return WorktreeTarget(id="target-1", repo_root=repo, name=repo.name, branch="main")


def _serve_state(tmp_path: Path, target: WorktreeTarget) -> ServeState:
    state = ServeState(
        anchor_root=tmp_path,
        team_store=ServeTeamStore(path=tmp_path / "teams.sqlite3"),
    )
    state.cached_targets = [target]
    # Active-mode Serve owns the reconciler every lane decision is submitted to.
    start_lifecycle_reconciler(state)
    return state


def _record_target_identity(state: ServeState, target: WorktreeTarget) -> None:
    state.team_store.record_agent_identity(
        actor_id=f"target:{target.id}",
        target_id=target.id,
        thread_id="",
        actual_driver="",
        actual_model="",
        actual_effort="",
        actual_service_tier="",
        desired_driver="codex",
        desired_model="gpt-next",
        desired_effort="high",
        transcript_owner="",
    )


def _pending_identity() -> dict[str, object]:
    return {
        "pendingInboxCount": 0,
        "pendingInboxLabel": "0",
        "pendingInboxKeys": [],
        "pendingInboxRevision": "test-revision",
        "pendingInboxVersion": 100,
    }


def _patch_payload_dependencies(
    monkeypatch,
    *,
    thread_id: str,
    running: bool,
    ensure_calls: list[dict[str, object]] | None = None,
) -> None:
    status = SimpleNamespace(
        running=running,
        thread_id=thread_id,
        process_status="running" if running else "idle",
        started_at="",
    )

    def fake_ensure(target, **kwargs):
        if ensure_calls is not None:
            ensure_calls.append({"target": target, **kwargs})
        return None

    monkeypatch.setattr(identity, "agent_status", lambda _repo: status)
    monkeypatch.setattr(lane, "agent_status", lambda _repo: status)
    monkeypatch.setattr(message, "agent_status", lambda _repo: status)
    monkeypatch.setattr(inventory, "agent_status", lambda _repo: status)
    monkeypatch.setattr(agentapi, "agent_status", lambda _repo: status)
    monkeypatch.setattr(workroutes, "agent_status", lambda _repo: status)
    monkeypatch.setattr(lane, "agent_binding_error", lambda *_args: "")
    monkeypatch.setattr(message, "agent_binding_error", lambda *_args: "")
    monkeypatch.setattr(inventory, "agent_binding_error", lambda *_args: "")
    monkeypatch.setattr(identity, "configured_say_voice", lambda _repo: "")
    empty_task_board = SimpleNamespace(
        task_filter_inventory={},
        active_claim=lambda _actor: None,
        task_card_rows=lambda _actor: (),
        completed_review_rows=lambda _actors: (),
        open_review_followup_count=lambda _uuid: 0,
        drained_task_count=lambda _actor: 0,
    )
    monkeypatch.setattr(message, "open_task_board_projection", lambda: empty_task_board)
    monkeypatch.setattr(
        inventory, "open_task_board_projection", lambda: empty_task_board
    )
    monkeypatch.setattr(
        message,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    monkeypatch.setattr(
        inventory,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    monkeypatch.setattr(lifecycle, "ensure_agent_for_pending_inbox", fake_ensure)
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: message.message_reader.AssistantMessageRead(
            items=[],
            error=None,
            transcript=None,
        ),
    )
