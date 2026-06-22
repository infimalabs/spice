"""Validation and dispatch for UI team commands."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from spice.errors import SpiceError


@dataclass(frozen=True)
class TeamCommandResult:
    revision: int
    snapshot: Any


class TeamCommandService:
    """Validate and apply UI team commands with optimistic concurrency."""

    def __init__(self, store: Any | None = None) -> None:
        if store is None:
            from spice.serve.team.store import ServeTeamStore

            store = ServeTeamStore()
        self.store = store

    def apply(self, payload: dict[str, Any]) -> TeamCommandResult:
        command = str(payload.get("command") or "")
        # Stale client: commands apply against current state anyway when they
        # remain valid; an invalid one raises with detail from its handler.
        handler = self._COMMANDS.get(command)
        if handler is None:
            raise SpiceError(f"unknown team command {command!r}")
        handler(self, payload)
        snapshot = self.store.team_snapshot()
        return TeamCommandResult(revision=snapshot.global_revision, snapshot=snapshot)

    def _cmd_create_team(self, payload: dict[str, Any]) -> None:
        config = _config_from_payload(payload.get("config"))
        members = [str(item) for item in payload.get("members") or [] if item]
        self.store.create_team(config=config, members=members)

    def _cmd_close_team(self, payload: dict[str, Any]) -> None:
        self.store.close_team(_required(payload, "teamId"))

    def _cmd_assign_agent(self, payload: dict[str, Any]) -> None:
        self.store.assign_agent(
            _required(payload, "teamId"),
            _required(payload, "agentId"),
            aliases=_aliases(payload),
        )

    def _cmd_remove_agent(self, payload: dict[str, Any]) -> None:
        self.store.remove_agent(
            _required(payload, "teamId"),
            _required(payload, "agentId"),
            aliases=_aliases(payload),
        )

    def _cmd_split_team(self, payload: dict[str, Any]) -> None:
        agent_ids = [str(item) for item in payload.get("agentIds") or [] if item]
        self.store.split_team(_required(payload, "sourceTeamId"), agent_ids=agent_ids)

    def _cmd_split_team_back(self, payload: dict[str, Any]) -> None:
        self.store.split_team_back(_required(payload, "sourceTeamId"))

    def _cmd_merge_teams(self, payload: dict[str, Any]) -> None:
        self.store.merge_teams(
            _required(payload, "sourceTeamId"),
            _required(payload, "destinationTeamId"),
        )

    def _cmd_reorder_team_agents(self, payload: dict[str, Any]) -> None:
        agent_ids = [str(item) for item in payload.get("agentIds") or [] if item]
        self.store.reorder_team_agents(_required(payload, "teamId"), agent_ids)

    def _cmd_update_team_config(self, payload: dict[str, Any]) -> None:
        team_id = _required(payload, "teamId")
        current = self.store.team_config(team_id)
        patch = payload.get("configPatch") or {}
        self.store.update_team_config(
            team_id,
            _patched_config(current, patch),
            replace_task_filters="taskFilters" in patch,
        )

    def _cmd_set_agent_renewal_intent(self, payload: dict[str, Any]) -> None:
        self.store.set_agent_renewal_request(
            _required(payload, "agentId"),
            requested=bool(payload.get("requested")),
        )

    _COMMANDS = {
        "createTeam": _cmd_create_team,
        "closeTeam": _cmd_close_team,
        "moveAgentToTeam": _cmd_assign_agent,
        "moveComposerToTeam": _cmd_assign_agent,
        "removeAgentFromTeam": _cmd_remove_agent,
        "splitTeam": _cmd_split_team,
        "splitTeamBack": _cmd_split_team_back,
        "mergeTeams": _cmd_merge_teams,
        "reorderTeamAgents": _cmd_reorder_team_agents,
        "updateTeamConfig": _cmd_update_team_config,
        "setAgentRenewalIntent": _cmd_set_agent_renewal_intent,
    }


def _required(payload: dict[str, Any], key: str) -> str:
    from spice.serve.team.store import _normalized_id

    return _normalized_id(str(payload.get(key) or ""), key)


def _aliases(payload: dict[str, Any]) -> list[str]:
    return [str(item) for item in payload.get("agentAliases") or [] if item]


def _config_from_payload(raw: Any) -> Any:
    from spice.serve.team.store import TeamConfig

    if not isinstance(raw, dict):
        return TeamConfig()
    return _patched_config(TeamConfig(), raw)


def _patched_config(current: Any, patch: dict[str, Any]) -> Any:
    from spice.serve.team.filters import validated_task_filter_projects
    from spice.serve.team.store import TeamConfig

    task_filters = current.task_filters
    if isinstance(patch.get("taskFilters"), list):
        task_filters = validated_task_filter_projects(
            str(item) for item in patch["taskFilters"]
        )
    shell_settings = current.shell_settings
    if isinstance(patch.get("shellSettings"), dict):
        shell_settings = dict(patch["shellSettings"])
    return TeamConfig(
        lifetime=str(patch.get("lifetime") or current.lifetime),
        speech_mode=str(patch.get("speechMode") or current.speech_mode),
        task_filters=task_filters,
        selected_view=str(patch.get("selectedView") or current.selected_view),
        shell_settings=shell_settings,
    )
