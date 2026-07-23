"""Durable event store for maxim reminder metrics."""

from __future__ import annotations

import sqlite3
import time
from dataclasses import dataclass
from pathlib import Path
from threading import Lock
from typing import Iterable

from spice.errors import SpiceError
from spice.paths import shared_state_path
from spice.sqliteconnection import sqlite_connection

MAXIM_METRICS_DATABASE_FILENAME = "spicemaxims.sqlite3"
MAXIM_METRICS_DATA_SUBDIR = "data"
MAXIM_METRICS_SQLITE_BUSY_TIMEOUT_MS = 5000
MAXIM_RECURRENCE_HORIZON_SECONDS = 24 * 60 * 60

MAXIM_EVENT_FIRE = "fire"
MAXIM_EVENT_JUDGED_CONFIRMED = "judged_confirmed"
MAXIM_EVENT_JUDGED_REJECTED = "judged_rejected"
MAXIM_EVENT_GATE_SUPPRESSED = "gate_suppressed"
MAXIM_EVENT_PUBLISHED = "published"
MAXIM_EVENT_TYPES = frozenset(
    {
        MAXIM_EVENT_FIRE,
        MAXIM_EVENT_JUDGED_CONFIRMED,
        MAXIM_EVENT_JUDGED_REJECTED,
        MAXIM_EVENT_GATE_SUPPRESSED,
        MAXIM_EVENT_PUBLISHED,
    }
)

MAXIM_METRICS_TABLE_SQL = """
CREATE TABLE IF NOT EXISTS maxim_metric_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  occurred_at REAL NOT NULL,
  event_type TEXT NOT NULL,
  bag_name TEXT NOT NULL,
  driver_name TEXT NOT NULL,
  thread_id TEXT NOT NULL DEFAULT '',
  trigger_family TEXT NOT NULL DEFAULT '',
  statement TEXT NOT NULL DEFAULT '',
  reminder_key TEXT NOT NULL DEFAULT '',
  reminder_body TEXT NOT NULL DEFAULT ''
);
"""
MAXIM_METRICS_EVENT_INDEX_SQL = """
CREATE INDEX IF NOT EXISTS maxim_metric_events_lookup_idx
  ON maxim_metric_events(bag_name, driver_name, event_type, occurred_at);
"""
MAXIM_METRICS_RECURRENCE_INDEX_SQL = """
CREATE INDEX IF NOT EXISTS maxim_metric_events_recurrence_idx
  ON maxim_metric_events(trigger_family, driver_name, occurred_at);
"""
# Supports the bounded per-command working-state read: the leading event_type
# equality plus the ordered (occurred_at, id) tail lets the most-recent fire
# lookup seek the fire partition and take one row instead of scanning the table.
MAXIM_METRICS_FIRE_RECENCY_INDEX_SQL = """
CREATE INDEX IF NOT EXISTS maxim_metric_events_fire_recency_idx
  ON maxim_metric_events(event_type, occurred_at, id);
"""

# Schema is initialized once per database path per process. The full DDL sweep
# (CREATE TABLE + CREATE INDEX x3) previously ran on every connection -- reads
# included -- taking a write lock each time. Under the concurrent lanes that
# append maxim events forever, that serialized every access on the un-retryable
# RESERVED->EXCLUSIVE promotion the default rollback journal refuses to wait
# out, so a per-command working-state read could raise "database is locked".
# Initializing once and opening WAL (mirroring ServeTeamStore and the ack-state
# store) lets a reader observe committed rows while the single writer holds the
# write lock.
_SCHEMA_INIT_LOCK = Lock()
_INITIALIZED_PATHS: set[Path] = set()


@dataclass(frozen=True)
class MaximMetricEventWrite:
    event_type: str
    bag_name: str
    driver_name: str
    thread_id: str = ""
    trigger_family: str = ""
    statement: str = ""
    reminder_key: str = ""
    reminder_body: str = ""


@dataclass(frozen=True)
class MaximMetricRecord:
    id: int
    occurred_at: float
    event_type: str
    bag_name: str
    driver_name: str
    thread_id: str
    trigger_family: str
    statement: str
    reminder_key: str
    reminder_body: str


@dataclass(frozen=True)
class MaximMetricCounts:
    bag_name: str
    driver_name: str
    thread_id: str
    fire_count: int
    judged_confirmed_count: int
    judged_rejected_count: int
    gate_suppressed_count: int
    published_count: int


@dataclass(frozen=True)
class MaximRecurrenceCounts:
    bag_name: str
    driver_name: str
    thread_id: str
    trigger_family: str
    recurrence_count: int


@dataclass(frozen=True)
class MaximRecurrenceInput:
    bag_name: str
    driver_name: str
    thread_id: str
    trigger_family: str
    event_type: str
    occurred_at: float
    statement: str
    reminder_key: str
    reminder_body: str


def maxim_metrics_database_path(repo_root: str | Path) -> Path:
    return shared_state_path(
        Path(repo_root),
        Path(MAXIM_METRICS_DATA_SUBDIR) / MAXIM_METRICS_DATABASE_FILENAME,
    )


