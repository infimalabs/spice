"""Assistant message envelopes streamed from the agent transcript.

Each transcript line becomes at most one envelope keyed `timestamp#offset`
(the byte offset doubles as a stable cursor). Visible envelopes are assistant
prose (ACK-segmented), final answers, plan updates, and compaction dividers;
tool calls and reasoning become *presence* records that carry activity previews
without consuming the visible message budget. Files larger than the tail cap are
scanned backwards in chunks so a season-long transcript stays cheap to page.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path

from spice.agent.driver import (
    AgentDriver,
    all_drivers,
    driver_for,
    driver_for_transcript,
)
from spice.agent.identity import canonical_thread_id
from spice.serve.messagepresentation import (
    AssistantMessage,
    MessagePresenter,
)
from spice.transcript.reader import (
    REVERSE_WINDOW_BYTES,
    TranscriptCursor,
    TranscriptEventReader,
    cursor_offset,
    locked_cursor,
    offset_after_line,
    transcript_file_identity,
    transcript_size,
)
from spice.transcript.timestamps import parse_timestamp

ACTIVE_ASSISTANT_SECONDS = 60
ACTIVEISH_ASSISTANT_SECONDS = 5 * 60
DEFAULT_MESSAGE_LIMIT = 200
MAX_MESSAGE_LIMIT = 400


@dataclass
class RolloutCursor(TranscriptCursor):
    # Append-only transcripts: the initial no-`before` read seeds this cache;
    # same-size reads reuse it, and watcher growth reads extend it from
    # `offset` instead of rescanning the transcript tail.
    window: list[AssistantMessage] | None = field(default=None, repr=False)
    window_size: int = -1
    window_limit: int = -1
    removed_keys: list[str] = field(default_factory=list, repr=False)


@dataclass(frozen=True)
class TranscriptResolution:
    thread_id: str
    path: Path
    owner_driver: AgentDriver


@dataclass(frozen=True)
class AssistantMessageRead:
    items: list[AssistantMessage]
    error: str | None
    transcript: TranscriptResolution | None


def resolve_thread_transcript(
    thread_id: str, repo_root: Path | None = None
) -> TranscriptResolution | None:
    """Locate a thread's transcript and the driver that owns it."""
    canonical = canonical_thread_id(thread_id)
    if not canonical:
        return None
    preferred = driver_for(repo_root)
    ordered = [preferred, *(d for d in all_drivers() if d.name != preferred.name)]
    for driver in ordered:
        try:
            return TranscriptResolution(
                thread_id=canonical,
                path=driver.thread_transcript_path(canonical),
                owner_driver=driver,
            )
        except (RuntimeError, SystemExit):
            continue
    return None


def assistant_messages_for_thread_id(
    thread_id: str,
    *,
    limit: int = DEFAULT_MESSAGE_LIMIT,
    after: str | None = None,
    before: str | None = None,
    append_only: bool = False,
    cursor: RolloutCursor | None = None,
    worktree_id: str | None = None,
    repo_root: Path | None = None,
) -> AssistantMessageRead:
    transcript = resolve_thread_transcript(thread_id, repo_root)
    if transcript is None or not transcript.path.is_file():
        return AssistantMessageRead(
            items=[],
            error=f"Could not resolve transcript for {thread_id}",
            transcript=transcript,
        )
    return AssistantMessageRead(
        items=read_assistant_messages(
            transcript.path,
            limit=limit,
            after=after,
            before=before,
            append_only=append_only,
            cursor=cursor,
            worktree_id=worktree_id,
            driver=transcript.owner_driver,
        ),
        error=None,
        transcript=transcript,
    )


def read_assistant_messages(
    transcript_path: Path,
    *,
    limit: int = DEFAULT_MESSAGE_LIMIT,
    after: str | None = None,
    before: str | None = None,
    append_only: bool = False,
    cursor: RolloutCursor | None = None,
    worktree_id: str | None = None,
    driver: AgentDriver | None = None,
) -> list[AssistantMessage]:
    bounded = max(1, min(limit, MAX_MESSAGE_LIMIT))
    owner_driver = driver or driver_for_transcript(transcript_path)
    if cursor is not None:
        with locked_cursor(cursor):
            cursor.removed_keys = []
            return _read_locked(
                transcript_path,
                limit=bounded,
                after=after,
                before=before,
                append_only=append_only,
                cursor=cursor,
                worktree_id=worktree_id,
                driver=owner_driver,
            )
    return _read_locked(
        transcript_path,
        limit=bounded,
        after=after,
        before=before,
        append_only=append_only,
        cursor=None,
        worktree_id=worktree_id,
        driver=owner_driver,
    )


