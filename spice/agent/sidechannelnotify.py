"""Notifier helpers for the agent side-channel."""

from __future__ import annotations

import contextlib
import json
import os
import socket
from pathlib import Path
from threading import Lock

from spice.agent.paths import agent_worktree_state_dir
from spice.errors import SpiceError

SIDE_CHANNEL_NOTIFY_EVENT = "notify"
SIDE_CHANNEL_INBOX_EVENT = "inbox"
SIDE_CHANNEL_NOTICE_EVENT = "notice"

_NOTICE_LOCK = Lock()
_NOTICES_BY_REPO_ROOT: dict[str, list[str]] = {}


def side_channel_marker_path(repo_root: Path) -> Path:
    return agent_worktree_state_dir(repo_root) / "stderr.sock"


def active_agent_side_channel_socket_path(repo_root: Path | None) -> Path | None:
    if repo_root is None:
        return None
    try:
        marker_path = side_channel_marker_path(repo_root)
    except SpiceError as exc:
        if str(exc) != "not inside a git worktree":
            raise
        return None
    try:
        raw_socket_path = marker_path.read_text(encoding="utf-8").strip()
    except OSError:
        return None
    if not raw_socket_path:
        return None
    return Path(raw_socket_path)


def notify_agent_side_channel(
    repo_root: Path | None, *, event: str = SIDE_CHANNEL_INBOX_EVENT
) -> None:
    socket_path = active_agent_side_channel_socket_path(repo_root)
    if socket_path is None:
        return
    side_socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        side_socket.connect(str(socket_path))
        side_socket.sendall(
            json.dumps(
                side_channel_notify_hello(repo_root, event=event),
                separators=(",", ":"),
            ).encode("utf-8")
            + b"\n"
        )
    except OSError:
        return
    finally:
        with contextlib.suppress(OSError):
            side_socket.close()


def side_channel_notify_hello(
    repo_root: Path | None, *, event: str = SIDE_CHANNEL_INBOX_EVENT
) -> dict[str, object]:
    resolved_root = repo_root.expanduser().resolve() if repo_root is not None else None
    return {
        "type": "hello",
        "pid": os.getpid(),
        "ppid": os.getppid(),
        "runner": "inbox.notify",
        "cwd": str(resolved_root or Path.cwd()),
        "repoRoot": str(resolved_root or ""),
        SIDE_CHANNEL_NOTIFY_EVENT: event,
    }


def publish_side_channel_notice(repo_root: Path | None, text: str) -> str | None:
    """Queue in-process stderr feedback for the current supervisor side-channel."""
    clean = _clean_notice_text(text)
    key = _notice_queue_key(repo_root)
    if key is None or not clean:
        return None
    with _NOTICE_LOCK:
        _NOTICES_BY_REPO_ROOT.setdefault(key, []).append(clean)
    notify_agent_side_channel(repo_root, event=SIDE_CHANNEL_NOTICE_EVENT)
    return clean


def consume_side_channel_notices(repo_root: Path | None) -> list[str]:
    """Read and remove queued in-process stderr feedback in publish order."""
    key = _notice_queue_key(repo_root)
    if key is None:
        return []
    with _NOTICE_LOCK:
        return _NOTICES_BY_REPO_ROOT.pop(key, [])


def _notice_queue_key(repo_root: Path | None) -> str | None:
    if repo_root is None:
        return None
    try:
        return str(repo_root.expanduser().resolve())
    except OSError:
        return str(repo_root.expanduser())


def _clean_notice_text(text: str) -> str:
    return "\n".join(line.rstrip() for line in text.strip().splitlines())
