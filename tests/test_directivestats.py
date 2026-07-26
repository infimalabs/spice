"""Canonical steering/ACK facts projected into Serve directive metrics."""

from __future__ import annotations

import pytest

from spice.errors import SpiceError
from spice.mail.ackstate import (
    AckStateWrite,
    directive_history_records_from_database,
    record_acked_inbox_items_to_database,
)
from spice.sqliteconnection import sqlite_connection
from spice.serve.directivestats import DirectiveTotals
from spice.serve.team.store import ServeTeamStore
from tests.test_directivefacthelpers import (
    complete_directive_fact,
    publish_directive_fact,
)
from spice.serve.team.schema import (
    TEAM_AUTHORITY_SCHEMA,
    TEAM_AUTHORITY_SCHEMA_VERSION,
)

DIRECTIVE_SENT_AT = 100.0
DIRECTIVE_ACKED_AT = 140.0
LEGACY_DIRECTIVE_SCHEMA = """
CREATE TABLE directives (
    directive_key TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    sent_at REAL NOT NULL,
    acked INTEGER NOT NULL DEFAULT 0,
    acked_at REAL
);
CREATE TABLE directive_totals (
    agent_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    sends INTEGER NOT NULL DEFAULT 0,
    acked INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (agent_id, team_id)
);
"""


def _store(tmp_path):
    return ServeTeamStore(path=tmp_path / "teams.sqlite3")


def test_each_directive_counts_once_and_ack_is_a_subset_of_send(tmp_path):
    store = _store(tmp_path)
    # Three directives sent to one agent on one team; two acknowledged. It does
    # not matter that an agent might ack several keys in one message — each key
    # was individually sent, so each is its own send, and each ack flips one.
    for key in ("k1", "k2", "k3"):
        publish_directive_fact(
            store.directive_state_path, key, agent_id="agent-a", team_id="team-1"
        )
    assert complete_directive_fact(store.directive_state_path, "k1") is True
    assert complete_directive_fact(store.directive_state_path, "k3") is True

    totals = store.directive_totals_for_agents(["agent-a"])
    assert totals == DirectiveTotals(sends=3, acked=2)
    assert totals.acked <= totals.sends


def test_resending_the_same_key_does_not_double_count(tmp_path):
    store = _store(tmp_path)
    publish_directive_fact(
        store.directive_state_path,
        "k1",
        agent_id="agent-a",
        team_id="team-1",
        sent_at=100,
    )
    publish_directive_fact(
        store.directive_state_path,
        "k1",
        agent_id="agent-a",
        team_id="team-1",
        sent_at=100,
    )

    assert store.directive_totals_for_agents(["agent-a"]) == DirectiveTotals(
        sends=1, acked=0
    )


def test_acking_is_idempotent_and_unknown_keys_are_noops(tmp_path):
    store = _store(tmp_path)
    publish_directive_fact(
        store.directive_state_path, "k1", agent_id="agent-a", team_id="team-1"
    )

    assert complete_directive_fact(store.directive_state_path, "k1") is True
    assert complete_directive_fact(store.directive_state_path, "k1") is False
    assert complete_directive_fact(store.directive_state_path, "nope") is False

    totals = store.directive_totals_for_agents(["agent-a"])
    assert totals == DirectiveTotals(sends=1, acked=1)
    assert totals.acked <= totals.sends


def test_totals_sum_across_agents_and_capture_team(tmp_path):
    store = _store(tmp_path)
    # agent-a sent two directives while on different teams (team-at-capture is
    # recorded per row); agent-b one. Per-agent totals sum across teams.
    for key, agent_id, team_id in (
        ("a1", "agent-a", "team-1"),
        ("a2", "agent-a", "team-2"),
        ("b1", "agent-b", "team-1"),
    ):
        publish_directive_fact(
            store.directive_state_path,
            key,
            agent_id=agent_id,
            team_id=team_id,
        )
    complete_directive_fact(store.directive_state_path, "a1")
    complete_directive_fact(store.directive_state_path, "b1")

    assert store.directive_totals_for_agents(["agent-a"]) == DirectiveTotals(
        sends=2, acked=1
    )
    assert store.directive_totals_for_agents(["agent-a", "agent-b"]) == DirectiveTotals(
        sends=3, acked=2
    )
    assert store.directive_totals_for_agents([]) == DirectiveTotals(sends=0, acked=0)


def test_directive_rows_are_the_stable_series_with_team_at_capture(tmp_path):
    store = _store(tmp_path)
    publish_directive_fact(
        store.directive_state_path,
        "a1",
        agent_id="agent-a",
        team_id="team-1",
        sent_at=DIRECTIVE_SENT_AT,
    )
    complete_directive_fact(
        store.directive_state_path, "a1", acked_at=DIRECTIVE_ACKED_AT
    )

    row = directive_history_records_from_database(store.directive_state_path)[0]

    assert (row.target_actor, row.team_id) == ("agent-a", "team-1")
    assert row.sent_at == DIRECTIVE_SENT_AT
    assert row.disposition == "acked"
    assert row.acknowledged_at == DIRECTIVE_ACKED_AT


def test_same_key_with_changed_publication_provenance_fails_actionably(tmp_path):
    store = _store(tmp_path)
    publish_directive_fact(
        store.directive_state_path,
        "k1",
        agent_id="agent-a",
        team_id="team-1",
        sent_at=100,
    )

    with pytest.raises(SpiceError, match="immutable publication provenance differs"):
        publish_directive_fact(
            store.directive_state_path,
            "k1",
            agent_id="agent-b",
            team_id="team-2",
            sent_at=101,
        )


