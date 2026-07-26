"""Team membership command storage for the serve team control plane."""

from __future__ import annotations

import json
import sqlite3
import time
import uuid as uuidlib
from contextlib import AbstractContextManager
from typing import Any, Iterable, Mapping, Protocol

from spice.errors import SpiceError
from spice.serve.team.filters import config_from_row
from spice.serve.team.ids import agent_alias_ids as _agent_alias_ids
from spice.serve.team.ids import normalized_id as _normalized_id
from spice.serve.team.models import TeamConfig, TeamState
from spice.serve.team.schema import (
    RENEWAL_STATE_STARTED,
    TEAM_ID_HEX_CHARS,
)

# A team fills six accent slots (the message occupant palette in
# app.render.js has exactly six colors and throws on a seventh). Enforce that
# ceiling at every membership-growing path -- create, single assign/move, and
# merge -- so a merge or a driver switch that appends a successor can never
# push a team past what it can render. Renewal successors that inherit an
# existing slot do not count as growth and are never blocked.
MAX_TEAM_MEMBERS = 6


class _TeamMemberStore(Protocol):
    def connect(self) -> AbstractContextManager[sqlite3.Connection]: ...

    def _replace_task_filters_locked(
        self, connection: sqlite3.Connection, team_id: str, projects: Iterable[str]
    ) -> tuple[str, ...]: ...

    def _task_filter_entries_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> tuple[Any, ...]: ...

    def _record_event(
        self,
        connection: sqlite3.Connection,
        kind: str,
        team_id: str,
        payload: dict[str, Any],
        *,
        wake: bool = True,
    ) -> int: ...

    def _mark_team_revisions_locked(
        self,
        connection: sqlite3.Connection,
        team_ids: Iterable[str],
        revision: int,
    ) -> None: ...

    def _require_team(
        self, connection: sqlite3.Connection, team_id: str
    ) -> sqlite3.Row: ...

    def _team_state_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> TeamState: ...

    def _ensure_open_team_locked(
        self, connection: sqlite3.Connection
    ) -> TeamState | None: ...

    def _transfer_active_renewal_locked(
        self,
        connection: sqlite3.Connection,
        old_agent_id: str,
        new_agent_id: str,
        team_id: str,
    ) -> None: ...

    def _record_merge_subgroup_locked(
        self,
        connection: sqlite3.Connection,
        *,
        parent_team_id: str,
        child_team_id: str,
        merged_revision: int,
        agent_ids: Iterable[str],
    ) -> None: ...

    def _latest_restorable_subgroup_locked(
        self, connection: sqlite3.Connection, parent_team_id: str
    ) -> tuple[sqlite3.Row, tuple[str, ...]] | None: ...

    def _create_team_locked(
        self,
        connection: sqlite3.Connection,
        team_id: str | None,
        config: TeamConfig,
        members: Iterable[str],
        *,
        member_aliases: Mapping[str, Iterable[str]] | None = None,
    ) -> TeamState: ...

    def _reuse_open_shell_team_locked(
        self,
        connection: sqlite3.Connection,
        config: TeamConfig,
        member_list: list[str],
        *,
        member_aliases: Mapping[str, Iterable[str]],
    ) -> TeamState | None: ...

    def _assign_agent_locked(
        self,
        connection: sqlite3.Connection,
        team_id: str,
        agent_id: str,
        aliases: Iterable[str] = (),
    ) -> int: ...

    def _assign_locked(
        self,
        connection: sqlite3.Connection,
        team_id: str,
        agent_id: str,
        aliases: Iterable[str] = (),
    ) -> list[str]: ...

    def _vacate_assigned_slots_locked(
        self,
        connection: sqlite3.Connection,
        team_id: str,
        agent_id: str,
        alias_ids: list[str],
    ) -> tuple[int | None, list[str]]: ...

    def _close_empty_teams_locked(
        self, connection: sqlite3.Connection, team_ids: Iterable[str]
    ) -> None: ...

    def _team_member_count_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> int: ...


