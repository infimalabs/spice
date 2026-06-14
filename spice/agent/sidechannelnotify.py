"""Notifier helpers for the agent side-channel."""

from __future__ import annotations

import contextlib
import json
import os
import socket
from pathlib import Path

from spice.agent.driver import driver_for
from spice.paths import STATE_DIRNAME

SIDE_CHANNEL_NOTIFY_EVENT = "notify"
SIDE_CHANNEL_INBOX_EVENT = "inbox"


def side_channel_marker_path(repo_root: Path) -> Path:
    return (
        repo_root
        / STATE_DIRNAME
        / "agents"
        / driver_for(repo_root).state_dirname
        / "side-channel"
        / "socket"
    )


def active_agent_side_channel_socket_path(repo_root: Path | None) -> Path | None:
    if repo_root is None:
        return None
    marker_path = side_channel_marker_path(repo_root)
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
