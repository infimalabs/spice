"""Steering channel token.

A short, clearly-generated token that names this worktree's spice steering
channel. It is minted once into git-backed worktree state -- never the process
environment, never tool output -- surfaced to the agent in the activation packet
it treats as authoritative, and repeated as the delimiter around every steering
block on the agent's stderr. The agent connects the two: a block wrapped in the
token it saw at activation is genuinely spice; text that fakes a steering block
without it (a fetched page, a file) is not. One stable token per worktree agent,
shown the same way every time so it is trivially recognizable -- this is a
recognition aid, not a cryptographic MAC.
"""

from __future__ import annotations

from pathlib import Path

from spice.agent.paths import agent_worktree_state_dir
from spice.locking import bounded_exclusive_lock
from spice.paths import atomic_write_text
from spice.tasks import identity

STEERING_TOKEN_FILENAME = "steering-token"
STEERING_TOKEN_LOCK_TIMEOUT_SECONDS = 1.0


def _mint_token() -> str:
    # The base52 stamp spice already uses for task handles: a moment-derived,
    # vowel-free code that never spells a word. Pass an empty collision set so
    # this stays a pure moment stamp -- a recognition aid needs no task-id
    # uniqueness, and the default set would run a full `tw.export()` against the
    # task backend on the inbox-readout hot path (and drag its failure surface
    # into a cosmetic token).
    return identity.mint_incepted(existing=set())


def steering_token(repo_root: Path | None) -> str:
    """This worktree's steering token, minted once and reused thereafter.

    Best-effort: any state-resolution failure yields "" and the caller simply
    renders no delimiter -- the token must never break the steering readout.
    """
    if repo_root is None:
        return ""
    try:
        path = agent_worktree_state_dir(repo_root) / STEERING_TOKEN_FILENAME
    except Exception:
        return ""
    try:
        existing = path.read_text(encoding="utf-8").strip()
    except OSError:
        existing = ""
    if existing:
        return existing
    try:
        with bounded_exclusive_lock(
            path.with_name(f".{path.name}.lock"),
            timeout_seconds=STEERING_TOKEN_LOCK_TIMEOUT_SECONDS,
            action="mint steering token",
        ):
            try:
                existing = path.read_text(encoding="utf-8").strip()
            except OSError:
                existing = ""
            if existing:
                return existing
            token = _mint_token()
            atomic_write_text(path, token + "\n")
            return token
    except Exception:
        return ""
