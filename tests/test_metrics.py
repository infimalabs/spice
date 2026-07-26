"""Durable serve metric ingestion."""

from __future__ import annotations

import json
from datetime import UTC, datetime

import pytest

from spice.serve.directivestats import DirectiveTotals
from spice.serve.metrics import record_transcript_metrics_for_agent
from spice.serve.team import metrics as team_metrics
from spice.serve.team.schema import (
    TEAM_AUTHORITY_SCHEMA,
    TEAM_AUTHORITY_SCHEMA_VERSION,
    TEAM_PROJECTION_SCHEMA,
)
from spice.serve.team.store import ServeTeamStore
from spice.sqliteconnection import sqlite_connection
from tests.test_directivefacthelpers import (
    complete_directive_fact,
    publish_directive_fact,
)


# A Claude transcript is recognized by its thread-id filename, which is what
# routes these fixtures to the Claude dialect instead of Codex's rollout shape.
CLAUDE_TRANSCRIPT_NAME = "0123456789abcdef0123456789abcdef.jsonl"
METRIC_PROJECTION_TABLES = (
    "agent_metrics",
    "agent_metric_buckets",
    "agent_metric_cursors",
)
# The checkpoint shape shipped before a resume point carried source identity.
LEGACY_CURSOR_SCHEMA = """
CREATE TABLE agent_metric_cursors (
    agent_id TEXT NOT NULL,
    source_path TEXT NOT NULL,
    offset INTEGER NOT NULL,
    updated_at REAL NOT NULL,
    PRIMARY KEY (agent_id, source_path)
);
"""


def _write_rollout(path, entries):
    path.write_text(
        "".join(json.dumps(entry, separators=(",", ":")) + "\n" for entry in entries),
        encoding="utf-8",
    )


def _assistant_entry(timestamp: str, text: str) -> dict[str, object]:
    return {
        "timestamp": timestamp,
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    }


def _tool_call_entry(timestamp: str, payload_type: str) -> dict[str, object]:
    argument_key = "input" if payload_type == "custom_tool_call" else "arguments"
    return {
        "timestamp": timestamp,
        "type": "response_item",
        "payload": {
            "type": payload_type,
            "name": "exec_command",
            "call_id": f"call-{timestamp}",
            argument_key: "{}",
        },
    }


def _claude_turn_entry(timestamp: str, text: str, tool_name: str) -> dict[str, object]:
    """One Claude line carrying prose and a tool call, as the CLI writes it."""
    return {
        "timestamp": timestamp,
        "type": "assistant",
        "message": {
            "stop_reason": "end_turn",
            "content": [
                {"type": "text", "text": text},
                {"type": "tool_use", "id": "toolu-1", "name": tool_name, "input": {}},
            ],
        },
    }


def _reasoning_entry(timestamp: str, summary: str) -> dict[str, object]:
    return {
        "timestamp": timestamp,
        "type": "response_item",
        "payload": {
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": summary}],
        },
    }


def test_transcript_metric_ingestion_advances_cursor_without_double_count(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    publish_directive_fact(
        store.directive_state_path,
        "1k4Yh5gP",
        agent_id="agent-a",
        team_id="agent-a",
    )
    complete_directive_fact(store.directive_state_path, "1k4Yh5gP")
    rollout = tmp_path / "rollout.jsonl"
    _write_rollout(
        rollout,
        [
            _assistant_entry(
                "2026-06-10T12:00:00.000000Z",
                "ACK 1k4Yh5gP: handled",
            ),
            _tool_call_entry("2026-06-10T12:00:01.000000Z", "function_call"),
            _reasoning_entry("2026-06-10T12:00:02.000000Z", "weighing options"),
        ],
    )

    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=rollout
    )
    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=rollout
    )

    now = datetime(2026, 6, 10, 12, 0, 2, tzinfo=UTC).timestamp()
    summary = store.lane_metric_summary("agent-a", bucket_count=12, now=now)

    assert summary.acked == 1
    assert summary.tool_calls == 1
    assert sum(summary.sparkline) == 3


