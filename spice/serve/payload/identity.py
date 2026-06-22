"""Agent, team, and target identity payload builders."""

from __future__ import annotations

from typing import Any, Iterable

from spice.agent.driver import ALL_DRIVERS
from spice.agent.identity import canonical_thread_id
from spice.agent.lifecycle import agent_status
from spice.config import configured_say_voice, effective_agent_config
from spice.errors import SpiceError
from spice.serve.team.store import ServeTeamStore, renewal_intent_payload
from spice.serve.team.ids import (
    normalize_actor_id,
    target_actor_id,
    thread_actor_id,
    thread_id_for_actor,
)
from spice.serve.worktree.target import WorktreeTarget


def agent_ensure_thread_id(agent_ensure: dict[str, Any] | None) -> str:
    if not isinstance(agent_ensure, dict):
        return ""
    return canonical_thread_id(agent_ensure.get("threadId") or "")


def record_started_renewal_from_ensure(
    store: ServeTeamStore,
    *,
    predecessor_agent_id: str,
    agent_ensure: dict[str, Any] | None,
) -> str:
    successor_thread_id = agent_ensure_thread_id(agent_ensure)
    if not predecessor_agent_id or not successor_thread_id:
        return successor_thread_id
    successor_agent_id = thread_actor_id(successor_thread_id)
    if successor_agent_id == predecessor_agent_id:
        return successor_thread_id
    if not store.agent_renewal_active(predecessor_agent_id):
        return successor_thread_id
    try:
        store.record_started_renewal(
            predecessor_agent_id=predecessor_agent_id,
            successor_agent_id=successor_agent_id,
            ancestor_thread_id=thread_id_for_actor(predecessor_agent_id),
        )
    except SpiceError:
        pass
    return successor_thread_id


def resolve_thread_id_for_target(state: Any, target: WorktreeTarget) -> str | None:
    thread_id = canonical_thread_id(agent_status(target.repo_root).thread_id)
    with state.cache_lock:
        if thread_id:
            state.cached_thread_ids[target.id] = thread_id
            return thread_id
        return state.cached_thread_ids.get(target.id)


def team_facts_for_actor(store: ServeTeamStore, actor: str) -> dict[str, Any]:
    if not actor:
        return {}
    team_id = store.current_team_for_agent(actor)
    if team_id is None:
        return {}
    team = store.team_state(team_id)
    return {
        "teamId": team.team_id,
        "teamRevision": team.revision,
        "configRevision": team.config_revision,
        "taskFilters": list(team.config.task_filters),
        "taskFilterEntries": [
            entry.to_payload() for entry in team.config.task_filter_entries
        ],
        "lifetime": team.config.lifetime,
        "renewalIntent": renewal_intent_for_actor(store, actor),
    }


def team_actor_for_target(
    store: ServeTeamStore, target: WorktreeTarget, thread_id: str | None
) -> str:
    """Return the single durable team actor for a target.

    A target id is only a placeholder before the worktree has a thread. Once a
    real thread exists, any placeholder membership is rewritten to that thread
    before callers read team facts.
    """
    actor = target_bound_actor(target, thread_id)
    _promote_team_actor(store, actor, _target_actor_previous_names(target, actor))
    return actor


def team_facts_for_target(
    store: ServeTeamStore, target: WorktreeTarget, thread_id: str | None
) -> dict[str, Any]:
    return team_facts_for_actor(store, team_actor_for_target(store, target, thread_id))


def target_bound_actor(target: WorktreeTarget, thread_id: str | None) -> str:
    thread = canonical_thread_id(thread_id or "")
    return thread_actor_id(thread) if thread else target_actor_id(target.id)


