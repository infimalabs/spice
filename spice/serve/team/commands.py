"""Validation and dispatch for UI team commands."""

from __future__ import annotations

import sqlite3
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
        handler = self._COMMANDS.get(command)
        if handler is None:
            raise SpiceError(f"unknown team command {command!r}")
        expected_revision = _expected_revision(payload)
        snapshot = self.store.apply_team_command(
            expected_revision=expected_revision,
            command=lambda connection: handler(self, payload, connection),
        )
        return TeamCommandResult(revision=snapshot.global_revision, snapshot=snapshot)

    def _cmd_create_team(
        self, payload: dict[str, Any], connection: sqlite3.Connection
    ) -> None:
        config = _config_from_payload(payload.get("config"))
        members = [str(item) for item in payload.get("members") or [] if item]
        self.store._create_team_locked(connection, None, config, members)

    def _cmd_close_team(
        self, payload: dict[str, Any], connection: sqlite3.Connection
    ) -> None:
        self.store._close_team_locked(connection, _required(payload, "teamId"))

    def _cmd_assign_agent(
        self, payload: dict[str, Any], connection: sqlite3.Connection
    ) -> None:
        self.store._assign_agent_locked(
            connection,
            _required(payload, "teamId"),
            _required(payload, "agentId"),
            aliases=_aliases(payload),
        )

    def _cmd_remove_agent(
        self, payload: dict[str, Any], connection: sqlite3.Connection
    ) -> None:
        self.store._remove_agent_locked(
            connection,
            _required(payload, "teamId"),
            _required(payload, "agentId"),
            aliases=_aliases(payload),
        )

    def _cmd_split_team(
        self, payload: dict[str, Any], connection: sqlite3.Connection
    ) -> None:
        agent_ids = [str(item) for item in payload.get("agentIds") or [] if item]
        self.store._split_team_locked(
            connection,
            _required(payload, "sourceTeamId"),
            agent_ids=agent_ids,
        )

    def _cmd_split_team_back(
        self, payload: dict[str, Any], connection: sqlite3.Connection
    ) -> None:
        self.store._split_team_back_locked(
            connection, _required(payload, "sourceTeamId")
        )

    def _cmd_merge_teams(
        self, payload: dict[str, Any], connection: sqlite3.Connection
    ) -> None:
        self.store._merge_teams_locked(
            connection,
            _required(payload, "sourceTeamId"),
            _required(payload, "destinationTeamId"),
        )

    def _cmd_reorder_team_agents(
        self, payload: dict[str, Any], connection: sqlite3.Connection
    ) -> None:
        agent_ids = [str(item) for item in payload.get("agentIds") or [] if item]
        self.store._reorder_team_agents_locked(
            connection, _required(payload, "teamId"), agent_ids
        )

    def _cmd_update_team_config(
        self, payload: dict[str, Any], connection: sqlite3.Connection
    ) -> None:
        team_id = _required(payload, "teamId")
        current = _team_config_locked(self.store, connection, team_id)
        patch = payload.get("configPatch") or {}
        self.store._update_team_config_locked(
            connection,
            team_id,
            _patched_config(current, patch),
            replace_task_filters="taskFilters" in patch,
        )

    def _cmd_set_agent_renewal_intent(
        self, payload: dict[str, Any], connection: sqlite3.Connection
    ) -> None:
        self.store._set_agent_renewal_request_locked(
            connection,
            _required(payload, "agentId"),
            requested=bool(payload.get("requested")),
        )

    def _cmd_set_global_fast_mode(
        self, payload: dict[str, Any], connection: sqlite3.Connection
    ) -> None:
        raw_fast_mode = payload.get("fastMode")
        if not isinstance(raw_fast_mode, bool):
            raise SpiceError("fastMode must be a boolean")
        self.store._set_global_fast_mode_enabled_locked(connection, raw_fast_mode)

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
        "setGlobalFastMode": _cmd_set_global_fast_mode,
    }


def _required(payload: dict[str, Any], key: str) -> str:
    from spice.serve.team.store import _normalized_id

    return _normalized_id(str(payload.get(key) or ""), key)


def _aliases(payload: dict[str, Any]) -> list[str]:
    return [str(item) for item in payload.get("agentAliases") or [] if item]


def _expected_revision(payload: dict[str, Any]) -> int | None:
    if "expectedRevision" not in payload:
        return None
    raw_revision = payload.get("expectedRevision")
    if raw_revision is None:
        raise SpiceError("expectedRevision must be a non-negative integer")
    try:
        revision = int(raw_revision)
    except (TypeError, ValueError) as exc:
        raise SpiceError("expectedRevision must be a non-negative integer") from exc
    if revision < 0:
        raise SpiceError("expectedRevision must be a non-negative integer")
    return revision


def _team_config_locked(
    store: Any, connection: sqlite3.Connection, team_id: str
) -> Any:
    from spice.serve.team.filters import config_from_row

    row = store._require_team(connection, team_id)
    entries = store._task_filter_entries_locked(connection, team_id)
    return config_from_row(row, entries)


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
