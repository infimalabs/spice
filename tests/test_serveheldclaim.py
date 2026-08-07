"""Serve restarting a stopped lane onto the task its own actor still holds."""

from __future__ import annotations

from http import HTTPStatus

from spice.agent.lifecycle import AgentOutOfCreditsError, AgentRestartRefusedError
from spice.serve import agentapi
from spice.tasks import identity
from tests.test_servehelpers import (
    THREAD_A,
    _patch_agent_status,
    _repo,
    _retry_gate,
    _target,
)

HELD_ROW = {"uuid": "task-held", "project": "lifecycle.restart"}
HELD_HANDLE = identity.render_handle(HELD_ROW)


def _patch_held_claim(monkeypatch, row):
    monkeypatch.setattr(agentapi.claimstate, "active_claim", lambda _actor: row)


def _patch_started_lane(monkeypatch, trace, payload=None):
    started = payload or {"ok": True, "action": "start", "threadId": THREAD_A}
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        lambda _target, **kwargs: (
            trace.append(("start", kwargs)) or (dict(started), HTTPStatus.OK)
        ),
    )


def test_held_claim_restart_starts_the_lane_on_the_task_it_already_holds(
    tmp_path, monkeypatch
):
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    trace: list[tuple[str, object]] = []
    _patch_held_claim(monkeypatch, HELD_ROW)
    monkeypatch.setattr(
        agentapi.claimstate,
        "do_claim",
        lambda *_args, **_kwargs: trace.append(("claim", _args)) or True,
    )
    monkeypatch.setattr(
        agentapi,
        "preflight_automatic_agent_launch",
        lambda _root: trace.append(("preflight", None)),
    )
    _patch_started_lane(monkeypatch, trace)

    payload = agentapi.ensure_agent_for_held_claim(
        target,
        thread_id=THREAD_A,
        retry_due=_retry_gate(),
        retry_seconds=0.0,
    )

    assert payload == {
        "ok": True,
        "action": "start",
        "threadId": THREAD_A,
        "trigger": "held-claim",
        "taskHandle": HELD_HANDLE,
    }
    # No claim step: the row is already this actor's, and reserving it again as
    # a LaunchClaim would hand it back the moment a restart failed to settle.
    assert [event[0] for event in trace] == ["preflight", "start"]
    assert trace[1][1]["launch_preflighted"] is True
    assert trace[1][1].get("launch_claim") is None


def test_held_claim_restart_leaves_a_lane_holding_nothing_to_the_board(
    tmp_path, monkeypatch
):
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    trace: list[tuple[str, object]] = []
    _patch_held_claim(monkeypatch, None)
    _patch_started_lane(monkeypatch, trace)

    payload = agentapi.ensure_agent_for_held_claim(
        target,
        thread_id=THREAD_A,
        retry_due=_retry_gate(),
        retry_seconds=0.0,
    )

    # A lane with nothing held is the available-work arm's business, so this
    # arm answers with the None that lets the decision fall through to it.
    assert payload is None
    assert trace == []


def test_held_claim_restart_leaves_a_running_lane_alone(tmp_path, monkeypatch):
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)
    trace: list[tuple[str, object]] = []
    _patch_held_claim(monkeypatch, HELD_ROW)
    _patch_started_lane(monkeypatch, trace)

    payload = agentapi.ensure_agent_for_held_claim(
        target,
        thread_id=THREAD_A,
        retry_due=_retry_gate(),
        retry_seconds=0.0,
    )

    assert payload is None
    assert trace == []


def test_held_claim_restart_reports_an_unbound_lane(tmp_path):
    target = _target(_repo(tmp_path))

    payload = agentapi.ensure_agent_for_held_claim(target, thread_id="")

    assert payload == {
        "ok": True,
        "action": "skipped",
        "trigger": "held-claim",
        "reason": "unbound",
    }


