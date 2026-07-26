"""Work-tree response payloads shared by HTTP and live-bus routes."""

from __future__ import annotations

from collections.abc import Iterator
from concurrent.futures import Future
from contextlib import contextmanager
from dataclasses import dataclass
from http import HTTPStatus
from typing import Any

from spice.agent.lifecycle import agent_status
from spice.agent.renewal import renewal_handoff_request_text, renewal_steering_text
from spice.mail.ackstate import (
    DirectivePublicationWrite,
    record_directive_publications,
)
from spice.serve.payload import identity
from spice.serve.agentapi import (
    explicit_send_decision,
    sent_steering_payload,
    sent_steering_response_payload,
)
from spice.serve.drive import drive_drain_queue_controls
from spice.serve.lifecycle import (
    LifecycleOutcome,
    explicit_send_publication,
    submit_explicit_send_intent,
    submit_inbox_wake,
)
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.payload.lane import lane_chrome_payload
from spice.serve.payload.wire import validate_emitter_payload
from spice.serve.taskboard import open_task_board_projection
from spice.serve.steering import steering_submit_error_status, submit_steering_message
from spice.serve.team.store import ServeTeamStore, TeamConfig
from spice.serve.worktree.target import WorktreeTarget, match_serve_worktree

LIFETIME_LABELS = ("Steer", "Drive", "Drain")


@dataclass(frozen=True)
class _WorkTreeSendRequest:
    text: str
    drive_agent: bool
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
    response, status = _work_tree_send_response_payload(
        state, target, payload, ensure_agent_before_reply=True
    )
    return (
        validate_emitter_payload(
            "workroutes.work_tree_send_response_payload", response
        ),
        status,
    )


def work_tree_send_accepted_response_payload(
    state: Any,
    target: WorktreeTarget,
    payload: dict[str, Any],
) -> tuple[dict[str, Any], HTTPStatus]:
    response, status = _work_tree_send_response_payload(
        state, target, payload, ensure_agent_before_reply=False
    )
    return (
        validate_emitter_payload(
            "workroutes.work_tree_send_accepted_response_payload", response
        ),
        status,
    )