def record_maxim_metric_events(
    repo_root: str | Path,
    events: Iterable[MaximMetricEventWrite],
    *,
    now: float | None = None,
) -> list[int]:
    rows = [_event_row(event, now=now) for event in events]
    if not rows:
        return []
    path = maxim_metrics_database_path(repo_root)
    _ensure_schema_once(path)
    ids: list[int] = []
    with sqlite_connection(
        path,
        busy_timeout_ms=MAXIM_METRICS_SQLITE_BUSY_TIMEOUT_MS,
        wal=True,
    ) as connection:
        for row in rows:
            cursor = connection.execute(
                """
                INSERT INTO maxim_metric_events
                  (occurred_at, event_type, bag_name, driver_name, thread_id,
                   trigger_family, statement, reminder_key, reminder_body)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                row,
            )
            lastrowid = cursor.lastrowid
            if lastrowid is None:
                raise SpiceError("maxim metric event insert did not return an id")
            ids.append(lastrowid)
    return ids


def latest_fire_bag_name(repo_root: str | Path) -> str:
    """Return the most-recent fire event's bag name, or '' when none exists.

    The per-command working-state line reads this on every command. A single
    LIMIT 1 query over the fire-recency index returns at most one row -- it never
    materializes the full event table -- and opens WAL so it observes committed
    rows without contending on a concurrent writer's lock. The (occurred_at DESC,
    id DESC) order reproduces the reversed full-table scan it replaces exactly.
    """
    path = maxim_metrics_database_path(repo_root)
    if not path.is_file():
        return ""
    with sqlite_connection(path, wal=True) as connection:
        row = connection.execute(
            """
            SELECT bag_name
            FROM maxim_metric_events
            WHERE event_type = ?
            ORDER BY occurred_at DESC, id DESC
            LIMIT 1
            """,
            (MAXIM_EVENT_FIRE,),
        ).fetchone()
    return str(row[0]) if row is not None else ""


def maxim_metric_records(repo_root: str | Path) -> list[MaximMetricRecord]:
    path = maxim_metrics_database_path(repo_root)
    if not path.is_file():
        return []
    with sqlite_connection(path, wal=True) as connection:
        rows = connection.execute(
            """
            SELECT id, occurred_at, event_type, bag_name, driver_name, thread_id,
                   trigger_family, statement, reminder_key, reminder_body
            FROM maxim_metric_events
            ORDER BY occurred_at ASC, id ASC
            """
        ).fetchall()
    return [_record_from_row(row) for row in rows]


def maxim_metric_counts(repo_root: str | Path) -> list[MaximMetricCounts]:
    path = maxim_metrics_database_path(repo_root)
    if not path.is_file():
        return []
    with sqlite_connection(path, wal=True) as connection:
        rows = connection.execute(
            """
            SELECT
              bag_name,
              driver_name,
              thread_id,
              SUM(CASE WHEN event_type = ? THEN 1 ELSE 0 END) AS fire_count,
              SUM(CASE WHEN event_type = ? THEN 1 ELSE 0 END)
                AS judged_confirmed_count,
              SUM(CASE WHEN event_type = ? THEN 1 ELSE 0 END)
                AS judged_rejected_count,
              SUM(CASE WHEN event_type = ? THEN 1 ELSE 0 END)
                AS gate_suppressed_count,
              SUM(CASE WHEN event_type = ? THEN 1 ELSE 0 END) AS published_count
            FROM maxim_metric_events
            GROUP BY bag_name, driver_name, thread_id
            ORDER BY bag_name ASC, driver_name ASC, thread_id ASC
            """,
            (
                MAXIM_EVENT_FIRE,
                MAXIM_EVENT_JUDGED_CONFIRMED,
                MAXIM_EVENT_JUDGED_REJECTED,
                MAXIM_EVENT_GATE_SUPPRESSED,
                MAXIM_EVENT_PUBLISHED,
            ),
        ).fetchall()
    return [
        MaximMetricCounts(
            bag_name=str(row[0]),
            driver_name=str(row[1]),
            thread_id=str(row[2]),
            fire_count=int(row[3]),
            judged_confirmed_count=int(row[4]),
            judged_rejected_count=int(row[5]),
            gate_suppressed_count=int(row[6]),
            published_count=int(row[7]),
        )
        for row in rows
    ]


def maxim_recurrence_inputs(repo_root: str | Path) -> list[MaximRecurrenceInput]:
    path = maxim_metrics_database_path(repo_root)
    if not path.is_file():
        return []
    with sqlite_connection(path, wal=True) as connection:
        rows = connection.execute(
            """
            SELECT bag_name, driver_name, thread_id, trigger_family, event_type,
                   occurred_at, statement, reminder_key, reminder_body
            FROM maxim_metric_events
            WHERE trigger_family != '' OR reminder_key != '' OR reminder_body != ''
            ORDER BY occurred_at ASC, id ASC
            """
        ).fetchall()
    return [
        MaximRecurrenceInput(
            bag_name=str(row[0]),
            driver_name=str(row[1]),
            thread_id=str(row[2]),
            trigger_family=str(row[3]),
            event_type=str(row[4]),
            occurred_at=float(row[5]),
            statement=str(row[6]),
            reminder_key=str(row[7]),
            reminder_body=str(row[8]),
        )
        for row in rows
    ]


def maxim_recurrence_counts(
    repo_root: str | Path,
    *,
    horizon_seconds: float = MAXIM_RECURRENCE_HORIZON_SECONDS,
) -> list[MaximRecurrenceCounts]:
    horizon = max(0.0, float(horizon_seconds))
    inputs = maxim_recurrence_inputs(repo_root)
    published = [item for item in inputs if item.event_type == MAXIM_EVENT_PUBLISHED]
    if not published:
        return []
    recurrence_indexes_by_key: dict[tuple[str, str, str, str], set[int]] = {}
    for index, item in enumerate(inputs):
        if item.event_type != MAXIM_EVENT_FIRE:
            continue
        key = (
            item.bag_name,
            item.driver_name,
            item.thread_id,
            item.trigger_family,
        )
        if any(_fire_recurred_after(item, reminder, horizon) for reminder in published):
            recurrence_indexes_by_key.setdefault(key, set()).add(index)
    return [
        MaximRecurrenceCounts(
            bag_name=key[0],
            driver_name=key[1],
            thread_id=key[2],
            trigger_family=key[3],
            recurrence_count=len(indexes),
        )
        for key, indexes in sorted(recurrence_indexes_by_key.items())
    ]


def _ensure_schema_once(path: Path) -> None:
    """Create the schema at most once per database path per process.

    Guarded by a lock and a per-path record so the DDL never re-runs on the hot
    read or write path, and the initializing connection opens WAL so the journal
    mode persists for every later reader and writer. Mirrors ServeTeamStore and
    the ack-state store.
    """
    if path in _INITIALIZED_PATHS:
        return
    with _SCHEMA_INIT_LOCK:
        if path in _INITIALIZED_PATHS:
            return
        with sqlite_connection(
            path,
            busy_timeout_ms=MAXIM_METRICS_SQLITE_BUSY_TIMEOUT_MS,
            wal=True,
            ensure_parent=True,
        ) as connection:
            _ensure_schema(connection)
        _INITIALIZED_PATHS.add(path)


def _ensure_schema(connection: sqlite3.Connection) -> None:
    connection.execute(MAXIM_METRICS_TABLE_SQL)
    connection.execute(MAXIM_METRICS_EVENT_INDEX_SQL)
    connection.execute(MAXIM_METRICS_RECURRENCE_INDEX_SQL)
    connection.execute(MAXIM_METRICS_FIRE_RECENCY_INDEX_SQL)


def _event_row(
    event: MaximMetricEventWrite, *, now: float | None
) -> tuple[float, str, str, str, str, str, str, str, str]:
    event_type = _normalize_event_type(event.event_type)
    bag_name = _required_label(event.bag_name, "bag_name")
    driver_name = _required_label(event.driver_name, "driver_name")
    trigger_family = _optional_label(event.trigger_family) or bag_name
    return (
        float(time.time() if now is None else now),
        event_type,
        bag_name,
        driver_name,
        _optional_label(event.thread_id),
        trigger_family,
        str(event.statement or ""),
        _optional_label(event.reminder_key),
        str(event.reminder_body or ""),
    )


def _record_from_row(row: tuple[object, ...]) -> MaximMetricRecord:
    return MaximMetricRecord(
        id=_as_int(row[0]),
        occurred_at=_as_float(row[1]),
        event_type=_normalize_event_type(str(row[2])),
        bag_name=str(row[3]),
        driver_name=str(row[4]),
        thread_id=str(row[5]),
        trigger_family=str(row[6]),
        statement=str(row[7]),
        reminder_key=str(row[8]),
        reminder_body=str(row[9]),
    )


def _fire_recurred_after(
    fire: MaximRecurrenceInput, reminder: MaximRecurrenceInput, horizon_seconds: float
) -> bool:
    return (
        reminder.event_type == MAXIM_EVENT_PUBLISHED
        and fire.bag_name == reminder.bag_name
        and fire.driver_name == reminder.driver_name
        and fire.thread_id == reminder.thread_id
        and fire.trigger_family == reminder.trigger_family
        and reminder.occurred_at < fire.occurred_at
        and fire.occurred_at - reminder.occurred_at <= horizon_seconds
    )


def _normalize_event_type(value: str) -> str:
    clean = str(value or "").strip().lower().replace("-", "_")
    if clean not in MAXIM_EVENT_TYPES:
        expected = ", ".join(sorted(MAXIM_EVENT_TYPES))
        raise SpiceError(
            f"unknown maxim metric event type {value!r}; expected {expected}"
        )
    return clean


def _required_label(value: str, field: str) -> str:
    clean = _optional_label(value)
    if not clean:
        raise SpiceError(f"maxim metric {field} must be non-empty")
    return clean


def _optional_label(value: str) -> str:
    return " ".join(str(value or "").strip().split())


def _as_int(value: object) -> int:
    return int(str(value))


def _as_float(value: object) -> float:
    return float(str(value))
