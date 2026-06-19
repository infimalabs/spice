"""Serve team identity contracts for unstarted worktree targets."""

from __future__ import annotations

from http import HTTPStatus
from pathlib import Path
from types import SimpleNamespace

from spice.serve import agentapi, app, payloads
from spice.serve.app import (
    ServeState,
    work_tree_send_response_payload,
    work_tree_task_drain_response_payload,
)
from spice.serve.teams import ServeTeamStore, TeamCommandService, TeamConfig
from spice.serve.worktrees import WorktreeTarget

THREAD_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


def test_unstarted_target_id_membership_is_visible_in_target_payload(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(
        config=TeamConfig(lifetime="Drain", task_filters=("serve",)),
        members=[target.id],
    )
    _patch_payload_dependencies(monkeypatch, thread_id="", running=False)

    result = payloads.work_trees_payload(state)

    work_tree = result["workTrees"][0]
    assert work_tree["targetIdentity"]["thread"] == {"state": "unbound"}
    assert work_tree["teamIdentity"]["teamId"] == created.team_id
    assert work_tree["lifetime"] == "Drain"
    assert work_tree["taskFilters"] == ["serve"]
    assert [
        member.agent_id
        for member in state.team_store.team_state(created.team_id).members
    ] == [target.id]


def test_unstarted_target_id_membership_is_visible_in_lane_payload(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(
        config=TeamConfig(lifetime="Drain", task_filters=("serve",)),
        members=[target.id],
    )
    _patch_payload_dependencies(monkeypatch, thread_id="", running=False)

    result = payloads.messages_payload_for_worktree(state, target, limit=5)
    signature = app.lane_signature_for_target(state, target, "", None)

    assert result["targetIdentity"]["thread"] == {"state": "unbound"}
    assert result["teamIdentity"]["teamId"] == created.team_id
    assert result["lifetime"] == "Drain"
    assert result["taskFilters"] == ["serve"]
    assert signature[2][0] == created.team_id


def test_bound_target_rewrites_placeholder_membership_and_renewal_atomically(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[target.id])
    state.team_store.set_agent_renewal_request(target.id, requested=True)
    ensure_calls: list[dict[str, object]] = []
    _patch_payload_dependencies(
        monkeypatch, thread_id=THREAD_A, running=False, ensure_calls=ensure_calls
    )

    result = payloads.work_trees_payload(state)

    work_tree = result["workTrees"][0]
    members = state.team_store.team_state(created.team_id).members
    snapshot_members = state.team_store.team_snapshot().teams[0].members
    assert work_tree["targetIdentity"]["thread"] == {
        "state": "bound",
        "threadId": THREAD_A,
    }
    assert work_tree["teamIdentity"]["teamId"] == created.team_id
    assert work_tree["renewalIntent"]["agentId"] == THREAD_A
    assert work_tree["renewalIntent"]["requested"] is True
    assert [member.agent_id for member in members] == [THREAD_A]
    assert [member.agent_id for member in snapshot_members] == [THREAD_A]
    renewal = snapshot_members[0].renewal
    assert renewal.agent_id == THREAD_A
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
            "taskFilters": ["serve", ""],
            "lifetime": "Drive",
        },
    )

    team_id = state.team_store.current_team_for_agent(target.id)
    assert status == HTTPStatus.OK
    assert result["route"]["actor"] == target.id
    assert result["route"]["targetIdentity"]["thread"] == {"state": "unbound"}
    assert result["route"]["teamIdentity"]["teamId"] == team_id
    assert result["route"]["taskFilters"] == ["serve"]
    assert [
        member.agent_id for member in state.team_store.team_state(team_id).members
    ] == [target.id]


def test_unstarted_send_rewrites_placeholder_membership_to_ensured_thread(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[target.id])
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
    assert [member.agent_id for member in members] == [THREAD_A]


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


def _pending_identity() -> dict[str, object]:
    return {
        "pendingInboxCount": 0,
        "pendingInboxLabel": "0",
        "pendingInboxKeys": [],
        "pendingInboxRevision": "test-revision",
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

    monkeypatch.setattr(payloads, "agent_status", lambda _repo: status)
    monkeypatch.setattr(app, "agent_status", lambda _repo: status)
    monkeypatch.setattr(agentapi, "agent_status", lambda _repo: status)
    monkeypatch.setattr(payloads, "agent_binding_error", lambda *_args: "")
    monkeypatch.setattr(payloads, "configured_say_voice", lambda _repo: "")
    monkeypatch.setattr(payloads, "task_filter_inventory", lambda: {})
    monkeypatch.setattr(
        payloads, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )
    monkeypatch.setattr(payloads, "ensure_agent_for_pending_inbox", fake_ensure)
    monkeypatch.setattr(
        payloads.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: ([], None),
    )
