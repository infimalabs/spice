"""Durable serve metric ingestion."""

from __future__ import annotations

import json
from datetime import UTC, datetime

from spice.serve.directivestats import DirectiveTotals
from spice.serve.metrics import record_transcript_metrics_for_agent
from spice.serve.teams import ServeTeamStore


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


def _presence_entry(timestamp: str, payload_type: str) -> dict[str, object]:
    return {
        "timestamp": timestamp,
        "type": "response_item",
        "payload": {"type": payload_type, "name": "exec_command"},
    }


def test_transcript_metric_ingestion_advances_cursor_without_double_count(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.record_directive_sent(
        "20260610T120000000001Z", agent_id="agent-a", team_id="agent-a"
    )
    rollout = tmp_path / "rollout.jsonl"
    _write_rollout(
        rollout,
        [
            _assistant_entry(
                "2026-06-10T12:00:00.000000Z",
                "ACK 20260610T120000000001Z: handled",
            ),
            _presence_entry("2026-06-10T12:00:01.000000Z", "function_call"),
            _presence_entry("2026-06-10T12:00:02.000000Z", "reasoning"),
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


def test_transcript_metric_cursors_follow_alias_rewrite_per_source_path(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["thread:predecessor"])
    predecessor_rollout = tmp_path / "predecessor.jsonl"
    successor_rollout = tmp_path / "successor.jsonl"
    _write_rollout(
        predecessor_rollout,
        [
            _assistant_entry(
                "2026-06-10T12:00:00.000000Z",
                "ACK 20260610T120000000001Z: predecessor",
            ),
            _presence_entry("2026-06-10T12:00:01.000000Z", "function_call"),
        ],
    )
    _write_rollout(
        successor_rollout,
        [
            _assistant_entry(
                "2026-06-10T12:01:00.000000Z",
                "ACK 20260610T120100000001Z: successor",
            ),
            _presence_entry("2026-06-10T12:01:01.000000Z", "custom_tool_call"),
        ],
    )

    store.record_directive_sent(
        "20260610T120000000001Z",
        agent_id="thread:predecessor",
        team_id=team.team_id,
    )
    record_transcript_metrics_for_agent(
        store, agent_id="thread:predecessor", transcript_path=predecessor_rollout
    )
    store.assign_agent(
        team.team_id,
        "thread:successor",
        aliases=["thread:predecessor"],
    )
    store.record_directive_sent(
        "20260610T120100000001Z",
        agent_id="thread:successor",
        team_id=team.team_id,
    )
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


def test_transcript_ack_flips_its_sent_directive(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    directive_key = "20260610T120000000001Z"
    store.record_directive_sent(directive_key, agent_id="agent-a", team_id="team-1")
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

    # The agent acknowledged the key, so its sent directive flips to acked.
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
                "ACK 20260610T120000000099Z: handled",
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
