"""Work-tree response payloads shared by HTTP and live-bus routes."""

from __future__ import annotations

from dataclasses import dataclass
from http import HTTPStatus
from typing import Any

from spice.agent.lifecycle import agent_status
from spice.agent.renewal import renewal_handoff_request_text, renewal_steering_text
from spice.errors import SpiceError
from spice.serve import identitypayload
from spice.serve.agentapi import sent_steering_response_payload
from spice.serve.drive import drive_drain_queue_controls
from spice.serve.steering import steering_submit_error_status, submit_steering_message
from spice.serve.teams import TeamConfig
from spice.serve.worktrees import WorktreeTarget, match_serve_worktree

LIFETIME_LABELS = ("Steer", "Drive", "Drain")


@dataclass(frozen=True)
class _WorkTreeSendRequest:
    text: str
    drive_agent: bool
    fast_mode: bool
    no_say: bool
    attachments: Any


def resolve_worktree_for_request(
    state: Any, selector: str | None
) -> WorktreeTarget | None:
    return match_serve_worktree(state.worktree_targets(), selector)


def _validate_work_tree_send_request(
    payload: dict[str, Any],
) -> tuple[_WorkTreeSendRequest | None, tuple[dict[str, Any], HTTPStatus] | None]:
    text = str(payload.get("text") or "").strip()
    if not text:
        return None, (
            {
                "ok": False,
                "error": "Message text is required.",
            },
            HTTPStatus.BAD_REQUEST,
        )
    lifetime = str(payload.get("lifetime") or "").strip()
    return (
        _WorkTreeSendRequest(
            text=text,
            drive_agent=lifetime in {"Drive", "Drain"},
            fast_mode=bool(payload.get("fastMode")),
            no_say=bool(payload.get("noSay")),
            attachments=payload.get("attachments"),
        ),
        None,
    )


def work_tree_send_response_payload(
    state: Any,
    target: WorktreeTarget,
    payload: dict[str, Any],
) -> tuple[dict[str, Any], HTTPStatus]:
    request, error_response = _validate_work_tree_send_request(payload)
    if error_response is not None:
        return error_response
    assert request is not None
    predecessor = identitypayload.resolve_thread_id_for_target(state, target) or ""
    predecessor_actor = identitypayload.team_actor_for_target(
        state.team_store, target, predecessor
    )
    renew_intent = _work_tree_send_renewal_active(
        state, predecessor=predecessor, predecessor_actor=predecessor_actor
    )
    _apply_lifetime_to_team(state, target, payload)
    text = request.text
    force_new = False
    if renew_intent:
        text, force_new = _work_tree_renewal_request_text(
            state,
            target,
            text,
            predecessor=predecessor,
            predecessor_actor=predecessor_actor,
        )
    try:
        sent = submit_steering_message(
            text=text,
            priority=None,
            stop=False,
            no_say=request.no_say,
            attachments=request.attachments,
            controls=drive_drain_queue_controls(request.drive_agent),
            target_repo_root=target.repo_root,
        )
    except (RuntimeError, ValueError) as exc:
        return {"ok": False, "error": str(exc)}, steering_submit_error_status(exc)
    if predecessor_actor:
        # One operator directive = one send, keyed by its inbox key. team-at-
        # capture is the actor's current team, or the actor itself when it is in
        # no team / a private solo team. Acked when the agent acknowledges the
        # key (see metrics.record_transcript_metrics_for_agent).
        capture_team = (
            state.team_store.current_team_for_agent(predecessor_actor)
            or predecessor_actor
        )
        state.team_store.record_directive_sent(
            sent.key, agent_id=predecessor_actor, team_id=capture_team
        )
    response_payload = _work_tree_send_result_payload(
        state,
        target,
        sent,
        fast_mode=request.fast_mode,
        force_new=force_new,
        renew_intent=renew_intent,
        predecessor=predecessor,
        predecessor_actor=predecessor_actor,
    )
    return response_payload, HTTPStatus.OK


def _work_tree_send_renewal_active(
    state: Any, *, predecessor: str, predecessor_actor: str
) -> bool:
    if not predecessor or not predecessor_actor:
        return False
    return state.team_store.agent_renewal_active(predecessor_actor)


def _work_tree_renewal_request_text(
    state: Any,
    target: WorktreeTarget,
    text: str,
    *,
    predecessor: str,
    predecessor_actor: str,
) -> tuple[str, bool]:
    status = agent_status(target.repo_root)
    identitypayload.serve_agent_identity_payload(
        target,
        predecessor,
        actor_id=predecessor_actor,
        store=state.team_store,
    )
    if status.running:
        # Renew never yanks a running agent; the message asks for a clean
        # handoff and the successor starts on the next send.
        try:
            state.team_store.record_pending_renewal(
                agent_id=predecessor_actor, ancestor_thread_id=predecessor
            )
        except SpiceError:
            pass  # renewal bookkeeping requires a team; steering still lands
        return renewal_handoff_request_text(text), False
    return renewal_steering_text(text, previous_thread_id=predecessor), True