def test_transcript_metric_cursors_are_inherited_without_moving_source_checkpoint(
    tmp_path,
):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["thread:predecessor"])
    predecessor_rollout = tmp_path / "predecessor.jsonl"
    successor_rollout = tmp_path / "successor.jsonl"
    _write_rollout(
        predecessor_rollout,
        [
            _assistant_entry(
                "2026-06-10T12:00:00.000000Z",
                "ACK 1k4Yh5gP: predecessor",
            ),
            _tool_call_entry("2026-06-10T12:00:01.000000Z", "function_call"),
        ],
    )
    _write_rollout(
        successor_rollout,
        [
            _assistant_entry(
                "2026-06-10T12:01:00.000000Z",
                "ACK 1k4YhWsF: successor",
            ),
            _tool_call_entry("2026-06-10T12:01:01.000000Z", "custom_tool_call"),
        ],
    )

    publish_directive_fact(
        store.directive_state_path,
        "1k4Yh5gP",
        agent_id="thread:predecessor",
        team_id=team.team_id,
    )
    complete_directive_fact(store.directive_state_path, "1k4Yh5gP")
    record_transcript_metrics_for_agent(
        store, agent_id="thread:predecessor", transcript_path=predecessor_rollout
    )
    store.assign_agent(
        team.team_id,
        "thread:successor",
        aliases=["thread:predecessor"],
    )
    publish_directive_fact(
        store.directive_state_path,
        "1k4YhWsF",
        agent_id="thread:successor",
        team_id=team.team_id,
    )
    complete_directive_fact(store.directive_state_path, "1k4YhWsF")
    record_transcript_metrics_for_agent(
        store, agent_id="thread:successor", transcript_path=predecessor_rollout
    )
    record_transcript_metrics_for_agent(
        store, agent_id="thread:successor", transcript_path=successor_rollout
    )
    record_transcript_metrics_for_agent(
        store, agent_id="thread:successor", transcript_path=successor_rollout
    )

    now = datetime(2026, 6, 10, 12, 1, 1, tzinfo=UTC).timestamp()
    summary = store.lane_metric_summary("thread:successor", bucket_count=12, now=now)
    with store.connect() as connection:
        cursor_rows = connection.execute(
            "SELECT agent_id, source_path, offset FROM agent_metric_cursors "
            "ORDER BY source_path"
        ).fetchall()

    assert summary.acked == 2
    assert summary.tool_calls == 2
    assert sum(summary.sparkline) == 4
    assert [
        (row["agent_id"], row["source_path"], row["offset"]) for row in cursor_rows
    ] == [
        (
            "thread:predecessor",
            str(predecessor_rollout),
            predecessor_rollout.stat().st_size,
        ),
        (
            "thread:successor",
            str(predecessor_rollout),
            predecessor_rollout.stat().st_size,
        ),
        ("thread:successor", str(successor_rollout), successor_rollout.stat().st_size),
    ]


