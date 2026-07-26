"""Durable lane metric ingestion from the typed transcript fact stream.

Serve counts lane activity from the public typed facts the transcript reader
decodes, never from its own reading of provider JSON. Each fact carries its own
locus -- byte offset, event time, and the actor whose transcript produced it --
so ingestion resumes at an exact byte boundary and one line carrying prose, a
reasoning summary, and a tool call contributes all three rather than collapsing
to whichever block a one-message-per-line projection reached first.

Resumption is a checkpoint, not an offset: the reader compares the source's
filesystem identity against the stored one, so a transcript replaced under the
same path restarts from its first byte instead of resuming into the middle of a
different file.

The counted facts and the checkpoint they were read to are one write. A pass
that dies before its commit leaves neither, so the next pass reads the same
bytes once rather than counting them twice or stepping over them unread.
"""

from __future__ import annotations

from dataclasses import replace
from pathlib import Path

from spice.agent.driver import driver_for_transcript
from spice.serve.team.store import ServeTeamStore
from spice.transcript.events import (
    AssistantText,
    Compaction,
    Image,
    Reasoning,
    ToolCall,
    ToolOutput,
    TranscriptEvent,
    WebSearch,
)
from spice.transcript.reader import TranscriptCursor, TranscriptEventReader
from spice.transcript.timestamps import parse_timestamp

# A provider-side web search is a tool call the model ran itself, so it is
# charged to the lane exactly like a harness-run tool.
TOOL_CALL_EVENTS = (ToolCall, WebSearch)
# Activity is what the agent itself produced. Operator input, token accounting,
# and undecodable lines are facts about the lane rather than work it did, so
# they carry no activity of their own.
ACTIVITY_EVENTS = (
    AssistantText,
    Compaction,
    Image,
    Reasoning,
    ToolCall,
    ToolOutput,
    WebSearch,
)


def record_transcript_metrics_for_agent(
    store: ServeTeamStore, *, agent_id: str, transcript_path: Path
) -> None:
    source_path = str(transcript_path)
    checkpoint = store.agent_metric_checkpoint(agent_id, source_path)
    cursor = TranscriptCursor(
        offset=checkpoint.offset, file_identity=checkpoint.file_identity
    )
    reader = TranscriptEventReader(
        path=transcript_path,
        driver=driver_for_transcript(transcript_path),
        source_actor=agent_id,
    )
    read = reader.read("forward", cursor=cursor)
    if read.error is not None:
        # An unreadable source leaves the checkpoint untouched, so the next pass
        # resumes from the same byte rather than replaying or skipping ahead.
        return
    if read.end_offset == checkpoint.offset and not read.events:
        return
    store.record_agent_metric_delta(
        agent_id,
        tool_calls=sum(
            1 for event in read.events if isinstance(event, TOOL_CALL_EVENTS)
        ),
        tool_call_timestamps=_event_times(read.events, TOOL_CALL_EVENTS),
        message_timestamps=_event_times(read.events, ACTIVITY_EVENTS),
        checkpoint=replace(
            checkpoint, offset=cursor.offset, file_identity=cursor.file_identity
        ),
    )


def _event_times(
    events: tuple[TranscriptEvent, ...],
    kinds: tuple[type[TranscriptEvent], ...],
) -> tuple[float, ...]:
    """Instants for the facts of one kind, skipping those stamped unreadably."""
    return tuple(
        parsed.timestamp()
        for event in events
        if isinstance(event, kinds)
        if (parsed := parse_timestamp(event.at.timestamp)) is not None
    )
