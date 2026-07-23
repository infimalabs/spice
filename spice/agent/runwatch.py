"""Lifetime-bound side-channel watcher for the agent-run child process."""

from __future__ import annotations

import contextlib
import json
import os
import select
import socket
from pathlib import Path
from threading import Thread
from typing import Any, Protocol, TextIO

from spice.agent.sidechannelnotify import active_agent_side_channel_socket_path

AGENT_RUN_SIDE_CHANNEL_READ_BYTES = 8192
AGENT_RUN_SIDE_CHANNEL_CONNECT_TIMEOUT_S = 5.0
InboxSignature = tuple[tuple[str, int, int], ...]


class _KqueueHandle(Protocol):
    def fileno(self) -> int: ...

    def close(self) -> None: ...

    def control(
        self, changelist: Any, max_events: int, timeout: float | None = None
    ) -> Any: ...


def _select_has_attrs(*names: str) -> bool:
    return all(hasattr(select, name) for name in names)


def _select_attr(name: str) -> Any:
    return getattr(select, name)


def start_agent_side_channel_watch(
    repo_root: Path | None,
    *,
    parent_pid: int,
    stderr: TextIO,
    initial_inbox_signature: InboxSignature | None = None,
) -> Thread | None:
    if parent_pid <= 0 or active_agent_side_channel_socket_path(repo_root) is None:
        return None
    thread = Thread(
        target=watch_agent_side_channel,
        kwargs={
            "repo_root": repo_root,
            "parent_pid": parent_pid,
            "stderr": stderr,
            "initial_inbox_signature": initial_inbox_signature,
        },
        daemon=True,
    )
    thread.start()
    return thread


def join_agent_side_channel_watch(thread: Thread | None) -> None:
    if thread is not None:
        thread.join(timeout=1.0)


def watch_agent_side_channel(
    repo_root: Path | None,
    *,
    parent_pid: int,
    stderr: TextIO,
    initial_inbox_signature: InboxSignature | None = None,
) -> None:
    socket_path = active_agent_side_channel_socket_path(repo_root)
    if socket_path is None:
        return
    side_socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    side_socket.settimeout(AGENT_RUN_SIDE_CHANNEL_CONNECT_TIMEOUT_S)
    parent_exit = _parent_exit_watcher(parent_pid)
    try:
        if parent_pid > 0 and parent_exit is None and _process_has_exited(parent_pid):
            return
        side_socket.connect(str(socket_path))
        side_socket.sendall(
            json.dumps(
                agent_side_channel_hello(
                    repo_root,
                    runner="agent.run.watch",
                    stream_until_parent_exit=parent_pid,
                    initial_inbox_signature=initial_inbox_signature,
                ),
                separators=(",", ":"),
            ).encode("utf-8")
            + b"\n"
        )
        side_socket.settimeout(None)
        read_targets: list[socket.socket | _ParentExitWatcher] = [side_socket]
        if parent_exit is not None:
            read_targets.append(parent_exit)
        while True:
            readable, _, _ = select.select(read_targets, [], [])
            if parent_exit is not None and parent_exit in readable:
                return
            if side_socket not in readable:
                continue
            chunk = side_socket.recv(AGENT_RUN_SIDE_CHANNEL_READ_BYTES)
            if not chunk:
                return
            write_side_channel_chunk(stderr, chunk)
    except OSError:
        return
    finally:
        with contextlib.suppress(OSError):
            side_socket.close()
        if parent_exit is not None:
            parent_exit.close()


def agent_side_channel_hello(
    repo_root: Path | None,
    *,
    runner: str = "agent.run",
    stream_until_parent_exit: int | None = None,
    initial_inbox_signature: InboxSignature | None = None,
) -> dict[str, object]:
    hello: dict[str, object] = {
        "type": "hello",
        "pid": os.getpid(),
        "ppid": os.getppid(),
        "runner": runner,
        "cwd": os.getcwd(),
        "repoRoot": str(repo_root) if repo_root is not None else "",
    }
    if stream_until_parent_exit is not None:
        hello["streamUntilParentExit"] = stream_until_parent_exit
        if initial_inbox_signature is not None:
            hello["initialInboxSignature"] = [
                list(row) for row in initial_inbox_signature
            ]
    return hello


def write_side_channel_chunk(stderr: TextIO, chunk: bytes) -> None:
    buffer = getattr(stderr, "buffer", None)
    if buffer is not None:
        buffer.write(chunk)
        buffer.flush()
        return
    stderr.write(chunk.decode("utf-8", errors="replace"))
    stderr.flush()


class _ParentExitWatcher:
    def __init__(self, handle: int | _KqueueHandle):
        self.handle = handle

    def fileno(self) -> int:
        if isinstance(self.handle, int):
            return self.handle
        return self.handle.fileno()

    def close(self) -> None:
        if isinstance(self.handle, int):
            with contextlib.suppress(OSError):
                os.close(self.handle)
            return
        self.handle.close()


def _parent_exit_watcher(parent_pid: int) -> _ParentExitWatcher | None:
    if parent_pid <= 0:
        return None
    pidfd_open = getattr(os, "pidfd_open", None)
    if pidfd_open is not None:
        try:
            return _ParentExitWatcher(pidfd_open(parent_pid))
        except OSError:
            return None
    if _select_has_attrs(
        "kqueue",
        "kevent",
        "KQ_FILTER_PROC",
        "KQ_EV_ADD",
        "KQ_EV_ENABLE",
        "KQ_EV_ONESHOT",
        "KQ_NOTE_EXIT",
    ):
        try:
            kqueue: _KqueueHandle = _select_attr("kqueue")()
        except OSError:
            return None
        try:
            event = _select_attr("kevent")(
                parent_pid,
                filter=_select_attr("KQ_FILTER_PROC"),
                flags=(
                    _select_attr("KQ_EV_ADD")
                    | _select_attr("KQ_EV_ENABLE")
                    | _select_attr("KQ_EV_ONESHOT")
                ),
                fflags=_select_attr("KQ_NOTE_EXIT"),
            )
            kqueue.control([event], 0, 0)
            return _ParentExitWatcher(kqueue)
        except OSError:
            kqueue.close()
            return None
    return None


def _process_exists(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def _process_has_exited(pid: int) -> bool:
    """Detect an exited child without reaping it from the command owner.

    A child that has exited but has not yet been reaped still answers
    ``kill(pid, 0)``. On kqueue platforms that same zombie is too late for a new
    process-exit registration, so existence alone would leave the side-channel
    watcher with no exit handle. ``waitid(..., WNOWAIT)`` closes that gap while
    preserving the caller's later ``Popen.wait()``.
    """
    waitid = getattr(os, "waitid", None)
    waitid_names = ("P_PID", "WEXITED", "WNOHANG", "WNOWAIT")
    if waitid is not None and all(hasattr(os, name) for name in waitid_names):
        options = os.WEXITED | os.WNOHANG | os.WNOWAIT
        try:
            if waitid(os.P_PID, pid, options) is not None:
                return True
        except ChildProcessError:
            pass
        except OSError:
            pass
    return not _process_exists(pid)
