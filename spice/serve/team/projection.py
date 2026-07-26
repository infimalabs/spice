"""Rebuildable Serve projections, in their own database.

A projection holds nothing that cannot be produced again from a native fact
source, so it lives in its own file with its own schema, its own version, and
its own failure domain. Losing it costs a replay. Losing team authority --
topology, routing, filters, renewals, identities -- costs facts no replay can
recover, so the two never share a file, a connection, or a transaction.

Every family here registers where its facts come from, what records how far it
got, how far back that source can be replayed, what refills it, and what it can
still say when the source no longer reaches back far enough. A table with no
answer to those does not belong in this store.
"""

from __future__ import annotations

import sqlite3
import tempfile
import time
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from threading import Lock
from typing import Callable, Iterable, Iterator

from spice.errors import SpiceError
from spice.sqliteconnection import sqlite_connection

PROJECTION_DATABASE_FILENAME = "spiceprojections.sqlite3"
PROJECTION_SQLITE_BUSY_TIMEOUT_MS = 5000
PROJECTION_SCHEMA_VERSION = 1
FIRST_GENERATION = 1
PROJECTION_STATUS_READY = "ready"
PROJECTION_STATUS_REBUILDING = "rebuilding"
PROJECTION_STATUS_STALE = "stale"
PROJECTION_STATUS_UNAVAILABLE = "unavailable"
PROJECTION_STATUS_INCOMPATIBLE = "incompatible"


class ProjectionUnavailableError(SpiceError):
    """A family has no writable/servable generation for this operation."""


@dataclass(frozen=True)
class ProjectionFamily:
    """One set of tables that is dropped, replayed, and published together.

    The tables of a family are halves of one fact: activity counts and the
    checkpoint recording how far they were built cannot survive each other.
    Keeping a surviving half reads as fact what the replay is about to
    contradict, so the unit of reset is the family, never the table.
    """

    name: str
    tables: tuple[str, ...]
    source: str
    cursor: str
    horizon: str
    rebuild: str
    beyond_horizon: str
    recovery_action: str


AGENT_ACTIVITY = ProjectionFamily(
    name="agentActivity",
    tables=("agent_metrics", "agent_metric_buckets", "agent_metric_cursors"),
    source=(
        "driver transcripts, read as typed events through "
        "spice.transcript.reader.TranscriptEventReader"
    ),
    cursor=(
        "agent_metric_cursors: a byte offset per (agent_id, source_path), "
        "carrying the source device and inode that offset counts against; "
        "recovery supplements a servable cursor manifest with transcript paths "
        "discoverable from authority identities"
    ),
    horizon=(
        "the transcript files still on disk; per-bucket counts are pruned at the "
        "metric history retention horizon, and lifetime counters are not"
    ),
    rebuild=(
        "spice.serve.metrics.rebuild_transcript_metrics, which replays every "
        "selected source into an isolated store and atomically publishes the "
        "complete family"
    ),
    beyond_horizon=(
        "counts rebuild from the transcript bytes that remain; activity whose "
        "source file is gone does not come back, and the rebuilt family says so "
        "by starting at the earliest bucket the surviving sources produce"
    ),
    recovery_action="spice serve reset-projections agentActivity",
)

PROJECTION_FAMILIES: tuple[ProjectionFamily, ...] = (AGENT_ACTIVITY,)

PROJECTION_FAMILIES_BY_NAME = {family.name: family for family in PROJECTION_FAMILIES}

PROJECTION_TABLES: tuple[str, ...] = tuple(
    table for family in PROJECTION_FAMILIES for table in family.tables
)

