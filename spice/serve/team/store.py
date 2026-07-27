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
from dataclasses import dataclass
from pathlib import Path
from threading import Lock
from typing import Any, Callable, Iterable, Iterator

from spice.errors import SpiceError
from spice.mail.ackstate import (
    ACK_STATE_DATABASE_FILENAME,
    ack_state_database_path,
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
from spice.serve.team.rosterstore import (
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
from spice.serve.team.projection import (
    PROJECTION_DATABASE_FILENAME as PROJECTION_DATABASE_FILENAME,
    ProjectionUnavailableError,
    ServeProjectionStore,
    projection_database_path,
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
    TASK_FILTER_SOURCE_AUTO_CLAIM as TASK_FILTER_SOURCE_AUTO_CLAIM,
    TASK_FILTER_SOURCE_AUTO_CREATE as TASK_FILTER_SOURCE_AUTO_CREATE,
    TASK_FILTER_SOURCE_MANUAL as TASK_FILTER_SOURCE_MANUAL,
    TASK_FILTER_SOURCES as TASK_FILTER_SOURCES,
    TEAM_AUTHORITY_MIGRATIONS,
    TEAM_AUTHORITY_MONOTONIC_VERSION_MAX,
    TEAM_AUTHORITY_SCHEMA_VERSION,
    TEAM_AUTHORITY_SCHEMAS,
    TEAM_AUTHORITY_TABLES,
    TEAM_DATABASE_FILENAME as TEAM_DATABASE_FILENAME,
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
GLOBAL_STORE_GENERATION_KEY = "store_generation"
GLOBAL_LANE_SCHEMA_KEY_PREFIX = "lane_schema:"
GLOBAL_PENDING_AUTHORITY_MIGRATION_KEY = "pending_authority_migration"
# SQLite reads `_` in a LIKE pattern as a single-character wildcard, and this
# prefix has one, so the unescaped pattern also selects keys nothing here
# wrote. Every key it selects is read as a schema version, and a value that
# never was one raises inside the migration transaction -- turning a store that
# would have opened into one no process in the fleet can open at all.
_LANE_SCHEMA_KEY_PATTERN = GLOBAL_LANE_SCHEMA_KEY_PREFIX.replace("_", r"\_") + "%"
_NANOSECONDS_PER_MICROSECOND = 1000
_SECONDS_PER_HOUR = 3600
# How long one lane's recorded schema version keeps a migration waiting. A lane
# refreshes its record every time it asks for work, so this has to outlast the
# time a lane spends on a single task or a working lane would look departed.
# It is also what bounds the wait: nothing has to prove a process dead, so a
# lane that exits without cleanup stops deferring on its own once it goes this
# long without being heard from.
LANE_SCHEMA_RECORD_HORIZON_HOURS = 4
LANE_SCHEMA_RECORD_HORIZON_SECONDS = (
    LANE_SCHEMA_RECORD_HORIZON_HOURS * _SECONDS_PER_HOUR
)


@dataclass(frozen=True)
class PendingAuthorityMigration:
    """One bounded signal that a writer is waiting to change the store."""

    source_version: int
    target_version: int


class AuthorityStoreSupersededError(SpiceError):
    """The store moved to a schema past the one this process was built for.

    Alone among this store's refusals in being about the process rather than
    the database: nothing is wrong with the store, and nothing this process
    can do will make it readable again, because the version it requires is
    compiled in. Restarting on current code is the whole repair, which is why
    it is worth telling apart from a refusal that a restart would only repeat.

    A distinct type rather than a distinct message because the callers who
    have to tell them apart are `except SpiceError` handlers that turn a
    refusal into an error response -- matching on prose there would put the
    wording of an operator-facing message in the way of a process exiting.
    """


class AuthorityMigrationDeferredError(SpiceError):
    """A migration refusal whose pending-intent write must survive the raise."""


def record_lane_schema_version(
    connection: sqlite3.Connection, lane: str, version: int
) -> None:
    """Record which authority schema the process working `lane` is running.

    This store is the only thing every lane already shares, so it is where a
    writer about to migrate can learn who else is running -- without walking
    every worktree's agent state to find out, and without a second sensing
    layer to keep alive. `global_settings` carries it because both retained
    authority shapes already have that table, which is what makes the record
    possible at all: it has to be written by a process compiled against the
    older constant, into a store that has not been migrated yet.

    One row per lane, overwritten in place. The only question a migrator asks
    is which schemas are running now, so a lane's earlier records answer a
    question nobody is asking, and keeping them would strand the horizon on
    whatever that lane was doing hours ago.

    Revision zero, like the store's own generation: this is a fact about a
    process, not a team event, and spending a revision on it would wake every
    connected client every time a lane asked for work.
    """
    connection.execute(
        "INSERT INTO global_settings (key, value, updated_at, revision) "
        "VALUES (?, ?, ?, 0) "
        "ON CONFLICT(key) DO UPDATE SET "
        "value = excluded.value, updated_at = excluded.updated_at",
        (f"{GLOBAL_LANE_SCHEMA_KEY_PREFIX}{lane}", str(version), time.time()),
    )


def retire_lane_schema_version(connection: sqlite3.Connection, lane: str) -> None:
    """Stop one lane from holding a pending authority migration back."""
    connection.execute(
        "DELETE FROM global_settings WHERE key = ?",
        (f"{GLOBAL_LANE_SCHEMA_KEY_PREFIX}{lane}",),
    )


def _record_pending_authority_migration(
    connection: sqlite3.Connection, source_version: int, target_version: int
) -> None:
    """Publish migration intent in the table both retained shapes can read."""
    connection.execute(
        "INSERT INTO global_settings (key, value, updated_at, revision) "
        "VALUES (?, ?, ?, 0) "
        "ON CONFLICT(key) DO UPDATE SET "
        "value = excluded.value, updated_at = excluded.updated_at",
        (
            GLOBAL_PENDING_AUTHORITY_MIGRATION_KEY,
            f"{source_version}:{target_version}",
            time.time(),
        ),
    )


def pending_authority_migration_from_connection(
    connection: sqlite3.Connection, *, now: float | None = None
) -> PendingAuthorityMigration | None:
    """Read migration intent through an already-open compatible store."""
    settings_table = connection.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'global_settings'"
    ).fetchone()
    row = (
        connection.execute(
            "SELECT value, updated_at FROM global_settings WHERE key = ?",
            (GLOBAL_PENDING_AUTHORITY_MIGRATION_KEY,),
        ).fetchone()
        if settings_table is not None
        else None
    )
    if row is None:
        return None
    value, updated_at = row
    observed_at = time.time() if now is None else now
    if float(updated_at) < observed_at - LANE_SCHEMA_RECORD_HORIZON_SECONDS:
        return None
    try:
        source_text, target_text = str(value).split(":", 1)
        return PendingAuthorityMigration(
            source_version=int(source_text),
            target_version=int(target_text),
        )
    except (TypeError, ValueError) as error:
        raise SpiceError(
            "pending team authority migration record has an invalid version "
            f"pair: {value!r}"
        ) from error


def pending_authority_migration(
    path: Path | None = None, *, now: float | None = None
) -> PendingAuthorityMigration | None:
    """Read a live migration intent without initializing or migrating the store."""
    selected_path = Path(path) if path is not None else team_database_path()
    if not selected_path.exists():
        return None
    with sqlite_connection(
        selected_path, busy_timeout_ms=TEAM_SQLITE_BUSY_TIMEOUT_MS
    ) as connection:
        return pending_authority_migration_from_connection(connection, now=now)


def _lagging_lanes(
    connection: sqlite3.Connection, version: int
) -> list[tuple[str, int]]:
    """Return each recently heard-from lane still running older than `version`."""
    rows = connection.execute(
        "SELECT key, value FROM global_settings "
        "WHERE key LIKE ? ESCAPE '\\' AND updated_at >= ? ORDER BY key",
        (
            _LANE_SCHEMA_KEY_PATTERN,
            time.time() - LANE_SCHEMA_RECORD_HORIZON_SECONDS,
        ),
    ).fetchall()
    recorded = (
        (str(key)[len(GLOBAL_LANE_SCHEMA_KEY_PREFIX) :], int(value))
        for key, value in rows
    )
    return [(lane, found) for lane, found in recorded if found < version]


def _require_drained_lanes(
    connection: sqlite3.Connection, source_version: int, target_version: int
) -> None:
    """Hold the migration back while a lane is still running the older shape.

    A migration is the one moment this store stops being readable by the code
    that was already using it: the writer that arrives with a newer constant
    replaces the shape underneath every process holding the old one, and those
    processes then refuse the store rather than corrupt it -- which is a fleet
    outage produced by an upgrade, not by anything going wrong. Waiting costs
    only the new process's start, and it is the only party that can wait,
    because it is the only one that knows a change is about to happen.
    """
    lagging = _lagging_lanes(connection, target_version)
    if not lagging:
        return
    _record_pending_authority_migration(connection, source_version, target_version)
    detail = ", ".join(f"{lane} at schema {recorded}" for lane, recorded in lagging)
    raise AuthorityMigrationDeferredError(
        f"team authority schema {target_version} is not being applied yet: "
        f"{len(lagging)} lane(s) are still running an older schema and would "
        f"lose this store the moment it moves -- {detail}. The migration runs "
        "on its own once those lanes finish, or "
        f"{LANE_SCHEMA_RECORD_HORIZON_HOURS} hours after the last one was "
        "heard from."
    )


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


def _authority_migration(version: int) -> str:
    try:
        return TEAM_AUTHORITY_MIGRATIONS[version]
    except KeyError as exc:
        raise SpiceError(
            f"missing team authority migration for version {version}"
        ) from exc


def _authority_schema(version: int) -> str:
    try:
        return TEAM_AUTHORITY_SCHEMAS[version]
    except KeyError as exc:
        raise SpiceError(
            f"missing team authority schema for version {version}"
        ) from exc


def _authority_schema_shape(
    version: int,
) -> dict[str, tuple[str, tuple[tuple[object, ...], ...]]]:
    """Return the one table shape `version` describes.

    The shape comes from that version's own frozen DDL, never from the writer's
    current one. Reading it from the current DDL is what failed before: editing
    the schema silently redefined a version that databases in the field were
    already stamped with, so one version named two different sets of columns.
    """
    expected = sqlite3.connect(":memory:")
    try:
        _execute_schema_script(expected, _authority_schema(version))
        return _authority_table_shape(expected)
    finally:
        expected.close()


def _authority_shape_mismatches(
    connection: sqlite3.Connection, version: int
) -> tuple[str, ...]:
    expected = _authority_schema_shape(version)
    actual = _authority_table_shape(connection)
    return tuple(
        table
        for table in sorted(TEAM_AUTHORITY_TABLES)
        if actual.get(table) != expected.get(table)
    )


def _authority_shape_error(connection: sqlite3.Connection, version: int) -> SpiceError:
    names = ", ".join(_authority_shape_mismatches(connection, version))
    return SpiceError(
        f"team authority schema version {version} has incompatible "
        f"durable table shape ({names}); refusing to rebuild or open it"
    )


def _require_authority_shape(connection: sqlite3.Connection, version: int) -> None:
    if _authority_shape_mismatches(connection, version):
        raise _authority_shape_error(connection, version)


def team_database_path(repo_root: Path | None = None) -> Path:
    from spice.tasks import config as task_config

    if repo_root is None or task_config.backend_override() is not None:
        backend_root = task_config.backend_root()
    else:
        from spice.paths import shared_state_root

        backend_root = shared_state_root(repo_root)
    return task_config.data_dir(backend_root) / TEAM_DATABASE_FILENAME


def _default_directive_state_path(team_path: Path | None) -> Path:
    if team_path is not None:
        return Path(team_path).with_name(ACK_STATE_DATABASE_FILENAME)
    from spice.tasks import config as task_config

    return ack_state_database_path(task_config.repo_root())


def _default_projection_path(team_path: Path | None) -> Path:
    if team_path is not None:
        return Path(team_path).with_name(PROJECTION_DATABASE_FILENAME)
    return projection_database_path()


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
    # readers. Authority changes are forward migrations, and this file holds
    # nothing else: rebuildable projections carry their own database, version,
    # and schema, so their DDL never opens this connection.
    _init_lock = Lock()
    _initialized_paths: set[Path] = set()

    def __init__(
        self,
        path: Path | None = None,
        *,
        directive_state_path: Path | None = None,
        projection_path: Path | None = None,
        superseded_hook: Callable[[AuthorityStoreSupersededError], None] | None = None,
    ) -> None:
        self.path = path or team_database_path()
        self._superseded_hook = superseded_hook
        self.directive_state_path = (
            directive_state_path
            if directive_state_path is not None
            else _default_directive_state_path(path)
        )
        # A store of its own, opened on its own connection, rather than an
        # `ATTACH` on this one: a projection that is missing, empty, drifted, or
        # corrupt must not be able to fail an authority read, and an attached
        # database shares the authority connection's fate.
        self.projections = ServeProjectionStore(
            projection_path
            if projection_path is not None
            else _default_projection_path(path)
        )
        self._task_event_wake_connection_ids: set[int] = set()

    def _superseded(self, message: str) -> AuthorityStoreSupersededError:
        """Build the refusal for a store that moved past this writer, and tell.

        Told before it is raised, because raising is not how this reaches the
        one party that can act on it. A long-running process meets this from
        whatever thread happened to touch the store next, inside a caller that
        already turns any `SpiceError` into an error response and carries on
        serving; the exception is answered locally and the process never learns
        it has been left behind. The hook is the process itself, hearing about
        it at the only moment anything knows.
        """
        error = AuthorityStoreSupersededError(message)
        if self._superseded_hook is not None:
            self._superseded_hook(error)
        return error

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
            if source_version == 0:
                # A store that does not exist yet is created at the current
                # shape rather than replayed into existence through history, so
                # the shapes this writer retains stay a record of what it can
                # open rather than the steps by which anything is built.
                _execute_schema_script(
                    connection, _authority_schema(TEAM_AUTHORITY_SCHEMA_VERSION)
                )
                self._write_store_generation_locked(connection)
            elif source_version != TEAM_AUTHORITY_SCHEMA_VERSION:
                # Checked inside the write transaction, beside the step it
                # guards. Deciding outside it would leave a window where a
                # lane records itself after the check and loses the store to
                # the bump that follows -- the one lane whose timing this
                # cannot afford to get wrong.
                _require_drained_lanes(
                    connection,
                    source_version,
                    TEAM_AUTHORITY_SCHEMA_VERSION,
                )
                # The source resolver admits no version but the predecessor
                # here, so this is the one forward step a writer ever runs.
                _execute_schema_script(
                    connection, _authority_migration(TEAM_AUTHORITY_SCHEMA_VERSION)
                )
            _require_authority_shape(connection, TEAM_AUTHORITY_SCHEMA_VERSION)
            connection.execute(f"PRAGMA user_version = {TEAM_AUTHORITY_SCHEMA_VERSION}")
            connection.execute(
                "DELETE FROM global_settings WHERE key = ?",
                (GLOBAL_PENDING_AUTHORITY_MIGRATION_KEY,),
            )
            connection.commit()
        except AuthorityMigrationDeferredError:
            # The only write before this refusal is the pending intent. It has
            # to outlive the failed open so old-code launchers can see the
            # migration window; every other failure remains fully atomic.
            connection.commit()
            raise
        except BaseException:
            connection.rollback()
            raise

    def _write_store_generation_locked(self, connection: sqlite3.Connection) -> None:
        """Date this store the instant it is created, and only then.

        Every counter this store keeps -- a team's event revision, an agent's
        renewal revision -- restarts from zero in a store that was deleted and
        remade, so a reader keeping the highest revision it has seen would
        refuse the rebuilt store until it counted back past where the replaced
        one stopped. The generation is what carries a reader across that: a
        store is only ever created after every store it replaces, so this
        instant rises exactly where those revisions restart.

        Only a store being created writes one. A store that already existed is
        left with no generation at all, which is what it truthfully has and
        which orders below every minted one -- correctly, because that store
        does predate them all. Its own remake mints one and rises above it.
        Migration reaches authority rows for no other reason, and dating a
        store by when it was upgraded would be the wrong instant anyway.

        It is counted in microseconds because that is what every generation
        this repo mints is counted in, so a reader that meets more than one of
        them meets one kind of token rather than one encoding per authority.
        """
        connection.execute(
            "INSERT INTO global_settings (key, value, updated_at, revision) "
            "VALUES (?, ?, ?, 0)",
            (
                GLOBAL_STORE_GENERATION_KEY,
                str(time.time_ns() // _NANOSECONDS_PER_MICROSECOND),
                time.time(),
            ),
        )

    def store_generation(self) -> str:
        """Return the instant this store was created, as its readers order it."""
        with self.connect() as connection:
            row = connection.execute(
                "SELECT value FROM global_settings WHERE key = ?",
                (GLOBAL_STORE_GENERATION_KEY,),
            ).fetchone()
        return str(row["value"]) if row is not None else ""

    def _authority_source_version_locked(self, connection: sqlite3.Connection) -> int:
        """Return the version whose shape this database actually carries.

        The stamp says which migrations a database has been through, and the
        shape is what it has to show for them. Where they disagree the shape is
        what the next migration has to operate on, so the shape decides. A stamp
        that outran its shape is exactly how a fleet lost its authority store:
        every reader believed a version number that no longer named the columns
        in front of it, and the recovery was to edit the database by hand.

        Reading the source from the shape makes that recoverable instead: a
        database whose columns match a shape this writer carries is migrated
        forward from there, however it came to be stamped otherwise, and one
        whose columns match none of them still fails untouched. Exactly one can
        match, because no two of the retained shapes describe the same tables.

        The stamp keeps the two jobs it can still do honestly. Consecutive
        versions own a reserved low positive namespace, so a stamp above this
        writer within that namespace means a newer writer has been here and is
        refused before shape matching. That ordering matters when a future
        version adds an authority table this writer does not know to inspect.

        Before monotonic versions, this database stored a 31-bit CRC32 schema
        fingerprint in the same field. The v0.27 fingerprint sits outside the
        version namespace while its authority tables still carry a retained
        source shape, so that shape authenticates the migration source without
        a fingerprint registry. A stamp of zero on a populated database is from
        before either contract, and no migration claims to know what that is.

        The shapes are bounded to the current version and the one predecessor
        it converts, so a database older than that matches nothing and is
        refused for the release that still owns its conversion.
        """
        stored = int(connection.execute("PRAGMA user_version").fetchone()[0])
        if (
            TEAM_AUTHORITY_SCHEMA_VERSION
            < stored
            <= TEAM_AUTHORITY_MONOTONIC_VERSION_MAX
        ):
            raise self._superseded(
                "team authority database was written by newer schema version "
                f"{stored}; this writer supports through "
                f"{TEAM_AUTHORITY_SCHEMA_VERSION} and will not mutate it"
            )
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
        for version in sorted(TEAM_AUTHORITY_SCHEMAS):
            if not _authority_shape_mismatches(connection, version):
                return version
        if stored > TEAM_AUTHORITY_SCHEMA_VERSION:
            raise SpiceError(
                "pre-version team authority database has unsupported durable "
                f"table shape under schema fingerprint {stored}; refusing to "
                "rebuild or open it"
            )
        # Report the drift against the version the database claims to be, which
        # is the comparison an operator reading the message is already holding.
        raise _authority_shape_error(connection, stored)

    @contextmanager
    def connect(self) -> Iterator[sqlite3.Connection]:
        self._ensure_schema()
        connection = sqlite3.connect(self.path)
        connection.row_factory = sqlite3.Row
        try:
            connection.execute(f"PRAGMA busy_timeout = {TEAM_SQLITE_BUSY_TIMEOUT_MS}")
            stored = int(connection.execute("PRAGMA user_version").fetchone()[0])
            if stored != TEAM_AUTHORITY_SCHEMA_VERSION:
                superseded = stored > TEAM_AUTHORITY_SCHEMA_VERSION
                relation = "newer" if superseded else "unsupported"
                message = (
                    f"team authority database changed to {relation} schema "
                    f"version {stored}; this writer requires "
                    f"{TEAM_AUTHORITY_SCHEMA_VERSION} and will not mutate it"
                )
                # Only forward. A store stamped below what this process runs is
                # not something a restart resolves -- the same code would come
                # back and refuse it again -- so it stays the refusal it was.
                if superseded:
                    raise self._superseded(message)
                raise SpiceError(message)
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
        try:
            self._prune_metric_history_locked(connection, now=time.time())
        except ProjectionUnavailableError:
            # Projection maintenance may be unavailable during an isolated
            # rebuild or after an incompatible/corrupt projection was
            # discarded. Authority remains readable in that state; projection
            # diagnostics carry the exact recovery action.
            pass
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
