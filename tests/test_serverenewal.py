"""Serve renewal handoff contracts."""

from __future__ import annotations

from http import HTTPStatus
from pathlib import Path
from types import SimpleNamespace

from spice.agent.renewal import renewal_rehydration_text
from spice.mail.inbox import collect_inbox_items, inbox_request_body
from spice.serve import agentapi, app, payloads
from spice.serve.app import ServeState, work_tree_send_response_payload
from spice.serve.teams import ServeTeamStore, TeamCommandService
from spice.serve.worktrees import WorktreeTarget

THREAD_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
THREAD_B = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"


def test_stopped_pending_renewal_starts_successor_and_moves_team_membership(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[THREAD_A])
    state.team_store.record_pending_renewal(
        agent_id=THREAD_A, ancestor_thread_id=THREAD_A
    )
    ensure_calls: list[dict[str, object]] = []
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        return {"ok": True, "threadId": THREAD_B}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

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
    assert renewal_rehydration_text(THREAD_A) in body
    assert ensure_calls == [
        {
            "target": target,
            "fast_mode": True,
            "force_new": True,
        }
    ]
    assert state.team_store.current_team_for_agent(THREAD_A) is None
    assert state.team_store.current_team_for_agent(THREAD_B) == created.team_id


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
    monkeypatch.setattr(app, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(agentapi, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(payloads, "agent_status", lambda *_args, **_kwargs: status)
