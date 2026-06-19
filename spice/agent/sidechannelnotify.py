"""Notifier helpers for the agent side-channel."""

from __future__ import annotations

import contextlib
from datetime import UTC, datetime
import json
import os
import socket
import uuid
from pathlib import Path

from spice.agent.paths import agent_worktree_state_dir
from spice.errors import SpiceError
from spice.paths import atomic_write_text

SIDE_CHANNEL_NOTIFY_EVENT = "notify"
SIDE_CHANNEL_INBOX_EVENT = "inbox"
SIDE_CHANNEL_NOTICE_EVENT = "notice"
SIDE_CHANNEL_NOTICE_DIRNAME = "stderr-notices"
SIDE_CHANNEL_NOTICE_SUFFIX = ".txt"


def side_channel_marker_path(repo_root: Path) -> Path:
    return agent_worktree_state_dir(repo_root) / "stderr.sock"


def side_channel_notice_dir(repo_root: Path) -> Path:
    return agent_worktree_state_dir(repo_root) / SIDE_CHANNEL_NOTICE_DIRNAME


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


def publish_side_channel_notice(repo_root: Path | None, text: str) -> Path | None:
    """Queue transient stderr feedback for the next side-channel payload."""
    clean = _clean_notice_text(text)
    if repo_root is None or not clean:
        return None
    try:
        directory = side_channel_notice_dir(repo_root)
    except SpiceError:
        return None
    name = (
        datetime.now(UTC).strftime("%Y%m%dT%H%M%S%fZ")
        + f".{os.getpid()}.{uuid.uuid4().hex}{SIDE_CHANNEL_NOTICE_SUFFIX}"
    )
    path = atomic_write_text(directory / name, clean + "\n")
    notify_agent_side_channel(repo_root, event=SIDE_CHANNEL_NOTICE_EVENT)
    return path


def consume_side_channel_notices(repo_root: Path | None) -> list[str]:
    """Read and remove queued transient stderr feedback in publish order."""
    if repo_root is None:
        return []
    try:
        directory = side_channel_notice_dir(repo_root)
    except SpiceError:
        return []
    try:
        entries = sorted(
            path
            for path in directory.iterdir()
            if path.is_file() and path.name.endswith(SIDE_CHANNEL_NOTICE_SUFFIX)
        )
    except OSError:
        return []
    notices: list[str] = []
    for path in entries:
        claimed = path.with_name(f".{path.name}.{os.getpid()}.claim")
        try:
            path.rename(claimed)
        except OSError:
            continue
        try:
            text = claimed.read_text(encoding="utf-8")
        except OSError:
            with contextlib.suppress(FileNotFoundError):
                claimed.unlink()
            continue
        with contextlib.suppress(FileNotFoundError):
            claimed.unlink()
        clean = _clean_notice_text(text)
        if clean:
            notices.append(clean)
    return notices


def _clean_notice_text(text: str) -> str:
    return "\n".join(line.rstrip() for line in text.strip().splitlines())
