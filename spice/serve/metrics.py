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

import time
from dataclasses import replace
from pathlib import Path
from typing import Iterable

from spice.agent.driver import driver_for_transcript, select_driver
from spice.serve.team.projection import (
    AGENT_ACTIVITY,
    ProjectionFamilyState,
    PROJECTION_STATUS_INCOMPATIBLE,
    PROJECTION_STATUS_UNAVAILABLE,
    ProjectionUnavailableError,
    ServeProjectionStore,
    rebuild_projection_family,
)
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


def transcript_metric_sources(
    store: ServeTeamStore,
) -> tuple[tuple[str, Path], ...]:
    """Resolve replay sources by the one documented recovery order.

    A servable projection supplies its exact checkpoint manifest. Authority
    identities then add any transcript still discoverable by its recorded
    owner/thread pair, which is also how recovery proceeds after an
    incompatible projection discarded its cursor table. Neither source is a
    metric answer: both only name native transcript files for typed replay.
    """
    selected: dict[tuple[str, str], tuple[str, Path]] = {}
    checkpointed_agents: set[str] = set()
    try:
        with store.projections.read(AGENT_ACTIVITY) as projection:
            rows = projection.execute(
                "SELECT agent_id, source_path FROM agent_metric_cursors "
                "ORDER BY agent_id, source_path"
            ).fetchall()
        for row in rows:
            agent_id = str(row["agent_id"])
            source_path = str(row["source_path"])
            if agent_id and source_path:
                path = Path(source_path)
                selected[(agent_id, str(path))] = (agent_id, path)
                checkpointed_agents.add(agent_id)
    except ProjectionUnavailableError:
        pass
    with store.connect() as authority:
        identities = authority.execute(
            "SELECT actor_id, thread_id, transcript_owner "
            "FROM agent_identities WHERE thread_id != '' "
            "AND transcript_owner != '' ORDER BY actor_id"
        ).fetchall()
    for row in identities:
        agent_id = str(row["actor_id"])
        if agent_id in checkpointed_agents:
            continue
        transcript = select_driver(
            str(row["transcript_owner"])
        ).find_session_transcript(str(row["thread_id"]))
        if transcript is not None:
            selected[(agent_id, str(transcript))] = (agent_id, transcript)
    return tuple(selected[key] for key in sorted(selected))


def rebuild_transcript_metrics(
    store: ServeTeamStore,
    *,
    sources: Iterable[tuple[str, Path]] | None = None,
) -> ProjectionFamilyState:
    """Replay transcript activity in isolation and publish it atomically."""
    published = next(
        state
        for state in store.projections.family_states()
        if state.family == AGENT_ACTIVITY
    )
    retention_floor = published.retention_floor
    if retention_floor is None and published.status in {
        PROJECTION_STATUS_INCOMPATIBLE,
        PROJECTION_STATUS_UNAVAILABLE,
    }:
        retention_floor = int(time.time()) - store.metric_history_retention_seconds()
    selected = (
        transcript_metric_sources(store)
        if sources is None
        else tuple(
            (str(agent_id), Path(path))
            for agent_id, path in sources
            if str(agent_id) and str(path)
        )
    )

    def populate(stage: ServeProjectionStore) -> float | None:
        staging_store = ServeTeamStore(
            path=store.path,
            directive_state_path=store.directive_state_path,
            projection_path=stage.path,
        )
        freshness: float | None = None
        for agent_id, transcript_path in selected:
            record_transcript_metrics_for_agent(
                staging_store,
                agent_id=agent_id,
                transcript_path=transcript_path,
            )
            try:
                modified = transcript_path.stat().st_mtime
            except OSError:
                continue
            freshness = modified if freshness is None else max(freshness, modified)
        if retention_floor is not None:
            staging_store.prune_metric_history(
                now=(retention_floor + staging_store.metric_history_retention_seconds())
            )
        return freshness

    return rebuild_projection_family(store.projections, AGENT_ACTIVITY.name, populate)


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
