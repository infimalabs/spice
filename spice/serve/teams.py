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
from collections import Counter
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable, Iterator

from spice.errors import SpiceError

TEAM_DATABASE_FILENAME = "serve-teams.sqlite3"
DEFAULT_LIFETIME = "Drive"
DEFAULT_SPEECH_MODE = "speak"
DEFAULT_SELECTED_VIEW = "compose"
TEAM_ID_HEX_CHARS = 12
RENEWAL_STATE_REQUESTED = "requested"
RENEWAL_STATE_PENDING = "pending"
RENEWAL_STATE_STARTED = "started"
TASK_FILTER_SOURCE_MANUAL = "manual"
TASK_FILTER_SOURCE_AUTO_CREATE = "auto:create"
TASK_FILTER_SOURCE_AUTO_CLAIM = "auto:claim"
TASK_FILTER_SOURCES = frozenset(
    {
        TASK_FILTER_SOURCE_MANUAL,
        TASK_FILTER_SOURCE_AUTO_CREATE,
        TASK_FILTER_SOURCE_AUTO_CLAIM,
    }
)
TEAM_SQLITE_BUSY_TIMEOUT_MS = 5000

_SCHEMA = """
CREATE TABLE IF NOT EXISTS events (
    revision INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL,
    kind TEXT NOT NULL,
    team_id TEXT NOT NULL,
    payload TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS teams (
    team_id TEXT PRIMARY KEY,
    status TEXT NOT NULL DEFAULT 'open',
    created_at REAL NOT NULL,
    revision INTEGER NOT NULL,
    config_revision INTEGER NOT NULL DEFAULT 0,
    lifetime TEXT NOT NULL,
    speech_mode TEXT NOT NULL,
    selected_view TEXT NOT NULL,
    task_filters TEXT NOT NULL DEFAULT '[]',
    shell_settings TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE IF NOT EXISTS memberships (
    team_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    joined_at REAL NOT NULL,
    PRIMARY KEY (team_id, agent_id)
);
CREATE TABLE IF NOT EXISTS team_task_filters (
    team_id TEXT NOT NULL,
    project TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    PRIMARY KEY (team_id, project, source)
);
CREATE TABLE IF NOT EXISTS team_agent_history (
    team_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    first_seen_at REAL NOT NULL,
    last_seen_at REAL NOT NULL,
    PRIMARY KEY (team_id, agent_id)
);
CREATE TABLE IF NOT EXISTS renewals (
    agent_id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL,
    state TEXT NOT NULL,
    ancestor_thread_id TEXT NOT NULL,
    successor_agent_id TEXT NOT NULL DEFAULT '',
    revision INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_metrics (
    agent_id TEXT PRIMARY KEY,
    acked INTEGER NOT NULL DEFAULT 0,
    sends INTEGER NOT NULL DEFAULT 0,
    tool_calls INTEGER NOT NULL DEFAULT 0,
    updated_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_metric_buckets (
    agent_id TEXT NOT NULL,
    bucket_start INTEGER NOT NULL,
    messages INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (agent_id, bucket_start)
);
CREATE TABLE IF NOT EXISTS team_agent_metrics (
    team_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    acked INTEGER NOT NULL DEFAULT 0,
    sends INTEGER NOT NULL DEFAULT 0,
    tool_calls INTEGER NOT NULL DEFAULT 0,
    updated_at REAL NOT NULL,
    PRIMARY KEY (team_id, agent_id)
);
CREATE TABLE IF NOT EXISTS team_agent_metric_buckets (
    team_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    bucket_start INTEGER NOT NULL,
    messages INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (team_id, agent_id, bucket_start)
);
CREATE TABLE IF NOT EXISTS agent_metric_cursors (
    agent_id TEXT PRIMARY KEY,
    source_path TEXT NOT NULL,
    offset INTEGER NOT NULL,
    updated_at REAL NOT NULL
);
"""

