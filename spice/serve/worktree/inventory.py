"""Worktree list payload builders."""

from __future__ import annotations

from typing import Any

from spice.agent.lifecycle import agent_binding_error, agent_status
from spice.serve.agentapi import ensure_agent_for_pending_inbox
from spice.serve.payload.identity import (
    _agent_name_for_target,
    _binding_status,
    record_started_renewal_from_ensure,
    renewal_intent_for_actor,
    renewal_intent_for_target,
    resolve_thread_id_for_target,
    serve_agent_identity_payload,
    target_identity_payload,
    team_actor_for_target,
    team_facts_for_target,
    team_identity_payload,
)
from spice.serve.payload.lane import (
    _lane_info_payload,
    _status_line_payload_from_status,
    task_filter_inventory,
)
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.worktree.target import WorktreeTarget


def work_trees_payload(state: Any) -> dict[str, Any]:
    targets = state.worktree_targets()
    inventory = task_filter_inventory()
    return {
        "workTrees": [
            _work_tree_payload(state, target, inventory) for target in targets
        ],
        "defaultTargetId": targets[0].id if targets else "",
        "taskFilterInventory": inventory,
    }


def _work_tree_payload(
    state: Any,
    target: WorktreeTarget,
    inventory: dict[str, Any],
) -> dict[str, Any]:
    thread_id = resolve_thread_id_for_target(state, target) or ""
    thread_id, predecessor_actor, renew_intent, agent_ensure = _ensure_work_tree_agent(
        state, target, thread_id
    )
    pending_identity = pending_inbox_identity_payload(target.repo_root)
    pending = int(pending_identity["pendingInboxCount"])
    status = agent_status(target.repo_root)
    binding_error = agent_binding_error(target.repo_root, status)
    binding_status = _binding_status(thread_id, binding_error)
    team_facts = team_facts_for_target(state.team_store, target, thread_id)
    team_identity = team_identity_payload(team_facts)
    agent_name = _agent_name_for_target(target)
    renewal_intent = _work_tree_renewal_intent(
        state, target, thread_id, predecessor_actor, renew_intent
    )
    serve_identity, status_line = _work_tree_status_payloads(
        state,
        target,
        thread_id=thread_id,
        binding_status=binding_status,
        binding_error=binding_error,
        status=status,
        pending_identity=pending_identity,
    )
    return {
        "id": target.id,
        "repoRoot": str(target.repo_root),
        "displayName": target.display_name,
        "branch": target.branch or target.name,
        "targetIdentity": target_identity_payload(
            target,
            thread_id,
            binding_status=binding_status,
            binding_error=binding_error,
            agent_name=agent_name,
        ),
        "serveAgentIdentity": serve_identity,
        "taskFilters": team_facts.get("taskFilters", []),
        "laneFilterVersion": "",
        "teamIdentity": team_identity,
        "lifetime": team_facts.get("lifetime", ""),
        "renewalIntent": renewal_intent,
        "taskFilterInventory": inventory,
        "laneInfo": _lane_info_payload(target, serve_identity),
        "pendingCount": pending,
        "pendingLabel": str(pending),
        **pending_identity,
        "privateTaskCount": 0,
        "agentProcessStatus": status.process_status,
        "agentVisualStatus": status_line["agentVisualStatus"],
        "agentEnsure": agent_ensure or {},
        "lastAssistantAt": status_line["lastAssistantAt"],
        "statusLine": status_line,
    }


def _ensure_work_tree_agent(
    state: Any, target: WorktreeTarget, thread_id: str, *, fast_mode: bool | None = None
) -> tuple[str, str, bool, dict[str, Any] | None]:
    pending_identity = pending_inbox_identity_payload(target.repo_root)
    pending = int(pending_identity["pendingInboxCount"])
    predecessor_actor = team_actor_for_target(state.team_store, target, thread_id)
    renew_intent = bool(
        thread_id
        and predecessor_actor
        and state.team_store.agent_renewal_active(predecessor_actor)
    )
    if renew_intent:
        serve_agent_identity_payload(
            target,
            thread_id,
            actor_id=predecessor_actor,
            store=state.team_store,
        )
    ensure_kwargs: dict[str, Any] = {
        "attempt_cache": state.pending_agent_ensure_attempts,
        "force_new": renew_intent,
    }
    if fast_mode is not None:
        ensure_kwargs["fast_mode"] = fast_mode
    agent_ensure = ensure_agent_for_pending_inbox(target, pending, **ensure_kwargs)
    ensured_thread_id = record_started_renewal_from_ensure(
        state.team_store,
        predecessor_agent_id=predecessor_actor,
        agent_ensure=agent_ensure,
    )
    return ensured_thread_id or thread_id, predecessor_actor, renew_intent, agent_ensure


def _work_tree_status_payloads(
    state: Any,
    target: WorktreeTarget,
    *,
    thread_id: str,
    binding_status: str,
    binding_error: str,
    status: Any,
    pending_identity: dict[str, Any],
) -> tuple[dict[str, Any], dict[str, Any]]:
    from spice.serve.payload.message import target_activity_items

    items, error, transcript = target_activity_items(target, thread_id)
    transcript_owner = transcript.owner_driver.name if transcript else ""
    serve_identity = serve_agent_identity_payload(
        target,
        thread_id,
        binding_status=binding_status,
        binding_error=binding_error,
        transcript_owner=transcript_owner,
        store=state.team_store,
    )
    status_line = _status_line_payload_from_status(
        status=status,
        thread_id=thread_id,
        binding_error=binding_error,
        items=items,
        error=error,
        pending_identity=pending_identity,
    )
    return serve_identity, status_line


def _work_tree_renewal_intent(
    state: Any,
    target: WorktreeTarget,
    thread_id: str,
    predecessor_actor: str,
    renew_intent: bool,
) -> dict[str, Any]:
    if renew_intent and predecessor_actor:
        return renewal_intent_for_actor(state.team_store, predecessor_actor)
    return renewal_intent_for_target(state.team_store, target, thread_id)
