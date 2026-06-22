"""Serve renewal handoff contracts."""

from __future__ import annotations

import subprocess
from http import HTTPStatus
from pathlib import Path
from types import SimpleNamespace

from spice.agent.renewal import renewal_rehydration_text
from spice.mail.inbox import (
    collect_inbox_items,
    compose_inbox_text,
    inbox_request_body,
    write_inbox_item,
)
from spice.serve import agentapi, workroutes
from spice.serve.worktree import inventory
from spice.serve.payload import identity, lane, message
from spice.serve.app import ServeState
from spice.serve.team.store import ServeTeamStore, TeamCommandService
from spice.serve.workroutes import work_tree_send_response_payload
from spice.serve.worktree.target import WorktreeTarget

THREAD_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
THREAD_B = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
ACTOR_A = f"thread:{THREAD_A}"
ACTOR_B = f"thread:{THREAD_B}"


def test_stopped_pending_renewal_starts_successor_and_moves_team_membership(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[ACTOR_A])
    _record_identity(state, target)
    state.team_store.record_pending_renewal(
        agent_id=ACTOR_A, ancestor_thread_id=THREAD_A
    )
    ensure_calls: list[dict[str, object]] = []
    send_records: list[dict[str, object]] = []
    record_directive_sent = state.team_store.record_directive_sent
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    monkeypatch.setattr(
        identity,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "gpt-next", "effort": "high"},
    )

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        return {"ok": True, "threadId": THREAD_B}, HTTPStatus.OK

    def observe_directive_sent(
        directive_key: str, *, agent_id: str, team_id: str
    ) -> None:
        send_records.append(
            {
                "agent_id": agent_id,
                "team_id": team_id,
                "predecessor_team": state.team_store.current_team_for_agent(ACTOR_A),
                "successor_team": state.team_store.current_team_for_agent(ACTOR_B),
            }
        )
        record_directive_sent(directive_key, agent_id=agent_id, team_id=team_id)

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)
    monkeypatch.setattr(
        state.team_store, "record_directive_sent", observe_directive_sent
    )

    payload, status = work_tree_send_response_payload(
        state,
        target,
        {"text": "continue from pending handoff", "fastMode": True},
    )

    body = inbox_request_body(collect_inbox_items(repo)[0].text)
    assert status == HTTPStatus.OK
    assert payload["agentEnsure"]["threadId"] == THREAD_B
    assert payload["renewalIntent"]["requested"] is False
    assert payload["renewalIntent"]["state"] == "started"
    assert payload["renewalIntent"]["successorThreadId"] == THREAD_B
    assert payload["renewalIntent"]["teamSlot"] == 0
    assert payload["renewalIntent"]["predecessorIdentity"]["actualModel"] == (
        "gpt-test"
    )
    assert payload["renewalIntent"]["successorIdentity"]["desiredModel"] == ("gpt-next")
    assert renewal_rehydration_text(THREAD_A) in body
    assert ensure_calls == [
        {
            "target": target,
            "fast_mode": True,
            "force_new": True,
        }
    ]
    assert send_records == [
        {
            "agent_id": ACTOR_B,
            "team_id": created.team_id,
            "predecessor_team": None,
            "successor_team": created.team_id,
        }
    ]
    assert state.team_store.current_team_for_agent(ACTOR_A) is None
    assert state.team_store.current_team_for_agent(ACTOR_B) == created.team_id


