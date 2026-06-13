"""Durable serve metric ingestion."""

from __future__ import annotations

import json

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

    summary = store.lane_metric_summary("agent-a", bucket_count=12)

    assert summary.acked == 1
    assert summary.tool_calls == 1
    assert sum(summary.sparkline) == 3
