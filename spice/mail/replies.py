"""Per-thread agent reply log.

`spice agent reply` retires steering keys without emitting assistant prose, so
the lane may have nothing to render for that turn. A newly consumed reply is
appended here and Serve synthesizes its own lane card immediately. Transcript
prose remains an independently authored message and is not merged with it.
Legacy rows carrying a redundant leading key are migrated atomically to the
same canonical header-first shape used for new appends.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from spice.agent.paths import agent_thread_state_dir
from spice.locking import bounded_exclusive_lock
from spice.mail.inbox import inbox_item_key
from spice.paths import atomic_write_text

REPLY_LOG_FILENAME = "replies.jsonl"
REPLY_LOG_LOCK_FILENAME = "replies.lock"
REPLY_LOG_LOCK_TIMEOUT_SECONDS = 2.0


def canonical_reply_text(
    text: str,
    *,
    ack_keys: list[str],
    nack_keys: list[str],
) -> str:
    """Remove redundant positional keys before the first ACK/NACK header.

    Older callers sometimes invoked ``spice agent reply <key> 'ACK <key>: …'``.
    ``reply`` accepts free-form positional text, so that first argument became
    visible preamble. A leading token is removable only when the authoritative
    parser also found it in this reply's ACK/NACK headers; arbitrary prose is
    preserved.
    """
    value = str(text or "").strip()
    response_keys = {
        inbox_item_key(key) for key in (*ack_keys, *nack_keys) if str(key).strip()
    }
    if not value or not response_keys:
        return value
    candidate = value
    removed = False
    while True:
        parts = candidate.split(maxsplit=1)
        if len(parts) != 2 or inbox_item_key(parts[0]) not in response_keys:
            break
        candidate = parts[1]
        removed = True
    if removed and candidate.split(maxsplit=1)[0] in {"ACK", "NACK"}:
        return candidate
    return value


def reply_log_path(repo_root: Path, thread_id: str) -> Path:
    return agent_thread_state_dir(repo_root, thread_id) / REPLY_LOG_FILENAME


def reply_log_lock_path(repo_root: Path, thread_id: str) -> Path:
    return agent_thread_state_dir(repo_root, thread_id) / REPLY_LOG_LOCK_FILENAME


def ensure_reply_log(repo_root: Path, thread_id: str) -> Path | None:
    """Best-effort create-and-return the reply log so watchers can arm it.

    The serve lane watcher arms file descriptors, and an append to a file never
    wakes a watch on its parent directory, so the log must exist before the
    first reply lands. Created only when missing — an unconditional touch would
    churn the mtime the lane signature reads. Returns None when worktree state
    is unresolvable (e.g. a synthetic target).
    """
    try:
        path = reply_log_path(repo_root, thread_id)
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists():
            path.touch()
    except Exception:
        return None
    return path


def append_reply_record(
    repo_root: Path,
    thread_id: str,
    *,
    timestamp: str,
    text: str,
    ack_keys: list[str],
    nack_keys: list[str],
) -> None:
    """Append one reply submission. One line equals one reply-card identity."""
    path = reply_log_path(repo_root, thread_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    record = {
        "timestamp": timestamp,
        "text": canonical_reply_text(
            text,
            ack_keys=ack_keys,
            nack_keys=nack_keys,
        ),
        "ackKeys": list(ack_keys),
        "nackKeys": list(nack_keys),
    }
    with bounded_exclusive_lock(
        reply_log_lock_path(repo_root, thread_id),
        timeout_seconds=REPLY_LOG_LOCK_TIMEOUT_SECONDS,
        action="append agent reply",
    ):
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
    records, migrated_text = _read_reply_log_snapshot(path)
    if migrated_text is None:
        return records
    with bounded_exclusive_lock(
        reply_log_lock_path(repo_root, thread_id),
        timeout_seconds=REPLY_LOG_LOCK_TIMEOUT_SECONDS,
        action="migrate agent reply log",
    ):
        records, migrated_text = _read_reply_log_snapshot(path)
        if migrated_text is not None:
            atomic_write_text(path, migrated_text)
    return records


def _read_reply_log_snapshot(
    path: Path,
) -> tuple[list[dict[str, Any]], str | None]:
    raw = path.read_text(encoding="utf-8")
    records: list[dict[str, Any]] = []
    migrated_lines: list[str] = []
    changed = False
    for original_line in raw.splitlines():
        line = original_line.strip()
        if not line:
            migrated_lines.append(original_line)
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            migrated_lines.append(original_line)
            continue
        if isinstance(record, dict) and record.get("timestamp"):
            normalized = _normalize_reply_record(record)
            records.append(normalized)
            if normalized != record:
                migrated_lines.append(json.dumps(normalized, separators=(",", ":")))
                changed = True
            else:
                migrated_lines.append(original_line)
            continue
        migrated_lines.append(original_line)
    if not changed:
        return records, None
    migrated_text = "\n".join(migrated_lines)
    if raw.endswith("\n"):
        migrated_text += "\n"
    return records, migrated_text


def _normalize_reply_record(record: dict[str, Any]) -> dict[str, Any]:
    text = record.get("text")
    if not isinstance(text, str):
        return dict(record)
    ack_keys = record.get("ackKeys")
    nack_keys = record.get("nackKeys")
    normalized = dict(record)
    normalized["text"] = canonical_reply_text(
        text,
        ack_keys=[str(key) for key in ack_keys] if isinstance(ack_keys, list) else [],
        nack_keys=[str(key) for key in nack_keys]
        if isinstance(nack_keys, list)
        else [],
    )
    return normalized