def test_target_refresh_force_news_pending_renewal_into_original_team(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[ACTOR_A])
    _record_identity(state, target)
    state.team_store.record_pending_renewal(
        agent_id=ACTOR_A, ancestor_thread_id=THREAD_A
    )
    write_inbox_item(
        repo,
        "20260101T000000000001Z.txt",
        compose_inbox_text(body="external renewal steering", priority=None, stop=False),
    )
    ensure_calls: list[dict[str, object]] = []
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    monkeypatch.setattr(
        identity,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "gpt-next", "effort": "high"},
    )

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        return {"ok": True, "threadId": THREAD_B}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)
    monkeypatch.setattr(inventory, "task_filter_inventory", lambda: {})
    monkeypatch.setattr(inventory, "agent_binding_error", lambda *_args: "")
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: message.message_reader.AssistantMessageRead(
            items=[],
            error=None,
            transcript=None,
        ),
    )

    result = inventory.work_trees_payload(state)

    work_tree = result["workTrees"][0]
    assert work_tree["targetIdentity"]["thread"] == {
        "state": "bound",
        "threadId": THREAD_B,
    }
    assert work_tree["teamIdentity"]["teamId"] == created.team_id
    assert work_tree["teamIdentity"]["teamRevision"] > created.revision
    assert work_tree["renewalIntent"]["successorThreadId"] == THREAD_B
    assert work_tree["renewalIntent"]["teamSlot"] == 0
    assert ensure_calls == [
        {
            "target": target,
            "fast_mode": False,
            "force_new": True,
        }
    ]
    assert state.team_store.current_team_for_agent(ACTOR_A) is None
    assert state.team_store.current_team_for_agent(ACTOR_B) == created.team_id


def test_messages_refresh_force_news_pending_renewal_into_original_team(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[ACTOR_A])
    _record_identity(state, target)
    state.team_store.record_pending_renewal(
        agent_id=ACTOR_A, ancestor_thread_id=THREAD_A
    )
    write_inbox_item(
        repo,
        "20260101T000000000001Z.txt",
        compose_inbox_text(body="external renewal steering", priority=None, stop=False),
    )
    ensure_calls: list[dict[str, object]] = []
    message_threads: list[str] = []
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    monkeypatch.setattr(
        identity,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "gpt-next", "effort": "high"},
    )

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        return {"ok": True, "threadId": THREAD_B}, HTTPStatus.OK

    def fake_messages(thread_id, **_kwargs):
        message_threads.append(thread_id)
        return message.message_reader.AssistantMessageRead(
            items=[],
            error=None,
            transcript=None,
        )

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)
    monkeypatch.setattr(inventory, "task_filter_inventory", lambda: {})
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        fake_messages,
    )

    result = message.messages_payload_for_worktree(
        state, target, limit=5, expected_thread_id=THREAD_A
    )

    assert result["targetIdentity"]["thread"] == {
        "state": "bound",
        "threadId": THREAD_B,
    }
    assert result["teamIdentity"]["teamId"] == created.team_id
    assert result["teamIdentity"]["teamRevision"] > created.revision
    assert result["agentEnsure"]["threadId"] == THREAD_B
    assert result["renewalIntent"]["successorThreadId"] == THREAD_B
    assert result["renewalIntent"]["teamSlot"] == 0
    assert message_threads == [THREAD_B]
    assert ensure_calls == [
        {
            "target": target,
            "fast_mode": False,
            "force_new": True,
        }
    ]
    assert state.team_store.current_team_for_agent(ACTOR_A) is None
    assert state.team_store.current_team_for_agent(ACTOR_B) == created.team_id


def _repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir(exist_ok=True)
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    return repo


def _target(repo: Path) -> WorktreeTarget:
    return WorktreeTarget(id="target-1", repo_root=repo, name=repo.name, branch="main")


def _serve_state(tmp_path: Path, target: WorktreeTarget) -> ServeState:
    state = ServeState(anchor_root=tmp_path)
    state.cached_targets = [target]
    state.team_store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    state.team_commands = TeamCommandService(state.team_store)
    return state


def _record_identity(state: ServeState, target: WorktreeTarget) -> None:
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


def _patch_agent_status(monkeypatch, *, thread_id: str, running: bool) -> None:
    status = SimpleNamespace(
        running=running,
        thread_id=thread_id,
        process_status="running" if running else "idle",
        pid=123 if running else 0,
        process_group_id=123 if running else 0,
        model="gpt-test",
        reasoning_effort="low",
        service_tier="fast",
        started_at="",
        log_path=None,
        prompt_skill_path=None,
    )
    monkeypatch.setattr(agentapi, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(identity, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(lane, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(message, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(workroutes, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(inventory, "agent_status", lambda *_args, **_kwargs: status)
