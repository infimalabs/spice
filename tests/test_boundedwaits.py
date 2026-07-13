"""Deadline and deterministic-failure contracts for agent side-channel
handshakes and helper probes (RELIABI-1kCzJcnr).

Each subprocess seam is stalled by injecting ``subprocess.TimeoutExpired`` and
asserting both that its named budget reaches ``subprocess.run`` and that the
caller degrades deterministically. Each socket seam is stalled with a real (or
faithfully faked) connection and observed to reap or to stay open per its
documented lifetime.
"""

from __future__ import annotations

import io
import json
import os
import socket
import subprocess
import time
from pathlib import Path
from threading import Event, Thread

import pytest

from spice import procs
from spice.agent import driver as agent_driver
from spice.agent import lifecycle, shadow, sidechannel, sidechannelnotify, wrap
from spice.agent.sidechannelnotify import (
    active_agent_side_channel_socket_path,
    side_channel_marker_path,
)
from spice.mail.inbox import compose_inbox_text, write_inbox_item


@pytest.fixture
def git_worktree(tmp_path: Path) -> Path:
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    return tmp_path


def _recording_run(
    box: dict[str, object], *, returncode: int = 0, stdout: str = "", stderr: str = ""
):
    def run(cmd, **kwargs):
        box["timeout"] = kwargs.get("timeout")
        return subprocess.CompletedProcess(
            cmd, returncode, stdout=stdout, stderr=stderr
        )

    return run


def _stalling_run(cmd, **kwargs):
    raise subprocess.TimeoutExpired(cmd, float(kwargs.get("timeout") or 0.0))


def _raise_permission_error(*_args: object, **_kwargs: object):
    raise PermissionError


def _raise_git_launch_error(*_args: object, **_kwargs: object):
    raise FileNotFoundError("git unavailable")


# --- Subprocess probe seams: macOS appearance lookup -----------------------


def test_operator_appearance_threads_named_budget(monkeypatch):
    monkeypatch.setattr(agent_driver.sys, "platform", "darwin")
    box: dict[str, object] = {}
    monkeypatch.setattr(
        agent_driver.subprocess,
        "run",
        _recording_run(box, returncode=0, stdout="Dark\n", stderr=""),
    )
    assert agent_driver.operator_color_scheme() == "dark"
    assert box["timeout"] == agent_driver.OPERATOR_APPEARANCE_TIMEOUT_SECONDS


def test_operator_appearance_defaults_to_light_when_probe_stalls(monkeypatch):
    monkeypatch.setattr(agent_driver.sys, "platform", "darwin")
    monkeypatch.setattr(agent_driver.subprocess, "run", _stalling_run)
    assert agent_driver.operator_color_scheme() == "light"


# --- Subprocess probe seams: process-liveness ps/taskkill helpers -----------


def test_process_id_running_bounds_probe_and_assumes_alive_on_stall(monkeypatch):
    # A permission-denied os.kill routes liveness to the bounded `ps` probe; a
    # wedged `ps` must degrade to assume-alive so a supervisor never reaps a
    # process it cannot positively confirm has exited.
    monkeypatch.setattr(procs.os, "kill", _raise_permission_error)
    box: dict[str, object] = {}
    monkeypatch.setattr(
        procs.subprocess, "run", _recording_run(box, returncode=0, stdout="S\n")
    )
    assert procs.process_id_is_running(424242) is True
    assert box["timeout"] == procs.PROCESS_PROBE_TIMEOUT_SECONDS
    monkeypatch.setattr(procs.subprocess, "run", _stalling_run)
    assert procs.process_id_is_running(424242) is True


def test_process_group_running_bounds_probe_and_assumes_alive_on_stall(monkeypatch):
    monkeypatch.setattr(procs.os, "kill", _raise_permission_error)
    box: dict[str, object] = {}
    monkeypatch.setattr(
        procs.subprocess, "run", _recording_run(box, returncode=0, stdout="S\n")
    )
    assert procs.process_group_is_running(424242) is True
    assert box["timeout"] == procs.PROCESS_PROBE_TIMEOUT_SECONDS
    monkeypatch.setattr(procs.subprocess, "run", _stalling_run)
    assert procs.process_group_is_running(424242) is True