# `projection_generations` is the store's own bookkeeping rather than a family:
# it records which build of each family a reader is looking at. A reset bumps
# the generation in the same transaction that empties the tables, so a reader
# never sees a new generation beside old rows.
PROJECTION_SCHEMA = """
CREATE TABLE IF NOT EXISTS projection_generations (
    family TEXT PRIMARY KEY,
    generation INTEGER NOT NULL,
    updated_at REAL NOT NULL
);
-- Rebuild state is bookkeeping beside the published generation, not a
-- projection family. A failed isolated build leaves `servable = 1` when an
-- older complete generation still exists; a destructive reset or incompatible
-- schema has no published answer and says so explicitly.
CREATE TABLE IF NOT EXISTS projection_status (
    family TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    servable INTEGER NOT NULL,
    last_successful_rebuild REAL,
    freshness REAL,
    retention_floor REAL,
    detail TEXT NOT NULL,
    recovery_action TEXT NOT NULL
);
-- Counted activity carries the source that produced it, so losing one
-- source's checkpoint reverses that source's contribution and leaves every
-- other source -- still covered by its own checkpoint -- standing. Activity
-- counted outside a transcript pass has no source to replay from and holds
-- the empty path. Lane reads sum across sources.
CREATE TABLE IF NOT EXISTS agent_metrics (
    agent_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    source_path TEXT NOT NULL DEFAULT '',
    tool_calls INTEGER NOT NULL DEFAULT 0,
    updated_at REAL NOT NULL,
    PRIMARY KEY (agent_id, team_id, source_path)
);
CREATE TABLE IF NOT EXISTS agent_metric_buckets (
    agent_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    source_path TEXT NOT NULL DEFAULT '',
    bucket_start INTEGER NOT NULL,
    messages INTEGER NOT NULL DEFAULT 0,
    tool_calls INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (agent_id, team_id, source_path, bucket_start)
);
-- A resume checkpoint carries the source's filesystem identity beside its byte
-- offset: a replaced transcript reuses the path, and only device/inode separate
-- a resumable append from a new file whose bytes start over.
CREATE TABLE IF NOT EXISTS agent_metric_cursors (
    agent_id TEXT NOT NULL,
    source_path TEXT NOT NULL,
    offset INTEGER NOT NULL,
    source_device INTEGER,
    source_inode INTEGER,
    updated_at REAL NOT NULL,
    PRIMARY KEY (agent_id, source_path)
);
CREATE INDEX IF NOT EXISTS agent_metric_buckets_by_start
    ON agent_metric_buckets (bucket_start);
"""


@dataclass(frozen=True)
class ProjectionFamilyState:
    """What one family currently holds, for an operator reading diagnostics."""

    family: ProjectionFamily
    generation: int
    updated_at: float
    row_counts: dict[str, int]
    status: str
    servable: bool
    last_successful_rebuild: float | None
    freshness: float | None
    retention_floor: float | None
    detail: str
    recovery_action: str


def projection_database_path() -> Path:
    from spice.tasks import config as task_config

    return task_config.data_dir() / PROJECTION_DATABASE_FILENAME


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
        raise SpiceError("projection schema contains an incomplete SQL statement")
    return tuple(statements)


def _execute_schema_script(connection: sqlite3.Connection, script: str) -> None:
    for statement in _schema_statements(script):
        connection.execute(statement)


def _table_columns(connection: sqlite3.Connection, table: str) -> tuple[str, ...]:
    return tuple(
        str(row[1]) for row in connection.execute(f'PRAGMA table_info("{table}")')
    )


def _canonical_columns() -> dict[str, tuple[str, ...]]:
    """The columns each projection table has when built from the current DDL."""
    probe = sqlite3.connect(":memory:")
    try:
        _execute_schema_script(probe, PROJECTION_SCHEMA)
        return {table: _table_columns(probe, table) for table in PROJECTION_TABLES}
    finally:
        probe.close()


def _bump_generation_locked(
    connection: sqlite3.Connection, family: ProjectionFamily, now: float
) -> None:
    """Publish an emptied family as a build no reader has seen before."""
    connection.execute(
        "UPDATE projection_generations "
        "SET generation = generation + 1, updated_at = ? WHERE family = ?",
        (now, family.name),
    )


