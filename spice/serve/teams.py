"""The serve team control plane: durable, revisioned lane grouping.

Teams are the server-side truth behind the UI's lane groups. Every mutation
is an event with a monotonically increasing global revision; clients carry
`expectedRevision` for optimistic concurrency and re-pull snapshots when they
lose. The store is SQLite under the task backend root so every worktree of a
repository shares one control plane.

Commands: create, close, move agent (composer drag), remove agent, split,
merge, update config, record renewal (pending while the predecessor runs;
started once the successor exists).
"""

from __future__ import annotations

import json
import sqlite3
import time
import uuid as uuidlib
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable, Iterator

from spice.errors import SpiceError
from spice.serve.teamids import agent_alias_ids as _agent_alias_ids
from spice.serve.teamids import normalized_id as _normalized_id
from spice.serve.teammetrics import (
    METRIC_BUCKET_SECONDS as METRIC_BUCKET_SECONDS,
    LaneMetricSummary as LaneMetricSummary,
    TeamMetricStoreMixin,
)
from spice.serve.teamschema import (
    DEFAULT_LIFETIME as DEFAULT_LIFETIME,
    DEFAULT_SELECTED_VIEW as DEFAULT_SELECTED_VIEW,
    DEFAULT_SPEECH_MODE as DEFAULT_SPEECH_MODE,
    RENEWAL_STATE_PENDING as RENEWAL_STATE_PENDING,
    RENEWAL_STATE_REQUESTED as RENEWAL_STATE_REQUESTED,
    RENEWAL_STATE_STARTED as RENEWAL_STATE_STARTED,
    TASK_FILTER_SOURCE_AUTO_CLAIM as TASK_FILTER_SOURCE_AUTO_CLAIM,
    TASK_FILTER_SOURCE_AUTO_CREATE as TASK_FILTER_SOURCE_AUTO_CREATE,
    TASK_FILTER_SOURCE_MANUAL as TASK_FILTER_SOURCE_MANUAL,
    TASK_FILTER_SOURCES as TASK_FILTER_SOURCES,
    TEAM_DATABASE_FILENAME as TEAM_DATABASE_FILENAME,
    TEAM_ID_HEX_CHARS as TEAM_ID_HEX_CHARS,
    TEAM_SCHEMA,
    TEAM_SQLITE_BUSY_TIMEOUT_MS as TEAM_SQLITE_BUSY_TIMEOUT_MS,
)

ZERO_ACTIVITY_EVENT_KINDS = frozenset(
    {
        "createTeam",
        "closeTeam",
        "closeEmptyTeam",
        "assignAgent",
        "removeAgent",
        "reorderTeamAgents",
    }
)
PRUNE_EVENT_TEAM_ID = "__system__"


@dataclass(frozen=True)
class TeamTaskFilter:
    project: str
    source: str = TASK_FILTER_SOURCE_MANUAL

    def to_payload(self) -> dict[str, str]:
        return {"project": self.project, "source": self.source}


@dataclass(frozen=True)
class TeamConfig:
    lifetime: str = DEFAULT_LIFETIME
    speech_mode: str = DEFAULT_SPEECH_MODE
    task_filters: tuple[str, ...] = ()
    task_filter_entries: tuple[TeamTaskFilter, ...] = ()
    selected_view: str = DEFAULT_SELECTED_VIEW
    shell_settings: dict[str, Any] = field(default_factory=dict)

    def to_payload(self, revision: int) -> dict[str, Any]:
        return {
            "lifetime": self.lifetime,
            "speechMode": self.speech_mode,
            "taskFilters": list(self.task_filters),
            "taskFilterEntries": [
                entry.to_payload() for entry in self.task_filter_entries
            ],
            "selectedView": self.selected_view,
            "shellSettings": dict(self.shell_settings),
            "revision": revision,
        }


@dataclass(frozen=True)
class TeamMember:
    agent_id: str
    agent_facts: dict[str, str] = field(default_factory=dict)
    renewal: TeamRenewalState | None = None

    def to_payload(self) -> dict[str, Any]:
        return {
            "agentId": self.agent_id,
            "agentFacts": dict(self.agent_facts),
            "renewalIntent": renewal_intent_payload(
                self.renewal, agent_id=self.agent_id
            ),
        }


@dataclass(frozen=True)
class TeamState:
    team_id: str
    status: str
    revision: int
    config_revision: int
    config: TeamConfig
    members: tuple[TeamMember, ...]
    split_back_available: bool = False
    split_back_member_count: int = 0

    def to_payload(self) -> dict[str, Any]:
        return {
            "teamId": self.team_id,
            "status": self.status,
            "revision": self.revision,
            "config": self.config.to_payload(self.config_revision),
            "members": [member.to_payload() for member in self.members],
            "splitBack": {
                "available": self.split_back_available,
                "memberCount": self.split_back_member_count,
            },
        }


@dataclass(frozen=True)
class TeamSnapshot:
    global_revision: int
    teams: tuple[TeamState, ...]

    def to_payload(self) -> dict[str, Any]:
        return {
            "globalRevision": self.global_revision,
            "teams": [team.to_payload() for team in self.teams],
        }


@dataclass(frozen=True)
class TeamRenewalState:
    agent_id: str
    team_id: str
    state: str
    ancestor_thread_id: str
    successor_agent_id: str
    revision: int

    @property
    def requested(self) -> bool:
        return self.state == RENEWAL_STATE_REQUESTED