def test_force_windows_terminate_bounds_and_reaches_terminal_after_taskkill_stall(
    monkeypatch,
):
    box: dict[str, object] = {}
    monkeypatch.setattr(procs.subprocess, "run", _recording_run(box, returncode=0))
    procs._force_windows_process_tree(424242)
    assert box["timeout"] == procs.PROCESS_PROBE_TIMEOUT_SECONDS
    # A wedged taskkill is swallowed so termination escalates rather than blocking.
    terminal_events: list[str] = []

    def stalling_taskkill(cmd, **kwargs):
        terminal_events.append("taskkill-timeout")
        _stalling_run(cmd, **kwargs)

    monkeypatch.setattr(procs.subprocess, "run", stalling_taskkill)
    procs._force_windows_process_tree(424242)
    terminal_events.append("termination-helper-returned")
    assert terminal_events == ["taskkill-timeout", "termination-helper-returned"]


# --- Subprocess probe seams: supervisor lifecycle git probes ----------------


def test_worktree_dirty_bounds_probe_and_resolves_clean_status_on_stall(
    monkeypatch, tmp_path
):
    box: dict[str, object] = {}
    monkeypatch.setattr(
        lifecycle.subprocess,
        "run",
        _recording_run(box, returncode=0, stdout=" M file\n"),
    )
    assert lifecycle._worktree_dirty(tmp_path) is True
    assert box["timeout"] == lifecycle.GIT_PROBE_TIMEOUT_SECONDS
    monkeypatch.setattr(lifecycle.subprocess, "run", _stalling_run)
    status = "dirty" if lifecycle._worktree_dirty(tmp_path) else "clean"
    assert status == "clean"


def test_worktree_dirty_resolves_clean_status_when_git_cannot_launch(
    monkeypatch, tmp_path
):
    monkeypatch.setattr(lifecycle.subprocess, "run", _raise_git_launch_error)

    status = "dirty" if lifecycle._worktree_dirty(tmp_path) else "clean"

    assert status == "clean"


def test_git_tracks_relative_path_bounds_probe_and_resolves_untracked_status_on_stall(
    monkeypatch, tmp_path
):
    box: dict[str, object] = {}
    monkeypatch.setattr(lifecycle.subprocess, "run", _recording_run(box, returncode=0))
    assert lifecycle.git_tracks_relative_path(tmp_path, Path("kept.txt")) is True
    assert box["timeout"] == lifecycle.GIT_PROBE_TIMEOUT_SECONDS
    monkeypatch.setattr(lifecycle.subprocess, "run", _stalling_run)
    status = (
        "tracked"
        if lifecycle.git_tracks_relative_path(tmp_path, Path("kept.txt"))
        else "untracked"
    )
    assert status == "untracked"


# --- Subprocess probe seams: git-shadow reads -------------------------------


def test_shadow_git_bounds_reads_and_resolves_unavailable_status_on_stall(
    monkeypatch, tmp_path
):
    box: dict[str, object] = {}
    monkeypatch.setattr(
        subprocess, "run", _recording_run(box, returncode=0, stdout="main\n")
    )
    assert shadow.current_git_branch(tmp_path) == "main"
    assert box["timeout"] == shadow.SHADOW_GIT_TIMEOUT_SECONDS
    monkeypatch.setattr(subprocess, "run", _stalling_run)
    status = {
        "branch": shadow.current_git_branch(tmp_path) or "unavailable",
        "git_dir": shadow.current_git_dir(tmp_path) or "unavailable",
    }
    assert status == {"branch": "unavailable", "git_dir": "unavailable"}


# --- Socket handshake seams: server reaps a silent peer ---------------------