def _requested_families(names: Iterable[str]) -> tuple[ProjectionFamily, ...]:
    requested = tuple(str(name or "").strip() for name in names)
    if not requested:
        return PROJECTION_FAMILIES
    unknown = sorted(
        {name for name in requested if name not in PROJECTION_FAMILIES_BY_NAME}
    )
    if unknown:
        known = ", ".join(sorted(PROJECTION_FAMILIES_BY_NAME))
        raise SpiceError(
            f"unknown Serve projection family {', '.join(unknown)}; known: {known}"
        )
    return tuple(PROJECTION_FAMILIES_BY_NAME[name] for name in dict.fromkeys(requested))


class ServeProjectionStore:
    """The disposable half of Serve's storage.

    Creation, drift, reset, rebuild, and corruption here reach nothing but this
    file. The store carries no forward migration ladder on purpose: a shape
    change costs a replay, so a drifted family is dropped and rebuilt rather
    than migrated, and a database written by a newer schema is discarded rather
    than refused. A file that goes missing under a running process is rebuilt
    on the next read for the same reason: losing this store costs a replay, and
    a replay is not something a process has to restart to perform.
    """

    _init_lock = Lock()
    _initialized_files: dict[Path, tuple[int, int]] = {}

    def __init__(self, path: Path | None = None) -> None:
        self.path = path or projection_database_path()

    def _ensure_schema(self) -> None:
        if self._matches_initialized_file():
            return
        with self._init_lock:
            if self._matches_initialized_file():
                return
            try:
                self._open_and_sync_locked()
            except sqlite3.DatabaseError:
                # A file SQLite cannot read is not a dilemma here: every byte in
                # it is replayable, so it is discarded and rebuilt rather than
                # reported. Refusing instead would let a corrupt projection
                # block the reads it exists to accelerate.
                self._discard_file_locked()
                self._open_and_sync_locked(
                    incompatible_detail=(
                        "projection file was unreadable and was recreated"
                    )
                )
            if (identity := self._file_identity()) is not None:
                self._initialized_files[self.path] = identity

    def _matches_initialized_file(self) -> bool:
        """Whether the file on disk is still the one this process synced.

        An operator who deletes a database documented as disposable is owed a
        rebuild on the next read, not a process that answers `no such table`
        until it restarts, so what is remembered is the file rather than the
        path. Establishing that costs one stat, which keeps an unchanged file on
        the fast path instead of repeating the schema pass per connection.
        """
        identity = self._file_identity()
        if identity is None:
            return False
        return self._initialized_files.get(self.path) == identity

    def _file_identity(self) -> tuple[int, int] | None:
        """Device and inode of the database, or None when nothing is there."""
        try:
            status = self.path.stat()
        except OSError:
            return None
        return (status.st_dev, status.st_ino)

    def _open_and_sync_locked(self, *, incompatible_detail: str = "") -> None:
        with sqlite_connection(
            self.path,
            busy_timeout_ms=PROJECTION_SQLITE_BUSY_TIMEOUT_MS,
            ensure_parent=True,
        ) as connection:
            self._sync_schema_locked(
                connection,
                incompatible_detail=incompatible_detail,
            )
            connection.execute("PRAGMA journal_mode = WAL")

    def _discard_file_locked(self) -> None:
        _discard_projection_files(self.path)

    def _sync_schema_locked(
        self,
        connection: sqlite3.Connection,
        *,
        incompatible_detail: str = "",
    ) -> None:
        connection.execute("BEGIN IMMEDIATE")
        stored = int(connection.execute("PRAGMA user_version").fetchone()[0])
        existing_tables = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table' "
                "AND name NOT LIKE 'sqlite_%'"
            )
        }
        discarded_all = bool(existing_tables) and stored != PROJECTION_SCHEMA_VERSION
        if stored != PROJECTION_SCHEMA_VERSION:
            self._drop_all_locked(connection)
            dropped: tuple[ProjectionFamily, ...] = ()
        else:
            dropped = self._drop_drifted_locked(connection)
        _execute_schema_script(connection, PROJECTION_SCHEMA)
        now = time.time()
        for family in PROJECTION_FAMILIES:
            connection.execute(
                "INSERT INTO projection_generations (family, generation, updated_at) "
                "VALUES (?, ?, ?) ON CONFLICT(family) DO NOTHING",
                (family.name, FIRST_GENERATION, now),
            )
            connection.execute(
                "INSERT INTO projection_status "
                "(family, status, servable, last_successful_rebuild, freshness, "
                "retention_floor, detail, recovery_action) "
                "VALUES (?, ?, 1, NULL, NULL, NULL, '', ?) "
                "ON CONFLICT(family) DO NOTHING",
                (
                    family.name,
                    PROJECTION_STATUS_READY,
                    family.recovery_action,
                ),
            )
        # A family discarded for drift is republished exactly as a reset one is,
        # so an operator reading generations sees the rebuild rather than having
        # to infer it from empty tables.
        for family in dropped:
            _bump_generation_locked(connection, family, now)
            _set_family_status_locked(
                connection,
                family,
                status=PROJECTION_STATUS_INCOMPATIBLE,
                servable=False,
                detail=(
                    "projection table shape was incompatible and the family was emptied"
                ),
            )
        if discarded_all or incompatible_detail:
            detail = incompatible_detail or (
                f"projection schema version {stored} was incompatible with "
                f"version {PROJECTION_SCHEMA_VERSION} and was recreated"
            )
            for family in PROJECTION_FAMILIES:
                _set_family_status_locked(
                    connection,
                    family,
                    status=PROJECTION_STATUS_INCOMPATIBLE,
                    servable=False,
                    detail=detail,
                )
        connection.execute(f"PRAGMA user_version = {PROJECTION_SCHEMA_VERSION}")
        connection.commit()

    def _drop_all_locked(self, connection: sqlite3.Connection) -> None:
        """Discard a build this writer has no contract for, whichever way it drifted.

        A version this writer does not recognize -- older, newer, or a
        half-created file -- describes tables it cannot reason about, and every
        one of them is replayable. Dropping them costs a rebuild; keeping them
        would let a stale shape answer a query as if it were current.
        """
        rows = connection.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table' "
            "AND name NOT LIKE 'sqlite_%'"
        ).fetchall()
        for row in rows:
            connection.execute(f'DROP TABLE IF EXISTS "{str(row[0])}"')

    def _drop_drifted_locked(
        self, connection: sqlite3.Connection
    ) -> tuple[ProjectionFamily, ...]:
        """Discard families whose shape no longer matches the current DDL."""
        canonical = _canonical_columns()
        dropped = []
        for family in PROJECTION_FAMILIES:
            drifted = any(
                (live := _table_columns(connection, table)) and live != canonical[table]
                for table in family.tables
            )
            if drifted:
                self._drop_family_locked(connection, family)
                dropped.append(family)
        return tuple(dropped)

    def _drop_family_locked(
        self, connection: sqlite3.Connection, family: ProjectionFamily
    ) -> None:
        for table in family.tables:
            connection.execute(f'DROP TABLE IF EXISTS "{table}"')

    @contextmanager
    def connect(self) -> Iterator[sqlite3.Connection]:
        """Open the physical store for schema, diagnostics, and test tooling.

        Production fact readers and writers use ``read`` and ``write`` below so
        an unavailable generation cannot be mistaken for an empty answer.
        """
        self._ensure_schema()
        with sqlite_connection(
            self.path, busy_timeout_ms=PROJECTION_SQLITE_BUSY_TIMEOUT_MS
        ) as connection:
            connection.row_factory = sqlite3.Row
            yield connection

    @contextmanager
    def read(self, family: ProjectionFamily) -> Iterator[sqlite3.Connection]:
        """Read a complete published generation, including a prior stale one."""
        with self.connect() as connection:
            connection.execute("BEGIN")
            status = _family_status_locked(connection, family)
            if not status["servable"]:
                raise ProjectionUnavailableError(_unavailable_message(family, status))
            yield connection

    @contextmanager
    def _write(self, family: ProjectionFamily) -> Iterator[sqlite3.Connection]:
        """Mutate only the current ready generation.

        A staged rebuild writes to its own fresh store. The published store
        rejects concurrent mutations while rebuilding so the atomic swap cannot
        omit activity that landed after the staging snapshot.
        """
        with self.connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            status = _family_status_locked(connection, family)
            if status["status"] != PROJECTION_STATUS_READY:
                raise ProjectionUnavailableError(_unavailable_message(family, status))
            yield connection

    def _mark_rebuilding(self, family: ProjectionFamily) -> None:
        with self.connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            current = _family_status_locked(connection, family)
            _set_family_status_locked(
                connection,
                family,
                status=PROJECTION_STATUS_REBUILDING,
                servable=bool(current["servable"]),
                detail="isolated rebuild is in progress",
            )

    def _mark_rebuild_failed(
        self, family: ProjectionFamily, error: BaseException
    ) -> None:
        with self.connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            current = _family_status_locked(connection, family)
            servable = bool(current["servable"])
            _set_family_status_locked(
                connection,
                family,
                status=(
                    PROJECTION_STATUS_STALE
                    if servable
                    else PROJECTION_STATUS_UNAVAILABLE
                ),
                servable=servable,
                detail=f"isolated rebuild failed: {error}",
            )

    def _publish_rebuild(
        self,
        family: ProjectionFamily,
        rows: dict[str, tuple[tuple[object, ...], ...]],
        *,
        freshness: float | None,
        retention_floor: float | None,
    ) -> None:
        now = time.time()
        columns = _canonical_columns()
        with self.connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            for table in family.tables:
                connection.execute(f'DELETE FROM "{table}"')
                values = rows[table]
                if values:
                    names = columns[table]
                    placeholders = ",".join("?" for _ in names)
                    connection.executemany(
                        f'INSERT INTO "{table}" '
                        f"({','.join(names)}) VALUES ({placeholders})",
                        values,
                    )
            _bump_generation_locked(connection, family, now)
            connection.execute(
                "UPDATE projection_status SET status = ?, servable = 1, "
                "last_successful_rebuild = ?, freshness = ?, "
                "retention_floor = COALESCE(?, retention_floor), detail = '', "
                "recovery_action = ? WHERE family = ?",
                (
                    PROJECTION_STATUS_READY,
                    now,
                    freshness,
                    retention_floor,
                    family.recovery_action,
                    family.name,
                ),
            )

    def family_states(self) -> tuple[ProjectionFamilyState, ...]:
        with self.connect() as connection:
            generations = {
                str(row["family"]): (int(row["generation"]), float(row["updated_at"]))
                for row in connection.execute(
                    "SELECT family, generation, updated_at FROM projection_generations"
                )
            }
            statuses = {
                str(row["family"]): row
                for row in connection.execute(
                    "SELECT family, status, servable, last_successful_rebuild, "
                    "freshness, retention_floor, detail, recovery_action "
                    "FROM projection_status"
                )
            }
            states = []
            for family in PROJECTION_FAMILIES:
                generation, updated_at = generations.get(
                    family.name, (FIRST_GENERATION, 0.0)
                )
                counts = {
                    table: int(
                        connection.execute(
                            f'SELECT COUNT(*) FROM "{table}"'
                        ).fetchone()[0]
                    )
                    for table in family.tables
                }
                status = statuses[family.name]
                states.append(
                    ProjectionFamilyState(
                        family=family,
                        generation=generation,
                        updated_at=updated_at,
                        row_counts=counts,
                        status=str(status["status"]),
                        servable=bool(status["servable"]),
                        last_successful_rebuild=(
                            None
                            if status["last_successful_rebuild"] is None
                            else float(status["last_successful_rebuild"])
                        ),
                        freshness=(
                            None
                            if status["freshness"] is None
                            else float(status["freshness"])
                        ),
                        retention_floor=(
                            None
                            if status["retention_floor"] is None
                            else float(status["retention_floor"])
                        ),
                        detail=str(status["detail"]),
                        recovery_action=str(status["recovery_action"]),
                    )
                )
        return tuple(states)