def test_lane_metric_sparkline_ages_old_buckets_out(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.record_agent_metric_delta(
        "agent-a",
        message_timestamps=[0, 60, 120],
    )

    initial = store.lane_metric_summary(
        "agent-a", bucket_count=4, bucket_seconds=60, now=180
    )
    shifted = store.lane_metric_summary(
        "agent-a", bucket_count=4, bucket_seconds=60, now=240
    )
    expired = store.lane_metric_summary(
        "agent-a", bucket_count=4, bucket_seconds=60, now=360
    )

    assert initial.sparkline == (1, 1, 1, 0)
    assert shifted.sparkline == (1, 1, 0, 0)
    assert expired.sparkline == (0, 0, 0, 0)


def test_transcript_ack_does_not_duplicate_canonical_ack_consumption(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    directive_key = "1k4Yh5gP"
    publish_directive_fact(
        store.directive_state_path,
        directive_key,
        agent_id="agent-a",
        team_id="team-1",
    )
    rollout = tmp_path / "rollout.jsonl"
    _write_rollout(
        rollout,
        [
            _assistant_entry(
                "2026-06-10T12:00:00.000000Z", f"ACK {directive_key}: handled"
            )
        ],
    )

    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=rollout
    )

    # Transcript ingestion owns activity/cursors only. The durable ACK archive
    # is the sole directive disposition writer.
    assert store.directive_totals_for_agents(["agent-a"]) == DirectiveTotals(
        sends=1, acked=0
    )
    assert complete_directive_fact(store.directive_state_path, directive_key) is True
    assert store.directive_totals_for_agents(["agent-a"]) == DirectiveTotals(
        sends=1, acked=1
    )


def test_transcript_ack_of_unsent_key_is_a_noop(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    rollout = tmp_path / "rollout.jsonl"
    _write_rollout(
        rollout,
        [
            _assistant_entry(
                "2026-06-10T12:00:00.000000Z",
                "ACK 1k4Yh5jH: handled",
            )
        ],
    )

    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=rollout
    )

    # Nothing was recorded as sent, so acking it cannot push acked above sends.
    assert store.directive_totals_for_agents(["agent-a"]) == DirectiveTotals(
        sends=0, acked=0
    )


def test_one_line_carrying_prose_and_a_tool_call_counts_both_facts(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    transcript = tmp_path / CLAUDE_TRANSCRIPT_NAME
    _write_rollout(
        transcript,
        [_claude_turn_entry("2026-06-10T12:00:00.000Z", "on it", "Bash")],
    )

    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )
    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )

    now = datetime(2026, 6, 10, 12, 0, 0, tzinfo=UTC).timestamp()
    summary = store.lane_metric_summary("agent-a", bucket_count=12, now=now)

    # The typed stream carries both blocks of the one line, and the second pass
    # resumes past them rather than counting them again.
    assert summary.tool_calls == 1
    assert sum(summary.sparkline) == 2


def test_replaced_transcript_restarts_from_its_first_byte(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    transcript = tmp_path / "rollout.jsonl"
    _write_rollout(
        transcript, [_tool_call_entry("2026-06-10T12:00:00.000000Z", "function_call")]
    )

    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )
    resumed_from = transcript.stat().st_size
    replacement = tmp_path / "replacement.jsonl"
    _write_rollout(
        replacement,
        [
            _tool_call_entry("2026-06-10T12:01:00.000000Z", "function_call"),
            _tool_call_entry("2026-06-10T12:01:01.000000Z", "function_call"),
            _tool_call_entry("2026-06-10T12:01:02.000000Z", "function_call"),
        ],
    )
    replacement.replace(transcript)
    replaced_inode = transcript.stat().st_ino
    # The new file is longer than the old resume point, so a byte offset alone
    # would have resumed into the middle of it and lost the leading calls.
    assert transcript.stat().st_size > resumed_from

    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )

    now = datetime(2026, 6, 10, 12, 1, 2, tzinfo=UTC).timestamp()
    summary = store.lane_metric_summary("agent-a", bucket_count=12, now=now)
    with store.connect() as connection:
        checkpoint = connection.execute(
            "SELECT offset, source_inode FROM agent_metric_cursors "
            "WHERE agent_id = ? AND source_path = ?",
            ("agent-a", str(transcript)),
        ).fetchone()

    assert summary.tool_calls == 4
    assert (checkpoint["offset"], checkpoint["source_inode"]) == (
        transcript.stat().st_size,
        replaced_inode,
    )


def test_truncated_transcript_replays_the_same_source_from_zero(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    transcript = tmp_path / "rollout.jsonl"
    _write_rollout(
        transcript,
        [
            _tool_call_entry("2026-06-10T12:00:00.000000Z", "function_call"),
            _tool_call_entry("2026-06-10T12:00:01.000000Z", "function_call"),
        ],
    )

    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )
    original_inode = transcript.stat().st_ino
    _write_rollout(
        transcript, [_tool_call_entry("2026-06-10T12:00:02.000000Z", "function_call")]
    )

    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )

    now = datetime(2026, 6, 10, 12, 0, 2, tzinfo=UTC).timestamp()
    summary = store.lane_metric_summary("agent-a", bucket_count=12, now=now)

    # Same file, fewer bytes: the shortened source is replayed from its start,
    # so the surviving call is counted rather than skipped.
    assert transcript.stat().st_ino == original_inode
    assert summary.tool_calls == 3