METRIC_BUCKET_SECONDS = 60


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

    def to_payload(self) -> dict[str, Any]:
        return {
            "teamId": self.team_id,
            "status": self.status,
            "revision": self.revision,
            "config": self.config.to_payload(self.config_revision),
            "members": [member.to_payload() for member in self.members],
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


@dataclass(frozen=True)
class LaneMetricSummary:
    agent_ids: tuple[str, ...]
    acked: int
    sends: int
    tool_calls: int
    sparkline: tuple[int, ...]


def team_database_path() -> Path:
    from spice.tasks import config as task_config

    return task_config.backend_root() / TEAM_DATABASE_FILENAME


class ServeTeamStore:
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
            connection.executescript(_SCHEMA)
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
        connection.execute(
            "DELETE FROM team_task_filters WHERE team_id = ?", (team_id,)
        )
        now = time.time()
        for project in _validated_task_filter_projects(projects):
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
            previous_rows = connection.execute(
                "SELECT DISTINCT team_id FROM memberships WHERE agent_id = ?",
                (alias_id,),
            ).fetchall()
            previous_team_ids.extend(
                row["team_id"]
                for row in previous_rows
                if row["team_id"] not in previous_team_ids
            )
            connection.execute(
                "DELETE FROM memberships WHERE agent_id = ?", (alias_id,)
            )
        connection.execute(
            "INSERT INTO memberships (team_id, agent_id, joined_at) VALUES (?, ?, ?)",
            (team_id, agent_id, time.time()),
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
            for row in rows:
                self._assign_locked(connection, destination_team_id, row["agent_id"])
            connection.execute(
                "UPDATE teams SET status = 'closed' WHERE team_id = ?",
                (source_team_id,),
            )
            return self._record_event(
                connection,
                "mergeTeams",
                destination_team_id,
                {"sourceTeamId": source_team_id},
            )

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

    # ---- lane metrics --------------------------------------------------

    def record_agent_metric_delta(
        self,
        agent_id: str,
        *,
        acked: int = 0,
        sends: int = 0,
        tool_calls: int = 0,
        message_timestamps: Iterable[float] = (),
    ) -> None:
        agent_id = _normalized_id(agent_id, "agent_id")
        acked = _nonnegative_int(acked)
        sends = _nonnegative_int(sends)
        tool_calls = _nonnegative_int(tool_calls)
        buckets = Counter(
            _metric_bucket_start(timestamp) for timestamp in message_timestamps
        )
        if acked == sends == tool_calls == 0 and not buckets:
            return
        now = time.time()
        with self.connect() as connection:
            self._record_agent_metric_delta_locked(
                connection,
                agent_id,
                acked=acked,
                sends=sends,
                tool_calls=tool_calls,
                buckets=buckets,
                now=now,
            )
            row = connection.execute(
                "SELECT team_id FROM memberships WHERE agent_id = ?", (agent_id,)
            ).fetchone()
            if row is not None:
                self._record_team_agent_metric_delta_locked(
                    connection,
                    str(row["team_id"]),
                    agent_id,
                    acked=acked,
                    sends=sends,
                    tool_calls=tool_calls,
                    buckets=buckets,
                    now=now,
                )

    def agent_metric_cursor(self, agent_id: str, source_path: str) -> int:
        agent_id = _normalized_id(agent_id, "agent_id")
        with self.connect() as connection:
            row = connection.execute(
                "SELECT source_path, offset FROM agent_metric_cursors "
                "WHERE agent_id = ?",
                (agent_id,),
            ).fetchone()
        if row is None or str(row["source_path"]) != source_path:
            return 0
        return max(0, int(row["offset"] or 0))

    def record_agent_metric_cursor(
        self, agent_id: str, *, source_path: str, offset: int
    ) -> None:
        agent_id = _normalized_id(agent_id, "agent_id")
        with self.connect() as connection:
            connection.execute(
                "INSERT INTO agent_metric_cursors "
                "(agent_id, source_path, offset, updated_at) VALUES (?, ?, ?, ?) "
                "ON CONFLICT(agent_id) DO UPDATE SET "
                "source_path = excluded.source_path, "
                "offset = excluded.offset, "
                "updated_at = excluded.updated_at",
                (agent_id, source_path, max(0, int(offset)), time.time()),
            )

    def lane_metric_summary(
        self,
        agent_id: str,
        *,
        bucket_count: int,
        bucket_seconds: int = METRIC_BUCKET_SECONDS,
    ) -> LaneMetricSummary:
        if not str(agent_id or "").strip():
            return LaneMetricSummary(
                agent_ids=(),
                acked=0,
                sends=0,
                tool_calls=0,
                sparkline=tuple(0 for _ in range(max(0, bucket_count))),
            )
        agent_id = _normalized_id(agent_id, "agent_id")
        bucket_count = max(1, int(bucket_count))
        bucket_seconds = max(1, int(bucket_seconds))
        with self.connect() as connection:
            row = connection.execute(
                "SELECT team_id FROM memberships WHERE agent_id = ?", (agent_id,)
            ).fetchone()
            if row is not None:
                return self._team_lane_metric_summary_locked(
                    connection,
                    str(row["team_id"]),
                    bucket_count=bucket_count,
                    bucket_seconds=bucket_seconds,
                )
            return self._agent_lane_metric_summary_locked(
                connection,
                (agent_id,),
                bucket_count=bucket_count,
                bucket_seconds=bucket_seconds,
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

    def _record_agent_metric_delta_locked(
        self,
        connection: sqlite3.Connection,
        agent_id: str,
        *,
        acked: int,
        sends: int,
        tool_calls: int,
        buckets: Counter[int],
        now: float,
    ) -> None:
        connection.execute(
            "INSERT INTO agent_metrics "
            "(agent_id, acked, sends, tool_calls, updated_at) "
            "VALUES (?, ?, ?, ?, ?) "
            "ON CONFLICT(agent_id) DO UPDATE SET "
            "acked = agent_metrics.acked + excluded.acked, "
            "sends = agent_metrics.sends + excluded.sends, "
            "tool_calls = agent_metrics.tool_calls + excluded.tool_calls, "
            "updated_at = excluded.updated_at",
            (agent_id, acked, sends, tool_calls, now),
        )
        for bucket_start, count in buckets.items():
            connection.execute(
                "INSERT INTO agent_metric_buckets "
                "(agent_id, bucket_start, messages) VALUES (?, ?, ?) "
                "ON CONFLICT(agent_id, bucket_start) DO UPDATE SET "
                "messages = agent_metric_buckets.messages + excluded.messages",
                (agent_id, bucket_start, int(count)),
            )

    def _record_team_agent_metric_delta_locked(
        self,
        connection: sqlite3.Connection,
        team_id: str,
        agent_id: str,
        *,
        acked: int,
        sends: int,
        tool_calls: int,
        buckets: Counter[int],
        now: float,
    ) -> None:
        connection.execute(
            "INSERT INTO team_agent_metrics "
            "(team_id, agent_id, acked, sends, tool_calls, updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?) "
            "ON CONFLICT(team_id, agent_id) DO UPDATE SET "
            "acked = team_agent_metrics.acked + excluded.acked, "
            "sends = team_agent_metrics.sends + excluded.sends, "
            "tool_calls = team_agent_metrics.tool_calls + excluded.tool_calls, "
            "updated_at = excluded.updated_at",
            (team_id, agent_id, acked, sends, tool_calls, now),
        )
        for bucket_start, count in buckets.items():
            connection.execute(
                "INSERT INTO team_agent_metric_buckets "
                "(team_id, agent_id, bucket_start, messages) "
                "VALUES (?, ?, ?, ?) "
                "ON CONFLICT(team_id, agent_id, bucket_start) DO UPDATE SET "
                "messages = team_agent_metric_buckets.messages + excluded.messages",
                (team_id, agent_id, bucket_start, int(count)),
            )

    def _team_lane_metric_summary_locked(
        self,
        connection: sqlite3.Connection,
        team_id: str,
        *,
        bucket_count: int,
        bucket_seconds: int,
    ) -> LaneMetricSummary:
        agent_rows = connection.execute(
            "SELECT agent_id FROM team_agent_metrics WHERE team_id = ? "
            "UNION SELECT agent_id FROM memberships WHERE team_id = ? "
            "ORDER BY agent_id",
            (team_id, team_id),
        ).fetchall()
        agent_ids = tuple(str(row["agent_id"]) for row in agent_rows)
        totals = connection.execute(
            "SELECT COALESCE(SUM(acked), 0) AS acked, "
            "COALESCE(SUM(sends), 0) AS sends, "
            "COALESCE(SUM(tool_calls), 0) AS tool_calls "
            "FROM team_agent_metrics WHERE team_id = ?",
            (team_id,),
        ).fetchone()
        bucket_rows = connection.execute(
            "SELECT bucket_start, SUM(messages) AS messages "
            "FROM team_agent_metric_buckets WHERE team_id = ? "
            "GROUP BY bucket_start ORDER BY bucket_start",
            (team_id,),
        ).fetchall()
        return _lane_metric_summary_from_rows(
            agent_ids,
            totals,
            bucket_rows,
            bucket_count=bucket_count,
            bucket_seconds=bucket_seconds,
        )

    def _agent_lane_metric_summary_locked(
        self,
        connection: sqlite3.Connection,
        agent_ids: tuple[str, ...],
        *,
        bucket_count: int,
        bucket_seconds: int,
    ) -> LaneMetricSummary:
        placeholders = ",".join("?" for _ in agent_ids)
        totals = connection.execute(
            "SELECT COALESCE(SUM(acked), 0) AS acked, "
            "COALESCE(SUM(sends), 0) AS sends, "
            "COALESCE(SUM(tool_calls), 0) AS tool_calls "
            f"FROM agent_metrics WHERE agent_id IN ({placeholders})",
            agent_ids,
        ).fetchone()
        bucket_rows = connection.execute(
            "SELECT bucket_start, SUM(messages) AS messages "
            "FROM agent_metric_buckets "
            f"WHERE agent_id IN ({placeholders}) "
            "GROUP BY bucket_start ORDER BY bucket_start",
            agent_ids,
        ).fetchall()
        return _lane_metric_summary_from_rows(
            agent_ids,
            totals,
            bucket_rows,
            bucket_count=bucket_count,
            bucket_seconds=bucket_seconds,
        )


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


def _normalized_id(value: str, field_name: str) -> str:
    normalized = str(value or "").strip()
    if not normalized:
        raise SpiceError(f"{field_name} must be non-empty")
    return normalized


def _nonnegative_int(value: int) -> int:
    return max(0, int(value or 0))


def _metric_bucket_start(timestamp: float) -> int:
    raw = max(0, int(float(timestamp)))
    return raw - (raw % METRIC_BUCKET_SECONDS)


def _metric_sparkline(
    rows: Iterable[tuple[int, int]],
    *,
    bucket_count: int,
    bucket_seconds: int,
) -> tuple[int, ...]:
    values = [0] * bucket_count
    bucket_rows = [(bucket, count) for bucket, count in rows if count > 0]
    if not bucket_rows:
        return tuple(values)
    latest = max(bucket for bucket, _count in bucket_rows)
    start = latest - ((bucket_count - 1) * bucket_seconds)
    for bucket, count in bucket_rows:
        index = (bucket - start) // bucket_seconds
        values[max(0, min(index, bucket_count - 1))] += count
    return tuple(values)


def _lane_metric_summary_from_rows(
    agent_ids: tuple[str, ...],
    totals: sqlite3.Row | None,
    bucket_rows: Iterable[sqlite3.Row],
    *,
    bucket_count: int,
    bucket_seconds: int,
) -> LaneMetricSummary:
    return LaneMetricSummary(
        agent_ids=agent_ids,
        acked=int(totals["acked"] or 0) if totals else 0,
        sends=int(totals["sends"] or 0) if totals else 0,
        tool_calls=int(totals["tool_calls"] or 0) if totals else 0,
        sparkline=_metric_sparkline(
            (
                (int(row["bucket_start"]), int(row["messages"] or 0))
                for row in bucket_rows
            ),
            bucket_count=bucket_count,
            bucket_seconds=bucket_seconds,
        ),
    )


@dataclass(frozen=True)
class TeamCommandResult:
    revision: int
    snapshot: TeamSnapshot


class TeamCommandService:
    """Validate and apply UI team commands with optimistic concurrency."""

    def __init__(self, store: ServeTeamStore | None = None) -> None:
        self.store = store or ServeTeamStore()

    def apply(self, payload: dict[str, Any]) -> TeamCommandResult:
        command = str(payload.get("command") or "")
        expected = payload.get("expectedRevision")
        if (
            isinstance(expected, int)
            and expected
            and expected < self.store.global_revision()
        ):
            # Stale client: commands apply against current state anyway when
            # they remain valid; an invalid one raises with detail below.
            pass
        if command == "createTeam":
            config = _config_from_payload(payload.get("config"))
            members = [str(item) for item in payload.get("members") or [] if item]
            self.store.create_team(config=config, members=members)
        elif command == "closeTeam":
            self.store.close_team(_required(payload, "teamId"))
        elif command in ("moveAgentToTeam", "moveComposerToTeam"):
            self.store.assign_agent(
                _required(payload, "teamId"),
                _required(payload, "agentId"),
                aliases=_aliases(payload),
            )
        elif command == "removeAgentFromTeam":
            self.store.remove_agent(
                _required(payload, "teamId"),
                _required(payload, "agentId"),
                aliases=_aliases(payload),
            )
        elif command == "splitTeam":
            agent_ids = [str(item) for item in payload.get("agentIds") or [] if item]
            self.store.split_team(
                _required(payload, "sourceTeamId"), agent_ids=agent_ids
            )
        elif command == "mergeTeams":
            self.store.merge_teams(
                _required(payload, "sourceTeamId"),
                _required(payload, "destinationTeamId"),
            )
        elif command == "reorderTeamAgents":
            agent_ids = [str(item) for item in payload.get("agentIds") or [] if item]
            self.store.reorder_team_agents(_required(payload, "teamId"), agent_ids)
        elif command == "updateTeamConfig":
            team_id = _required(payload, "teamId")
            current = self.store.team_config(team_id)
            patch = payload.get("configPatch") or {}
            self.store.update_team_config(
                team_id,
                _patched_config(current, patch),
                replace_task_filters="taskFilters" in patch,
            )
        elif command == "setAgentRenewalIntent":
            self.store.set_agent_renewal_request(
                _required(payload, "agentId"),
                requested=bool(payload.get("requested")),
            )
        else:
            raise SpiceError(f"unknown team command {command!r}")
        snapshot = self.store.team_snapshot()
        return TeamCommandResult(revision=snapshot.global_revision, snapshot=snapshot)


def _required(payload: dict[str, Any], key: str) -> str:
    return _normalized_id(str(payload.get(key) or ""), key)


def _aliases(payload: dict[str, Any]) -> list[str]:
    return [str(item) for item in payload.get("agentAliases") or [] if item]


def _agent_alias_ids(agent_id: str, aliases: Iterable[str]) -> list[str]:
    ids = [_normalized_id(agent_id, "agent_id")]
    for alias in aliases:
        normalized = _normalized_id(alias, "agent_alias")
        if normalized not in ids:
            ids.append(normalized)
    return ids


def _renewal_state_from_row(row: sqlite3.Row) -> TeamRenewalState:
    return TeamRenewalState(
        agent_id=str(row["agent_id"]),
        team_id=str(row["team_id"]),
        state=str(row["state"]),
        ancestor_thread_id=str(row["ancestor_thread_id"]),
        successor_agent_id=str(row["successor_agent_id"]),
        revision=int(row["revision"]),
    )


def _config_from_payload(raw: Any) -> TeamConfig:
    if not isinstance(raw, dict):
        return TeamConfig()
    return _patched_config(TeamConfig(), raw)


def _patched_config(current: TeamConfig, patch: dict[str, Any]) -> TeamConfig:
    task_filters = current.task_filters
    if isinstance(patch.get("taskFilters"), list):
        task_filters = _validated_task_filter_projects(
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
