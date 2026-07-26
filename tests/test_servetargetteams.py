"""Serve team identity contracts for unstarted worktree targets."""

from __future__ import annotations

from http import HTTPStatus
from pathlib import Path
import subprocess
from types import SimpleNamespace
from typing import Any

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

    result = _run_automatic_decision(state, target, "lifetime-scope")

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

    result = _run_automatic_decision(state, target, "drain-backlog")

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

    result = _run_automatic_decision(state, target, "operator-wake")

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
    assert _chrome_value(work_tree, "teamConfig")["teamIdentity"]["teamId"] == (
        created.team_id
    )
    assert _chrome_value(work_tree, "renewal")["lifetime"] == "Drain"
    assert _chrome_value(work_tree, "taskBoard")["taskFilters"] == ["serve.ui"]
    # Drain dissolves the boundary: the UI-facing effective set is every
    # assignable stem, even though the durable pin is only serve.ui.
    assert _chrome_value(work_tree, "taskBoard")["effectiveTaskFilters"] == sorted(
        config.assignable_stems()
    )
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
    assert _chrome_value(result, "teamConfig")["teamIdentity"]["teamId"] == (
        created.team_id
    )
    assert _chrome_value(result, "renewal")["lifetime"] == "Drain"
    assert _chrome_value(result, "taskBoard")["taskFilters"] == ["serve.ui"]
    assert _chrome_value(result, "taskBoard")["effectiveTaskFilters"] == sorted(
        config.assignable_stems()
    )
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
    assert _chrome_value(work_tree, "teamConfig")["teamIdentity"]["teamId"] == (
        created.team_id
    )
    renewal = _chrome_value(work_tree, "renewal")["renewalIntent"]
    assert renewal["agentId"] == ACTOR_A
    assert renewal["requested"] is True
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


def test_automatic_first_start_converges_identity_and_membership_once(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[f"target:{target.id}"])
    _record_target_identity(state, target)
    _patch_payload_dependencies(monkeypatch, thread_id="", running=False)
    status = SimpleNamespace(
        running=False,
        thread_id="",
        process_status="idle",
        started_at="",
    )
    for module in (agentapi, identity, lane, message, inventory, workroutes):
        monkeypatch.setattr(module, "agent_status", lambda _repo: status)
    ensure_calls: list[str] = []

    def ensure_pending(_target, **_kwargs):
        ensure_calls.append(target.id)
        status.thread_id = THREAD_A
        return {"ok": True, "trigger": "pending-inbox", "threadId": THREAD_A}

    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_pending_inbox",
        ensure_pending,
    )
    writes: list[str] = []
    assign_agent = state.team_store.assign_agent
    record_identity = state.team_store.record_agent_identity

    def counted_assign(*args, **kwargs):
        writes.append("membership")
        return assign_agent(*args, **kwargs)

    def counted_record(**kwargs):
        writes.append("identity")
        return record_identity(**kwargs)

    monkeypatch.setattr(state.team_store, "assign_agent", counted_assign)
    monkeypatch.setattr(state.team_store, "record_agent_identity", counted_record)

    first = lifecycle.submit_inbox_wake(state, target, "first-start").result()
    second = lifecycle.submit_inbox_wake(state, target, "second-observation").result()

    assert first.decision is not None
    assert second.decision is not None
    assert first.decision.thread_id == THREAD_A
    assert second.decision.thread_id == THREAD_A
    assert ensure_calls == [target.id, target.id]
    assert writes == ["membership", "identity"]
    assert [
        member.agent_id
        for member in state.team_store.team_state(created.team_id).members
    ] == [ACTOR_A]
    recorded = state.team_store.agent_identity_for_actor(ACTOR_A)
    assert recorded is not None
    assert recorded.target_id == target.id
    assert recorded.thread_id == THREAD_A

    def reject(*_args, **_kwargs):
        raise AssertionError("payload projection attempted a durable write")

    monkeypatch.setattr(state.team_store, "assign_agent", reject)
    monkeypatch.setattr(state.team_store, "record_agent_identity", reject)
    inventory_payload = inventory.work_trees_payload(state)["workTrees"][0]
    message_payload = message.messages_payload_for_worktree(state, target, limit=5)
    history_payload = message.messages_payload_for_worktree(
        state,
        target,
        limit=5,
        before="2026-01-01T00:00:00.000000Z#0",
        expected_thread_id=THREAD_A,
    )

    assert inventory_payload["targetIdentity"]["thread"]["threadId"] == THREAD_A
    assert message_payload["targetIdentity"]["thread"]["threadId"] == THREAD_A
    assert history_payload["targetIdentity"]["thread"]["threadId"] == THREAD_A
    assert (
        _chrome_value(inventory_payload, "teamConfig")["teamIdentity"]["teamId"]
        == created.team_id
    )
    assert (
        _chrome_value(message_payload, "teamConfig")["teamIdentity"]["teamId"]
        == created.team_id
    )


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
        "_evaluate_target_locked",
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
    assert all(_chrome_value(lane, "renewal")["lifetime"] == "Drain" for lane in lanes)
    assert all(_chrome_value(lane, "pendingInbox")["count"] == 1 for lane in lanes)
    assert all(
        _chrome_value(payload, "pendingInbox")["count"] == 1 for payload in messages
    )
    assert all(
        _chrome_value(lane, "renewal")["renewalIntent"]["agentId"] == ACTOR_A
        and _chrome_value(lane, "renewal")["renewalIntent"]["requested"] is True
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
    assert _chrome_value(work_tree, "teamConfig")["teamIdentity"]["teamId"] == (
        created.team_id
    )
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
    assert _chrome_value(result["route"], "teamConfig")["teamIdentity"]["teamId"] == (
        team_id
    )
    assert _chrome_value(result["route"], "taskBoard")["taskFilters"] == ["serve.ui"]
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
    assert (
        _chrome_value(result["route"], "teamConfig")["teamIdentity"]["teamId"]
        == created.team_id
    )
    assert _chrome_value(result["route"], "taskBoard")["taskFilters"] == ["serve.ui"]
    assert _chrome_value(result["route"], "renewal")["lifetime"] == "Drain"
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


def _run_automatic_decision(
    state: ServeState,
    target: WorktreeTarget,
    source_identity: str,
):
    outcome = lifecycle.submit_inbox_wake(
        state,
        target,
        source_identity,
    ).result(timeout=1.0)
    assert outcome.decision is not None
    return outcome.decision


def _record_target_identity(state: ServeState, target: WorktreeTarget) -> None:
    state.team_store.record_agent_identity(
        actor_id=f"target:{target.id}",
        target_id=target.id,
        thread_id="",
        actual_driver="",
        actual_model="",
        actual_effort="",
        desired_driver="codex",
        desired_model="gpt-next",
        desired_effort="high",
        transcript_owner="",
    )


def _chrome_value(payload: dict[str, Any], facet: str) -> dict[str, Any]:
    return payload["chrome"][facet]["value"]


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
