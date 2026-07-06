"""Per-thread agent reply log.

`spice agent reply` retires steering keys without emitting assistant prose, so
the lane has nothing to render for that turn. It appends one record here per
reply submission; the serve message payload reads these and synthesizes a single
lane card (response text + acknowledged keys + ACK chip) per submission, exactly
as if the agent had replied in prose. This is a spice-owned log, decoupled from
the ack-state database, so it never double-renders a prose ACK that already has
its own transcript card.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from spice.agent.paths import agent_thread_state_dir

REPLY_LOG_FILENAME = "replies.jsonl"


def reply_log_path(repo_root: Path, thread_id: str) -> Path:
    return agent_thread_state_dir(repo_root, thread_id) / REPLY_LOG_FILENAME


def append_reply_record(
    repo_root: Path,
    thread_id: str,
    *,
    timestamp: str,
    text: str,
    ack_keys: list[str],
    nack_keys: list[str],
) -> None:
    """Append one reply submission. One line == one lane card."""
    path = reply_log_path(repo_root, thread_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    record = {
        "timestamp": timestamp,
        "text": text,
        "ackKeys": list(ack_keys),
        "nackKeys": list(nack_keys),
    }
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, separators=(",", ":")) + "\n")


def read_reply_records(repo_root: Path, thread_id: str) -> list[dict[str, Any]]:
    try:
        path = reply_log_path(repo_root, thread_id)
    except Exception:
        # No resolvable worktree state (e.g. a synthetic target) has no replies;
        # reply cards are best-effort and must never break the message payload.
        return []
    if not path.is_file():
        return []
    records: list[dict[str, Any]] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(record, dict) and record.get("timestamp"):
            records.append(record)
    return records
