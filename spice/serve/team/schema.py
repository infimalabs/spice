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
OBSERVATION_ATTRIBUTION_SAFE = "immutable"
OBSERVATION_ATTRIBUTION_REBUILD_REQUIRED = "rebuildRequired"

TEAM_AUTHORITY_SCHEMA_VERSION = 1
# Databases written before authority versions used this CRC32 value for the
# exact schema below. It is recognized only as a one-time source version; new
# databases and every successful upgrade are stamped with the explicit version.
LEGACY_TEAM_SCHEMA_FINGERPRINT = 783663365

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

TEAM_AUTHORITY_SCHEMA = """
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
"""

TEAM_PROJECTION_TABLES = frozenset(
    {
        "agent_metrics",
        "agent_metric_buckets",
        "agent_metric_cursors",
        "observation_attribution_state",
    }
)

# Lane activity and the checkpoint recording how far it was built are two halves
# of one fact, so they are dropped and replayed as a unit. Dropping one half
# alone leaves the survivor unaccountable: surviving aggregates would be counted
# again by a replay from the first byte, and a surviving checkpoint would hold
# the replay back from aggregates that no longer exist. Tables outside a listed
# family are their own family.
TEAM_PROJECTION_FAMILIES = (
    frozenset({"agent_metrics", "agent_metric_buckets", "agent_metric_cursors"}),
)

TEAM_PROJECTION_SCHEMA = """
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
CREATE TABLE IF NOT EXISTS observation_attribution_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    status TEXT NOT NULL CHECK (status IN ('immutable', 'rebuildRequired'))
);
CREATE INDEX IF NOT EXISTS agent_metric_buckets_by_start
    ON agent_metric_buckets (bucket_start);
"""

# Authority migrations are append-only and keyed by their destination version.
# A future authority change adds the next integer and its forward migration;
# projection-only changes do not bump this version.
TEAM_AUTHORITY_MIGRATIONS = {
    1: TEAM_AUTHORITY_SCHEMA,
}

# Canonical shapes let the opener validate a source database before any
# migration acquires a write transaction. Preserve an entry when adding a
# later version so every supported source remains independently recognizable.
TEAM_AUTHORITY_SCHEMAS = {
    1: TEAM_AUTHORITY_SCHEMA,
}
