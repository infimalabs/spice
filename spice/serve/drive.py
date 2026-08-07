"""Drive semantics: steering that points the agent at the task queue."""

from __future__ import annotations

from spice.mail.inbox import INBOX_CONTROL_DRAIN_QUEUE


DRIVEN_LIFETIMES = frozenset({"Drive", "Drain"})


def lifetime_drives_agent(lifetime: str) -> bool:
    """Whether Serve pushes this lane's agent forward on its own.

    Steer lanes sit parked between operator sends, so waking one is the
    operator's call. Drive and Drain lanes are the ones Serve keeps working.
    """
    return lifetime.strip() in DRIVEN_LIFETIMES


def drive_drain_queue_controls(enabled: bool) -> tuple[str, ...]:
    return (INBOX_CONTROL_DRAIN_QUEUE,) if enabled else ()
