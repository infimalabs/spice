"""SQLite schema and defaults for serve team storage."""

TEAM_DATABASE_FILENAME = "spiceteams.sqlite3"
DEFAULT_LIFETIME = "Drive"
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
# Generous horizon for the high-growth per-minute observation series.
# Directive retention belongs to the canonical steering/ACK plane.
METRIC_HISTORY_RETENTION_SECONDS = 30 * 24 * 60 * 60
DEFAULT_STUCK_THRESHOLD_SECONDS = 15 * 60

TEAM_AUTHORITY_SCHEMA_VERSION = 2

TEAM_AUTHORITY_TABLES = frozenset(
    {
        "events",
        "global_settings",
        "teams",
        "memberships",
        "team_task_filters",
        "team_merge_subgroups",
        "renewals",
        "agent_identities",
    }
)

# Authority migrations are append-only and keyed by their destination version.
# A version's canonical shape is whatever applying migrations 1..N produces, so
# no second statement of a shape exists anywhere to drift from the migration
# that builds it. That drift is not hypothetical: version 1 was minted with an
# `actual_service_tier` column and later lost it in place, because the shape
# contract was an alias for the writer's current DDL rather than a record of
# what version 1 built. One version then described two shapes, every store
# stamped 1 became unidentifiable, and a shared authority database was edited by
# hand to get the fleet moving again.
#
# So an entry here is history, not source: it is the text that already ran
# against databases in the field, and editing one silently changes what an
# already-stamped version means. A future authority change adds the next
# integer and its own forward migration instead.
# `tests/test_teamschema.py` pins each version's derived shape to a digest, so
# editing an entry below fails rather than rewriting the past.
#
# Rebuildable projections live in their own database (spice.serve.team
# .projection) and cannot reach this version, this file, or this connection.
TEAM_AUTHORITY_MIGRATIONS = {
    1: """
CREATE TABLE IF NOT EXISTS events (
    revision INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL,
    kind TEXT NOT NULL,
    team_id TEXT NOT NULL,
    payload TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS global_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at REAL NOT NULL,
    revision INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS teams (
    team_id TEXT PRIMARY KEY,
    status TEXT NOT NULL DEFAULT 'open',
    created_at REAL NOT NULL,
    revision INTEGER NOT NULL,
    config_revision INTEGER NOT NULL DEFAULT 0,
    lifetime TEXT NOT NULL,
    task_filters TEXT NOT NULL DEFAULT '[]',
    shell_settings TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE IF NOT EXISTS memberships (
    team_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    joined_at REAL NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
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
""",
    # The authority half of DRIVERS-1kH6Kq6J: no driver could populate a service
    # tier, so the launch seam stopped carrying one and the column that recorded
    # it goes with it. Dropping it here rather than in the definition above is
    # what lets a database already stamped 1 arrive at this shape by being
    # opened, instead of by someone editing it.
    2: "ALTER TABLE agent_identities DROP COLUMN actual_service_tier;",
}
