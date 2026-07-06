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
from spice.tasks import identity

STEERING_TOKEN_FILENAME = "steering-token"


def _mint_token() -> str:
    # The base52 stamp spice already uses for task handles: a moment-derived,
    # vowel-free code that never spells a word.
    return identity.mint_incepted()


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
    token = _mint_token()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".tmp")
        tmp.write_text(token + "\n", encoding="utf-8")
        tmp.replace(path)
    except OSError:
        pass
    return token