class TeamMemberStoreMixin:
    """Own team membership mutations and their roster invariants."""

    def create_team(
        self: _TeamMemberStore,
        *,
        team_id: str | None = None,
        config: TeamConfig | None = None,
        members: Iterable[str] = (),
    ) -> TeamState:
        config = config or TeamConfig()
        with self.connect() as connection:
            return self._create_team_locked(connection, team_id, config, members)

    def _create_team_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        team_id: str | None,
        config: TeamConfig,
        members: Iterable[str],
        *,
        member_aliases: Mapping[str, Iterable[str]] | None = None,
    ) -> TeamState:
        member_list = list(members)
        aliases_by_member = member_aliases or {}
        if len(dict.fromkeys(member_list)) > MAX_TEAM_MEMBERS:
            raise SpiceError(
                f"team is limited to {MAX_TEAM_MEMBERS} agents; "
                f"cannot create a team of {len(member_list)}"
            )
        if team_id is None:
            reused = self._reuse_open_shell_team_locked(
                connection,
                config,
                member_list,
                member_aliases=aliases_by_member,
            )
            if reused is not None:
                return reused
        resolved_team_id = team_id or f"team-{uuidlib.uuid4().hex[:TEAM_ID_HEX_CHARS]}"
        connection.execute(
            "INSERT INTO teams (team_id, status, created_at, revision, "
            "config_revision, lifetime, "
            "task_filters, shell_settings) VALUES (?, 'open', ?, 0, 0, ?, ?, ?)",
            (
                resolved_team_id,
                time.time(),
                config.lifetime,
                json.dumps(list(config.task_filters)),
                json.dumps(config.shell_settings),
            ),
        )
        self._replace_task_filters_locked(
            connection, resolved_team_id, config.task_filters
        )
        previous_team_ids: list[str] = []
        for agent_id in member_list:
            previous_team_ids.extend(
                self._assign_locked(
                    connection,
                    resolved_team_id,
                    agent_id,
                    aliases=aliases_by_member.get(agent_id, ()),
                )
            )
        revision = self._record_event(
            connection, "createTeam", resolved_team_id, {"members": member_list}
        )
        self._mark_team_revisions_locked(connection, previous_team_ids, revision)
        return self._team_state_locked(connection, resolved_team_id)

    def _reuse_open_shell_team_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        config: TeamConfig,
        member_list: list[str],
        *,
        member_aliases: Mapping[str, Iterable[str]],
    ) -> TeamState | None:
        # The ensure-open-team affordance keeps one member-less shell around
        # so the operator can import an agent with a single click. A fresh
        # team request reuses the oldest shell instead of minting a sibling,
        # so shells never accumulate next to deliberately created teams.
        row = connection.execute(
            "SELECT teams.team_id FROM teams "
            "LEFT JOIN memberships ON memberships.team_id = teams.team_id "
            "WHERE teams.status = 'open' AND memberships.agent_id IS NULL "
            "ORDER BY teams.created_at LIMIT 1"
        ).fetchone()
        if row is None:
            return None
        shell_team_id = str(row["team_id"])
        connection.execute(
            "UPDATE teams SET lifetime = ?, "
            "shell_settings = ?, "
            "config_revision = config_revision + 1 WHERE team_id = ?",
            (
                config.lifetime,
                json.dumps(config.shell_settings),
                shell_team_id,
            ),
        )
        self._replace_task_filters_locked(
            connection, shell_team_id, config.task_filters
        )
        previous_team_ids: list[str] = []
        for agent_id in member_list:
            previous_team_ids.extend(
                self._assign_locked(
                    connection,
                    shell_team_id,
                    agent_id,
                    aliases=member_aliases.get(agent_id, ()),
                )
            )
        revision = self._record_event(
            connection,
            "createTeam",
            shell_team_id,
            {"members": member_list, "reusedOpenShell": True},
        )
        self._mark_team_revisions_locked(connection, previous_team_ids, revision)
        return self._team_state_locked(connection, shell_team_id)

    def _close_team_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        team_id: str,
    ) -> int:
        self._require_team(connection, team_id)
        connection.execute(
            "UPDATE teams SET status = 'closed' WHERE team_id = ?", (team_id,)
        )
        connection.execute("DELETE FROM memberships WHERE team_id = ?", (team_id,))
        revision = self._record_event(connection, "closeTeam", team_id, {})
        replacement = self._ensure_open_team_locked(connection)
        return replacement.revision if replacement else revision

    def assign_agent(
        self: _TeamMemberStore,
        team_id: str,
        agent_id: str,
        aliases: Iterable[str] = (),
    ) -> int:
        with self.connect() as connection:
            return self._assign_agent_locked(
                connection, team_id, agent_id, aliases=aliases
            )

    def _assign_agent_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        team_id: str,
        agent_id: str,
        aliases: Iterable[str] = (),
    ) -> int:
        self._require_team(connection, team_id)
        aliases = tuple(aliases)
        previous_team_ids = self._assign_locked(
            connection, team_id, agent_id, aliases=aliases
        )
        alias_ids = [
            alias_id
            for alias_id in _agent_alias_ids(agent_id, aliases)
            if alias_id != agent_id
        ]
        revision = self._record_event(
            connection,
            "assignAgent",
            team_id,
            {"agentId": agent_id, "aliases": alias_ids},
        )
        self._mark_team_revisions_locked(connection, previous_team_ids, revision)
        return revision

    def _assign_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        team_id: str,
        agent_id: str,
        aliases: Iterable[str] = (),
    ) -> list[str]:
        agent_id = _normalized_id(agent_id, "agent_id")
        alias_ids = _agent_alias_ids(agent_id, aliases)
        for alias_id in alias_ids:
            if alias_id != agent_id:
                self._transfer_active_renewal_locked(
                    connection, alias_id, agent_id, team_id
                )
        inherited_position, previous_team_ids = self._vacate_assigned_slots_locked(
            connection, team_id, agent_id, alias_ids
        )
        if inherited_position is None:
            # Genuinely new member (no alias already holds a slot here), so
            # this grows the team -- enforce the ceiling. Renewal successors
            # took the inherited-position branch above and are exempt.
            if self._team_member_count_locked(connection, team_id) >= MAX_TEAM_MEMBERS:
                raise SpiceError(
                    f"team is limited to {MAX_TEAM_MEMBERS} agents; "
                    f"{agent_id} would exceed it"
                )
            row = connection.execute(
                "SELECT COALESCE(MAX(position) + 1, 0) AS position "
                "FROM memberships WHERE team_id = ?",
                (team_id,),
            ).fetchone()
            inherited_position = int(row["position"] or 0)
        connection.execute(
            "INSERT INTO memberships (team_id, agent_id, joined_at, position) "
            "VALUES (?, ?, ?, ?)",
            (
                team_id,
                agent_id,
                time.time(),
                inherited_position,
            ),
        )
        self._close_empty_teams_locked(connection, previous_team_ids)
        return previous_team_ids

    def _vacate_assigned_slots_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        team_id: str,
        agent_id: str,
        alias_ids: list[str],
    ) -> tuple[int | None, list[str]]:
        """Free the slots this assignment takes over; report its inherited position.

        An alias is another name the same lane already answers to, so it is only
        ever a way to find a slot that lane already holds. Exactly two such
        slots belong to this assignment. One is held in this very team: a
        renewal successor (or a placeholder promoted to its real thread) takes
        that position instead of appending, which keeps it in the predecessor's
        visible slot on a roster ordered by position. The other is held in the
        team the lane is leaving -- the first team any of its names sits in, its
        own id first, the same precedence `_remove_agent_locked` uses to decide
        which team a client meant.

        A lane can hold a slot under one of its other names in some third team.
        That team is a bystander: this assignment neither leaves it nor joins
        it, so it keeps every member it had. Only the id actually being seated
        is freed everywhere, because the insert that follows is the one place
        that id now lives.

        Returns the position to inherit here (None when there is none to
        inherit) and the teams this vacated, which may now be empty.
        """
        slots = [
            (alias_id, str(row["team_id"]), int(row["position"] or 0))
            for alias_id in alias_ids
            for row in connection.execute(
                "SELECT team_id, position FROM memberships WHERE agent_id = ?",
                (alias_id,),
            ).fetchall()
        ]
        held_here = [
            position for _, slot_team, position in slots if slot_team == team_id
        ]
        departed_team_id = next(
            (slot_team for _, slot_team, _ in slots if slot_team != team_id), None
        )
        previous_team_ids: list[str] = []
        for alias_id, slot_team, _ in slots:
            vacated = (
                slot_team == team_id
                or slot_team == departed_team_id
                or alias_id == agent_id
            )
            if not vacated:
                continue
            connection.execute(
                "DELETE FROM memberships WHERE agent_id = ? AND team_id = ?",
                (alias_id, slot_team),
            )
            if slot_team != team_id and slot_team not in previous_team_ids:
                previous_team_ids.append(slot_team)
        return (min(held_here) if held_here else None, previous_team_ids)

    def _close_empty_teams_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        team_ids: Iterable[str],
    ) -> None:
        for team_id in team_ids:
            count = connection.execute(
                "SELECT COUNT(*) AS count FROM memberships WHERE team_id = ?",
                (team_id,),
            ).fetchone()
            if count and int(count["count"] or 0) > 0:
                continue
            connection.execute(
                "UPDATE teams SET status = 'closed' WHERE team_id = ?",
                (team_id,),
            )
            self._record_event(connection, "closeEmptyTeam", team_id, {})

    def _remove_agent_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        team_id: str,
        agent_id: str,
        aliases: Iterable[str] = (),
    ) -> int:
        alias_ids = _agent_alias_ids(agent_id, aliases)
        row = None
        for alias_id in alias_ids:
            row = connection.execute(
                "SELECT team_id FROM memberships WHERE agent_id = ?",
                (alias_id,),
            ).fetchone()
            if row is not None:
                break
        if row is None or row["team_id"] != team_id:
            raise SpiceError(f"agent {agent_id} is not assigned to team {team_id}")
        for alias_id in alias_ids:
            # Scoped to the named team: the aliases resolved which slot in it
            # the client meant, and a slot the same lane holds in another team
            # is that team's business, not this removal's.
            connection.execute(
                "DELETE FROM memberships WHERE agent_id = ? AND team_id = ?",
                (alias_id, team_id),
            )
            connection.execute(
                "DELETE FROM renewals WHERE agent_id = ? AND state != ?",
                (alias_id, RENEWAL_STATE_STARTED),
            )
        self._close_empty_teams_locked(connection, [team_id])
        revision = self._record_event(
            connection, "removeAgent", team_id, {"agentId": agent_id}
        )
        replacement = self._ensure_open_team_locked(connection)
        return replacement.revision if replacement else revision

    def _split_team_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        source_team_id: str,
        *,
        agent_ids: Iterable[str],
        new_team_id: str | None = None,
        config: TeamConfig | None = None,
    ) -> TeamState:
        agent_list = [_normalized_id(agent, "agent_id") for agent in agent_ids]
        if not agent_list:
            raise SpiceError("split requires at least one agent id")
        source_row = self._require_team(connection, source_team_id)
        source_config = config_from_row(
            source_row, self._task_filter_entries_locked(connection, source_team_id)
        )
        created = self._create_team_locked(
            connection, new_team_id, config or source_config, ()
        )
        for agent_id in agent_list:
            row = connection.execute(
                "SELECT team_id FROM memberships WHERE agent_id = ?", (agent_id,)
            ).fetchone()
            if row is None or row["team_id"] != source_team_id:
                raise SpiceError(
                    f"agent {agent_id} is not assigned to team {source_team_id}"
                )
            self._assign_locked(connection, created.team_id, agent_id)
        self._record_event(
            connection,
            "splitTeam",
            source_team_id,
            {"newTeamId": created.team_id, "agents": agent_list},
        )
        return self._team_state_locked(connection, created.team_id)

    def _split_team_back_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        source_team_id: str,
    ) -> TeamState:
        self._require_team(connection, source_team_id)
        subgroup = self._latest_restorable_subgroup_locked(connection, source_team_id)
        if subgroup is None:
            raise SpiceError(
                f"team {source_team_id} has no preserved subgroup to split"
            )
        row, agent_ids = subgroup
        child_team_id = str(row["child_team_id"])
        self._require_team(connection, child_team_id)
        connection.execute(
            "UPDATE teams SET status = 'open' WHERE team_id = ?",
            (child_team_id,),
        )
        for agent_id in agent_ids:
            self._assign_locked(connection, child_team_id, agent_id)
        revision = self._record_event(
            connection,
            "splitTeamBack",
            source_team_id,
            {"restoredTeamId": child_team_id, "agents": list(agent_ids)},
        )
        connection.execute(
            "UPDATE teams SET revision = ? WHERE team_id = ?",
            (revision, child_team_id),
        )
        connection.execute(
            "UPDATE team_merge_subgroups SET restored_revision = ? "
            "WHERE parent_team_id = ? AND child_team_id = ? "
            "AND merged_revision = ?",
            (
                revision,
                source_team_id,
                child_team_id,
                int(row["merged_revision"]),
            ),
        )
        return self._team_state_locked(connection, child_team_id)

    def _merge_teams_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        source_team_id: str,
        destination_team_id: str,
    ) -> int:
        if source_team_id == destination_team_id:
            raise SpiceError("merge requires two distinct teams")
        self._require_team(connection, source_team_id)
        self._require_team(connection, destination_team_id)
        rows = connection.execute(
            "SELECT agent_id FROM memberships WHERE team_id = ? ORDER BY position",
            (source_team_id,),
        ).fetchall()
        agent_ids = [str(row["agent_id"]) for row in rows]
        # Reject the whole merge up front rather than half-filling the
        # destination: source agents are distinct membership ids not present
        # in the destination, so every one is net-new growth.
        destination_count = self._team_member_count_locked(
            connection, destination_team_id
        )
        if destination_count + len(agent_ids) > MAX_TEAM_MEMBERS:
            raise SpiceError(
                f"team is limited to {MAX_TEAM_MEMBERS} agents: merging "
                f"{len(agent_ids)} into a team of {destination_count} exceeds it"
            )
        for agent_id in agent_ids:
            self._assign_locked(connection, destination_team_id, agent_id)
        connection.execute(
            "UPDATE teams SET status = 'closed' WHERE team_id = ?",
            (source_team_id,),
        )
        revision = self._record_event(
            connection,
            "mergeTeams",
            destination_team_id,
            {"sourceTeamId": source_team_id, "agents": agent_ids},
        )
        if agent_ids:
            self._record_merge_subgroup_locked(
                connection,
                parent_team_id=destination_team_id,
                child_team_id=source_team_id,
                merged_revision=revision,
                agent_ids=agent_ids,
            )
        return revision

    def _reorder_team_agents_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        team_id: str,
        agents: Iterable[str | tuple[str, Iterable[str]]],
    ) -> int:
        self._require_team(connection, team_id)
        rows = connection.execute(
            "SELECT agent_id, position FROM memberships"
            " WHERE team_id = ? ORDER BY position",
            (team_id,),
        ).fetchall()
        current_agent_ids = [str(row["agent_id"]) for row in rows]
        current_set = set(current_agent_ids)
        # A PARTIAL reorder. The client only knows the members it has open as
        # composers, which is routinely a subset of the team (a member can be
        # closed on the client, or an extra membership can linger) -- so
        # reorder must never demand the full set. Entries are bare ids or
        # (id, aliases); each resolves to its membership id via alias, exactly
        # like _remove_agent_locked. The resolved subset is permuted among the
        # slots those same members currently occupy; every unmentioned member
        # keeps its position. Requiring the exact set here is what rejected
        # every real drag ("reorder requires exactly the current team
        # members") on a team with any closed or lingering member.
        ordered_agent_ids = []
        for entry in agents:
            agent_id, aliases = entry if isinstance(entry, tuple) else (entry, ())
            alias_ids = _agent_alias_ids(_normalized_id(agent_id, "agent_id"), aliases)
            member_id = next(
                (alias for alias in alias_ids if alias in current_set), None
            )
            if member_id is None:
                raise SpiceError(f"agent {agent_id} is not assigned to team {team_id}")
            ordered_agent_ids.append(member_id)
        if len(set(ordered_agent_ids)) != len(ordered_agent_ids):
            raise SpiceError("reorder requires unique agent ids")
        reorder_set = set(ordered_agent_ids)
        # The slots the mentioned members occupy, ascending; fill them in the
        # requested order. Rows are already position-ordered.
        target_positions = [
            int(row["position"]) for row in rows if str(row["agent_id"]) in reorder_set
        ]
        for position, agent_id in zip(target_positions, ordered_agent_ids):
            connection.execute(
                "UPDATE memberships SET position = ? "
                "WHERE team_id = ? AND agent_id = ?",
                (position, team_id, agent_id),
            )
        # A reorder permutes member order only -- it adds and removes no
        # messages and no members, so no lane's content changes and it must
        # NOT wake the lane watchers. The new order reaches clients on the team
        # channel (the command response, and teams.refresh for others), never
        # the lane bus -- and that holds however the order is later read (a
        # lead, say): order semantics ride the team channel, not lane content.
        # Waking made every swap re-push all members' messages and re-render
        # the whole board (a visible reflow, and a spurious history re-pagination).
        return self._record_event(
            connection,
            "reorderTeamAgents",
            team_id,
            {"agentIds": ordered_agent_ids},
            wake=False,
        )

    def _team_member_count_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        team_id: str,
    ) -> int:
        row = connection.execute(
            "SELECT COUNT(*) AS count FROM memberships WHERE team_id = ?",
            (team_id,),
        ).fetchone()
        return int(row["count"] or 0) if row else 0

    def _team_member_ids_locked(
        self: _TeamMemberStore,
        connection: sqlite3.Connection,
        team_id: str,
    ) -> list[str]:
        rows = connection.execute(
            "SELECT agent_id FROM memberships WHERE team_id = ? ORDER BY position",
            (team_id,),
        ).fetchall()
        return [str(row["agent_id"]) for row in rows]