def normalize_team_command_payload(
    payload: dict[str, Any], targets: Iterable[WorktreeTarget]
) -> dict[str, Any]:
    target_ids = {str(target.id) for target in targets}
    normalized = dict(payload)
    command = str(normalized.get("command") or "")
    if command == "createTeam":
        normalized["members"] = [
            _normalize_command_actor(item, target_ids)
            for item in normalized.get("members") or []
            if str(item or "").strip()
        ]
    elif command in {"moveAgentToTeam", "moveComposerToTeam", "removeAgentFromTeam"}:
        _normalize_command_agent_field(normalized, target_ids)
    elif command == "setAgentRenewalIntent":
        if normalized.get("agentId"):
            normalized["agentId"] = _normalize_command_actor(
                normalized["agentId"], target_ids
            )
    elif command in {"splitTeam", "reorderTeamAgents"}:
        normalized["agentIds"] = [
            _normalize_command_actor(item, target_ids)
            for item in normalized.get("agentIds") or []
            if str(item or "").strip()
        ]
    return normalized


def serve_agent_identity_payload(
    target: WorktreeTarget,
    thread_id: str | None = None,
    *,
    actor_id: str = "",
    binding_status: str = "",
    binding_error: str = "",
    transcript_owner: str = "",
    store: ServeTeamStore | None = None,
) -> dict[str, Any]:
    """Resolve the driver-neutral serve identity for one worktree target."""
    status = agent_status(target.repo_root)
    desired = effective_agent_config(target.repo_root)
    bound_thread = canonical_thread_id(thread_id or getattr(status, "thread_id", ""))
    actor = _serve_actor_id(target, bound_thread, actor_id=actor_id)
    actual_launch = _actual_launch_identity(status)
    identity = {
        "actorId": actor,
        "target": {
            "id": _required_identity_string(target.id, "target id"),
            "worktreeName": _required_identity_string(
                target.name,
                "worktree name",
            ),
            "repoRoot": _required_identity_string(
                str(target.repo_root),
                "target repo root",
            ),
            "branch": _required_identity_string(
                target.branch or target.name,
                "target branch",
            ),
        },
        "thread": _serve_thread_identity(
            bound_thread,
            binding_status=binding_status,
            binding_error=binding_error,
        ),
        "driver": {
            "desired": _required_identity_string(
                desired.get("driver"),
                "desired driver",
            ),
            "actual": _actual_driver_identity(status, actual_launch),
            "transcriptOwner": str(transcript_owner or "").strip(),
        },
        "launch": {
            "desired": {
                "model": _required_identity_string(
                    desired.get("model"),
                    "desired model",
                ),
                "effort": _required_identity_string(
                    desired.get("effort"),
                    "desired effort",
                ),
                "source": "effective agent config",
            },
            "actual": actual_launch,
        },
        "renewal": _serve_renewal_identity(store, actor),
    }
    if store is not None:
        _record_serve_agent_identity(
            store, identity, actor, bound_thread, actual_launch
        )
    return identity


def _record_serve_agent_identity(
    store: ServeTeamStore,
    identity: dict[str, Any],
    actor: str,
    bound_thread: str,
    actual_launch: dict[str, str],
) -> None:
    renewal = identity["renewal"]
    store.record_agent_identity(
        actor_id=actor,
        target_id=identity["target"]["id"],
        thread_id=bound_thread,
        actual_driver=identity["driver"]["actual"],
        actual_model=actual_launch["model"],
        actual_effort=actual_launch["effort"],
        actual_service_tier=actual_launch["serviceTier"],
        desired_driver=identity["driver"]["desired"],
        desired_model=identity["launch"]["desired"]["model"],
        desired_effort=identity["launch"]["desired"]["effort"],
        transcript_owner=identity["driver"]["transcriptOwner"],
        renewal_state=str(renewal.get("state") or ""),
        renewal_ancestor_thread_id=str(renewal.get("ancestorThreadId") or ""),
        renewal_successor_thread_id=str(renewal.get("successorThreadId") or ""),
        renewal_revision=int(renewal.get("revision") or 0),
    )


def renewal_intent_for_target(
    store: ServeTeamStore, target: WorktreeTarget, thread_id: str | None
) -> dict[str, Any]:
    actor = team_actor_for_target(store, target, thread_id)
    return renewal_intent_for_actor(store, actor)


