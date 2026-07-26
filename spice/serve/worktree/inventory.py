"""Worktree list payload builders."""

from __future__ import annotations

from typing import Any

from spice.agent.lifecycle import agent_binding_error, agent_status
from spice.config.values import effective_agent_config
from spice.serve.lifecycle import project_lifecycle
from spice.serve.payload.identity import (
    _agent_name_for_target,
    _binding_status,
    renewal_intent_for_actor,
    renewal_intent_for_target,
    resolve_thread_id_for_target,
    serve_agent_identity_payload,
    target_identity_payload,
    team_facts_for_target,
    team_identity_payload,
)
from spice.serve.payload.lane import (
    _lane_info_payload,
    _status_line_payload_from_status,
    lane_activity_at,
    lane_chrome_payload,
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
    lifecycle = project_lifecycle(
        state,
        target,
        thread_id=resolve_thread_id_for_target(state, target) or "",
        prefer_outcome_thread=True,
    )
    thread_id = lifecycle.thread_id
    predecessor_actor = lifecycle.predecessor_actor
    renew_intent = lifecycle.renewal_intent
    agent_ensure = lifecycle.agent_ensure
    pending_identity = pending_inbox_identity_payload(target.repo_root)
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
    serve_identity, status_line, last_assistant_at = _work_tree_status_payloads(
        state,
        target,
        thread_id=thread_id,
        binding_status=binding_status,
        binding_error=binding_error,
        status=status,
        desired_config=desired_config,
        task_board=task_board,
    )
    chrome = lane_chrome_payload(
        target_id=target.id,
        team_identity=team_identity,
        team_facts=team_facts,
        renewal_intent=renewal_intent,
        task_filter_inventory=inventory,
        pending_identity=pending_identity,
        last_assistant_at=last_assistant_at,
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
        "laneInfo": _lane_info_payload(
            target,
            serve_identity,
            agent_name=agent_name,
            task_board=task_board,
        ),
        "agentProcessStatus": status.process_status,
        "agentVisualStatus": status_line["agentVisualStatus"],
        "agentEnsure": agent_ensure or {},
        "statusLine": status_line,
        "chrome": chrome,
    }


def _work_tree_status_payloads(
    state: Any,
    target: WorktreeTarget,
    *,
    thread_id: str,
    binding_status: str,
    binding_error: str,
    status: Any,
    desired_config: dict[str, str] | None = None,
    task_board: OpenTaskBoardProjection | None = None,
) -> tuple[dict[str, Any], dict[str, Any], str]:
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
        active_claims=task_board,
    )
    return serve_identity, status_line, lane_activity_at(items)


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