def test_side_channel_reaps_peer_that_never_sends_hello(git_worktree, monkeypatch):
    monkeypatch.setattr(sidechannel, "SIDE_CHANNEL_HELLO_TIMEOUT_S", 0.3)
    with sidechannel.AgentSideChannelServer(git_worktree):
        socket_path = active_agent_side_channel_socket_path(git_worktree)
        assert isinstance(socket_path, Path)
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.connect(str(socket_path))
        client.settimeout(2.0)
        try:
            # Send no hello line: the server must reap the handler and close the
            # peer within the handshake budget rather than parking forever.
            observed = "peer-eof" if client.recv(1) == b"" else "peer-data"
        except TimeoutError:
            observed = "peer-open"
        finally:
            client.close()
    assert observed == "peer-eof"


def test_side_channel_reaps_trickling_peer_within_total_hello_budget(
    git_worktree, monkeypatch
):
    hello_budget_s = 0.3
    monkeypatch.setattr(sidechannel, "SIDE_CHANNEL_HELLO_TIMEOUT_S", hello_budget_s)
    with sidechannel.AgentSideChannelServer(git_worktree):
        socket_path = active_agent_side_channel_socket_path(git_worktree)
        assert isinstance(socket_path, Path)
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.connect(str(socket_path))
        client.settimeout(2.0)
        stop_trickling = Event()
        bytes_sent = Event()
        handler_closed = Event()

        def trickle_without_newline() -> None:
            for _attempt in range(20):
                if stop_trickling.wait(hello_budget_s / 4):
                    return
                try:
                    client.sendall(b"x")
                    bytes_sent.set()
                except OSError:
                    return

        trickler = Thread(target=trickle_without_newline)
        started_at = time.monotonic()
        trickler.start()
        try:
            observed = client.recv(1)
            if observed == b"":
                handler_closed.set()
            elapsed_s = time.monotonic() - started_at
        finally:
            stop_trickling.set()
            trickler.join(timeout=2.0)
            client.close()
    assert bytes_sent.wait(0)
    assert handler_closed.wait(0)
    assert elapsed_s < hello_budget_s * 2


def test_side_channel_stream_stays_open_past_hello_deadline(git_worktree, monkeypatch):
    # Agents greet from their own worktree; match that so the hello binds cleanly.
    monkeypatch.chdir(git_worktree)
    monkeypatch.setattr(sidechannel, "SIDE_CHANNEL_HELLO_TIMEOUT_S", 0.2)
    with sidechannel.AgentSideChannelServer(git_worktree):
        socket_path = active_agent_side_channel_socket_path(git_worktree)
        assert isinstance(socket_path, Path)
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.connect(str(socket_path))
        hello = wrap.agent_side_channel_hello(
            git_worktree,
            runner="agent.run.watch",
            stream_until_parent_exit=os.getpid(),
        )
        client.sendall(json.dumps(hello, separators=(",", ":")).encode("utf-8") + b"\n")
        # Wait well past the hello deadline, then publish: the established stream
        # is lifetime-bound, not governed by the handshake budget, so it delivers.
        time.sleep(0.5)
        write_inbox_item(
            git_worktree,
            "20260101T000000000009Z.txt",
            compose_inbox_text(
                body="post-deadline steering", priority=None, stop=False
            ),
        )
        received = _recv_until(client, needle=b"post-deadline steering", deadline_s=2.0)
        client.close()
    assert b"post-deadline steering" in received


# --- Socket handshake seams: notifier connect/send deadline -----------------


class _StallingSocket:
    """A socket whose connect stalls out at its deadline (like a wedged peer)."""

    def __init__(self) -> None:
        self.timeout: float | None = None
        self.events = ["created"]

    def settimeout(self, value: float | None) -> None:
        self.timeout = value
        self.events.append(f"timeout={value}")

    def connect(self, _address: object) -> None:
        self.events.append("connect-timeout")
        raise TimeoutError("connect timed out")

    def sendall(self, _data: object) -> None:
        self.events.append("send-complete")

    def close(self) -> None:
        self.events.append("closed")


def _point_marker_at_phantom_socket(repo_root: Path) -> None:
    marker = side_channel_marker_path(repo_root)
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.write_text(str(repo_root / "phantom.sock"), encoding="utf-8")


