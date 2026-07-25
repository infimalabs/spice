"""Worktree list payload builders."""

from __future__ import annotations

from typing import Any

from spice.agent.lifecycle import agent_binding_error, agent_status
from spice.config.values import effective_agent_config
from spice.serve.agentapi import (
    ensure_agent_for_available_work,
    ensure_agent_for_pending_inbox,
)
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
)
from spice.serve.payload.wire import validate_emitter_payload
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.taskboard import OpenTaskBoardProjection, open_task_board_projection
from spice.serve.worktree.target import WorktreeTarget


def work_trees_payload(state: Any) -> dict[str, Any]:
    targets = state.worktree_targets()
    task_board = open_task_board_projection()
    inventory = task_board.task_filter_inventory
    payload: dict[str, Any] = {
        "workTrees": [
            _work_tree_payload(
                state,
                target,
                inventory,
                task_board,
            )
            for target in targets
        ],
        "defaultTargetId": targets[0].id if targets else "",
        "taskFilterInventory": inventory,
    }
    # A discovery failure must reach the client, which otherwise reads a short
    # workTrees list as proof those worktrees were removed and closes the lanes.
    # This is its own field rather than observerErrors: observer mode carries
    # unrelated errors there, and the client keys lane closure off this one.
    errors = state.targets_discovery_errors()
    if errors:
        payload["targetsDiscoveryErrors"] = errors
    return validate_emitter_payload("worktree.inventory.work_trees_payload", payload)


def _work_tree_payload(
    state: Any,
    target: WorktreeTarget,
    inventory: dict[str, Any],
    task_board: OpenTaskBoardProjection,
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
    # Resolve this target's effective agent config and say-voice name once, then
    # reuse them across the identity, driver, and lane-info builders below --
    # each otherwise re-resolves the same config for the same repo root.
    desired_config = effective_agent_config(target.repo_root)
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
        desired_config=desired_config,
        task_board=task_board,
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
            desired_config=desired_config,
        ),
        "serveAgentIdentity": serve_identity,
        "taskFilters": team_facts.get("taskFilters", []),
        "taskFilterEntries": team_facts.get("taskFilterEntries", []),
        "effectiveTaskFilters": team_facts.get("effectiveTaskFilters", []),
        "laneFilterVersion": "",
        "teamIdentity": team_identity,
        "lifetime": team_facts.get("lifetime", ""),
        "renewalIntent": renewal_intent,
        "taskFilterInventory": inventory,
        "laneInfo": _lane_info_payload(
            target,
            serve_identity,
            agent_name=agent_name,
            task_board=task_board,
        ),
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


def ensure_work_tree_agent(
    state: Any, target: WorktreeTarget, thread_id: str
) -> tuple[str, str, bool, dict[str, Any] | None]:
    """Public server-owned entry point for the inventory launch decision."""
    return _ensure_work_tree_agent(state, target, thread_id)


def _ensure_work_tree_agent(
    state: Any, target: WorktreeTarget, thread_id: str
) -> tuple[str, str, bool, dict[str, Any] | None]:
    """Run the shared pending-inbox and available-work launch decision."""
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
        "fast_mode": bool(state.team_store.global_fast_mode_enabled()),
        "force_new": renew_intent,
    }
    agent_ensure = ensure_agent_for_pending_inbox(target, **ensure_kwargs)
    team_facts = team_facts_for_target(state.team_store, target, thread_id)
    if agent_ensure is None and team_facts.get("lifetime") == "Drain":
        agent_ensure = ensure_agent_for_available_work(
            target,
            thread_id=thread_id,
            **ensure_kwargs,
        )
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
    desired_config: dict[str, str] | None = None,
    task_board: OpenTaskBoardProjection | None = None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    from spice.serve.payload.message import target_activity_items

    items, error, transcript = target_activity_items(
        target,
        thread_id,
        task_board=task_board,
    )
    transcript_owner = transcript.owner_driver.name if transcript else ""
    serve_identity = serve_agent_identity_payload(
        target,
        thread_id,
        binding_status=binding_status,
        binding_error=binding_error,
        transcript_owner=transcript_owner,
        store=state.team_store,
        desired_config=desired_config,
    )
    status_line = _status_line_payload_from_status(
        status=status,
        thread_id=thread_id,
        binding_error=binding_error,
        items=items,
        error=error,
        pending_identity=pending_identity,
        active_claims=task_board,
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