def test_malformed_line_stays_uncounted_without_blocking_later_facts(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text(
        json.dumps(_assistant_entry("2026-06-10T12:00:00.000000Z", "before"))
        + "\n{ this line is not JSON\n"
        + json.dumps(_tool_call_entry("2026-06-10T12:00:02.000000Z", "function_call"))
        + "\n",
        encoding="utf-8",
    )

    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )

    now = datetime(2026, 6, 10, 12, 0, 2, tzinfo=UTC).timestamp()
    summary = store.lane_metric_summary("agent-a", bucket_count=12, now=now)

    # An undecodable line is a visible fact, not lane activity, and the reader
    # carries on to the lines behind it.
    assert summary.tool_calls == 1
    assert sum(summary.sparkline) == 2


def _clean_replay_summary(tmp_path, transcript, *, now):
    """What one uninterrupted ingestion of the same transcript answers."""
    clean = ServeTeamStore(path=tmp_path / "clean-replay.sqlite3")
    record_transcript_metrics_for_agent(
        clean, agent_id="agent-a", transcript_path=transcript
    )
    return clean.lane_metric_summary("agent-a", bucket_count=12, now=now)


def _projection_row_counts(store):
    with store.connect() as connection:
        return {
            table: int(
                connection.execute(f"SELECT COUNT(*) AS rows FROM {table}").fetchone()[
                    "rows"
                ]
            )
            for table in METRIC_PROJECTION_TABLES
        }


def _activity_transcript(path):
    _write_rollout(
        path,
        [
            _assistant_entry("2026-06-10T12:00:00.000000Z", "starting"),
            _tool_call_entry("2026-06-10T12:00:01.000000Z", "function_call"),
            _reasoning_entry("2026-06-10T12:00:02.000000Z", "weighing options"),
        ],
    )
    return datetime(2026, 6, 10, 12, 0, 2, tzinfo=UTC).timestamp()


def test_a_pass_that_dies_before_its_checkpoint_leaves_nothing_behind(
    tmp_path, monkeypatch
):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    transcript = tmp_path / "rollout.jsonl"
    now = _activity_transcript(transcript)

    def die_after_the_facts(*args, **kwargs):
        raise RuntimeError("process lost between the facts and their checkpoint")

    monkeypatch.setattr(
        team_metrics, "_record_agent_metric_cursor_locked", die_after_the_facts
    )
    with pytest.raises(RuntimeError):
        record_transcript_metrics_for_agent(
            store, agent_id="agent-a", transcript_path=transcript
        )
    monkeypatch.undo()
    abandoned = _projection_row_counts(store)

    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )

    # The delta reached the same transaction as the checkpoint that never
    # landed, so the restart reads those bytes for the first time, not again.
    assert abandoned == dict.fromkeys(METRIC_PROJECTION_TABLES, 0)
    assert store.lane_metric_summary(
        "agent-a", bucket_count=12, now=now
    ) == _clean_replay_summary(tmp_path, transcript, now=now)


def test_deleted_checkpoint_rows_reset_the_counts_they_can_no_longer_account_for(
    tmp_path,
):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    transcript = tmp_path / "rollout.jsonl"
    now = _activity_transcript(transcript)
    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )

    with store.connect() as connection:
        connection.execute("DELETE FROM agent_metric_cursors")
    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )

    # Counts standing beside a lost checkpoint are exactly what the replay from
    # the first byte is about to produce, so they are cleared instead of doubled.
    assert store.lane_metric_summary(
        "agent-a", bucket_count=12, now=now
    ) == _clean_replay_summary(tmp_path, transcript, now=now)


def test_a_second_source_adds_to_the_lane_instead_of_clearing_it(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    first = tmp_path / "lane-a.jsonl"
    second = tmp_path / "lane-b.jsonl"
    now = _activity_transcript(first)
    _activity_transcript(second)

    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=first
    )
    after_first = store.lane_metric_summary("agent-a", bucket_count=12, now=now)
    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=second
    )
    after_second = store.lane_metric_summary("agent-a", bucket_count=12, now=now)
    with store.connect() as connection:
        cursor_paths = [
            str(row["source_path"])
            for row in connection.execute(
                "SELECT source_path FROM agent_metric_cursors WHERE agent_id = ? "
                "ORDER BY source_path",
                ("agent-a",),
            )
        ]

    # One agent reading a second transcript is one lane doing more work, not a
    # replay of the first: each source carries its own checkpoint, so the second
    # pass sums with the first and both resume points survive.
    assert (after_first.tool_calls, sum(after_first.sparkline)) == (1, 3)
    assert (after_second.tool_calls, sum(after_second.sparkline)) == (2, 6)
    assert cursor_paths == [str(first), str(second)]