def test_pending_refused_ack_latency_and_restart_share_one_history(tmp_path):
    store = _store(tmp_path)
    for key, sent_at in (("pending", 100), ("acked", 110), ("refused", 120)):
        publish_directive_fact(
            store.directive_state_path,
            key,
            agent_id="agent-a",
            team_id="team-a",
            sent_at=sent_at,
        )
    complete_directive_fact(store.directive_state_path, "acked", acked_at=150)
    complete_directive_fact(
        store.directive_state_path,
        "refused",
        acked_at=170,
        disposition="refused",
        ack_text="NACK refused: conflicts with policy",
        ack_content="conflicts with policy",
    )
    assert (
        complete_directive_fact(store.directive_state_path, "acked", acked_at=180)
        is False
    )

    restarted = _store(tmp_path)
    summary = restarted.directive_lifecycle_summary_for_agents(["agent-a"])
    assert (
        summary.sends,
        summary.acked,
        summary.refused,
        summary.pending,
        summary.minimum_latency_seconds,
        summary.maximum_latency_seconds,
    ) == (3, 1, 1, 1, 40.0, 50.0)


def test_legacy_serve_rows_migrate_losslessly_then_tables_are_removed(tmp_path):
    path = tmp_path / "teams.sqlite3"
    ack_path = tmp_path / "spiceacks.sqlite3"
    _seed_legacy_team_directives(
        path,
        directives=(
            ("pending", "agent-a", "team-a", 100.0, 0, None),
            ("acked", "agent-a", "team-a", 120.0, 1, 140.0),
        ),
        totals=(("agent-a", "team-a", 2, 1),),
    )
    record_acked_inbox_items_to_database(
        ack_path,
        [
            AckStateWrite(
                key="acked",
                inbox_name="acked.txt",
                text="do the work",
                ack_text="ACK acked: completed the work",
                ack_content="completed the work",
            )
        ],
        now=140.0,
    )

    store = ServeTeamStore(path=path, directive_state_path=ack_path)
    with store.connect() as connection:
        tables = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
        }

    records = {
        record.key: record
        for record in directive_history_records_from_database(ack_path)
    }
    assert "directives" not in tables
    assert "directive_totals" not in tables
    assert (records["pending"].target_actor, records["pending"].disposition) == (
        "agent-a",
        "pending",
    )
    assert (
        records["acked"].acknowledged_at,
        records["acked"].ack_content,
        records["acked"].legacy_metric["acked_at"],
    ) == (140.0, "completed the work", 140.0)
    assert store.directive_totals_for_agents(["agent-a"]) == DirectiveTotals(2, 1)


def test_legacy_ack_without_auditable_archive_blocks_cutover(tmp_path):
    path = tmp_path / "teams.sqlite3"
    ack_path = tmp_path / "spiceacks.sqlite3"
    _seed_legacy_team_directives(
        path,
        directives=(("acked", "agent-a", "team-a", 120.0, 1, 140.0),),
        totals=(("agent-a", "team-a", 1, 1),),
    )
    store = ServeTeamStore(path=path, directive_state_path=ack_path)

    with pytest.raises(SpiceError, match="no durable ACK archive"):
        store._ensure_schema()
    with sqlite_connection(path) as connection:
        tables = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
        }
    assert {"directives", "directive_totals"} <= tables
    assert directive_history_records_from_database(ack_path) == []


def test_pruned_legacy_totals_block_lossy_cutover(tmp_path):
    path = tmp_path / "teams.sqlite3"
    ack_path = tmp_path / "spiceacks.sqlite3"
    _seed_legacy_team_directives(
        path,
        directives=(("remaining", "agent-a", "team-a", 120.0, 0, None),),
        totals=(("agent-a", "team-a", 2, 0),),
    )
    store = ServeTeamStore(path=path, directive_state_path=ack_path)

    with pytest.raises(SpiceError, match="historical rows may have been pruned"):
        store._ensure_schema()
    with sqlite_connection(path) as connection:
        count = connection.execute("SELECT COUNT(*) FROM directives").fetchone()[0]
    assert count == 1
    assert not ack_path.exists()


def test_legacy_and_canonical_publication_collision_blocks_cutover(tmp_path):
    path = tmp_path / "teams.sqlite3"
    ack_path = tmp_path / "spiceacks.sqlite3"
    _seed_legacy_team_directives(
        path,
        directives=(("collision", "agent-a", "team-a", 120.0, 0, None),),
        totals=(("agent-a", "team-a", 1, 0),),
    )
    publish_directive_fact(
        ack_path,
        "collision",
        agent_id="agent-b",
        team_id="team-b",
        sent_at=120.0,
    )
    store = ServeTeamStore(path=path, directive_state_path=ack_path)

    with pytest.raises(SpiceError, match="publication provenance collide"):
        store._ensure_schema()
    with sqlite_connection(path) as connection:
        count = connection.execute("SELECT COUNT(*) FROM directives").fetchone()[0]
    assert count == 1
    record = directive_history_records_from_database(ack_path)[0]
    assert (record.target_actor, record.team_id) == ("agent-b", "team-b")


def _seed_legacy_team_directives(
    path,
    *,
    directives,
    totals,
) -> None:
    with sqlite_connection(path, ensure_parent=True) as connection:
        connection.executescript(TEAM_AUTHORITY_SCHEMA + LEGACY_DIRECTIVE_SCHEMA)
        connection.executemany(
            "INSERT INTO directives "
            "(directive_key, agent_id, team_id, sent_at, acked, acked_at) "
            "VALUES (?, ?, ?, ?, ?, ?)",
            directives,
        )
        connection.executemany(
            "INSERT INTO directive_totals "
            "(agent_id, team_id, sends, acked) VALUES (?, ?, ?, ?)",
            totals,
        )
        connection.execute(f"PRAGMA user_version = {TEAM_AUTHORITY_SCHEMA_VERSION}")