def renewal_intent_payload(
    renewal: TeamRenewalState | None, *, agent_id: str = ""
) -> dict[str, Any]:
    resolved_agent_id = renewal.agent_id if renewal is not None else agent_id
    return {
        "agentId": resolved_agent_id,
        "requested": bool(renewal and renewal.requested),
        "state": renewal.state if renewal is not None else "",
        "teamId": renewal.team_id if renewal is not None else "",
        "ancestorThreadId": renewal.ancestor_thread_id if renewal is not None else "",
        "successorAgentId": renewal.successor_agent_id if renewal is not None else "",
        "revision": renewal.revision if renewal is not None else 0,
    }


def team_database_path() -> Path:
    from spice.tasks import config as task_config

    return task_config.data_dir() / TEAM_DATABASE_FILENAME


class ServeTeamStore(TeamMetricStoreMixin):
    def __init__(self, path: Path | None = None) -> None:
        self.path = path or team_database_path()

    @contextmanager
    def connect(self) -> Iterator[sqlite3.Connection]:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        connection = sqlite3.connect(self.path)
        connection.row_factory = sqlite3.Row
        try:
            connection.execute(f"PRAGMA busy_timeout = {TEAM_SQLITE_BUSY_TIMEOUT_MS}")
            connection.execute("PRAGMA journal_mode = WAL")
            connection.executescript(TEAM_SCHEMA)
            self._migrate_task_filter_sources_locked(connection)
            yield connection
            connection.commit()
        finally:
            connection.close()

    # ---- events / revisions -------------------------------------------

    def _record_event(
        self,
        connection: sqlite3.Connection,
        kind: str,
        team_id: str,
        payload: dict[str, Any],
    ) -> int:
        cursor = connection.execute(
            "INSERT INTO events (ts, kind, team_id, payload) VALUES (?, ?, ?, ?)",
            (time.time(), kind, team_id, json.dumps(payload, separators=(",", ":"))),
        )
        revision = int(cursor.lastrowid or 0)
        connection.execute(
            "UPDATE teams SET revision = ? WHERE team_id = ?", (revision, team_id)
        )
        return revision

    def _current_revision_locked(self, connection: sqlite3.Connection) -> int:
        row = connection.execute("SELECT MAX(revision) AS r FROM events").fetchone()
        return int(row["r"] or 0)

    def global_revision(self) -> int:
        with self.connect() as connection:
            return self._current_revision_locked(connection)

    def prune_zero_activity_closed_teams(self) -> tuple[str, ...]:
        with self.connect() as connection:
            return self._prune_zero_activity_closed_teams_locked(connection)

    def _prune_zero_activity_closed_teams_locked(
        self, connection: sqlite3.Connection
    ) -> tuple[str, ...]:
        rows = connection.execute(
            "SELECT * FROM teams WHERE status = 'closed' ORDER BY created_at"
        ).fetchall()
        team_ids = tuple(
            str(row["team_id"])
            for row in rows
            if self._closed_team_has_zero_activity_locked(connection, row)
        )
        if not team_ids:
            return ()
        placeholders = ",".join("?" for _ in team_ids)
        for table in (
            "memberships",
            "team_task_filters",
            "team_agent_history",
            "team_merge_subgroups",
            "team_agent_metrics",
            "team_agent_metric_buckets",
            "renewals",
        ):
            if table == "team_merge_subgroups":
                connection.execute(
                    "DELETE FROM team_merge_subgroups "
                    f"WHERE parent_team_id IN ({placeholders}) "
                    f"OR child_team_id IN ({placeholders})",
                    (*team_ids, *team_ids),
                )
                continue
            connection.execute(
                f"DELETE FROM {table} WHERE team_id IN ({placeholders})", team_ids
            )
        connection.execute(
            f"DELETE FROM events WHERE team_id IN ({placeholders})", team_ids
        )
        connection.execute(
            f"DELETE FROM teams WHERE team_id IN ({placeholders})", team_ids
        )
        self._record_event(
            connection,
            "pruneZeroActivityTeams",
            PRUNE_EVENT_TEAM_ID,
            {"teams": list(team_ids), "count": len(team_ids)},
        )
        return team_ids

    def _closed_team_has_zero_activity_locked(
        self, connection: sqlite3.Connection, row: sqlite3.Row
    ) -> bool:
        team_id = str(row["team_id"])
        if int(row["config_revision"] or 0):
            return False
        if _task_filter_projects_from_json(row["task_filters"]):
            return False
        if _shell_settings_from_json(row["shell_settings"]):
            return False
        if self._team_has_rows_locked(
            connection, "team_task_filters", "team_id = ?", (team_id,)
        ):
            return False
        if self._team_has_rows_locked(
            connection, "renewals", "team_id = ?", (team_id,)
        ):
            return False
        if self._team_has_rows_locked(
            connection, "team_agent_metrics", "team_id = ?", (team_id,)
        ):
            return False
        if self._team_has_rows_locked(
            connection, "team_agent_metric_buckets", "team_id = ?", (team_id,)
        ):
            return False
        if self._team_has_rows_locked(
            connection,
            "team_merge_subgroups",
            "parent_team_id = ? OR child_team_id = ?",
            (team_id, team_id),
        ):
            return False
        events = connection.execute(
            "SELECT DISTINCT kind FROM events WHERE team_id = ?", (team_id,)
        ).fetchall()
        return {str(event["kind"]) for event in events} <= ZERO_ACTIVITY_EVENT_KINDS

    def _team_has_rows_locked(
        self,
        connection: sqlite3.Connection,
        table: str,
        where: str,
        params: tuple[Any, ...],
    ) -> bool:
        row = connection.execute(
            f"SELECT 1 FROM {table} WHERE {where} LIMIT 1", params
        ).fetchone()
        return row is not None

    def _migrate_task_filter_sources_locked(
        self, connection: sqlite3.Connection
    ) -> None:
        rows = connection.execute(
            "SELECT team_id, task_filters FROM teams ORDER BY created_at"
        ).fetchall()
        now = time.time()
        for row in rows:
            team_id = str(row["team_id"])
            existing = connection.execute(
                "SELECT COUNT(*) AS count FROM team_task_filters WHERE team_id = ?",
                (team_id,),
            ).fetchone()
            if existing and int(existing["count"] or 0) > 0:
                continue
            for project in _task_filter_projects_from_json(row["task_filters"]):
                connection.execute(
                    "INSERT OR IGNORE INTO team_task_filters "
                    "(team_id, project, source, created_at, updated_at) "
                    "VALUES (?, ?, ?, ?, ?)",
                    (team_id, project, TASK_FILTER_SOURCE_MANUAL, now, now),
                )
            self._sync_task_filter_projection_locked(connection, team_id)

    def _task_filter_entries_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> tuple[TeamTaskFilter, ...]:
        rows = connection.execute(
            "SELECT project, source FROM team_task_filters "
            "WHERE team_id = ? ORDER BY project, source",
            (team_id,),
        ).fetchall()
        return tuple(
            TeamTaskFilter(project=str(row["project"]), source=str(row["source"]))
            for row in rows
        )

    def _task_filter_projects_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> tuple[str, ...]:
        rows = connection.execute(
            "SELECT DISTINCT project FROM team_task_filters "
            "WHERE team_id = ? ORDER BY project",
            (team_id,),
        ).fetchall()
        return tuple(str(row["project"]) for row in rows)

    def _sync_task_filter_projection_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> tuple[str, ...]:
        projects = self._task_filter_projects_locked(connection, team_id)
        connection.execute(
            "UPDATE teams SET task_filters = ? WHERE team_id = ?",
            (json.dumps(list(projects)), team_id),
        )
        return projects

    def _replace_task_filters_locked(
        self, connection: sqlite3.Connection, team_id: str, projects: Iterable[str]
    ) -> tuple[str, ...]:
        validated = _validated_task_filter_projects(projects)
        if validated:
            placeholders = ",".join("?" for _ in validated)
            connection.execute(
                "DELETE FROM team_task_filters "
                f"WHERE team_id = ? AND project NOT IN ({placeholders})",
                (team_id, *validated),
            )
        else:
            connection.execute(
                "DELETE FROM team_task_filters WHERE team_id = ?", (team_id,)
            )
        now = time.time()
        for project in validated:
            existing = connection.execute(
                "SELECT 1 FROM team_task_filters "
                "WHERE team_id = ? AND project = ? LIMIT 1",
                (team_id, project),
            ).fetchone()
            if existing:
                continue
            connection.execute(
                "INSERT INTO team_task_filters "
                "(team_id, project, source, created_at, updated_at) "
                "VALUES (?, ?, ?, ?, ?)",
                (team_id, project, TASK_FILTER_SOURCE_MANUAL, now, now),
            )
        return self._sync_task_filter_projection_locked(connection, team_id)

    # ---- commands ------------------------------------------------------

    def create_team(
        self,
        *,
        team_id: str | None = None,
        config: TeamConfig | None = None,
        members: Iterable[str] = (),
    ) -> TeamState:
        config = config or TeamConfig()
        with self.connect() as connection:
            return self._create_team_locked(connection, team_id, config, members)

    def _create_team_locked(
        self,
        connection: sqlite3.Connection,
        team_id: str | None,
        config: TeamConfig,
        members: Iterable[str],
    ) -> TeamState:
        resolved_team_id = team_id or f"team-{uuidlib.uuid4().hex[:TEAM_ID_HEX_CHARS]}"
        member_list = list(members)
        connection.execute(
            "INSERT INTO teams (team_id, status, created_at, revision, "
            "config_revision, lifetime, speech_mode, selected_view, "
            "task_filters, shell_settings) VALUES (?, 'open', ?, 0, 0, ?, ?, ?, ?, ?)",
            (
                resolved_team_id,
                time.time(),
                config.lifetime,
                config.speech_mode,
                config.selected_view,
                json.dumps(list(config.task_filters)),
                json.dumps(config.shell_settings),
            ),
        )
        self._replace_task_filters_locked(
            connection, resolved_team_id, config.task_filters
        )
        for agent_id in member_list:
            self._assign_locked(connection, resolved_team_id, agent_id)
        self._record_event(
            connection, "createTeam", resolved_team_id, {"members": member_list}
        )
        return self._team_state_locked(connection, resolved_team_id)

    def close_team(self, team_id: str) -> int:
        with self.connect() as connection:
            self._require_team(connection, team_id)
            connection.execute(
                "UPDATE teams SET status = 'closed' WHERE team_id = ?", (team_id,)
            )
            connection.execute("DELETE FROM memberships WHERE team_id = ?", (team_id,))
            revision = self._record_event(connection, "closeTeam", team_id, {})
            replacement = self._ensure_open_team_locked(connection)
            return replacement.revision if replacement else revision

    def assign_agent(
        self, team_id: str, agent_id: str, aliases: Iterable[str] = ()
    ) -> int:
        with self.connect() as connection:
            self._require_team(connection, team_id)
            self._assign_locked(connection, team_id, agent_id, aliases=aliases)
            return self._record_event(
                connection, "assignAgent", team_id, {"agentId": agent_id}
            )

    def _assign_locked(
        self,
        connection: sqlite3.Connection,
        team_id: str,
        agent_id: str,
        aliases: Iterable[str] = (),
    ) -> None:
        agent_id = _normalized_id(agent_id, "agent_id")
        alias_ids = _agent_alias_ids(agent_id, aliases)
        previous_team_ids: list[str] = []
        for alias_id in alias_ids:
            if alias_id != agent_id:
                self._rewrite_renewal_agent_locked(
                    connection, alias_id, agent_id, team_id
                )
        # A renewal successor (or a placeholder promoted to its real thread)
        # arrives carrying its predecessor's id as an alias that already holds a
        # slot in this same team. The roster is ordered by joined_at, so reusing
        # the predecessor's joined_at keeps the successor in the ancestor's slot
        # instead of appending it to the end; a genuinely new agent has no such
        # slot and falls back to now.
        inherited_joined_at: float | None = None
        for alias_id in alias_ids:
            previous_rows = connection.execute(
                "SELECT team_id, joined_at FROM memberships WHERE agent_id = ?",
                (alias_id,),
            ).fetchall()
            for row in previous_rows:
                if row["team_id"] not in previous_team_ids:
                    previous_team_ids.append(row["team_id"])
                if row["team_id"] == team_id and row["joined_at"] is not None:
                    joined_at = float(row["joined_at"])
                    if inherited_joined_at is None or joined_at < inherited_joined_at:
                        inherited_joined_at = joined_at
            connection.execute(
                "DELETE FROM memberships WHERE agent_id = ?", (alias_id,)
            )
        connection.execute(
            "INSERT INTO memberships (team_id, agent_id, joined_at) VALUES (?, ?, ?)",
            (
                team_id,
                agent_id,
                time.time() if inherited_joined_at is None else inherited_joined_at,
            ),
        )
        self._note_team_agent_history_locked(connection, team_id, agent_id)
        self._close_empty_teams_locked(
            connection,
            [
                previous_team_id
                for previous_team_id in previous_team_ids
                if previous_team_id != team_id
            ],
        )

    def _rewrite_renewal_agent_locked(
        self,
        connection: sqlite3.Connection,
        old_agent_id: str,
        new_agent_id: str,
        team_id: str,
    ) -> None:
        old_row = connection.execute(
            "SELECT agent_id FROM renewals WHERE agent_id = ?", (old_agent_id,)
        ).fetchone()
        if old_row is None:
            return
        new_row = connection.execute(
            "SELECT agent_id FROM renewals WHERE agent_id = ?", (new_agent_id,)
        ).fetchone()
        if new_row is not None:
            connection.execute(
                "DELETE FROM renewals WHERE agent_id = ?", (old_agent_id,)
            )
            return
        connection.execute(
            "UPDATE renewals SET agent_id = ?, team_id = ? WHERE agent_id = ?",
            (new_agent_id, team_id, old_agent_id),
        )

    def _close_empty_teams_locked(
        self, connection: sqlite3.Connection, team_ids: Iterable[str]
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

    def remove_agent(
        self, team_id: str, agent_id: str, aliases: Iterable[str] = ()
    ) -> int:
        alias_ids = _agent_alias_ids(agent_id, aliases)
        with self.connect() as connection:
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
                connection.execute(
                    "DELETE FROM memberships WHERE agent_id = ?", (alias_id,)
                )
                connection.execute(
                    "DELETE FROM renewals WHERE agent_id = ?", (alias_id,)
                )
            self._close_empty_teams_locked(connection, [team_id])
            revision = self._record_event(
                connection, "removeAgent", team_id, {"agentId": agent_id}
            )
            replacement = self._ensure_open_team_locked(connection)
            return replacement.revision if replacement else revision

    def split_team(
        self,
        source_team_id: str,
        *,
        agent_ids: Iterable[str],
        new_team_id: str | None = None,
        config: TeamConfig | None = None,
    ) -> TeamState:
        agent_list = [_normalized_id(agent, "agent_id") for agent in agent_ids]
        if not agent_list:
            raise SpiceError("split requires at least one agent id")
        source_config = self.team_config(source_team_id)
        created = self.create_team(
            team_id=new_team_id, config=config or source_config, members=()
        )
        with self.connect() as connection:
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

    def split_team_back(self, source_team_id: str) -> TeamState:
        with self.connect() as connection:
            self._require_team(connection, source_team_id)
            subgroup = self._latest_restorable_subgroup_locked(
                connection, source_team_id
            )
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
            self._move_team_metric_rows_for_agents_locked(
                connection,
                source_team_id,
                child_team_id,
                agent_ids,
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

    def merge_teams(self, source_team_id: str, destination_team_id: str) -> int:
        if source_team_id == destination_team_id:
            raise SpiceError("merge requires two distinct teams")
        with self.connect() as connection:
            self._require_team(connection, source_team_id)
            self._require_team(connection, destination_team_id)
            rows = connection.execute(
                "SELECT agent_id FROM memberships WHERE team_id = ? ORDER BY joined_at",
                (source_team_id,),
            ).fetchall()
            agent_ids = [str(row["agent_id"]) for row in rows]
            self._move_team_metric_rows_locked(
                connection, source_team_id, destination_team_id
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

    def reorder_team_agents(self, team_id: str, agent_ids: Iterable[str]) -> int:
        ordered_agent_ids = [_normalized_id(agent, "agent_id") for agent in agent_ids]
        if len(set(ordered_agent_ids)) != len(ordered_agent_ids):
            raise SpiceError("reorder requires unique agent ids")
        with self.connect() as connection:
            self._require_team(connection, team_id)
            rows = connection.execute(
                "SELECT agent_id FROM memberships WHERE team_id = ? ORDER BY joined_at",
                (team_id,),
            ).fetchall()
            current_agent_ids = [str(row["agent_id"]) for row in rows]
            if set(ordered_agent_ids) != set(current_agent_ids):
                raise SpiceError("reorder requires exactly the current team members")
            now = time.time()
            for index, agent_id in enumerate(ordered_agent_ids):
                connection.execute(
                    "UPDATE memberships SET joined_at = ? "
                    "WHERE team_id = ? AND agent_id = ?",
                    (now + index * 0.000001, team_id, agent_id),
                )
            return self._record_event(
                connection,
                "reorderTeamAgents",
                team_id,
                {"agentIds": ordered_agent_ids},
            )

    def update_team_config(
        self,
        team_id: str,
        config: TeamConfig,
        *,
        replace_task_filters: bool = False,
    ) -> int:
        with self.connect() as connection:
            self._require_team(connection, team_id)
            connection.execute(
                "UPDATE teams SET lifetime = ?, speech_mode = ?, selected_view = ?, "
                "shell_settings = ?, "
                "config_revision = config_revision + 1 WHERE team_id = ?",
                (
                    config.lifetime,
                    config.speech_mode,
                    config.selected_view,
                    json.dumps(config.shell_settings),
                    team_id,
                ),
            )
            if replace_task_filters:
                self._replace_task_filters_locked(
                    connection, team_id, config.task_filters
                )
            task_filters = self._task_filter_projects_locked(connection, team_id)
            return self._record_event(
                connection,
                "updateTeamConfig",
                team_id,
                {"lifetime": config.lifetime, "taskFilters": list(task_filters)},
            )

    def add_task_filter(
        self,
        team_id: str,
        project: str,
        *,
        source: str = TASK_FILTER_SOURCE_MANUAL,
    ) -> int:
        project = _validated_task_filter_project(project)
        source = _validated_task_filter_source(source)
        with self.connect() as connection:
            self._require_team(connection, team_id)
            now = time.time()
            cursor = connection.execute(
                "INSERT OR IGNORE INTO team_task_filters "
                "(team_id, project, source, created_at, updated_at) "
                "VALUES (?, ?, ?, ?, ?)",
                (team_id, project, source, now, now),
            )
            if cursor.rowcount == 0:
                return self._current_revision_locked(connection)
            self._sync_task_filter_projection_locked(connection, team_id)
            connection.execute(
                "UPDATE teams SET config_revision = config_revision + 1 "
                "WHERE team_id = ?",
                (team_id,),
            )
            return self._record_event(
                connection,
                "addTaskFilter",
                team_id,
                {
                    "project": project,
                    "source": source,
                    "taskFilters": list(
                        self._task_filter_projects_locked(connection, team_id)
                    ),
                },
            )

    def remove_task_filter(
        self,
        team_id: str,
        project: str,
        *,
        source: str | None = None,
    ) -> int:
        project = _validated_task_filter_project(project)
        if source is not None:
            source = _validated_task_filter_source(source)
        with self.connect() as connection:
            self._require_team(connection, team_id)
            if source is None:
                cursor = connection.execute(
                    "DELETE FROM team_task_filters WHERE team_id = ? AND project = ?",
                    (team_id, project),
                )
            else:
                cursor = connection.execute(
                    "DELETE FROM team_task_filters "
                    "WHERE team_id = ? AND project = ? AND source = ?",
                    (team_id, project, source),
                )
            if cursor.rowcount == 0:
                return self._current_revision_locked(connection)
            self._sync_task_filter_projection_locked(connection, team_id)
            connection.execute(
                "UPDATE teams SET config_revision = config_revision + 1 "
                "WHERE team_id = ?",
                (team_id,),
            )
            return self._record_event(
                connection,
                "removeTaskFilter",
                team_id,
                {
                    "project": project,
                    "source": source or "",
                    "taskFilters": list(
                        self._task_filter_projects_locked(connection, team_id)
                    ),
                },
            )

    # ---- renewal -------------------------------------------------------

    def set_agent_renewal_request(
        self, agent_id: str, *, requested: bool
    ) -> TeamRenewalState | None:
        agent_id = _normalized_id(agent_id, "agent_id")
        team_id = self.open_team_for_agent(agent_id)
        with self.connect() as connection:
            current = self._renewal_state_locked(connection, agent_id)
            if requested:
                if current is not None:
                    return current
                revision = self._record_event(
                    connection,
                    "renewalRequested",
                    team_id,
                    {"agentId": agent_id},
                )
                connection.execute(
                    "INSERT OR REPLACE INTO renewals (agent_id, team_id, state, "
                    "ancestor_thread_id, successor_agent_id, revision) "
                    "VALUES (?, ?, ?, '', '', ?)",
                    (agent_id, team_id, RENEWAL_STATE_REQUESTED, revision),
                )
                return TeamRenewalState(
                    agent_id=agent_id,
                    team_id=team_id,
                    state=RENEWAL_STATE_REQUESTED,
                    ancestor_thread_id="",
                    successor_agent_id="",
                    revision=revision,
                )
            if current is None or current.state != RENEWAL_STATE_REQUESTED:
                return current
            self._record_event(
                connection,
                "renewalRequestCleared",
                current.team_id,
                {"agentId": agent_id},
            )
            connection.execute(
                "DELETE FROM renewals WHERE agent_id = ? AND state = ?",
                (agent_id, RENEWAL_STATE_REQUESTED),
            )
            return None

    def agent_renewal_requested(self, agent_id: str) -> bool:
        renewal = self.renewal_state_for_agent(agent_id)
        return bool(renewal and renewal.requested)

    def agent_renewal_active(self, agent_id: str) -> bool:
        renewal = self.renewal_state_for_agent(agent_id)
        return bool(
            renewal
            and renewal.state in {RENEWAL_STATE_REQUESTED, RENEWAL_STATE_PENDING}
        )

    def renewal_state_for_agent(self, agent_id: str) -> TeamRenewalState | None:
        agent_id = _normalized_id(agent_id, "agent_id")
        with self.connect() as connection:
            return self._renewal_state_locked(connection, agent_id)

    def record_pending_renewal(
        self, *, agent_id: str, ancestor_thread_id: str
    ) -> TeamRenewalState:
        agent_id = _normalized_id(agent_id, "agent_id")
        team_id = self.open_team_for_agent(agent_id)
        with self.connect() as connection:
            revision = self._record_event(
                connection,
                "renewalPending",
                team_id,
                {"agentId": agent_id, "ancestor": ancestor_thread_id},
            )
            connection.execute(
                "INSERT OR REPLACE INTO renewals (agent_id, team_id, state, "
                "ancestor_thread_id, successor_agent_id, revision) "
                "VALUES (?, ?, ?, ?, '', ?)",
                (
                    agent_id,
                    team_id,
                    RENEWAL_STATE_PENDING,
                    ancestor_thread_id,
                    revision,
                ),
            )
        return TeamRenewalState(
            agent_id=agent_id,
            team_id=team_id,
            state=RENEWAL_STATE_PENDING,
            ancestor_thread_id=ancestor_thread_id,
            successor_agent_id="",
            revision=revision,
        )

    def record_started_renewal(
        self,
        *,
        predecessor_agent_id: str,
        successor_agent_id: str,
        ancestor_thread_id: str = "",
    ) -> TeamRenewalState:
        predecessor_agent_id = _normalized_id(
            predecessor_agent_id, "predecessor_agent_id"
        )
        successor_agent_id = _normalized_id(successor_agent_id, "successor_agent_id")
        team_id = self.open_team_for_agent(predecessor_agent_id)
        with self.connect() as connection:
            predecessor_row = connection.execute(
                "SELECT joined_at FROM memberships WHERE team_id = ? AND agent_id = ?",
                (team_id, predecessor_agent_id),
            ).fetchone()
            revision = self._record_event(
                connection,
                "renewalStarted",
                team_id,
                {
                    "predecessor": predecessor_agent_id,
                    "successor": successor_agent_id,
                },
            )
            self._assign_locked(connection, team_id, successor_agent_id)
            connection.execute(
                "DELETE FROM memberships WHERE agent_id = ?", (predecessor_agent_id,)
            )
            if predecessor_row is not None:
                # Keep the successor in the predecessor's roster slot.
                connection.execute(
                    "UPDATE memberships SET joined_at = ? "
                    "WHERE team_id = ? AND agent_id = ?",
                    (
                        float(predecessor_row["joined_at"]),
                        team_id,
                        successor_agent_id,
                    ),
                )
            connection.execute(
                "INSERT OR REPLACE INTO renewals (agent_id, team_id, state, "
                "ancestor_thread_id, successor_agent_id, revision) "
                "VALUES (?, ?, ?, ?, ?, ?)",
                (
                    predecessor_agent_id,
                    team_id,
                    RENEWAL_STATE_STARTED,
                    ancestor_thread_id,
                    successor_agent_id,
                    revision,
                ),
            )
        return TeamRenewalState(
            agent_id=predecessor_agent_id,
            team_id=team_id,
            state=RENEWAL_STATE_STARTED,
            ancestor_thread_id=ancestor_thread_id,
            successor_agent_id=successor_agent_id,
            revision=revision,
        )

    # ---- reads ---------------------------------------------------------

    def current_team_for_agent(self, agent_id: str) -> str | None:
        with self.connect() as connection:
            row = connection.execute(
                "SELECT team_id FROM memberships WHERE agent_id = ?", (agent_id,)
            ).fetchone()
            return row["team_id"] if row else None

    def open_team_for_agent(self, agent_id: str) -> str:
        team_id = self.current_team_for_agent(agent_id)
        if team_id is None:
            raise SpiceError(f"agent {agent_id} is not assigned to any team")
        return team_id

    def team_config(self, team_id: str) -> TeamConfig:
        with self.connect() as connection:
            row = self._require_team(connection, team_id)
            entries = self._task_filter_entries_locked(connection, team_id)
            return _config_from_row(row, entries)

    def open_team_ids_with_task_filter(
        self, project: str, *, source: str | None = None
    ) -> tuple[str, ...]:
        project = _validated_task_filter_project(project)
        if source is not None:
            source = _validated_task_filter_source(source)
        source_clause = "" if source is None else " AND team_task_filters.source = ?"
        params: tuple[str, ...] = (project,) if source is None else (project, source)
        with self.connect() as connection:
            rows = connection.execute(
                "SELECT DISTINCT teams.team_id FROM teams "
                "JOIN team_task_filters ON team_task_filters.team_id = teams.team_id "
                "WHERE teams.status = 'open' AND team_task_filters.project = ?"
                f"{source_clause} "
                "ORDER BY teams.created_at",
                params,
            ).fetchall()
        return tuple(str(row["team_id"]) for row in rows)

    def open_task_filter_projects(
        self, *, source: str | None = None
    ) -> tuple[str, ...]:
        if source is not None:
            source = _validated_task_filter_source(source)
        source_clause = "" if source is None else " AND team_task_filters.source = ?"
        params: tuple[str, ...] = () if source is None else (source,)
        with self.connect() as connection:
            rows = connection.execute(
                "SELECT DISTINCT team_task_filters.project FROM team_task_filters "
                "JOIN teams ON teams.team_id = team_task_filters.team_id "
                f"WHERE teams.status = 'open'{source_clause} "
                "ORDER BY team_task_filters.project",
                params,
            ).fetchall()
        return tuple(str(row["project"]) for row in rows)

    def team_state(self, team_id: str) -> TeamState:
        with self.connect() as connection:
            return self._team_state_locked(connection, team_id)

    def team_snapshot(self, *, since_revision: int | None = None) -> TeamSnapshot:
        with self.connect() as connection:
            self._prune_zero_activity_closed_teams_locked(connection)
            self._ensure_open_team_locked(connection)
            revision_row = connection.execute(
                "SELECT MAX(revision) AS r FROM events"
            ).fetchone()
            global_revision = int(revision_row["r"] or 0)
            rows = connection.execute(
                "SELECT * FROM teams WHERE status = 'open' ORDER BY created_at"
            ).fetchall()
            teams = tuple(
                self._team_state_locked(connection, row["team_id"]) for row in rows
            )
        return TeamSnapshot(global_revision=global_revision, teams=teams)

    def _ensure_open_team_locked(
        self, connection: sqlite3.Connection
    ) -> TeamState | None:
        row = connection.execute(
            "SELECT team_id FROM teams WHERE status = 'open' "
            "ORDER BY created_at LIMIT 1"
        ).fetchone()
        if row is not None:
            return None
        return self._create_team_locked(connection, None, TeamConfig(), ())

    def _team_state_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> TeamState:
        row = self._require_team(connection, team_id)
        member_rows = connection.execute(
            "SELECT agent_id FROM memberships WHERE team_id = ? ORDER BY joined_at",
            (team_id,),
        ).fetchall()
        renewal_by_agent: dict[str, TeamRenewalState] = {}
        if member_rows:
            placeholders = ",".join("?" for _ in member_rows)
            renewal_rows = connection.execute(
                "SELECT agent_id, team_id, state, ancestor_thread_id, "
                f"successor_agent_id, revision FROM renewals "
                f"WHERE agent_id IN ({placeholders})",
                tuple(str(member["agent_id"]) for member in member_rows),
            ).fetchall()
            renewal_by_agent = {
                str(renewal["agent_id"]): _renewal_state_from_row(renewal)
                for renewal in renewal_rows
            }
        split_back_subgroup = self._latest_restorable_subgroup_locked(
            connection, team_id
        )
        split_back_member_count = (
            len(split_back_subgroup[1]) if split_back_subgroup is not None else 0
        )
        return TeamState(
            team_id=team_id,
            status=str(row["status"]),
            revision=int(row["revision"]),
            config_revision=int(row["config_revision"]),
            config=_config_from_row(
                row, self._task_filter_entries_locked(connection, team_id)
            ),
            members=tuple(
                TeamMember(
                    agent_id=member["agent_id"],
                    renewal=renewal_by_agent.get(str(member["agent_id"])),
                )
                for member in member_rows
            ),
            split_back_available=split_back_subgroup is not None,
            split_back_member_count=split_back_member_count,
        )

    def _renewal_state_locked(
        self, connection: sqlite3.Connection, agent_id: str
    ) -> TeamRenewalState | None:
        row = connection.execute(
            "SELECT agent_id, team_id, state, ancestor_thread_id, "
            "successor_agent_id, revision FROM renewals WHERE agent_id = ?",
            (agent_id,),
        ).fetchone()
        return _renewal_state_from_row(row) if row is not None else None

    def _require_team(
        self, connection: sqlite3.Connection, team_id: str
    ) -> sqlite3.Row:
        row = connection.execute(
            "SELECT * FROM teams WHERE team_id = ?", (team_id,)
        ).fetchone()
        if row is None:
            raise SpiceError(f"unknown team: {team_id}")
        return row

    def _note_team_agent_history_locked(
        self, connection: sqlite3.Connection, team_id: str, agent_id: str
    ) -> None:
        now = time.time()
        connection.execute(
            "INSERT INTO team_agent_history "
            "(team_id, agent_id, first_seen_at, last_seen_at) VALUES (?, ?, ?, ?) "
            "ON CONFLICT(team_id, agent_id) DO UPDATE SET "
            "last_seen_at = excluded.last_seen_at",
            (team_id, agent_id, now, now),
        )

    def _record_merge_subgroup_locked(
        self,
        connection: sqlite3.Connection,
        *,
        parent_team_id: str,
        child_team_id: str,
        merged_revision: int,
        agent_ids: Iterable[str],
    ) -> None:
        agent_list = list(dict.fromkeys(str(agent_id) for agent_id in agent_ids))
        if not agent_list:
            return
        connection.execute(
            "INSERT OR REPLACE INTO team_merge_subgroups "
            "(parent_team_id, child_team_id, merged_revision, agent_ids, "
            "created_at, restored_revision) VALUES (?, ?, ?, ?, ?, NULL)",
            (
                parent_team_id,
                child_team_id,
                int(merged_revision),
                json.dumps(agent_list, separators=(",", ":")),
                time.time(),
            ),
        )

    def _latest_restorable_subgroup_locked(
        self, connection: sqlite3.Connection, parent_team_id: str
    ) -> tuple[sqlite3.Row, tuple[str, ...]] | None:
        current_agent_ids = self._current_membership_agent_ids_locked(
            connection, parent_team_id
        )
        if not current_agent_ids:
            return None
        rows = connection.execute(
            "SELECT parent_team_id, child_team_id, merged_revision, agent_ids "
            "FROM team_merge_subgroups "
            "WHERE parent_team_id = ? AND restored_revision IS NULL "
            "ORDER BY merged_revision DESC LIMIT 1",
            (parent_team_id,),
        ).fetchall()
        if not rows:
            return None
        row = rows[0]
        agent_ids = _team_subgroup_agent_ids(row["agent_ids"])
        if agent_ids and set(agent_ids).issubset(current_agent_ids):
            return row, agent_ids
        return None

    def _current_membership_agent_ids_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> set[str]:
        rows = connection.execute(
            "SELECT agent_id FROM memberships WHERE team_id = ?",
            (team_id,),
        ).fetchall()
        return {str(row["agent_id"]) for row in rows}


def _config_from_row(
    row: sqlite3.Row, entries: Iterable[TeamTaskFilter] = ()
) -> TeamConfig:
    task_filter_entries = tuple(entries)
    task_filters = tuple(dict.fromkeys(entry.project for entry in task_filter_entries))
    if not task_filters:
        task_filters = _task_filter_projects_from_json(row["task_filters"])
    try:
        shell_settings = json.loads(row["shell_settings"])
    except (json.JSONDecodeError, TypeError):
        shell_settings = {}
    return TeamConfig(
        lifetime=str(row["lifetime"]),
        speech_mode=str(row["speech_mode"]),
        task_filters=task_filters,
        task_filter_entries=task_filter_entries,
        selected_view=str(row["selected_view"]),
        shell_settings=shell_settings if isinstance(shell_settings, dict) else {},
    )


def _task_filter_projects_from_json(raw: object) -> tuple[str, ...]:
    try:
        values = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return ()
    if not isinstance(values, list):
        return ()
    return _validated_task_filter_projects(str(item) for item in values)


def _shell_settings_from_json(raw: object) -> dict[str, Any]:
    try:
        values = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return {}
    return values if isinstance(values, dict) else {}


def _team_subgroup_agent_ids(raw: object) -> tuple[str, ...]:
    try:
        values = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return ()
    if not isinstance(values, list):
        return ()
    agent_ids = [str(item) for item in values if str(item or "").strip()]
    return tuple(dict.fromkeys(agent_ids))


def _validated_task_filter_projects(projects: Iterable[str]) -> tuple[str, ...]:
    seen: dict[str, None] = {}
    for project in projects:
        value = str(project or "").strip()
        if not value:
            continue
        seen.setdefault(_validated_task_filter_project(value), None)
    return tuple(sorted(seen))


def _validated_task_filter_project(project: str) -> str:
    from spice.tasks import config as task_config

    return task_config.validate_assignable_project(str(project or "").strip())


def _validated_task_filter_source(source: str) -> str:
    value = str(source or "").strip()
    if value not in TASK_FILTER_SOURCES:
        raise SpiceError(
            "task filter source must be one of "
            + ", ".join(sorted(TASK_FILTER_SOURCES))
        )
    return value


def _renewal_state_from_row(row: sqlite3.Row) -> TeamRenewalState:
    return TeamRenewalState(
        agent_id=str(row["agent_id"]),
        team_id=str(row["team_id"]),
        state=str(row["state"]),
        ancestor_thread_id=str(row["ancestor_thread_id"]),
        successor_agent_id=str(row["successor_agent_id"]),
        revision=int(row["revision"]),
    )


from spice.serve.teamcommands import (  # noqa: E402
    TeamCommandResult as TeamCommandResult,
    TeamCommandService as TeamCommandService,
)
