"""Executable proof for immutable actor-lineage metric projections."""

from __future__ import annotations

import sqlite3

FIRST_SESSION_BUCKET = 60
FIRST_RENEWAL_TS = 120
SECOND_SESSION_BUCKET = 180
LATEST_RENEWAL_TS = 240
CURRENT_SESSION_BUCKET = 300
SERIES_END_TS = 360


def test_actor_lineage_projection_derives_lineage_and_per_session_views():
    connection = _prototype_connection()
    _seed_actor_lineage_fixture(connection)

    lineage_points = _lineage_activity(
        connection,
        lineage_id="lineage-a",
        start=0,
        end=SERIES_END_TS,
    )
    per_session_start, per_session_points = _per_session_activity(
        connection,
        actor_id="actor-current",
        start=0,
        end=SERIES_END_TS,
    )

    assert lineage_points == [
        {"bucketStart": FIRST_SESSION_BUCKET, "messages": 1},
        {"bucketStart": SECOND_SESSION_BUCKET, "messages": 1},
        {"bucketStart": CURRENT_SESSION_BUCKET, "messages": 1},
    ]
    assert per_session_start == LATEST_RENEWAL_TS
    assert per_session_points == [
        {"bucketStart": CURRENT_SESSION_BUCKET, "messages": 1},
    ]


def _prototype_connection() -> sqlite3.Connection:
    connection = sqlite3.connect(":memory:")
    connection.row_factory = sqlite3.Row
    connection.executescript(
        """
        CREATE TABLE actor_lineage (
            lineage_id TEXT NOT NULL,
            actor_id TEXT NOT NULL PRIMARY KEY,
            valid_from REAL NOT NULL,
            valid_to REAL,
            session_start REAL NOT NULL
        );
        CREATE TABLE activity_facts (
            actor_id TEXT NOT NULL,
            bucket_start INTEGER NOT NULL,
            messages INTEGER NOT NULL,
            PRIMARY KEY (actor_id, bucket_start)
        );
        """
    )
    return connection


def _seed_actor_lineage_fixture(connection: sqlite3.Connection) -> None:
    connection.executemany(
        "INSERT INTO actor_lineage "
        "(lineage_id, actor_id, valid_from, valid_to, session_start) "
        "VALUES (?, ?, ?, ?, ?)",
        [
            ("lineage-a", "actor-original", 0, FIRST_RENEWAL_TS, 0),
            (
                "lineage-a",
                "actor-renewed",
                FIRST_RENEWAL_TS,
                LATEST_RENEWAL_TS,
                FIRST_RENEWAL_TS,
            ),
            (
                "lineage-a",
                "actor-current",
                LATEST_RENEWAL_TS,
                None,
                LATEST_RENEWAL_TS,
            ),
        ],
    )
    connection.executemany(
        "INSERT INTO activity_facts (actor_id, bucket_start, messages) "
        "VALUES (?, ?, ?)",
        [
            ("actor-original", FIRST_SESSION_BUCKET, 1),
            ("actor-renewed", SECOND_SESSION_BUCKET, 1),
            ("actor-current", CURRENT_SESSION_BUCKET, 1),
        ],
    )


def _lineage_activity(
    connection: sqlite3.Connection,
    *,
    lineage_id: str,
    start: float,
    end: float,
) -> list[dict[str, int]]:
    rows = connection.execute(
        "SELECT f.bucket_start, SUM(f.messages) AS messages "
        "FROM activity_facts AS f "
        "JOIN actor_lineage AS l ON l.actor_id = f.actor_id "
        "WHERE l.lineage_id = ? "
        "AND f.bucket_start >= ? "
        "AND f.bucket_start <= ? "
        "AND f.bucket_start >= l.valid_from "
        "AND (l.valid_to IS NULL OR f.bucket_start < l.valid_to) "
        "GROUP BY f.bucket_start "
        "ORDER BY f.bucket_start",
        (lineage_id, start, end),
    ).fetchall()
    return [
        {
            "bucketStart": int(row["bucket_start"]),
            "messages": int(row["messages"] or 0),
        }
        for row in rows
    ]


def _per_session_activity(
    connection: sqlite3.Connection,
    *,
    actor_id: str,
    start: float,
    end: float,
) -> tuple[float, list[dict[str, int]]]:
    row = connection.execute(
        "SELECT lineage_id, session_start FROM actor_lineage WHERE actor_id = ?",
        (actor_id,),
    ).fetchone()
    assert row is not None
    effective_start = max(start, float(row["session_start"] or 0.0))
    return effective_start, _lineage_activity(
        connection,
        lineage_id=str(row["lineage_id"]),
        start=effective_start,
        end=end,
    )
