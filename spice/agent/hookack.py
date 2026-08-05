"""Retire acknowledged inbox keys from the durable transcript at the hook.

Acknowledgment used to have exactly one producer: the stdout supervisor in
`spice.agent.watchdog`, reading the agent process's stdout pipe. That pipe is a
single point of failure with no liveness signal of its own. When it goes quiet
the supervisor keeps running and keeps its log open, it simply never sees
another assistant message -- so every inline ACK from that moment on silently
fails to retire, the operator watches keys sit pending, and the lane's only
remaining path is `spice agent reply` from the shell.

This is the second producer, placed where silence is not possible: the command
hook runs inside the agent's own shell, so it cannot be quietly dead while the
agent is still issuing commands. It reads the durable transcript -- which
carried the acknowledgment on the occasion the stdout stream did not -- forward
from a persisted byte cursor, folds it with the supervisor's own prose fold,
and hands each assembled message to the supervisor's own archival authority.

It narrates only what it retired. A key the supervisor already archived comes
back as already-acked and is passed over in silence, because this is a backstop
and not a second narrator; the one thing it does say, `ack.archived-at-hook`,
means the stdout stream missed an acknowledgment and the hook caught it, which
is the liveness signal that was missing before.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, TextIO

from spice.agent.driver import AgentDriver, driver_for
from spice.agent.paths import agent_state_dir, current_agent_thread_id
from spice.agent.runinbox import write_side_channel_notices
from spice.agent.watchdog import SupervisedProseFold, archive_message_acks
from spice.mail.ackarchive import summarize_nack_archival
from spice.mail.feedback import supervisor_feedback_line
from spice.paths import atomic_write_json
from spice.transcript.reader import (
    TranscriptCursor,
    TranscriptEventReader,
    TranscriptFileIdentity,
    transcript_file_identity,
    transcript_size,
)

HOOK_ACK_CURSOR_FILENAME = "post-tool-hook-ack.json"


def post_tool_hook_ack_cursor_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / HOOK_ACK_CURSOR_FILENAME


@dataclass(frozen=True)
class HookAckSweep:
    """What one hook-side sweep of the durable transcript actually read.

    A sweep that found no transcript, one that primed its cursor past existing
    history, one that read no new records, and one that read records carrying
    no acknowledgment all retire nothing -- and a backstop that read nothing at
    all must not report the same result as a backstop that read everything and
    had nothing to do. Each of those outcomes is a distinct field here so the
    difference survives into whatever the caller asserts on.
    """

    transcript: Path | None = None
    primed: bool = False
    records: int = 0
    messages: int = 0
    archived: tuple[str, ...] = ()
    refused: tuple[str, ...] = ()
    error: str | None = None


@dataclass
class _Retirement:
    """The keys one sweep retired, accumulated across assembled messages."""

    archived: list[str] = field(default_factory=list)
    refused: list[str] = field(default_factory=list)
    messages: int = 0


def sweep_transcript_acks(
    repo_root: Path,
    *,
    stderr: TextIO,
    state_path: Path | None = None,
) -> HookAckSweep:
    """Retire keys acknowledged in transcript records this hook has not read.

    Reads forward from the persisted cursor, so a message is swept once no
    matter how many commands the agent runs afterwards, and a transcript
    replaced under the cursor restarts rather than resuming into another
    session's bytes.
    """
    driver = driver_for(repo_root)
    path = _transcript_path(repo_root, driver)
    if path is None:
        return HookAckSweep()
    cursor_path = state_path or post_tool_hook_ack_cursor_path(repo_root)
    cursor = _load_cursor(cursor_path)
    if cursor is None:
        return _prime_cursor(path, cursor_path)
    read = TranscriptEventReader(path=path, driver=driver).read(
        "forward", cursor=cursor
    )
    _store_cursor(cursor_path, cursor)
    if read.error is not None:
        return HookAckSweep(transcript=path, error=read.error)
    retirement = _Retirement()
    fold = SupervisedProseFold(
        lambda text: _retire_message(repo_root, text, stderr, retirement),
        on_text_starvation=lambda count: None,
        on_activity=lambda: None,
    )
    fold.push(read.events)
    _publish_hook_retirement(stderr, retirement)
    return HookAckSweep(
        transcript=path,
        records=read.stats.lines_parsed,
        messages=retirement.messages,
        archived=tuple(retirement.archived),
        refused=tuple(retirement.refused),
    )


def _transcript_path(repo_root: Path, driver: AgentDriver) -> Path | None:
    """The transcript of the agent this worktree currently seats, if any."""
    thread_id = current_agent_thread_id(repo_root)
    if not thread_id:
        return None
    return driver.find_session_transcript(thread_id)


def _retire_message(
    repo_root: Path, text: str, stderr: TextIO, retirement: _Retirement
) -> None:
    """Run one assembled message through both keyed-response authorities."""
    retirement.messages += 1
    ack_summary = archive_message_acks(repo_root, text, stderr)
    if ack_summary is not None:
        retirement.archived.extend(ack_summary.archived)
    try:
        nack_summary = summarize_nack_archival(repo_root, text)
    except Exception as exc:  # surface-and-survive: the ACK sweep already landed
        stderr.write(f"spice nack archival hook error: {exc}\n")
        stderr.flush()
        return
    retirement.refused.extend(nack_summary.refused)


def _publish_hook_retirement(stderr: TextIO, retirement: _Retirement) -> None:
    """Say only what the stdout stream failed to say.

    Written straight into the readout this hook is already building rather than
    queued for the side channel: that queue is drained by the supervisor's
    socket server, which is another process entirely, so a notice published
    here would reach nobody and die with the command that produced it.
    """
    write_side_channel_notices(
        stderr,
        [
            supervisor_feedback_line(kind, keys=keys)
            for kind, keys in (
                ("ack.archived-at-hook", retirement.archived),
                ("nack.refused-at-hook", retirement.refused),
            )
            if keys
        ],
    )


def _prime_cursor(path: Path, cursor_path: Path) -> HookAckSweep:
    """Adopt the transcript's current end without sweeping its history.

    A lane that installs this backstop mid-session has a transcript full of
    acknowledgments the supervisor already handled; replaying them would mirror
    stale steering onto whatever task happens to be claimed now.
    """
    size = transcript_size(path)
    if size is None:
        return HookAckSweep(transcript=path, error="transcript size unavailable")
    cursor = TranscriptCursor(offset=size, file_identity=transcript_file_identity(path))
    _store_cursor(cursor_path, cursor)
    return HookAckSweep(transcript=path, primed=True)


def _load_cursor(path: Path) -> TranscriptCursor | None:
    """Rebuild the persisted cursor, or None when this hook has never swept."""
    payload = _read_cursor_payload(path)
    offset = payload.get("offset")
    if not isinstance(offset, int) or isinstance(offset, bool) or offset < 0:
        return None
    return TranscriptCursor(
        offset=offset, file_identity=_file_identity_payload(payload.get("file"))
    )


def _store_cursor(path: Path, cursor: TranscriptCursor) -> None:
    identity = cursor.file_identity
    atomic_write_json(
        path,
        {
            "offset": cursor.offset,
            "file": (
                None
                if identity is None
                else {"device": identity.device, "inode": identity.inode}
            ),
        },
        compact=True,
        sort_keys=True,
    )


def _read_cursor_payload(path: Path) -> dict[str, Any]:
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return loaded if isinstance(loaded, dict) else {}


def _file_identity_payload(value: Any) -> TranscriptFileIdentity | None:
    if not isinstance(value, dict):
        return None
    device = value.get("device")
    inode = value.get("inode")
    if not isinstance(device, int) or not isinstance(inode, int):
        return None
    return TranscriptFileIdentity(device=device, inode=inode)
