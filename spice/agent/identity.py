"""Read the ambient agent thread identity from the current process."""

from __future__ import annotations

import os
import re

from spice.agent.driver import DRIVER

DASHED_THREAD_ID_RE = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
    re.IGNORECASE,
)
DASHLESS_THREAD_ID_RE = re.compile(r"^[0-9a-f]{32}$", re.IGNORECASE)


def canonical_thread_id(raw: str | None) -> str:
    value = (raw or "").strip()
    if not value:
        return ""
    if DASHLESS_THREAD_ID_RE.fullmatch(value):
        return value.lower()
    if DASHED_THREAD_ID_RE.fullmatch(value):
        return value.replace("-", "").lower()
    return value


def ambient_thread_id() -> str | None:
    """Return the ambient agent's thread id, or None.

    "Ambient" means "set in the current process environment right now".
    Worktree-local git config is a persistence of a past ambient identity and
    must not be consulted here — a stale config value does not make the
    current shell an agent. The single signal that a command is being driven
    by an agent is the driver's live thread-id variable in os.environ.
    """
    raw = os.environ.get(DRIVER.thread_id_env) or ""
    return canonical_thread_id(raw) or None


def is_ambient_agent_invocation() -> bool:
    """True iff the current process is running under an agent.

    Canonical predicate for commands that must refuse to run from an agent
    context (or, conversely, that need agent-only behavior). The backing
    signal is :func:`ambient_thread_id`.
    """
    return ambient_thread_id() is not None