def _read_locked(
    transcript_path: Path,
    *,
    limit: int,
    after: str | None,
    before: str | None,
    append_only: bool,
    cursor: RolloutCursor | None,
    worktree_id: str | None,
    driver: AgentDriver,
) -> list[AssistantMessage]:
    if before is not None:
        end_offset = cursor_offset(before)
        if end_offset is None:
            return []
        return _read_window(
            transcript_path,
            limit=limit,
            end_offset=end_offset,
            cursor=None,
            worktree_id=worktree_id,
            driver=driver,
        )
    if (
        append_only
        and cursor is not None
        and (after is None or after == cursor.last_key)
    ):
        return _read_appended_window(
            transcript_path,
            limit=limit,
            cursor=cursor,
            worktree_id=worktree_id,
            driver=driver,
        )
    if cursor is not None and after and after == cursor.last_key:
        return _read_from_offset(
            transcript_path,
            start_offset=cursor.offset,
            limit=limit,
            cursor=cursor,
            worktree_id=worktree_id,
            driver=driver,
        )
    if after is not None:
        after_offset = cursor_offset(after)
        if after_offset is not None:
            return _read_from_offset(
                transcript_path,
                start_offset=offset_after_line(transcript_path, after_offset),
                limit=limit,
                cursor=cursor,
                worktree_id=worktree_id,
                driver=driver,
            )
    return _read_window(
        transcript_path,
        limit=limit,
        end_offset=None,
        cursor=cursor,
        worktree_id=worktree_id,
        driver=driver,
    )


def _read_from_offset(
    transcript_path: Path,
    *,
    start_offset: int,
    limit: int,
    cursor: RolloutCursor | None,
    worktree_id: str | None,
    driver: AgentDriver,
) -> list[AssistantMessage]:
    try:
        messages, end_offset = _read_chronological_from_offset(
            transcript_path,
            start_offset=start_offset,
            worktree_id=worktree_id,
            driver=driver,
        )
    except OSError:
        return []
    kept = _trim_chronological(messages, limit)
    if cursor is not None:
        cursor.offset = end_offset
        if messages:
            cursor.last_key = messages[-1].key
    return list(reversed(_collapse_view_image_pairs(kept)))


def _read_appended_window(
    transcript_path: Path,
    *,
    limit: int,
    cursor: RolloutCursor,
    worktree_id: str | None,
    driver: AgentDriver,
) -> list[AssistantMessage]:
    if cursor.window is None or cursor.window_limit != limit:
        return _read_window(
            transcript_path,
            limit=limit,
            end_offset=None,
            cursor=cursor,
            worktree_id=worktree_id,
            driver=driver,
        )
    file_size = transcript_size(transcript_path)
    file_identity = transcript_file_identity(transcript_path)
    if file_size is None or file_identity is None:
        return []
    if (
        file_size < cursor.offset
        or file_size < cursor.window_size
        or (cursor.file_identity is not None and cursor.file_identity != file_identity)
    ):
        return _read_window(
            transcript_path,
            limit=limit,
            end_offset=None,
            cursor=cursor,
            worktree_id=worktree_id,
            driver=driver,
        )
    if file_size == cursor.window_size:
        # Nothing has been written since the window was drawn, so the cursor is
        # already at the boundary that window ended on. Advancing it to the
        # observed size here would step over a tail the writer has not finished.
        return []
    read = _reader(transcript_path, driver).read("forward", cursor=cursor)
    if read.error is not None:
        return []
    if read.file_identity != file_identity:
        return _read_window(
            transcript_path,
            limit=limit,
            end_offset=None,
            cursor=cursor,
            worktree_id=worktree_id,
            driver=driver,
        )
    appended = MessagePresenter().project(read.events, worktree_id=worktree_id)
    end_offset = read.end_offset
    previous = list(reversed(cursor.window))
    previous_tail = previous[-1:] if previous else []
    combined = previous + appended
    window = _collapse_view_image_pairs(_trim_chronological(combined, limit))
    delta = _collapse_view_image_pairs(
        previous_tail + _trim_chronological(appended, limit)
    )
    tail_keys = {message.key for message in previous_tail}
    delta_keys = {message.key for message in delta}
    cursor.removed_keys = [
        message.key for message in previous_tail if message.key not in delta_keys
    ]
    cursor.offset = end_offset
    if appended:
        cursor.last_key = appended[-1].key
    cursor.window = list(reversed(window))
    cursor.window_size = end_offset
    cursor.window_limit = limit
    return [message for message in reversed(delta) if message.key not in tail_keys]


def _read_chronological_from_offset(
    transcript_path: Path,
    *,
    start_offset: int,
    worktree_id: str | None,
    driver: AgentDriver,
) -> tuple[list[AssistantMessage], int]:
    read = _reader(transcript_path, driver).read(
        "forward",
        cursor=TranscriptCursor(offset=start_offset),
    )
    return (
        MessagePresenter().project(read.events, worktree_id=worktree_id),
        read.resume_offset,
    )


