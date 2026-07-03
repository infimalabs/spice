"""Durable event store for maxim reminder metrics."""

from __future__ import annotations

import sqlite3
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

from spice.errors import SpiceError
from spice.paths import git_common_dir
from spice.tasks.config import SHARED_DIR

MAXIM_METRICS_DATABASE_FILENAME = "spicemaxims.sqlite3"
MAXIM_METRICS_DATA_SUBDIR = "data"
MAXIM_METRICS_SQLITE_BUSY_TIMEOUT_MS = 5000

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
    fire_count: int
    judged_confirmed_count: int
    judged_rejected_count: int
    gate_suppressed_count: int
    published_count: int


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
    common = git_common_dir(Path(repo_root))
    return (
        common
        / SHARED_DIR
        / MAXIM_METRICS_DATA_SUBDIR
        / MAXIM_METRICS_DATABASE_FILENAME
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
    path.parent.mkdir(parents=True, exist_ok=True)
    ids: list[int] = []
    with sqlite3.connect(path) as connection:
        _ensure_schema(connection)
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


def maxim_metric_records(repo_root: str | Path) -> list[MaximMetricRecord]:
    path = maxim_metrics_database_path(repo_root)
    if not path.is_file():
        return []
    with sqlite3.connect(path) as connection:
        _ensure_schema(connection)
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
    with sqlite3.connect(path) as connection:
        _ensure_schema(connection)
        rows = connection.execute(
            """
            SELECT
              bag_name,
              driver_name,
              SUM(CASE WHEN event_type = ? THEN 1 ELSE 0 END) AS fire_count,
              SUM(CASE WHEN event_type = ? THEN 1 ELSE 0 END)
                AS judged_confirmed_count,
              SUM(CASE WHEN event_type = ? THEN 1 ELSE 0 END)
                AS judged_rejected_count,
              SUM(CASE WHEN event_type = ? THEN 1 ELSE 0 END)
                AS gate_suppressed_count,
              SUM(CASE WHEN event_type = ? THEN 1 ELSE 0 END) AS published_count
            FROM maxim_metric_events
            GROUP BY bag_name, driver_name
            ORDER BY bag_name ASC, driver_name ASC
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
            fire_count=int(row[2]),
            judged_confirmed_count=int(row[3]),
            judged_rejected_count=int(row[4]),
            gate_suppressed_count=int(row[5]),
            published_count=int(row[6]),
        )
        for row in rows
    ]


def maxim_recurrence_inputs(repo_root: str | Path) -> list[MaximRecurrenceInput]:
    path = maxim_metrics_database_path(repo_root)
    if not path.is_file():
        return []
    with sqlite3.connect(path) as connection:
        _ensure_schema(connection)
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


def _ensure_schema(connection: sqlite3.Connection) -> None:
    connection.execute(f"PRAGMA busy_timeout = {MAXIM_METRICS_SQLITE_BUSY_TIMEOUT_MS}")
    connection.execute(MAXIM_METRICS_TABLE_SQL)
    connection.execute(MAXIM_METRICS_EVENT_INDEX_SQL)
    connection.execute(MAXIM_METRICS_RECURRENCE_INDEX_SQL)


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
