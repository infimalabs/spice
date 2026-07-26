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
from contextlib import contextmanager
from pathlib import Path
from threading import Lock
from typing import Any, Callable, Iterable, Iterator

from spice.errors import SpiceError
from spice.mail.ackstate import (
    ACK_STATE_DATABASE_FILENAME,
    ack_state_database_path,
    migrate_serve_directive_history,
    prepare_directive_history_database,
)
from spice.sqliteconnection import sqlite_connection
from spice.serve.team.filters import (
    TeamFilterStoreMixin,
    config_from_row,
    shell_settings_from_json,
    task_filter_projects_from_json,
)
from spice.serve.team.identity import (
    TeamIdentityStoreMixin,
    agent_identity_from_row,
    select_agent_identity_rows,
)
from spice.serve.team.memberstore import (
    MAX_TEAM_MEMBERS as MAX_TEAM_MEMBERS,
    TeamMemberStoreMixin,
)
from spice.serve.directivestats import (
    DirectiveStatsStoreMixin,
    DirectiveTotals as DirectiveTotals,
)
from spice.serve.team.history import (
    METRIC_BUCKET_SECONDS as METRIC_BUCKET_SECONDS,
    LaneMetricSummary as LaneMetricSummary,
    ObservationAttributionMode as ObservationAttributionMode,
    TeamHistoricalMetricSummary as TeamHistoricalMetricSummary,
)
from spice.serve.team.metrics import (
    MetricSeriesPoint as MetricSeriesPoint,
    TaskDistributionSeriesPoint as TaskDistributionSeriesPoint,
    TaskLifecycleSeriesPoint as TaskLifecycleSeriesPoint,
    TaskStallState as TaskStallState,
    TeamMetricStoreMixin,
)
from spice.serve.team.models import (
    GlobalSettings,
    TeamAgentIdentity as TeamAgentIdentity,
    TeamConfig as TeamConfig,
    TeamMember,
    TeamRenewalState,
    TeamSnapshot,
    TeamState,
    TeamTaskFilter as TeamTaskFilter,
    renewal_intent_payload as renewal_intent_payload,
)
from spice.serve.team.renewals import (
    TeamRenewalStoreMixin,
    renewal_state_from_row,
)
from spice.serve.team.schema import (
    DEFAULT_LIFETIME as DEFAULT_LIFETIME,
    LEGACY_TEAM_SCHEMA_FINGERPRINT,
    OBSERVATION_ATTRIBUTION_REBUILD_REQUIRED,
    OBSERVATION_ATTRIBUTION_SAFE,
    TASK_FILTER_SOURCE_AUTO_CLAIM as TASK_FILTER_SOURCE_AUTO_CLAIM,
    TASK_FILTER_SOURCE_AUTO_CREATE as TASK_FILTER_SOURCE_AUTO_CREATE,
    TASK_FILTER_SOURCE_MANUAL as TASK_FILTER_SOURCE_MANUAL,
    TASK_FILTER_SOURCES as TASK_FILTER_SOURCES,
    TEAM_AUTHORITY_MIGRATIONS,
    TEAM_AUTHORITY_SCHEMAS,
    TEAM_AUTHORITY_SCHEMA_VERSION,
    TEAM_AUTHORITY_TABLES,
    TEAM_DATABASE_FILENAME as TEAM_DATABASE_FILENAME,
    TEAM_PROJECTION_SCHEMA,
    TEAM_PROJECTION_TABLES,
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
GLOBAL_SETTINGS_EVENT_TEAM_ID = "__global_settings__"
GLOBAL_FAST_MODE_KEY = "fast_mode"


def _schema_statements(script: str) -> tuple[str, ...]:
    statements: list[str] = []
    pending = ""
    for character in script:
        pending += character
        if character == ";" and sqlite3.complete_statement(pending):
            statement = pending.strip()
            if statement:
                statements.append(statement)
            pending = ""
    if pending.strip():
        raise SpiceError("team schema contains an incomplete SQL statement")
    return tuple(statements)


def _execute_schema_script(connection: sqlite3.Connection, script: str) -> None:
    # sqlite3.executescript() commits an existing transaction before running.
    # Execute complete statements individually so the caller's migration
    # transaction remains the sole atomic boundary.
    for statement in _schema_statements(script):
        connection.execute(statement)


def _table_columns(connection: sqlite3.Connection, table: str) -> tuple[str, ...]:
    return tuple(
        str(row[1]) for row in connection.execute(f'PRAGMA table_info("{table}")')
    )


def _canonical_projection_columns() -> dict[str, tuple[str, ...]]:
    """The columns each projection table has when built from the current DDL."""
    probe = sqlite3.connect(":memory:")
    try:
        _execute_schema_script(probe, TEAM_PROJECTION_SCHEMA)
        return {table: _table_columns(probe, table) for table in TEAM_PROJECTION_TABLES}
    finally:
        probe.close()


def _drop_drifted_projections_locked(connection: sqlite3.Connection) -> None:
    """Discard projection tables whose shape no longer matches the current DDL.

    A projection is derived state, so a shape change costs a replay rather than
    a migration ladder: the drifted table is dropped and recreated empty by the
    schema script that follows. Authority tables never reach here -- they carry
    versioned migrations and are validated against their canonical shape.
    """
    for table, columns in _canonical_projection_columns().items():
        live = _table_columns(connection, table)
        if live and live != columns:
            connection.execute(f'DROP TABLE "{table}"')


def _authority_table_shape(
    connection: sqlite3.Connection,
) -> dict[str, tuple[str, tuple[tuple[object, ...], ...]]]:
    shape: dict[str, tuple[str, tuple[tuple[object, ...], ...]]] = {}
    for table in TEAM_AUTHORITY_TABLES:
        rows = connection.execute(f'PRAGMA table_xinfo("{table}")').fetchall()
        if rows:
            sql_row = connection.execute(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?",
                (table,),
            ).fetchone()
            table_sql = " ".join(str(sql_row[0]).split()) if sql_row else ""
            shape[table] = (
                table_sql,
                tuple(
                    (
                        str(row[1]),
                        str(row[2]).upper(),
                        int(row[3]),
                        row[4],
                        int(row[5]),
                        int(row[6]),
                    )
                    for row in rows
                ),
            )
    return shape


def _authority_schema_shape(
    schema: str,
) -> dict[str, tuple[str, tuple[tuple[object, ...], ...]]]:
    expected = sqlite3.connect(":memory:")
    try:
        _execute_schema_script(expected, schema)
        return _authority_table_shape(expected)
    finally:
        expected.close()


def team_database_path() -> Path:
    from spice.tasks import config as task_config

    return task_config.data_dir() / TEAM_DATABASE_FILENAME


def _default_directive_state_path(team_path: Path | None) -> Path:
    if team_path is not None:
        return Path(team_path).with_name(ACK_STATE_DATABASE_FILENAME)
    from spice.tasks import config as task_config

    return ack_state_database_path(task_config.repo_root())


class ServeTeamStore(
    TeamIdentityStoreMixin,
    TeamRenewalStoreMixin,
    TeamFilterStoreMixin,
    TeamMemberStoreMixin,
    TeamMetricStoreMixin,
    DirectiveStatsStoreMixin,
):
    # Schema is checked once per database path per process. Running even
    # idempotent DDL on every connect takes an exclusive lock and serializes
    # readers. Authority changes are forward migrations; projection DDL may
    # evolve independently without changing the authority version.
    _init_lock = Lock()
    _initialized_paths: set[Path] = set()

    def __init__(
        self,
        path: Path | None = None,
        *,
        directive_state_path: Path | None = None,
    ) -> None:
        self.path = path or team_database_path()
        self.directive_state_path = (
            directive_state_path
            if directive_state_path is not None
            else _default_directive_state_path(path)
        )
        self._task_event_wake_connection_ids: set[int] = set()

    def _ensure_schema(self) -> None:
        if self.path in self._initialized_paths:
            return
        with self._init_lock:
            if self.path in self._initialized_paths:
                return
            with sqlite_connection(
                self.path,
                busy_timeout_ms=TEAM_SQLITE_BUSY_TIMEOUT_MS,
                ensure_parent=True,
            ) as connection:
                self._sync_schema_locked(connection)
                # Compatibility is established before journal mode mutates the
                # database. A newer writer therefore fails without even a WAL
                # mode change.
                connection.execute("PRAGMA journal_mode = WAL")
            self._initialized_paths.add(self.path)

    def _sync_schema_locked(self, connection: sqlite3.Connection) -> None:
        # Do a read-only compatibility pass before acquiring a write
        # transaction. The same checks run again after BEGIN IMMEDIATE so a
        # concurrent migrator cannot make this decision stale while waiting.
        self._authority_source_version_locked(connection)
        connection.execute("BEGIN IMMEDIATE")
        try:
            source_version = self._authority_source_version_locked(connection)
            prepare_directive_history_database(self.directive_state_path)
            migration_scripts = [
                self._authority_migration(version)
                for version in range(
                    source_version + 1, TEAM_AUTHORITY_SCHEMA_VERSION + 1
                )
            ]
            for script in migration_scripts:
                _execute_schema_script(connection, script)
            self._migrate_legacy_directive_projection_locked(connection)
            _drop_drifted_projections_locked(connection)
            _execute_schema_script(connection, TEAM_PROJECTION_SCHEMA)
            self._initialize_observation_attribution_state_locked(connection)
            self._validate_authority_schema_locked(
                connection, TEAM_AUTHORITY_SCHEMA_VERSION
            )
            connection.execute(f"PRAGMA user_version = {TEAM_AUTHORITY_SCHEMA_VERSION}")
            connection.commit()
        except BaseException:
            connection.rollback()
            raise

    def _migrate_legacy_directive_projection_locked(
        self, connection: sqlite3.Connection
    ) -> None:
        tables = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table' "
                "AND name IN ('directives', 'directive_totals')"
            )
        }
        if not tables:
            return
        if tables != {"directives", "directive_totals"}:
            raise SpiceError(
                "legacy Serve directive projection is incomplete: expected both "
                "directives and directive_totals. Restore both tables or replay "
                "canonical steering/ACK facts; no legacy table was removed"
            )
        raw_directive_rows = connection.execute(
            "SELECT directive_key, agent_id, team_id, sent_at, acked, acked_at "
            "FROM directives ORDER BY sent_at, directive_key"
        ).fetchall()
        directive_rows = [
            {
                "directive_key": row[0],
                "agent_id": row[1],
                "team_id": row[2],
                "sent_at": row[3],
                "acked": row[4],
                "acked_at": row[5],
            }
            for row in raw_directive_rows
        ]
        raw_total_rows = connection.execute(
            "SELECT agent_id, team_id, sends, acked FROM directive_totals "
            "ORDER BY agent_id, team_id"
        ).fetchall()
        total_rows = [
            {
                "agent_id": row[0],
                "team_id": row[1],
                "sends": row[2],
                "acked": row[3],
            }
            for row in raw_total_rows
        ]
        migrate_serve_directive_history(
            self.directive_state_path, directive_rows, total_rows
        )
        connection.execute("DROP TABLE directives")
        connection.execute("DROP TABLE directive_totals")

    def _initialize_observation_attribution_state_locked(
        self, connection: sqlite3.Connection
    ) -> None:
        existing = connection.execute(
            "SELECT status FROM observation_attribution_state WHERE singleton = 1"
        ).fetchone()
        if existing is not None:
            return
        observation_row = connection.execute(
            "SELECT 1 FROM agent_metrics "
            "UNION ALL SELECT 1 FROM agent_metric_buckets "
            "UNION ALL SELECT 1 FROM task_events LIMIT 1"
        ).fetchone()
        status = (
            OBSERVATION_ATTRIBUTION_REBUILD_REQUIRED
            if observation_row is not None
            else OBSERVATION_ATTRIBUTION_SAFE
        )
        connection.execute(
            "INSERT INTO observation_attribution_state (singleton, status) "
            "VALUES (1, ?)",
            (status,),
        )

    def _authority_source_version_locked(self, connection: sqlite3.Connection) -> int:
        stored = int(connection.execute("PRAGMA user_version").fetchone()[0])
        if stored == LEGACY_TEAM_SCHEMA_FINGERPRINT:
            self._validate_authority_schema_locked(connection, 1)
            return 1
        if stored == 0:
            tables = {
                str(row[0])
                for row in connection.execute(
                    "SELECT name FROM sqlite_master "
                    "WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"
                )
            }
            if not tables:
                return 0
            raise SpiceError(
                "unversioned populated team database has no supported migration; "
                "refusing to rebuild or open durable team state"
            )
        if stored > TEAM_AUTHORITY_SCHEMA_VERSION:
            raise SpiceError(
                "team authority database was written by newer schema version "
                f"{stored}; this writer supports through "
                f"{TEAM_AUTHORITY_SCHEMA_VERSION} and will not mutate it"
            )
        if stored not in TEAM_AUTHORITY_SCHEMAS:
            raise SpiceError(
                f"unsupported team authority schema version {stored}; "
                "refusing to mutate durable team state"
            )
        self._validate_authority_schema_locked(connection, stored)
        return stored

    def _authority_migration(self, version: int) -> str:
        try:
            return TEAM_AUTHORITY_MIGRATIONS[version]
        except KeyError as exc:
            raise SpiceError(
                f"missing team authority migration for version {version}"
            ) from exc

    def _validate_authority_schema_locked(
        self, connection: sqlite3.Connection, version: int
    ) -> None:
        try:
            expected_schema = TEAM_AUTHORITY_SCHEMAS[version]
        except KeyError as exc:
            raise SpiceError(
                f"missing team authority schema contract for version {version}"
            ) from exc
        expected = _authority_schema_shape(expected_schema)
        actual = _authority_table_shape(connection)
        mismatches = [
            table
            for table in sorted(TEAM_AUTHORITY_TABLES)
            if actual.get(table) != expected.get(table)
        ]
        if mismatches:
            names = ", ".join(mismatches)
            raise SpiceError(
                f"team authority schema version {version} has incompatible "
                f"durable table shape ({names}); refusing to rebuild or open it"
            )

    @contextmanager
    def connect(self) -> Iterator[sqlite3.Connection]:
        self._ensure_schema()
        connection = sqlite3.connect(self.path)
        connection.row_factory = sqlite3.Row
        try:
            connection.execute(f"PRAGMA busy_timeout = {TEAM_SQLITE_BUSY_TIMEOUT_MS}")
            stored = int(connection.execute("PRAGMA user_version").fetchone()[0])
            if stored != TEAM_AUTHORITY_SCHEMA_VERSION:
                relation = (
                    "newer" if stored > TEAM_AUTHORITY_SCHEMA_VERSION else "unsupported"
                )
                raise SpiceError(
                    f"team authority database changed to {relation} schema "
                    f"version {stored}; this writer requires "
                    f"{TEAM_AUTHORITY_SCHEMA_VERSION} and will not mutate it"
                )
            yield connection
            connection.commit()
            if id(connection) in self._task_event_wake_connection_ids:
                # Wake only after the team transaction is visible. Otherwise the
                # lane watcher can observe the event file and re-read stale team
                # facts before the commit lands.
                from spice.tasks.config import mark_task_backend_changed

                mark_task_backend_changed("team")
        finally:
            self._task_event_wake_connection_ids.discard(id(connection))
            connection.close()

    # ---- events / revisions -------------------------------------------

    def _record_event(
        self,
        connection: sqlite3.Connection,
        kind: str,
        team_id: str,
        payload: dict[str, Any],
        *,
        wake: bool = True,
    ) -> int:
        cursor = connection.execute(
            "INSERT INTO events (ts, kind, team_id, payload) VALUES (?, ?, ?, ?)",
            (time.time(), kind, team_id, json.dumps(payload, separators=(",", ":"))),
        )
        revision = int(cursor.lastrowid or 0)
        connection.execute(
            "UPDATE teams SET revision = ? WHERE team_id = ?", (revision, team_id)
        )
        # Wake the serve lane watcher after commit: it watches the task event
        # file, not the team store (whose writes are dominated by non-display
        # metric churn), so a real team event surfaces in the UI without waking
        # readers before the transaction is visible. Events that change NO
        # lane's content (a composer reorder only permutes member order)
        # pass wake=False: the acting client already has the new order from the
        # command response and other clients pick it up from the team revision,
        # so waking every member lane into a message re-push -- and the full
        # re-render and history re-pagination that follows -- is pure churn.
        if wake:
            self._task_event_wake_connection_ids.add(id(connection))
        return revision

    def _mark_team_revisions_locked(
        self,
        connection: sqlite3.Connection,
        team_ids: Iterable[str],
        revision: int,
    ) -> None:
        for team_id in dict.fromkeys(team_ids):
            connection.execute(
                "UPDATE teams SET revision = ? WHERE team_id = ?",
                (revision, team_id),
            )

    def global_fast_mode_enabled(self) -> bool:
        with self.connect() as connection:
            return self._global_settings_locked(connection).fast_mode

    def set_global_fast_mode_enabled(self, enabled: bool) -> int:
        with self.connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            return self._set_global_fast_mode_enabled_locked(connection, enabled)

    def _global_settings_locked(self, connection: sqlite3.Connection) -> GlobalSettings:
        row = connection.execute(
            "SELECT value FROM global_settings WHERE key = ?",
            (GLOBAL_FAST_MODE_KEY,),
        ).fetchone()
        value = row["value"] if row is not None else ""
        return GlobalSettings(fast_mode=_settings_bool(value))

    def _set_global_fast_mode_enabled_locked(
        self, connection: sqlite3.Connection, enabled: bool
    ) -> int:
        current = self._global_settings_locked(connection)
        if current.fast_mode == enabled:
            return self._current_revision_locked(connection)
        revision = self._record_event(
            connection,
            "setGlobalFastMode",
            GLOBAL_SETTINGS_EVENT_TEAM_ID,
            {"fastMode": enabled},
        )
        connection.execute(
            "INSERT INTO global_settings (key, value, updated_at, revision) "
            "VALUES (?, ?, ?, ?) "
            "ON CONFLICT(key) DO UPDATE SET "
            "value = excluded.value, updated_at = excluded.updated_at, "
            "revision = excluded.revision",
            (GLOBAL_FAST_MODE_KEY, json.dumps(enabled), time.time(), revision),
        )
        return revision

    def _current_revision_locked(self, connection: sqlite3.Connection) -> int:
        row = connection.execute("SELECT MAX(revision) AS r FROM events").fetchone()
        return int(row["r"] or 0)

    def apply_team_command(
        self,
        *,
        expected_revision: int | None,
        command: Callable[[sqlite3.Connection], None],
    ) -> TeamSnapshot:
        with self.connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            self._require_expected_revision_locked(connection, expected_revision)
            command(connection)
            return self._team_snapshot_locked(connection)

    def _require_expected_revision_locked(
        self, connection: sqlite3.Connection, expected_revision: int | None
    ) -> None:
        if expected_revision is None:
            return
        current_revision = self._current_revision_locked(connection)
        if expected_revision != current_revision:
            raise SpiceError(
                "stale team command: expected revision "
                f"{expected_revision}, current revision {current_revision}"
            )

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
        # A pruned team is already closed -- absent from the open-team snapshot
        # every client renders -- so its garbage collection changes no lane's
        # displayed content. This prune runs inside team_snapshot(), which backs
        # every teams.refresh poll: waking here would let a plain read bump the
        # shared task event file and re-push every visible lane's full payload
        # (transcript re-read + history) on unrelated GC. Record the revision
        # bump without waking; clients reconcile the (identical open) topology on
        # their next natural poll.
        self._record_event(
            connection,
            "pruneZeroActivityTeams",
            PRUNE_EVENT_TEAM_ID,
            {"teams": list(team_ids), "count": len(team_ids)},
            wake=False,
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
            return self._team_snapshot_locked(connection)

    def team_snapshot_delta_payload(
        self, snapshot: TeamSnapshot, *, since_revision: int
    ) -> dict[str, Any]:
        active_team_ids = {team.team_id for team in snapshot.teams}
        with self.connect() as connection:
            removed_team_ids = self._removed_team_ids_since_locked(
                connection,
                active_team_ids=active_team_ids,
                since_revision=since_revision,
                through_revision=snapshot.global_revision,
            )
        changed_teams = [
            team.to_payload()
            for team in snapshot.teams
            if team.revision > since_revision
        ]
        return {
            "globalRevision": snapshot.global_revision,
            "globalSettings": snapshot.global_settings.to_payload(),
            "teamCount": len(snapshot.teams),
            "teams": changed_teams,
            "removedTeamIds": removed_team_ids,
        }

    def _removed_team_ids_since_locked(
        self,
        connection: sqlite3.Connection,
        *,
        active_team_ids: set[str],
        since_revision: int,
        through_revision: int,
    ) -> list[str]:
        rows = connection.execute(
            "SELECT kind, team_id, payload FROM events "
            "WHERE revision > ? AND revision <= ? ORDER BY revision",
            (since_revision, through_revision),
        ).fetchall()
        removed: list[str] = []
        for row in rows:
            event_team_id = str(row["team_id"])
            if event_team_id not in active_team_ids and event_team_id not in {
                GLOBAL_SETTINGS_EVENT_TEAM_ID,
                PRUNE_EVENT_TEAM_ID,
            }:
                removed.append(event_team_id)
            payload = json.loads(str(row["payload"]) or "{}")
            if row["kind"] == "mergeTeams":
                removed.append(str(payload.get("sourceTeamId") or ""))
            if row["kind"] == "pruneZeroActivityTeams":
                removed.extend(str(team_id) for team_id in payload.get("teams", ()))
        return [
            team_id
            for team_id in dict.fromkeys(removed)
            if team_id and team_id not in active_team_ids
        ]

    def _team_snapshot_locked(self, connection: sqlite3.Connection) -> TeamSnapshot:
        self._prune_zero_activity_closed_teams_locked(connection)
        self._prune_metric_history_locked(connection, now=time.time())
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
        return TeamSnapshot(
            global_revision=global_revision,
            teams=teams,
            global_settings=self._global_settings_locked(connection),
        )

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
            "SELECT agent_id FROM memberships WHERE team_id = ? ORDER BY position",
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
                str(renewal["agent_id"]): state
                for renewal in renewal_rows
                if (state := renewal_state_from_row(renewal)) is not None
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


def _settings_bool(value: object) -> bool:
    if isinstance(value, bool):
        return value
    if not isinstance(value, str) or not value:
        return False
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError as exc:
        raise SpiceError("global fast mode setting is malformed") from exc
    if not isinstance(parsed, bool):
        raise SpiceError("global fast mode setting must be boolean")
    return parsed


from spice.serve.team.commands import (  # noqa: E402
    TeamCommandResult as TeamCommandResult,
    TeamCommandService as TeamCommandService,
)
