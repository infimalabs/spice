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
from pathlib import Path
from typing import Any, Iterable, Iterator

from spice.errors import SpiceError
from spice.serve.teamfilters import (
    TeamFilterStoreMixin,
    config_from_row,
    shell_settings_from_json,
    task_filter_projects_from_json,
)
from spice.serve.teamids import agent_alias_ids as _agent_alias_ids
from spice.serve.teamids import normalized_id as _normalized_id
from spice.serve.teamidentity import (
    TeamIdentityStoreMixin,
    agent_identity_from_row,
    select_agent_identity_rows,
)
from spice.serve.teammetrics import (
    METRIC_BUCKET_SECONDS as METRIC_BUCKET_SECONDS,
    LaneMetricSummary as LaneMetricSummary,
    TeamMetricStoreMixin,
)
from spice.serve.teammodels import (
    TeamAgentIdentity as TeamAgentIdentity,
    TeamConfig as TeamConfig,
    TeamMember,
    TeamRenewalState,
    TeamSnapshot,
    TeamState,
    TeamTaskFilter as TeamTaskFilter,
    renewal_intent_payload as renewal_intent_payload,
)
from spice.serve.teamrenewals import (
    TeamRenewalStoreMixin,
    renewal_state_from_row,
)
from spice.serve.teamschema import (
    DEFAULT_LIFETIME as DEFAULT_LIFETIME,
    DEFAULT_SELECTED_VIEW as DEFAULT_SELECTED_VIEW,
    DEFAULT_SPEECH_MODE as DEFAULT_SPEECH_MODE,
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
DROPPED_TEAM_METRIC_TABLES = (
    "team_agent_metrics",
    "team_agent_metric_buckets",
    "team_agent_history",
)


def team_database_path() -> Path:
    from spice.tasks import config as task_config

    return task_config.data_dir() / TEAM_DATABASE_FILENAME


class ServeTeamStore(
    TeamIdentityStoreMixin,
    TeamRenewalStoreMixin,
    TeamFilterStoreMixin,
    TeamMetricStoreMixin,
):
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
            self._migrate_renewal_identity_columns_locked(connection)
            self._migrate_task_filter_sources_locked(connection)
            self._migrate_team_metric_model_locked(connection)
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

    def _migrate_team_metric_model_locked(self, connection: sqlite3.Connection) -> None:
        for table in DROPPED_TEAM_METRIC_TABLES:
            connection.execute(f"DROP TABLE IF EXISTS {table}")
        columns = {
            str(row["name"])
            for row in connection.execute("PRAGMA table_info(memberships)")
        }
        if "position" in columns:
            return
        connection.execute(
            "ALTER TABLE memberships ADD COLUMN position INTEGER NOT NULL DEFAULT 0"
        )
        connection.execute(
            "UPDATE memberships "
            "SET position = ("
            "  SELECT COUNT(*) FROM memberships AS prior "
            "  WHERE prior.team_id = memberships.team_id "
            "  AND ("
            "    prior.joined_at < memberships.joined_at "
            "    OR (prior.joined_at = memberships.joined_at "
            "        AND prior.agent_id < memberships.agent_id)"
            "  )"
            ")"
        )

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
            "team_merge_subgroups",
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
        if task_filter_projects_from_json(row["task_filters"]):
            return False
        if shell_settings_from_json(row["shell_settings"]):
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

    def _team_member_ids_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> list[str]:
        rows = connection.execute(
            "SELECT agent_id FROM memberships WHERE team_id = ? ORDER BY joined_at",
            (team_id,),
        ).fetchall()
        return [str(row["agent_id"]) for row in rows]

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
        identity_by_actor: dict[str, TeamAgentIdentity] = {}
        renewal_by_agent: dict[str, TeamRenewalState] = {}
        if member_rows:
            member_ids = tuple(str(member["agent_id"]) for member in member_rows)
            placeholders = ",".join("?" for _ in member_rows)
            identity_rows = select_agent_identity_rows(connection, member_ids)
            identity_by_actor = {
                str(identity["actor_id"]): agent_identity_from_row(identity)
                for identity in identity_rows
            }
            renewal_rows = connection.execute(
                "SELECT agent_id, team_id, state, ancestor_thread_id, "
                "successor_agent_id, successor_thread_id, team_slot, "
                "predecessor_identity, successor_identity, revision FROM renewals "
                f"WHERE agent_id IN ({placeholders})",
                member_ids,
            ).fetchall()
            renewal_by_agent = {
                str(renewal["agent_id"]): renewal_state_from_row(renewal)
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
            config=config_from_row(
                row, self._task_filter_entries_locked(connection, team_id)
            ),
            members=tuple(
                TeamMember(
                    agent_id=member["agent_id"],
                    agent_facts=(
                        identity.to_payload()
                        if (identity := identity_by_actor.get(str(member["agent_id"])))
                        else {}
                    ),
                    renewal=renewal_by_agent.get(str(member["agent_id"])),
                )
                for member in member_rows
            ),
            split_back_available=split_back_subgroup is not None,
            split_back_member_count=split_back_member_count,
        )

    def _require_team(
        self, connection: sqlite3.Connection, team_id: str
    ) -> sqlite3.Row:
        row = connection.execute(
            "SELECT * FROM teams WHERE team_id = ?", (team_id,)
        ).fetchone()
        if row is None:
            raise SpiceError(f"unknown team: {team_id}")
        return row

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


def _team_subgroup_agent_ids(raw: object) -> tuple[str, ...]:
    try:
        values = json.loads(_json_source(raw))
    except (json.JSONDecodeError, TypeError):
        return ()
    if not isinstance(values, list):
        return ()
    agent_ids = [str(item) for item in values if str(item or "").strip()]
    return tuple(dict.fromkeys(agent_ids))


def _json_source(raw: object) -> str | bytes | bytearray:
    return raw if isinstance(raw, str | bytes | bytearray) else ""


from spice.serve.teamcommands import (  # noqa: E402
    TeamCommandResult as TeamCommandResult,
    TeamCommandService as TeamCommandService,
)
