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
import time
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from threading import Lock
from typing import Iterable, Iterator

from spice.errors import SpiceError
from spice.sqliteconnection import sqlite_connection

PROJECTION_DATABASE_FILENAME = "spiceprojections.sqlite3"
PROJECTION_SQLITE_BUSY_TIMEOUT_MS = 5000
PROJECTION_SCHEMA_VERSION = 1
FIRST_GENERATION = 1


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


AGENT_ACTIVITY = ProjectionFamily(
    name="agentActivity",
    tables=("agent_metrics", "agent_metric_buckets", "agent_metric_cursors"),
    source=(
        "driver transcripts, read as typed events through "
        "spice.transcript.reader.TranscriptEventReader"
    ),
    cursor=(
        "agent_metric_cursors: a byte offset per (agent_id, source_path), "
        "carrying the source device and inode that offset counts against"
    ),
    horizon=(
        "the transcript files still on disk; per-bucket counts are pruned at the "
        "metric history retention horizon, and lifetime counters are not"
    ),
    rebuild=(
        "spice.serve.metrics.record_transcript_metrics_for_agent, which resumes "
        "each source from its checkpoint and therefore from its first byte once "
        "reset removed the checkpoint"
    ),
    beyond_horizon=(
        "counts rebuild from the transcript bytes that remain; activity whose "
        "source file is gone does not come back, and the rebuilt family says so "
        "by starting at the earliest bucket the surviving sources produce"
    ),
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
                self._open_and_sync_locked()
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

    def _open_and_sync_locked(self) -> None:
        with sqlite_connection(
            self.path,
            busy_timeout_ms=PROJECTION_SQLITE_BUSY_TIMEOUT_MS,
            ensure_parent=True,
        ) as connection:
            self._sync_schema_locked(connection)
            connection.execute("PRAGMA journal_mode = WAL")

    def _discard_file_locked(self) -> None:
        for path in (
            self.path,
            self.path.with_name(f"{self.path.name}-wal"),
            self.path.with_name(f"{self.path.name}-shm"),
        ):
            path.unlink(missing_ok=True)

    def _sync_schema_locked(self, connection: sqlite3.Connection) -> None:
        connection.execute("BEGIN IMMEDIATE")
        stored = int(connection.execute("PRAGMA user_version").fetchone()[0])
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
        # A family discarded for drift is republished exactly as a reset one is,
        # so an operator reading generations sees the rebuild rather than having
        # to infer it from empty tables.
        for family in dropped:
            _bump_generation_locked(connection, family, now)
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
        self._ensure_schema()
        with sqlite_connection(
            self.path, busy_timeout_ms=PROJECTION_SQLITE_BUSY_TIMEOUT_MS
        ) as connection:
            connection.row_factory = sqlite3.Row
            yield connection

    def reset(self, *family_names: str) -> tuple[ProjectionFamily, ...]:
        """Empty each named family and publish it as a new generation.

        Emptying the rows and bumping the generation are one transaction, so a
        concurrent reader sees either the whole previous build or an empty new
        one, never a generation stamped over surviving rows. Running it twice
        empties nothing the second time and still advances the generation, which
        is what makes a retried reset safe after an interrupted one.
        """
        families = _requested_families(family_names)
        now = time.time()
        with self.connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            for family in families:
                for table in family.tables:
                    connection.execute(f'DELETE FROM "{table}"')
                _bump_generation_locked(connection, family, now)
        return families

    def family_states(self) -> tuple[ProjectionFamilyState, ...]:
        with self.connect() as connection:
            generations = {
                str(row["family"]): (int(row["generation"]), float(row["updated_at"]))
                for row in connection.execute(
                    "SELECT family, generation, updated_at FROM projection_generations"
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
                states.append(
                    ProjectionFamilyState(
                        family=family,
                        generation=generation,
                        updated_at=updated_at,
                        row_counts=counts,
                    )
                )
        return tuple(states)