def target_identity_payload(
    target: WorktreeTarget,
    thread_id: str,
    *,
    binding_status: str = "",
    binding_error: str = "",
    agent_name: str | None = None,
) -> dict[str, Any]:
    status = binding_status or ("bound" if thread_id else "unbound")
    payload = {
        "targetId": _required_identity_string(target.id, "target id"),
        "worktreeName": _required_identity_string(target.name, "worktree name"),
        "branch": _required_identity_string(
            target.branch or target.name,
            "target branch",
        ),
        "driver": _driver_identity_payload(target),
        "agent": _agent_identity_payload(
            _agent_name_for_target(target) if agent_name is None else agent_name
        ),
        "thread": _thread_identity_payload(
            thread_id,
            binding_status=status,
            binding_error=binding_error,
        ),
    }
    return payload


def _driver_identity_payload(target: WorktreeTarget) -> dict[str, str]:
    config = effective_agent_config(target.repo_root)
    return {
        "name": _required_identity_string(
            config.get("driver"),
            "driver name",
        ),
        "model": _required_identity_string(
            config.get("model"),
            "driver model",
        ),
        "effort": _required_identity_string(
            config.get("effort"),
            "driver effort",
        ),
    }


def team_identity_payload(team_facts: dict[str, Any]) -> dict[str, Any]:
    team_id = str(team_facts.get("teamId") or "").strip()
    if not team_id:
        return {"state": "none"}
    return {
        "state": "member",
        "teamId": _required_identity_string(team_id, "team id"),
        "teamRevision": _nonnegative_payload_int(
            team_facts.get("teamRevision"),
            "team revision",
        ),
        "configRevision": _nonnegative_payload_int(
            team_facts.get("configRevision"),
            "config revision",
        ),
    }


def _agent_identity_payload(agent_name: str) -> dict[str, str]:
    name = str(agent_name or "").strip()
    if not name:
        return {"state": "unconfigured"}
    return {
        "state": "configured",
        "name": _required_identity_string(name, "agent name"),
    }


def _thread_identity_payload(
    thread_id: str,
    *,
    binding_status: str,
    binding_error: str = "",
) -> dict[str, str]:
    state = str(binding_status or "").strip()
    if state == "unbound":
        return {"state": "unbound"}
    if state == "mismatch":
        payload = {"state": "mismatch"}
        thread = str(thread_id or "").strip()
        if thread:
            payload["threadId"] = _required_identity_string(thread, "thread id")
        error = str(binding_error or "").strip()
        if error:
            payload["error"] = error
        return payload
    if state != "bound":
        raise SpiceError(f"invalid thread identity state: {state or '-'}")
    payload = {
        "state": state,
        "threadId": _required_identity_string(thread_id, "thread id"),
    }
    error = str(binding_error or "").strip()
    if error:
        payload["error"] = error
    return payload


def _serve_thread_identity(
    thread_id: str,
    *,
    binding_status: str,
    binding_error: str,
) -> dict[str, str]:
    status = binding_status or ("bound" if thread_id else "unbound")
    return _thread_identity_payload(
        thread_id,
        binding_status=status,
        binding_error=binding_error,
    )


def _serve_actor_id(
    target: WorktreeTarget,
    thread_id: str,
    *,
    actor_id: str,
) -> str:
    actor = str(actor_id or "").strip()
    if actor:
        return normalize_actor_id(actor, target_ids=(target.id,))
    return target_bound_actor(target, thread_id)


def _normalize_command_actor(actor_id: Any, target_ids: set[str]) -> str:
    return normalize_actor_id(str(actor_id or ""), target_ids=target_ids)


def _normalize_command_agent_field(
    payload: dict[str, Any], target_ids: set[str]
) -> None:
    raw_agent = str(payload.get("agentId") or "").strip()
    if not raw_agent:
        return
    normalized_agent = normalize_actor_id(raw_agent, target_ids=target_ids)
    payload["agentId"] = normalized_agent


