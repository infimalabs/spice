"""Operator diagnostics for the serve team control plane."""

from __future__ import annotations

import json
import sqlite3
from typing import Any, Iterable

from spice.serve.teams import ServeTeamStore, TeamState
from spice.tasks import config as task_config
from spice.tasks import lanes


def team_diagnostics_payload(store: ServeTeamStore | None = None) -> dict[str, Any]:
    team_store = store or ServeTeamStore()
    snapshot = team_store.team_snapshot()
    with team_store.connect() as connection:
        events = _event_rows(connection)
        teams = _team_rows(connection)
        members = _member_rows(connection)
        renewals = _renewal_rows(connection)
    return {
        "storePath": str(team_store.path),
        "globalRevision": snapshot.global_revision,
        "events": events,
        "teams": [team.to_payload() for team in snapshot.teams],
        "teamRecords": teams,
        "closedTeams": [team for team in teams if team["status"] == "closed"],
        "members": members,
        "effectiveRoutes": _route_payloads(snapshot.teams),
        "taskDrainFilters": _task_drain_filters(snapshot.teams),
        "renewals": renewals,
    }


def render_team_diagnostics(
    payload: dict[str, Any] | None = None,
    *,
    store: ServeTeamStore | None = None,
) -> str:
    data = payload or team_diagnostics_payload(store=store)
    lines = [
        f"serve teams store={data['storePath']} "
        f"globalRevision={data['globalRevision']}",
        "events:",
    ]
    lines.extend(_render_events(data["events"]))
    lines.append("teams:")
    lines.extend(_render_teams(data["teamRecords"]))
    lines.append("members:")
    lines.extend(_render_members(data["members"]))
    lines.append("effective routes:")
    lines.extend(_render_routes(data["effectiveRoutes"]))
    lines.append("taskdrain filters:")
    lines.extend(_render_task_drain_filters(data["taskDrainFilters"]))
    lines.append("renewals:")
    lines.extend(_render_renewals(data["renewals"]))
    return "\n".join(lines)


def _event_rows(connection: sqlite3.Connection) -> list[dict[str, Any]]:
    rows = connection.execute(
        "SELECT revision, ts, kind, team_id, payload FROM events ORDER BY revision"
    ).fetchall()
    return [
        {
            "revision": int(row["revision"]),
            "timestamp": row["ts"],
            "kind": str(row["kind"]),
            "teamId": str(row["team_id"]),
            "uiId": _ui_id(json.loads(row["payload"])),
            "payload": json.loads(row["payload"]),
        }
        for row in rows
    ]


def _team_rows(connection: sqlite3.Connection) -> list[dict[str, Any]]:
    rows = connection.execute(
        "SELECT team_id, status, created_at, revision, config_revision, lifetime, "
        "speech_mode, selected_view, task_filters, shell_settings "
        "FROM teams ORDER BY created_at"
    ).fetchall()
    return [_team_row(row) for row in rows]


def _team_row(row: sqlite3.Row) -> dict[str, Any]:
    return {
        "teamId": str(row["team_id"]),
        "status": str(row["status"]),
        "createdAt": row["created_at"],
        "revision": int(row["revision"]),
        "configRevision": int(row["config_revision"]),
        "config": {
            "lifetime": str(row["lifetime"]),
            "speechMode": str(row["speech_mode"]),
            "selectedView": str(row["selected_view"]),
            "taskFilters": json.loads(row["task_filters"]),
            "shellSettings": json.loads(row["shell_settings"]),
        },
    }


def _member_rows(connection: sqlite3.Connection) -> list[dict[str, Any]]:
    rows = connection.execute(
        "SELECT team_id, agent_id, joined_at "
        "FROM memberships ORDER BY team_id, joined_at"
    ).fetchall()
    return [
        {
            "teamId": str(row["team_id"]),
            "agentId": str(row["agent_id"]),
            "joinedAt": row["joined_at"],
        }
        for row in rows
    ]


def _renewal_rows(connection: sqlite3.Connection) -> list[dict[str, Any]]:
    rows = connection.execute(
        "SELECT agent_id, team_id, state, ancestor_thread_id, "
        "successor_agent_id, successor_thread_id, team_slot, "
        "predecessor_identity, successor_identity, revision "
        "FROM renewals ORDER BY revision, agent_id"
    ).fetchall()
    return [
        {
            "agentId": str(row["agent_id"]),
            "teamId": str(row["team_id"]),
            "state": str(row["state"]),
            "ancestorThreadId": str(row["ancestor_thread_id"]),
            "successorAgentId": str(row["successor_agent_id"]),
            "successorThreadId": str(row["successor_thread_id"]),
            "teamSlot": row["team_slot"],
            "predecessorIdentity": str(row["predecessor_identity"]),
            "successorIdentity": str(row["successor_identity"]),
            "revision": int(row["revision"]),
        }
        for row in rows
    ]


def _route_payloads(teams: Iterable[TeamState]) -> list[dict[str, Any]]:
    routes: list[dict[str, Any]] = []
    for team in teams:
        member_agents = [member.agent_id for member in team.members]
        configured_terms = _filter_terms(team)
        route = {"filter": configured_terms, "lifetime": team.config.lifetime}
        effective_terms = lanes.effective_filter_terms(route)
        for actor in member_agents:
            routes.append(
                {
                    "actor": actor,
                    "teamId": team.team_id,
                    "teamRevision": team.revision,
                    "configRevision": team.config_revision,
                    "memberAgents": member_agents,
                    "lifetime": team.config.lifetime,
                    "configuredTaskFilters": list(team.config.task_filters),
                    "filterTerms": effective_terms,
                    "configuredFilterTerms": configured_terms,
                    "scope": _filter_scope(team.config.lifetime),
                    "filterArgs": lanes.filter_terms_args(effective_terms),
                    "routeFilters": [
                        task_config.private_project(actor),
                        *effective_terms,
                    ],
                }
            )
    return routes


