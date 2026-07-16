"""Adapters for exercising the canonical locked team-store operations in tests."""

from collections.abc import Iterable

from spice.serve.team.models import TeamConfig, TeamState
from spice.serve.team.store import ServeTeamStore


def store_global_revision(store: ServeTeamStore) -> int:
    return store.team_snapshot().global_revision


def store_prune_zero_activity_closed_teams(
    store: ServeTeamStore,
) -> tuple[str, ...]:
    with store.connect() as connection:
        return store._prune_zero_activity_closed_teams_locked(connection)


def store_close_team(store: ServeTeamStore, team_id: str) -> int:
    with store.connect() as connection:
        return store._close_team_locked(connection, team_id)


def store_remove_agent(
    store: ServeTeamStore,
    team_id: str,
    agent_id: str,
    aliases: Iterable[str] = (),
) -> int:
    with store.connect() as connection:
        return store._remove_agent_locked(
            connection, team_id, agent_id, aliases=aliases
        )


def store_split_team(
    store: ServeTeamStore,
    source_team_id: str,
    *,
    agent_ids: Iterable[str],
    new_team_id: str | None = None,
    config: TeamConfig | None = None,
) -> TeamState:
    with store.connect() as connection:
        return store._split_team_locked(
            connection,
            source_team_id,
            agent_ids=agent_ids,
            new_team_id=new_team_id,
            config=config,
        )


def store_split_team_back(store: ServeTeamStore, source_team_id: str) -> TeamState:
    with store.connect() as connection:
        return store._split_team_back_locked(connection, source_team_id)


def store_merge_teams(
    store: ServeTeamStore, source_team_id: str, destination_team_id: str
) -> int:
    with store.connect() as connection:
        return store._merge_teams_locked(
            connection, source_team_id, destination_team_id
        )


def store_reorder_team_agents(
    store: ServeTeamStore, team_id: str, agent_ids: Iterable[str]
) -> int:
    with store.connect() as connection:
        return store._reorder_team_agents_locked(connection, team_id, agent_ids)