def rebuild_projection_family(
    store: ServeProjectionStore,
    family_name: str,
    populate: Callable[[ServeProjectionStore], float | None],
) -> ProjectionFamilyState:
    """Populate an isolated store and atomically publish one complete family.

    Readers keep the prior published generation while ``populate`` runs. A
    failed or interrupted callback never copies staging rows into the live
    file; diagnostics retain either a stale servable generation or an
    explicitly unavailable state.
    """
    family = _requested_families((family_name,))
    if len(family) != 1:
        raise SpiceError("projection rebuild requires exactly one family")
    selected = family[0]
    store._mark_rebuilding(selected)
    stage_path = _staging_path(store.path, selected)
    stage = ServeProjectionStore(stage_path)
    try:
        freshness = populate(stage)
        rows = _family_rows(stage, selected)
        staged_state = next(
            state for state in stage.family_states() if state.family == selected
        )
        store._publish_rebuild(
            selected,
            rows,
            freshness=freshness,
            retention_floor=staged_state.retention_floor,
        )
    except BaseException as exc:
        store._mark_rebuild_failed(selected, exc)
        raise
    finally:
        ServeProjectionStore._initialized_files.pop(stage_path, None)
        _discard_projection_files(stage_path)
    return next(state for state in store.family_states() if state.family == selected)