def _task_drain_filters(teams: Iterable[TeamState]) -> list[dict[str, Any]]:
    configs: list[dict[str, Any]] = []
    for team in teams:
        filter_terms = _filter_terms(team)
        effective_terms = lanes.effective_filter_terms(
            {"filter": filter_terms, "lifetime": team.config.lifetime}
        )
        configs.append(
            {
                "teamId": team.team_id,
                "lifetime": team.config.lifetime,
                "taskFilters": list(team.config.task_filters),
                "filterTerms": filter_terms,
                "effectiveTerms": effective_terms,
                "filterArgs": lanes.filter_terms_args(effective_terms),
                "applies": True,
                "scope": _filter_scope(team.config.lifetime),
            }
        )
    return configs


def _filter_scope(lifetime: str) -> str:
    return "all-assignable" if lifetime == "Drain" else "stored"


def _filter_terms(team: TeamState) -> list[str]:
    return sorted(
        {
            f"project:{task_config.validate_assignable_project(task_filter)}"
            for task_filter in team.config.task_filters
        }
    )


def _render_events(events: list[dict[str, Any]]) -> list[str]:
    if not events:
        return ["  (none)"]
    return [
        "  revision={revision} kind={kind} team={teamId} ui={ui} payload={payload}".format(
            revision=event["revision"],
            kind=event["kind"],
            teamId=event["teamId"],
            ui=event["uiId"] or "-",
            payload=json.dumps(event["payload"], sort_keys=True),
        )
        for event in events
    ]


def _render_teams(teams: list[dict[str, Any]]) -> list[str]:
    if not teams:
        return ["  (none)"]
    return [
        "  team {teamId} status={status} revision={revision} "
        "configRevision={configRevision} lifetime={lifetime} "
        "speech={speech} view={view} taskFilters={taskFilters} "
        "shellSettings={shellSettings}".format(
            teamId=team["teamId"],
            status=team["status"],
            revision=team["revision"],
            configRevision=team["configRevision"],
            lifetime=team["config"]["lifetime"],
            speech=team["config"]["speechMode"],
            view=team["config"]["selectedView"],
            taskFilters=_csv(team["config"]["taskFilters"]),
            shellSettings=json.dumps(team["config"]["shellSettings"], sort_keys=True),
        )
        for team in teams
    ]


def _render_members(members: list[dict[str, Any]]) -> list[str]:
    if not members:
        return ["  (none)"]
    return [
        "  member {agentId} team={teamId} joinedAt={joinedAt}".format(
            agentId=member["agentId"],
            teamId=member["teamId"],
            joinedAt=member["joinedAt"],
        )
        for member in members
    ]


def _render_routes(routes: list[dict[str, Any]]) -> list[str]:
    if not routes:
        return ["  (none)"]
    return [
        "  route {actor} team={teamId} lifetime={lifetime} "
        "scope={scope} members={members} routeFilters={routeFilters} "
        "filterArgs={filterArgs}".format(
            actor=route["actor"],
            teamId=route["teamId"],
            lifetime=route["lifetime"],
            scope=route["scope"],
            members=_csv(route["memberAgents"]),
            routeFilters=_csv(route["routeFilters"]),
            filterArgs=_csv(route["filterArgs"]),
        )
        for route in routes
    ]


def _render_task_drain_filters(filters: list[dict[str, Any]]) -> list[str]:
    if not filters:
        return ["  (none)"]
    return [
        "  taskdrain team={teamId} lifetime={lifetime} applies={applies} "
        "scope={scope} taskFilters={taskFilters} filterTerms={filterTerms} "
        "effectiveTerms={effectiveTerms} filterArgs={filterArgs}".format(
            teamId=config["teamId"],
            lifetime=config["lifetime"],
            applies="yes" if config["applies"] else "no",
            scope=config["scope"],
            taskFilters=_csv(config["taskFilters"]),
            filterTerms=_csv(config["filterTerms"]),
            effectiveTerms=_csv(config["effectiveTerms"]),
            filterArgs=_csv(config["filterArgs"]),
        )
        for config in filters
    ]


def _render_renewals(renewals: list[dict[str, Any]]) -> list[str]:
    if not renewals:
        return ["  (none)"]
    return [
        "  renewal {agentId} state={state} team={teamId} ancestor={ancestor} "
        "successor={successor} successor_thread={successor_thread} "
        "slot={slot} revision={revision}".format(
            agentId=renewal["agentId"],
            state=renewal["state"],
            teamId=renewal["teamId"],
            ancestor=renewal["ancestorThreadId"] or "-",
            successor=renewal["successorAgentId"] or "-",
            successor_thread=renewal["successorThreadId"] or "-",
            slot="-" if renewal["teamSlot"] is None else renewal["teamSlot"],
            revision=renewal["revision"],
        )
        for renewal in renewals
    ]


def _ui_id(payload: dict[str, Any]) -> str:
    return str(payload.get("uiId") or payload.get("ui_id") or "")


def _csv(values: Iterable[Any]) -> str:
    rendered = [str(value) for value in values if str(value)]
    return ",".join(rendered) if rendered else "-"
