"""Serve team identity contracts for unstarted worktree targets."""

from __future__ import annotations

from http import HTTPStatus
from pathlib import Path
from types import SimpleNamespace

from spice.serve import agentapi, app, workroutes
from spice.serve.worktree import inventory
from spice.serve.payload import identity, lane, message
from spice.serve.app import ServeState
from spice.serve.team.store import ServeTeamStore, TeamCommandService, TeamConfig
from spice.serve.workroutes import (
    work_tree_send_response_payload,
    work_tree_task_drain_response_payload,
)
from spice.serve.worktree.target import WorktreeTarget

THREAD_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ACTOR_A = f"thread:{THREAD_A}"


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
    assert signature.other[0] == created.team_id


def test_bound_target_rewrites_placeholder_membership_and_renewal_atomically(
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
    assert ensure_calls == [
        {
            "target": target,
            "pending": 0,
            "attempt_cache": state.pending_agent_ensure_attempts,
            "force_new": True,
        }
    ]


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

    def fake_ensure(ensured_target, **kwargs):
        return {"ok": True, "threadId": THREAD_A}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

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


def _repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir(exist_ok=True)
    return repo


def _target(repo: Path) -> WorktreeTarget:
    return WorktreeTarget(id="target-1", repo_root=repo, name=repo.name, branch="main")


def _serve_state(tmp_path: Path, target: WorktreeTarget) -> ServeState:
    state = ServeState(anchor_root=tmp_path)
    state.cached_targets = [target]
    state.team_store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    state.team_commands = TeamCommandService(state.team_store)
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

    def fake_ensure(target, pending, **kwargs):
        if ensure_calls is not None:
            ensure_calls.append({"target": target, "pending": pending, **kwargs})
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
    monkeypatch.setattr(message, "task_filter_inventory", lambda: {})
    monkeypatch.setattr(inventory, "task_filter_inventory", lambda: {})
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
    monkeypatch.setattr(inventory, "ensure_agent_for_pending_inbox", fake_ensure)
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: message.message_reader.AssistantMessageRead(
            items=[],
            error=None,
            transcript=None,
        ),
    )
