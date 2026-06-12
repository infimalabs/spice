"""Drive semantics: steering that points the agent at the task queue."""

from __future__ import annotations

from spice.mail.inbox import INBOX_CONTROL_DRAIN_QUEUE


def drive_drain_queue_controls(enabled: bool) -> tuple[str, ...]:
    return (INBOX_CONTROL_DRAIN_QUEUE,) if enabled else ()
