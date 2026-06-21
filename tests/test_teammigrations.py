import sqlite3

from spice.serve.teams import ServeTeamStore

MIGRATED_POSITION_SENTINEL = 99

OLD_TEAM_SCHEMA = """
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
CREATE TABLE IF NOT EXISTS team_merge_subgroups (
    parent_team_id TEXT NOT NULL,
    child_team_id TEXT NOT NULL,
    merged_revision INTEGER NOT NULL,
    agent_ids TEXT NOT NULL,
    created_at REAL NOT NULL,
    restored_revision INTEGER,
    PRIMARY KEY (parent_team_id, child_team_id, merged_revision)
);
CREATE TABLE IF NOT EXISTS renewals (
    agent_id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL,
    state TEXT NOT NULL,
    ancestor_thread_id TEXT NOT NULL,
    successor_agent_id TEXT NOT NULL DEFAULT '',
    successor_thread_id TEXT NOT NULL DEFAULT '',
    team_slot INTEGER,
    predecessor_identity TEXT NOT NULL DEFAULT '{}',
    successor_identity TEXT NOT NULL DEFAULT '{}',
    revision INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_identities (
    actor_id TEXT PRIMARY KEY,
    target_id TEXT NOT NULL DEFAULT '',
    thread_id TEXT NOT NULL DEFAULT '',
    actual_driver TEXT NOT NULL DEFAULT '',
    actual_model TEXT NOT NULL DEFAULT '',
    actual_effort TEXT NOT NULL DEFAULT '',
    actual_service_tier TEXT NOT NULL DEFAULT '',
    desired_driver TEXT NOT NULL DEFAULT '',
    desired_model TEXT NOT NULL DEFAULT '',
    desired_effort TEXT NOT NULL DEFAULT '',
    transcript_owner TEXT NOT NULL DEFAULT '',
    renewal_state TEXT NOT NULL DEFAULT '',
    renewal_ancestor_thread_id TEXT NOT NULL DEFAULT '',
    renewal_successor_thread_id TEXT NOT NULL DEFAULT '',
    renewal_revision INTEGER NOT NULL DEFAULT 0,
    updated_at REAL NOT NULL
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


def _schema_shape(
    connection: sqlite3.Connection,
) -> dict[str, list[tuple[object, ...]]]:
    table_rows = connection.execute(
        "SELECT name FROM sqlite_master "
        "WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
    ).fetchall()
    return {
        str(row[0]): [
            tuple(info) for info in connection.execute(f"PRAGMA table_info({row[0]})")
        ]
        for row in table_rows
    }


def test_team_metric_model_migration_drops_tables_and_backfills_position(tmp_path):
    path = tmp_path / "teams.sqlite3"
    with sqlite3.connect(path) as connection:
        connection.executescript(OLD_TEAM_SCHEMA)
        connection.executemany(
            "INSERT INTO teams "
            "(team_id, status, created_at, revision, lifetime, speech_mode, "
            "selected_view) VALUES (?, 'open', ?, 0, 'Drive', 'speak', 'compose')",
            (("team-a", 1.0), ("team-b", 2.0)),
        )
        connection.executemany(
            "INSERT INTO memberships (team_id, agent_id, joined_at) VALUES (?, ?, ?)",
            (
                ("team-a", "agent-c", 30.0),
                ("team-a", "agent-a", 10.0),
                ("team-a", "agent-b", 30.0),
                ("team-b", "agent-z", 5.0),
                ("team-b", "agent-y", 5.0),
            ),
        )
        connection.execute(
            "INSERT INTO team_agent_history VALUES (?, ?, ?, ?)",
            ("team-a", "agent-a", 1.0, 2.0),
        )
        connection.execute(
            "INSERT INTO team_agent_metrics VALUES (?, ?, ?, ?, ?, ?)",
            ("team-a", "agent-a", 1, 2, 3, 4.0),
        )
        connection.execute(
            "INSERT INTO team_agent_metric_buckets VALUES (?, ?, ?, ?)",
            ("team-a", "agent-a", 60, 1),
        )

    store = ServeTeamStore(path=path)
    with store.connect() as connection:
        migrated_shape = _schema_shape(connection)
        position_rows = connection.execute(
            "SELECT team_id, agent_id, position FROM memberships "
            "ORDER BY team_id, position"
        ).fetchall()
        tables = set(migrated_shape)

    fresh_path = tmp_path / "fresh.sqlite3"
    with ServeTeamStore(path=fresh_path).connect() as connection:
        fresh_shape = _schema_shape(connection)

    assert "team_agent_history" not in tables
    assert "team_agent_metrics" not in tables
    assert "team_agent_metric_buckets" not in tables
    assert [
        (row["team_id"], row["agent_id"], row["position"]) for row in position_rows
    ] == [
        ("team-a", "agent-a", 0),
        ("team-a", "agent-b", 1),
        ("team-a", "agent-c", 2),
        ("team-b", "agent-y", 0),
        ("team-b", "agent-z", 1),
    ]
    assert migrated_shape == fresh_shape

    with store.connect() as connection:
        connection.execute(
            "UPDATE memberships SET position = ? "
            "WHERE team_id = 'team-a' AND agent_id = 'agent-a'",
            (MIGRATED_POSITION_SENTINEL,),
        )
    with store.connect() as connection:
        assert (
            connection.execute(
                "SELECT position FROM memberships "
                "WHERE team_id = 'team-a' AND agent_id = 'agent-a'"
            ).fetchone()[0]
            == MIGRATED_POSITION_SENTINEL
        )