def _read_window(
    transcript_path: Path,
    *,
    limit: int,
    end_offset: int | None,
    cursor: RolloutCursor | None,
    worktree_id: str | None,
    driver: AgentDriver,
) -> list[AssistantMessage]:
    """Newest-first window ending at `end_offset` (or EOF), tail-scanned."""
    file_identity = transcript_file_identity(transcript_path)
    if file_identity is None:
        return []
    if end_offset is None and cursor is not None and cursor.window is not None:
        file_size = transcript_size(transcript_path)
        if file_size is None:
            return []
        if (
            cursor.window_size == file_size
            and cursor.window_limit == limit
            and cursor.file_identity == file_identity
        ):
            return list(cursor.window)
    reader = _reader(transcript_path, driver)
    read = reader.read(
        "reverse",
        end_offset=end_offset,
        max_bytes=REVERSE_WINDOW_BYTES,
    )
    presenter = MessagePresenter.paging()
    scanned = presenter.project(read.events, worktree_id=worktree_id)
    visible_count = sum(not message.kind.startswith("presence:") for message in scanned)
    scan_start = read.access_start_offset
    while visible_count < limit and scan_start > 0:
        older = reader.read(
            "bounded",
            start_offset=max(0, scan_start - REVERSE_WINDOW_BYTES),
            end_offset=scan_start,
            align_partial_start=True,
        )
        if older.access_start_offset >= scan_start:
            break
        if older.events:
            projected = presenter.project(older.events, worktree_id=worktree_id)
            scanned[0:0] = projected
            visible_count += sum(
                not message.kind.startswith("presence:") for message in projected
            )
        scan_start = older.access_start_offset
    scanned = presenter.resolve(scanned)
    visible = [
        message for message in scanned if not message.kind.startswith("presence:")
    ]
    presence = [message for message in scanned if message.kind.startswith("presence:")]
    kept = list(visible[-limit:])
    if end_offset is None:
        kept.extend(_kept_presence_messages(presence))
    kept.sort(key=lambda message: message.index)
    kept = _collapse_view_image_pairs(kept)
    if end_offset is not None and _line_has_tool_output_image(
        transcript_path, end_offset, driver=driver
    ):
        kept = _drop_trailing_view_image_call(kept)
    result = list(reversed(kept))
    if cursor is not None and end_offset is None:
        # The window this seeds from is a reverse read, which shows a partial
        # tail rather than holding it back, so the live cursor takes the last
        # complete boundary instead of the end of the file it observed.
        cursor.offset = read.resume_offset
        cursor.last_key = kept[-1].key if kept else None
        cursor.window = result
        cursor.window_size = read.file_size
        cursor.window_limit = limit
        cursor.file_identity = read.file_identity
    return result


def _reader(transcript_path: Path, driver: AgentDriver) -> TranscriptEventReader:
    return TranscriptEventReader(transcript_path, driver, source_actor=None)


def _collapse_view_image_pairs(
    messages: list[AssistantMessage],
) -> list[AssistantMessage]:
    """Drop a `view_image` call immediately followed by its output image."""
    collapsed: list[AssistantMessage] = []
    index = 0
    while index < len(messages):
        message = messages[index]
        follower = messages[index + 1] if index + 1 < len(messages) else None
        if (
            message.source_kind == "view_image_call"
            and follower is not None
            and follower.source_kind == "tool_output_image"
        ):
            index += 1
            continue
        collapsed.append(message)
        index += 1
    return collapsed


def _drop_trailing_view_image_call(
    messages: list[AssistantMessage],
) -> list[AssistantMessage]:
    if messages and messages[-1].source_kind == "view_image_call":
        return messages[:-1]
    return messages


def _line_has_tool_output_image(
    transcript_path: Path, offset: int, *, driver: AgentDriver
) -> bool:
    """The paging boundary line pairs with a trailing `view_image` call."""
    read = _reader(transcript_path, driver).read(
        "bounded", start_offset=offset, end_offset=offset + 1
    )
    return MessagePresenter.contains_tool_output_image(read.events)


def _trim_chronological(
    messages: list[AssistantMessage], limit: int
) -> list[AssistantMessage]:
    """Keep newest visible records plus retained presence records."""
    kept_presence = _kept_presence_messages(
        [message for message in messages if message.kind.startswith("presence:")]
    )
    kept: list[AssistantMessage] = []
    visible = 0
    for message in reversed(messages):
        if message.kind.startswith("presence:"):
            continue
        if visible >= limit:
            continue
        kept.append(message)
        visible += 1
    kept.extend(kept_presence)
    return sorted(
        {message.key: message for message in kept}.values(),
        key=lambda message: message.index,
    )


def _kept_presence_messages(messages: list[AssistantMessage]) -> list[AssistantMessage]:
    feedback = [message for message in messages if message.is_supervisor_feedback]
    latest_presence = next(
        (
            message
            for message in reversed(messages)
            if not message.is_supervisor_feedback
        ),
        None,
    )
    kept = [*feedback]
    if latest_presence is not None:
        kept.append(latest_presence)
    return sorted(
        {message.key: message for message in kept}.values(),
        key=lambda message: message.index,
    )


def activity_status(messages: list[AssistantMessage]) -> str:
    if not messages:
        return "unknown"
    timestamp = parse_timestamp(messages[0].timestamp)
    if timestamp is None:
        return "unknown"
    age_seconds = (datetime.now(UTC) - timestamp).total_seconds()
    if age_seconds < ACTIVE_ASSISTANT_SECONDS:
        return "active"
    if age_seconds < ACTIVEISH_ASSISTANT_SECONDS:
        return "active-ish"
    return "inactive"