def test_held_claim_restart_waits_out_its_own_attempt_interval(tmp_path, monkeypatch):
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    trace: list[tuple[str, object]] = []
    _patch_held_claim(monkeypatch, HELD_ROW)
    monkeypatch.setattr(
        agentapi,
        "preflight_automatic_agent_launch",
        lambda _root: trace.append(("preflight", None)),
    )
    _patch_started_lane(monkeypatch, trace)
    gate = _retry_gate()

    first = agentapi.ensure_agent_for_held_claim(
        target,
        thread_id=THREAD_A,
        retry_due=gate,
        retry_seconds=agentapi.HELD_CLAIM_ENSURE_RETRY_SECONDS,
    )
    second = agentapi.ensure_agent_for_held_claim(
        target,
        thread_id=THREAD_A,
        retry_due=gate,
        retry_seconds=agentapi.HELD_CLAIM_ENSURE_RETRY_SECONDS,
    )

    assert first["action"] == "start"
    # Nothing is reserved here, so the row stays held whatever the restart
    # accomplishes: the interval, not the board, is what bounds the attempts.
    assert second == {
        "ok": True,
        "action": "skipped",
        "trigger": "held-claim",
        "reason": "retry-wait",
        "taskHandle": HELD_HANDLE,
    }
    assert [event[0] for event in trace] == ["preflight", "start"]


def test_held_claim_restart_stops_on_an_out_of_credits_launch(tmp_path, monkeypatch):
    """Money runs out below the restart, not beside it.

    The restart goes through the same `ensure_agent` every other launch uses, so
    it inherits that call's stop criteria rather than needing its own copy. This
    drives the real ensure path with a spent account: the failure has to come
    back named, and the row has to stay held -- an out-of-credits lane is the
    case where handing the task to a peer would strand this worktree's changes
    for as long as the account stays empty.
    """
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    releases: list[tuple[str, str]] = []
    _patch_held_claim(monkeypatch, HELD_ROW)
    monkeypatch.setattr(
        agentapi.claimstate,
        "release_claim",
        lambda uuid, actor: releases.append((uuid, actor)),
    )
    monkeypatch.setattr(
        agentapi, "preflight_automatic_agent_launch", lambda _root: None
    )

    def out_of_credits(*_args, **_kwargs):
        raise AgentOutOfCreditsError("credit balance is too low")

    monkeypatch.setattr(agentapi, "ensure_agent", out_of_credits)

    payload = agentapi.ensure_agent_for_held_claim(
        target,
        thread_id=THREAD_A,
        retry_due=_retry_gate(),
        retry_seconds=0.0,
    )

    assert payload["ok"] is False
    assert payload["failure"] == agentapi.AGENT_FAILURE_OUT_OF_CREDITS
    assert payload["trigger"] == "held-claim"
    assert payload["taskHandle"] == HELD_HANDLE
    assert releases == []


def test_held_claim_restart_keeps_the_claim_when_the_launch_is_refused(
    tmp_path, monkeypatch
):
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    releases: list[tuple[str, str]] = []
    _patch_held_claim(monkeypatch, HELD_ROW)
    monkeypatch.setattr(
        agentapi.claimstate,
        "release_claim",
        lambda uuid, actor: releases.append((uuid, actor)),
    )

    def refuse(_root):
        raise AgentRestartRefusedError("lane keeps dying", refusal={"reason": "rapid"})

    monkeypatch.setattr(agentapi, "preflight_automatic_agent_launch", refuse)

    payload = agentapi.ensure_agent_for_held_claim(
        target,
        thread_id=THREAD_A,
        retry_due=_retry_gate(),
        retry_seconds=0.0,
    )

    assert payload["trigger"] == "held-claim"
    assert payload["taskHandle"] == HELD_HANDLE
    assert payload["failure"] == agentapi.AGENT_FAILURE_RESTART_REFUSED
    # The worktree still holds whatever the stopped agent was doing, so a
    # refused restart keeps the row off the board rather than reassigning work
    # onto a tree someone else's changes are sitting in.
    assert releases == []