def _set_family_status_locked(
    connection: sqlite3.Connection,
    family: ProjectionFamily,
    *,
    status: str,
    servable: bool,
    detail: str,
) -> None:
    connection.execute(
        "UPDATE projection_status SET status = ?, servable = ?, detail = ?, "
        "recovery_action = ? WHERE family = ?",
        (
            status,
            int(servable),
            detail,
            family.recovery_action,
            family.name,
        ),
    )


def _family_status_locked(
    connection: sqlite3.Connection, family: ProjectionFamily
) -> sqlite3.Row:
    row = connection.execute(
        "SELECT status, servable, detail, recovery_action "
        "FROM projection_status WHERE family = ?",
        (family.name,),
    ).fetchone()
    if row is None:
        raise SpiceError(f"projection family status is missing: {family.name}")
    return row


def _unavailable_message(family: ProjectionFamily, status: sqlite3.Row) -> str:
    detail = str(status["detail"] or status["status"])
    recovery = str(status["recovery_action"] or family.recovery_action)
    return (
        f"Serve projection {family.name} is {status['status']}: {detail}; "
        f"recover with `{recovery}`"
    )


def _staging_path(path: Path, family: ProjectionFamily) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    handle = tempfile.NamedTemporaryFile(
        prefix=f".{path.name}.{family.name}.",
        suffix=".rebuild",
        dir=path.parent,
        delete=False,
    )
    handle.close()
    return Path(handle.name)


def _discard_projection_files(path: Path) -> None:
    for candidate in (
        path,
        path.with_name(f"{path.name}-wal"),
        path.with_name(f"{path.name}-shm"),
    ):
        candidate.unlink(missing_ok=True)


def _family_rows(
    store: ServeProjectionStore, family: ProjectionFamily
) -> dict[str, tuple[tuple[object, ...], ...]]:
    columns = _canonical_columns()
    with store.read(family) as connection:
        return {
            table: tuple(
                tuple(row[column] for column in columns[table])
                for row in connection.execute(
                    f'SELECT * FROM "{table}" ORDER BY '
                    + ", ".join(f'"{column}"' for column in columns[table])
                )
            )
            for table in family.tables
        }