def _work_tree_send_result_payload(
    state: Any,
    target: WorktreeTarget,
    sent: Any,
    *,
    fast_mode: bool,
    force_new: bool,
    renew_intent: bool,
    predecessor: str,
    predecessor_actor: str,
) -> dict[str, Any]:
    response_payload = sent_steering_response_payload(
        sent,
        state=state,
        target=target,
        fast_mode=fast_mode,
        force_new=force_new,
    )
    agent_ensure = response_payload.get("agentEnsure")
    ensured_thread_id = _work_tree_send_ensured_thread_id(
        state,
        agent_ensure=agent_ensure,
        renew_intent=renew_intent,
        force_new=force_new,
        predecessor_actor=predecessor_actor,
    )
    send_agent_id = (
        ensured_thread_id
        or identitypayload.resolve_thread_id_for_target(state, target)
        or ""
    )
    send_actor = ""
    if send_agent_id:
        send_actor = identitypayload.team_actor_for_target(
            state.team_store, target, send_agent_id
        )
    state.record_lane_send(target.id, agent_id=send_actor)
    renewal_agent_id = predecessor_actor if renew_intent else send_actor
    if renewal_agent_id:
        response_payload["renewalIntent"] = identitypayload.renewal_intent_for_actor(
            state.team_store, renewal_agent_id
        )
    route_thread_id = send_agent_id or predecessor
    route_actor = identitypayload.team_actor_for_target(
        state.team_store, target, route_thread_id
    )
    response_payload["route"] = _work_tree_route_payload(
        state,
        target,
        thread_id=route_thread_id,
        actor=route_actor,
    )
    return response_payload


def _work_tree_send_ensured_thread_id(
    state: Any,
    *,
    agent_ensure: Any,
    renew_intent: bool,
    force_new: bool,
    predecessor_actor: str,
) -> str:
    agent_ensure_payload = agent_ensure if isinstance(agent_ensure, dict) else None
    if renew_intent and force_new:
        return identitypayload.record_started_renewal_from_ensure(
            state.team_store,
            predecessor_agent_id=predecessor_actor,
            agent_ensure=agent_ensure_payload,
        )
    return identitypayload.agent_ensure_thread_id(agent_ensure_payload)


def _apply_lifetime_to_team(
    state: Any, target: WorktreeTarget, payload: dict[str, Any]
) -> None:
    lifetime = str(payload.get("lifetime") or "").strip()
    if lifetime not in LIFETIME_LABELS:
        return
    thread_id = identitypayload.resolve_thread_id_for_target(state, target) or ""
    actor = identitypayload.team_actor_for_target(state.team_store, target, thread_id)
    team_id = state.team_store.current_team_for_agent(actor)
    if team_id is None:
        return
    current = state.team_store.team_config(team_id)
    if current.lifetime == lifetime:
        return
    state.team_store.update_team_config(
        team_id,
        TeamConfig(
            lifetime=lifetime,
            speech_mode=current.speech_mode,
            task_filters=current.task_filters,
            selected_view=current.selected_view,
            shell_settings=current.shell_settings,
        ),
        replace_task_filters=False,
    )


def work_tree_task_drain_response_payload(
    state: Any,
    target: WorktreeTarget,
    payload: dict[str, Any],
) -> tuple[dict[str, Any], HTTPStatus]:
    _apply_lifetime_to_team(state, target, payload)
    task_filters = payload.get("taskFilters")
    thread_id = identitypayload.resolve_thread_id_for_target(state, target) or ""
    actor = identitypayload.team_actor_for_target(state.team_store, target, thread_id)
    if bool(payload.get("replaceTaskFilters")) and isinstance(task_filters, list):
        if not actor:
            return (
                {"ok": False, "error": "task drain requires a bound agent"},
                HTTPStatus.CONFLICT,
            )
        team_id = state.team_store.current_team_for_agent(actor)
        if team_id is None:
            created = state.team_store.create_team(members=[actor])
            team_id = created.team_id
        current = state.team_store.team_config(team_id)
        from spice.tasks import config as task_config

        validated = tuple(
            task_config.validate_assignable_project(str(item))
            for item in task_filters
            if str(item or "").strip()
        )
        state.team_store.update_team_config(
            team_id,
            TeamConfig(
                lifetime=str(payload.get("lifetime") or current.lifetime),
                speech_mode=current.speech_mode,
                task_filters=validated,
                selected_view=current.selected_view,
                shell_settings=current.shell_settings,
            ),
            replace_task_filters=True,
        )
    route = _work_tree_route_payload(state, target, thread_id=thread_id, actor=actor)
    return {"ok": True, "route": route}, HTTPStatus.OK


def _work_tree_route_payload(
    state: Any,
    target: WorktreeTarget,
    *,
    thread_id: str,
    actor: str,
) -> dict[str, Any]:
    facts = identitypayload.team_facts_for_actor(state.team_store, actor)
    return {
        "actor": actor,
        "targetIdentity": identitypayload.target_identity_payload(target, thread_id),
        "serveAgentIdentity": identitypayload.serve_agent_identity_payload(
            target,
            thread_id,
            store=state.team_store,
        ),
        "teamIdentity": identitypayload.team_identity_payload(facts),
        "memberAgents": [actor] if actor else [],
        "laneName": target.name,
        "taskFilters": facts.get("taskFilters", []),
        "taskFilterEntries": facts.get("taskFilterEntries", []),
        "routeFilters": facts.get("taskFilters", []),
        "filterTerms": facts.get("taskFilters", []),
        "filterArgs": facts.get("taskFilters", []),
        "laneFilterVersion": "",
        "lifetime": facts.get("lifetime", ""),
    }