def test_a_lost_checkpoint_resets_only_the_source_it_covered(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    kept = tmp_path / "lane-a.jsonl"
    lost = tmp_path / "lane-b.jsonl"
    now = _activity_transcript(kept)
    _activity_transcript(lost)
    record_transcript_metrics_for_agent(store, agent_id="agent-a", transcript_path=kept)
    record_transcript_metrics_for_agent(store, agent_id="agent-a", transcript_path=lost)

    with store.connect() as connection:
        connection.execute(
            "DELETE FROM agent_metric_cursors WHERE source_path = ?", (str(lost),)
        )
    record_transcript_metrics_for_agent(store, agent_id="agent-a", transcript_path=lost)

    after_replay = store.lane_metric_summary("agent-a", bucket_count=12, now=now)

    # Only the replayed source is about to be counted again, so only its counts
    # are cleared; the source still holding its checkpoint keeps everything it
    # contributed, because nothing is going to produce those facts a second time.
    assert (after_replay.tool_calls, sum(after_replay.sparkline)) == (2, 6)


def test_a_drifted_checkpoint_shape_replays_its_whole_family(tmp_path):
    path = tmp_path / "drifted.sqlite3"
    transcript = tmp_path / "rollout.jsonl"
    now = _activity_transcript(transcript)
    with sqlite_connection(path) as connection:
        connection.executescript(TEAM_AUTHORITY_SCHEMA)
        connection.executescript(TEAM_PROJECTION_SCHEMA)
        connection.execute("DROP TABLE agent_metric_cursors")
        connection.executescript(LEGACY_CURSOR_SCHEMA)
        connection.execute(
            "INSERT INTO agent_metrics "
            "(agent_id, team_id, tool_calls, updated_at) "
            "VALUES ('agent-a', 'agent-a', 1, 300)"
        )
        connection.execute(
            "INSERT INTO agent_metric_cursors "
            "(agent_id, source_path, offset, updated_at) VALUES (?, ?, ?, 300)",
            ("agent-a", str(transcript), transcript.stat().st_size),
        )
        connection.execute(f"PRAGMA user_version = {TEAM_AUTHORITY_SCHEMA_VERSION}")

    store = ServeTeamStore(path=path)
    surviving = _projection_row_counts(store)
    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )

    # The checkpoint shape changed, so its aggregates went with it: a surviving
    # count would be replayed onto, and a surviving checkpoint would hold the
    # replay back from counts that no longer exist.
    assert surviving == dict.fromkeys(METRIC_PROJECTION_TABLES, 0)
    assert store.lane_metric_summary(
        "agent-a", bucket_count=12, now=now
    ) == _clean_replay_summary(tmp_path, transcript, now=now)


def test_metric_projections_replay_equivalently_after_deletion(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    transcript = tmp_path / "rollout.jsonl"
    _write_rollout(
        transcript,
        [
            _assistant_entry("2026-06-10T12:00:00.000000Z", "starting"),
            _tool_call_entry("2026-06-10T12:00:01.000000Z", "function_call"),
            _reasoning_entry("2026-06-10T12:00:02.000000Z", "weighing options"),
        ],
    )
    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )
    now = datetime(2026, 6, 10, 12, 0, 2, tzinfo=UTC).timestamp()
    ingested = store.lane_metric_summary("agent-a", bucket_count=12, now=now)

    with store.connect() as connection:
        for table in METRIC_PROJECTION_TABLES:
            connection.execute(f"DELETE FROM {table}")
    record_transcript_metrics_for_agent(
        store, agent_id="agent-a", transcript_path=transcript
    )

    # Buckets, lifetime totals, and cursors are projections: dropping their rows
    # and replaying the same facts lands on the same answers.
    assert store.lane_metric_summary("agent-a", bucket_count=12, now=now) == ingested