def _promote_team_actor(
    store: ServeTeamStore,
    actor: str,
    previous_names: Iterable[str],
) -> None:
    names = [name for name in dict.fromkeys((actor, *previous_names)) if name]
    if not names:
        return
    target_team_id = store.current_team_for_agent(actor)
    for name in names[1:]:
        team_id = store.current_team_for_agent(name)
        if team_id is None:
            continue
        store.assign_agent(target_team_id or team_id, actor, aliases=names[1:])
        return


def _target_actor_previous_names(target: WorktreeTarget, actor: str) -> list[str]:
    names: list[str] = []
    target_actor = target_actor_id(target.id)
    for name in (target_actor,):
        if name and name != actor and name not in names:
            names.append(name)
    return names


def _actual_launch_identity(status: Any) -> dict[str, str]:
    model = str(getattr(status, "model", "") or "")
    effort = str(getattr(status, "reasoning_effort", "") or "")
    service_tier = str(getattr(status, "service_tier", "") or "")
    started_at = str(getattr(status, "started_at", "") or "")
    has_actual = bool(
        getattr(status, "thread_id", "")
        or model
        or effort
        or service_tier
        or started_at
    )
    return {
        "model": model,
        "effort": effort,
        "serviceTier": service_tier,
        "source": "agent state" if has_actual else "",
    }


def _actual_driver_identity(status: Any, actual_launch: dict[str, str]) -> str:
    if not actual_launch.get("source"):
        return ""
    state_path = getattr(status, "state_path", None)
    if state_path is None:
        return ""
    parts = list(state_path.parts)
    for index, part in enumerate(parts[:-1]):
        if part != "agents":
            continue
        dirname = parts[index + 1]
        for driver in ALL_DRIVERS:
            if driver.state_dirname == dirname:
                return driver.name
    return ""


def _serve_renewal_identity(
    store: ServeTeamStore | None,
    actor_id: str,
) -> dict[str, Any]:
    empty = {
        "state": "none",
        "teamIndex": None,
        "ancestorThreadId": "",
        "successorThreadId": "",
        "revision": 0,
    }
    if store is None:
        return empty
    renewal = store.renewal_state_for_agent(actor_id)
    team_index = _serve_team_index(store, actor_id)
    if renewal is None:
        return {**empty, "teamIndex": team_index}
    return {
        "state": renewal.state,
        "teamIndex": team_index,
        "ancestorThreadId": renewal.ancestor_thread_id,
        "successorThreadId": renewal.successor_thread_id,
        "revision": renewal.revision,
    }


def _serve_team_index(store: ServeTeamStore, actor_id: str) -> int | None:
    team_id = store.current_team_for_agent(actor_id)
    if team_id is None:
        return None
    team = store.team_state(team_id)
    for index, member in enumerate(team.members):
        if member.agent_id == actor_id:
            return index
    return None


def _required_identity_string(value: Any, label: str) -> str:
    text = str(value or "").strip()
    if not text:
        raise SpiceError(f"{label} must be non-empty in identity payload")
    return text


def _nonnegative_payload_int(value: Any, label: str) -> int:
    if value is None or value == "":
        raise SpiceError(f"{label} is required in identity payload")
    number = int(value)
    if number < 0:
        raise SpiceError(f"{label} must be non-negative in identity payload")
    return number


def renewal_intent_for_actor(store: ServeTeamStore, actor: str) -> dict[str, Any]:
    if not actor:
        return renewal_intent_payload(None)
    return renewal_intent_payload(
        store.renewal_state_for_agent(actor),
        agent_id=actor,
    )


def _agent_name_for_target(target: WorktreeTarget) -> str:
    """The agent's voice name; empty when no voice is configured."""
    voice = configured_say_voice(target.repo_root)
    if not voice:
        return ""
    return voice.split("(", 1)[0].strip()


def _binding_status(thread_id: str, binding_error: str) -> str:
    if binding_error:
        return "mismatch"
    return "bound" if thread_id else "unbound"