def _work_tree_send_response_payload(
    state: Any,
    target: WorktreeTarget,
    payload: dict[str, Any],
    *,
    ensure_agent_before_reply: bool,
) -> tuple[dict[str, Any], HTTPStatus]:
    request, error_response = _validate_work_tree_send_request(payload)
    if error_response is not None:
        return error_response
    assert request is not None
    predecessor = identity.resolve_thread_id_for_target(state, target) or ""
    predecessor_actor = identity.team_actor_for_target(
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
    grants_explicit_launch = ensure_agent_before_reply or force_new
    try:
        with _publication_guard(state, target, grants_explicit_launch):
            sent = submit_steering_message(
                text=text,
                priority=None,
                stop=False,
                no_say=request.no_say,
                attachments=request.attachments,
                controls=drive_drain_queue_controls(request.drive_agent),
                target_repo_root=target.repo_root,
                # The synchronous route answers with the launch it caused, so it
                # has nothing to tell watchers that its own reply does not carry.
                # The accepted route replies first and leaves the lane's new
                # pending item to be observed like any other.
                wake_server=not grants_explicit_launch,
            )
            grant = (
                submit_explicit_send_intent(
                    state,
                    target,
                    sent.key,
                    fast_mode=bool(state.team_store.global_fast_mode_enabled()),
                    force_new=force_new,
                )
                if grants_explicit_launch
                else None
            )
        if grant is None:
            # The accepted route owes this send a lane start it will not wait
            # for: queue the decision here rather than leaving it to whoever
            # renders the lane next, and let the follow-up report its outcome.
            submit_inbox_wake(state, target, sent.key)
    except (RuntimeError, ValueError) as exc:
        return {"ok": False, "error": str(exc)}, steering_submit_error_status(exc)
    response_payload = _work_tree_send_result_payload(
        state,
        target,
        sent,
        renew_intent=renew_intent,
        predecessor=predecessor,
        predecessor_actor=predecessor_actor,
        grant=grant,
    )
    return response_payload, HTTPStatus.OK


@contextmanager
def _publication_guard(
    state: Any,
    target: WorktreeTarget,
    grants_explicit_launch: bool,
) -> Iterator[None]:
    """Hold the target guard exactly while a send reserves its launch attempt.

    A send that reserves an explicit grant must publish and reserve as one step,
    or a background decision already inside the guard consumes the item in
    between. A send that hands its lane to the background watcher has no grant to
    protect. Sibling targets never enter this lane's guard.
    """
    if not grants_explicit_launch:
        yield
        return
    with explicit_send_publication(state, target):
        yield


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
    identity.record_serve_agent_identity(
        state.team_store,
        target,
        predecessor,
        actor_id=predecessor_actor,
    )
    if status.running:
        # Renew never yanks a running agent; the message asks for a clean
        # handoff and the successor starts on the next send. Choosing the text
        # is all this does -- the reconciler settles the renewal itself once it
        # sees whether the attempt produced a successor.
        return renewal_handoff_request_text(text), False
    return renewal_steering_text(text, previous_thread_id=predecessor), True


def _work_tree_send_result_payload(
    state: Any,
    target: WorktreeTarget,
    sent: Any,
    *,
    renew_intent: bool,
    predecessor: str,
    predecessor_actor: str,
    grant: Future[LifecycleOutcome] | None,
) -> dict[str, Any]:
    if grant is None:
        # One read of the inbox this send just published into, shared by the
        # flat result fields and the facet that orders them, so the reply cannot
        # describe two different instants of the same lane.
        pending_identity = pending_inbox_identity_payload(target.repo_root)
        response_payload = sent_steering_payload(
            sent,
            target=target,
            pending_identity=pending_identity,
        )
        send_actor = identity.team_actor_for_target(
            state.team_store, target, predecessor
        )
        _record_directive_publication(state, target, sent, send_actor=send_actor)
        response_payload["chrome"] = lane_chrome_payload(
            target_id=target.id, pending_identity=pending_identity
        )
        return response_payload

    decision = explicit_send_decision(grant)
    # The reply reports the inbox as it stands once the reconciler has settled
    # this send: a launch that consumed the item leaves nothing pending, so this
    # read waits for the decision instead of racing it.
    pending_identity = pending_inbox_identity_payload(target.repo_root)
    response_payload = sent_steering_response_payload(
        sent,
        target=target,
        decision=decision,
        pending_identity=pending_identity,
    )
    agent_ensure = response_payload.get("agentEnsure")
    # The reconciler already settled which thread this send belongs to, renewal
    # included. Reading it here keeps the reply a report of that decision. A
    # decision that never arrives is a lane that did not start, not a send that
    # did not land, so the publication is still attributed to the lane's own
    # binding.
    send_agent_id = (
        decision.thread_id
        if decision is not None
        else identity.resolve_thread_id_for_target(state, target) or ""
    )
    send_actor = ""
    if send_agent_id:
        send_actor = identity.team_actor_for_target(
            state.team_store, target, send_agent_id
        )
    deadlettered = isinstance(agent_ensure, dict) and agent_ensure.get(
        "deadletteredInboxKey"
    )
    if send_actor and not deadlettered:
        _record_directive_publication(state, target, sent, send_actor=send_actor)
    renewal_agent_id = predecessor_actor if renew_intent else send_actor
    renewal_facts: dict[str, Any] = {}
    if renewal_agent_id:
        # The lifetime and the renewal request are one team-store observation of
        # the actor this send leaves the lane with. An actor outside any team
        # still has a renewal state to report, but no lifetime to pair it with,
        # so the facet stays unnamed rather than carrying half of itself.
        renewal_facts = identity.team_facts_for_actor(
            state.team_store, renewal_agent_id
        )
        response_payload["renewalIntent"] = identity.renewal_intent_for_actor(
            state.team_store, renewal_agent_id
        )
    response_payload["chrome"] = lane_chrome_payload(
        target_id=target.id,
        pending_identity=pending_identity,
        team_facts=renewal_facts or None,
        renewal_intent=renewal_facts.get("renewalIntent"),
    )
    route_thread_id = send_agent_id or predecessor
    route_actor = identity.team_actor_for_target(
        state.team_store, target, route_thread_id
    )
    response_payload["route"] = _work_tree_route_payload(
        state,
        target,
        thread_id=route_thread_id,
        actor=route_actor,
    )
    return response_payload


def _record_directive_publication(
    state: Any, target: WorktreeTarget, sent: Any, *, send_actor: str
) -> None:
    if not send_actor:
        return
    # One operator directive = one canonical steering fact, keyed by its inbox
    # key and attributed to the actor/team that will process it. ACK archival
    # completes this same row; Serve never owns a second directive write.
    capture_team = state.team_store.current_team_for_agent(send_actor) or send_actor
    record_directive_publications(
        target.repo_root,
        [
            DirectivePublicationWrite(
                key=sent.key,
                inbox_name=sent.path.name,
                text=sent.text,
                target_actor=send_actor,
                team_id=capture_team,
                sent_at=sent.path.stat().st_mtime,
                attachments=tuple(
                    {
                        "path": str(attachment.path),
                        "name": attachment.name,
                        "content_type": attachment.content_type,
                        "size": attachment.size,
                    }
                    for attachment in sent.attachments
                ),
            )
        ],
    )


def _apply_lifetime_to_team(
    state: Any, target: WorktreeTarget, payload: dict[str, Any]
) -> None:
    lifetime = str(payload.get("lifetime") or "").strip()
    if lifetime not in LIFETIME_LABELS:
        return
    thread_id = identity.resolve_thread_id_for_target(state, target) or ""
    actor = identity.team_actor_for_target(state.team_store, target, thread_id)
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
            task_filters=current.task_filters,
            shell_settings=current.shell_settings,
        ),
        replace_task_filters=False,
    )


