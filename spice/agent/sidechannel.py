"""The supervisor side-channel: a Unix socket the agent wrapper greets.

The supervisor binds a socket in the tmp dir, publishes its path through a
marker file under `.spice/agents/<driver>/side-channel/socket`, and answers
each wrapper hello with the same payload the wrapper would synthesize itself
(pending inbox readout + context-pressure warning). The agent hears the
operator through stderr without anyone touching its stdin.
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import socket
import tempfile
from collections.abc import Callable
from pathlib import Path
from threading import Event, Thread

from spice.agent.wrap import (
    AGENT_RUN_CONTEXT_WARNING_REPEAT_SECONDS,
    AGENT_RUN_INBOX_REPEAT_SECONDS,
    AgentContextMeterInjector,
    AgentInboxInjector,
    agent_context_meter,
    side_channel_marker_path,
)

SOCKET_READ_BYTES = 8192
LISTENER_ACCEPT_TIMEOUT_S = 0.1


class AgentSideChannelServer:
    def __init__(
        self,
        repo_root: Path,
        *,
        payload_factory: Callable[[Path], str] | None = None,
    ) -> None:
        self.repo_root = repo_root
        self.payload_factory = payload_factory or render_side_channel_payload
        self.socket_marker_path = side_channel_marker_path(repo_root)
        socket_name = f"spice-agent-side-{os.getpid()}.sock"
        self.socket_path = Path(tempfile.gettempdir()) / socket_name
        self._listener: socket.socket | None = None
        self._thread: Thread | None = None
        self._stopping = Event()

    def __enter__(self) -> AgentSideChannelServer:
        self.start()
        return self

    def __exit__(self, *_exc: object) -> None:
        self.stop()

    def start(self) -> None:
        self.socket_marker_path.parent.mkdir(parents=True, exist_ok=True)
        with contextlib.suppress(FileNotFoundError):
            self.socket_path.unlink()
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(str(self.socket_path))
        listener.listen()
        listener.settimeout(LISTENER_ACCEPT_TIMEOUT_S)
        self._listener = listener
        self._write_socket_marker()
        self._thread = Thread(
            target=self._serve,
            name=f"spice-agent-side-channel-{os.getpid()}",
            daemon=True,
        )
        self._thread.start()

    def stop(self) -> None:
        self._stopping.set()
        if self._listener is not None:
            with contextlib.suppress(OSError):
                self._listener.close()
        if self._thread is not None:
            self._thread.join(timeout=1.0)
        with contextlib.suppress(FileNotFoundError):
            self.socket_path.unlink()
        self._remove_socket_marker()

    def _serve(self) -> None:
        listener = self._listener
        if listener is None:
            return
        while not self._stopping.is_set():
            try:
                connection, _address = listener.accept()
            except TimeoutError:
                continue
            except OSError:
                return
            Thread(
                target=self._handle_connection,
                args=(connection,),
                name=f"spice-agent-side-channel-client-{os.getpid()}",
                daemon=True,
            ).start()

    def _handle_connection(self, connection: socket.socket) -> None:
        with connection:
            try:
                line = _read_line(connection)
            except OSError:
                return
            payload = parse_side_channel_hello(line)
            if payload:
                diagnostic = side_channel_binding_diagnostic(self.repo_root, payload)
                if diagnostic:
                    with contextlib.suppress(OSError):
                        connection.sendall(diagnostic.encode("utf-8", errors="replace"))
                    return
                message = self.payload_factory(self.repo_root)
                if message:
                    with contextlib.suppress(OSError):
                        connection.sendall(message.encode("utf-8", errors="replace"))
                return
            elif line:
                with contextlib.suppress(OSError):
                    connection.sendall(line)
            _echo_connection(connection)

    def _write_socket_marker(self) -> None:
        temp_path = self.socket_marker_path.with_name(
            f".{self.socket_marker_path.name}.{os.getpid()}.tmp"
        )
        temp_path.write_text(str(self.socket_path), encoding="utf-8")
        temp_path.replace(self.socket_marker_path)

    def _remove_socket_marker(self) -> None:
        try:
            active_socket = self.socket_marker_path.read_text(encoding="utf-8").strip()
        except OSError:
            return
        if active_socket == str(self.socket_path):
            with contextlib.suppress(FileNotFoundError):
                self.socket_marker_path.unlink()


def parse_side_channel_hello(line: bytes) -> dict[str, object] | None:
    try:
        payload = json.loads(line.decode("utf-8", errors="replace"))
    except json.JSONDecodeError:
        return None
    if not isinstance(payload, dict) or payload.get("type") != "hello":
        return None
    return payload


def side_channel_binding_diagnostic(repo_root: Path, hello: dict[str, object]) -> str:
    expected = repo_root.expanduser().resolve()
    rows: list[str] = []
    reported_root = _hello_path(hello.get("repoRoot"))
    if reported_root is None:
        rows.append("  wrapper_repo_root=-")
    elif reported_root.expanduser().resolve() != expected:
        rows.append(f"  wrapper_repo_root={reported_root.expanduser().resolve()}")
    reported_cwd = _hello_path(hello.get("cwd"))
    if reported_cwd is None:
        rows.append("  wrapper_cwd=-")
    else:
        resolved_cwd = reported_cwd.expanduser().resolve()
        if resolved_cwd != expected and not resolved_cwd.is_relative_to(expected):
            rows.append(f"  wrapper_cwd={resolved_cwd}")
    if not rows:
        return ""
    return "\n".join(
        [
            "Agent Binding Mismatch",
            f"  lane_repo_root={expected}",
            *rows,
            "  steering_delivery=refused",
            "  restart the lane agent from its own worktree",
            "",
        ]
    )


def _hello_path(value: object) -> Path | None:
    if not isinstance(value, str) or not value.strip():
        return None
    return Path(value).expanduser()


def render_side_channel_payload(repo_root: Path) -> str:
    stderr = io.StringIO()
    AgentInboxInjector(
        repo_root,
        stderr=stderr,
        repeat_interval_seconds=AGENT_RUN_INBOX_REPEAT_SECONDS,
    ).inject(force=True)
    AgentContextMeterInjector(
        repo_root,
        stderr=stderr,
        repeat_interval_seconds=AGENT_RUN_CONTEXT_WARNING_REPEAT_SECONDS,
        meter_factory=agent_context_meter,
    ).inject(force=True)
    return stderr.getvalue()


def _read_line(connection: socket.socket) -> bytes:
    raw = b""
    while not raw.endswith(b"\n"):
        chunk = connection.recv(1)
        if not chunk:
            break
        raw += chunk
        if len(raw) > SOCKET_READ_BYTES:
            break
    return raw


def _echo_connection(connection: socket.socket) -> None:
    while True:
        try:
            chunk = connection.recv(SOCKET_READ_BYTES)
        except OSError:
            return
        if not chunk:
            return
        with contextlib.suppress(OSError):
            connection.sendall(chunk)