def test_notify_side_channel_bounds_connect_and_swallows_stall(
    git_worktree, monkeypatch
):
    _point_marker_at_phantom_socket(git_worktree)
    created: list[_StallingSocket] = []

    def fake_socket(*_a, **_k) -> _StallingSocket:
        stalling = _StallingSocket()
        created.append(stalling)
        return stalling

    monkeypatch.setattr(sidechannelnotify.socket, "socket", fake_socket)
    # A wedged supervisor socket must return cleanly so inbox publication is not
    # held behind the notification.
    sidechannelnotify.notify_agent_side_channel(git_worktree)
    assert len(created) == 1
    assert created[0].timeout == sidechannelnotify.SIDE_CHANNEL_NOTIFY_TIMEOUT_S
    assert created[0].events == [
        "created",
        f"timeout={sidechannelnotify.SIDE_CHANNEL_NOTIFY_TIMEOUT_S}",
        "connect-timeout",
        "closed",
    ]


def test_agent_run_watch_bounds_connect_and_swallows_stall(git_worktree, monkeypatch):
    _point_marker_at_phantom_socket(git_worktree)
    created: list[_StallingSocket] = []

    def fake_socket(*_a, **_k) -> _StallingSocket:
        stalling = _StallingSocket()
        created.append(stalling)
        return stalling

    monkeypatch.setattr(wrap.socket, "socket", fake_socket)
    stderr = io.StringIO()
    wrap.watch_agent_side_channel(git_worktree, parent_pid=os.getpid(), stderr=stderr)
    assert len(created) == 1
    assert created[0].timeout == wrap.AGENT_RUN_SIDE_CHANNEL_CONNECT_TIMEOUT_S
    assert created[0].events == [
        "created",
        f"timeout={wrap.AGENT_RUN_SIDE_CHANNEL_CONNECT_TIMEOUT_S}",
        "connect-timeout",
        "closed",
    ]


def test_agent_run_watch_streams_past_connect_deadline_and_stops_on_server_close(
    git_worktree, monkeypatch
):
    # Agents greet from their own worktree; match that so the hello binds cleanly.
    monkeypatch.chdir(git_worktree)
    monkeypatch.setattr(wrap, "AGENT_RUN_SIDE_CHANNEL_CONNECT_TIMEOUT_S", 0.2)
    stderr = io.StringIO()
    watch_stopped = Event()

    def watch_to_terminal() -> None:
        try:
            wrap.watch_agent_side_channel(
                repo_root=git_worktree,
                parent_pid=os.getpid(),
                stderr=stderr,
            )
        finally:
            watch_stopped.set()

    with sidechannel.AgentSideChannelServer(git_worktree):
        thread = Thread(target=watch_to_terminal)
        thread.start()
        # Past the connect deadline: the established stream is lifetime-bound and
        # still delivers a later publish to stderr.
        time.sleep(0.5)
        write_inbox_item(
            git_worktree,
            "20260101T000000000011Z.txt",
            compose_inbox_text(body="late-after-deadline", priority=None, stop=False),
        )
        output = _eventually(lambda: stderr.getvalue(), needle="late-after-deadline")
    # Server stop (context exit) is the documented cancellation: the watch ends.
    thread.join(timeout=2.0)
    assert "late-after-deadline" in output
    assert watch_stopped.wait(timeout=2.0) is True


def _recv_until(client: socket.socket, *, needle: bytes, deadline_s: float) -> bytes:
    client.settimeout(0.2)
    deadline = time.monotonic() + deadline_s
    buffer = b""
    while time.monotonic() < deadline:
        try:
            chunk = client.recv(8192)
        except TimeoutError:
            continue
        if not chunk:
            break
        buffer += chunk
        if needle in buffer:
            break
    return buffer


def _eventually(factory, *, needle: str) -> str:
    deadline = time.monotonic() + 2.0
    latest = factory()
    while time.monotonic() < deadline:
        if needle in latest:
            return latest
        time.sleep(0.05)
        latest = factory()
    return latest