def _team_for_task_drain_actor(team_store: ServeTeamStore, actor: str) -> str:
    team_id = team_store.current_team_for_agent(actor)
    if team_id is not None:
        return team_id
    return ServeTeamStore.create_team(team_store, members=[actor]).team_id


def work_tree_task_drain_response_payload(
    state: Any,
    target: WorktreeTarget,
    payload: dict[str, Any],
) -> tuple[dict[str, Any], HTTPStatus]:
    _apply_lifetime_to_team(state, target, payload)
    task_filters = payload.get("taskFilters")
    thread_id = identity.resolve_thread_id_for_target(state, target) or ""
    actor = identity.team_actor_for_target(state.team_store, target, thread_id)
    if bool(payload.get("replaceTaskFilters")) and isinstance(task_filters, list):
        if not actor:
            payload = validate_emitter_payload(
                "workroutes.work_tree_task_drain_response_payload",
                {"ok": False, "error": "task drain requires a bound agent"},
            )
            return payload, HTTPStatus.CONFLICT
        team_id = _team_for_task_drain_actor(state.team_store, actor)
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
                task_filters=validated,
                shell_settings=current.shell_settings,
            ),
            replace_task_filters=True,
        )
    route = _work_tree_route_payload(state, target, thread_id=thread_id, actor=actor)
    response = validate_emitter_payload(
        "workroutes.work_tree_task_drain_response_payload",
        {"ok": True, "route": route},
    )
    return response, HTTPStatus.OK


def _work_tree_route_payload(
    state: Any,
    target: WorktreeTarget,
    *,
    thread_id: str,
    actor: str,
) -> dict[str, Any]:
    facts = identity.team_facts_for_actor(state.team_store, actor)
    team_identity = identity.team_identity_payload(facts)
    return {
        "actor": actor,
        "targetIdentity": identity.target_identity_payload(target, thread_id),
        "serveAgentIdentity": identity.record_serve_agent_identity(
            state.team_store,
            target,
            thread_id,
        ),
        "teamIdentity": team_identity,
        "memberAgents": [actor] if actor else [],
        "taskFilters": facts.get("taskFilters", []),
        "effectiveTaskFilters": facts.get("effectiveTaskFilters", []),
        "taskFilterEntries": facts.get("taskFilterEntries", []),
        "laneFilterVersion": "",
        "lifetime": facts.get("lifetime", ""),
        # A route reply reports the team configuration it just settled and
        # nothing else: it read no inbox and no transcript, so those facets stay
        # unnamed and the client keeps what it already holds for them.
        "chrome": lane_chrome_payload(
            target_id=target.id,
            team_identity=team_identity,
            team_facts=facts,
            task_filter_inventory=open_task_board_projection().task_filter_inventory,
        ),
    }
