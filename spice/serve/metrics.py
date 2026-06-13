"""Durable lane metric ingestion from supervisor-observed transcripts."""

from __future__ import annotations

from pathlib import Path

from spice.serve import messages as message_reader
from spice.serve.teams import ServeTeamStore

TOOL_CALL_KINDS = frozenset(
    {
        "presence:function_call",
        "presence:custom_tool_call",
        "presence:web_search_call",
    }
)


def record_transcript_metrics_for_agent(
    store: ServeTeamStore, *, agent_id: str, transcript_path: Path
) -> None:
    source_path = str(transcript_path)
    start_offset = store.agent_metric_cursor(agent_id, source_path)
    items, end_offset = message_reader.read_metric_messages_from_offset(
        transcript_path, start_offset=start_offset
    )
    if end_offset == start_offset and not items:
        return
    store.record_agent_metric_delta(
        agent_id,
        acked=sum(max(0, item.ack_count) for item in items),
        tool_calls=sum(1 for item in items if item.kind in TOOL_CALL_KINDS),
        message_timestamps=(
            parsed.timestamp()
            for item in items
            if (parsed := message_reader.parse_timestamp(item.timestamp)) is not None
        ),
    )
    store.record_agent_metric_cursor(
        agent_id, source_path=source_path, offset=end_offset
    )
