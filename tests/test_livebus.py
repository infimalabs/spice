"""Live bus lane subscriptions: push triggers beyond transcript appends."""

from __future__ import annotations

import os
import subprocess
import sys
import time
from dataclasses import dataclass
from http import HTTPStatus
from pathlib import Path
from threading import Event, Lock
from types import SimpleNamespace
from typing import Any

import pytest

from spice.agent.driver import CODEX_DRIVER
from spice.mail.inbox import inbox_dir
from spice.serve import (
    agentapi,
    app,
    identitypayload,
    lanepayload,
    livebus,
    messagepayload,
    worktreepayload,
)
from spice.serve.app import ServeState
from spice.serve.livebus import LiveBusCallbacks, LiveBusSession
from spice.serve.messages import TranscriptResolution
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.team.store import ServeTeamStore
from spice.serve.worktrees import WorktreeTarget

PROJECT_ROOT = Path(__file__).resolve().parents[1]
THREAD_ID = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


@dataclass(frozen=True)
class _Target:
    id: str
    repo_root: Path


class _Connection:
    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []
        self.lock = Lock()

    def send_json(self, payload: dict[str, Any]) -> None:
        with self.lock:
            self.sent.append(payload)


def test_existing_watch_paths_returns_existing_input_paths(tmp_path):
    parent = tmp_path / "parent"
    parent.mkdir()
    missing = parent / "missing.txt"

    assert livebus._existing_watch_paths((parent, missing)) == (parent,)


def test_lane_subscription_pushes_when_external_inbox_write_changes_pending_count(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(livebus, "LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S", 0.05)
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".spice").mkdir()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=repo)
    connection = _Connection()
    watcher_ready = Event()
    change_written = Event()

    def observed_wait(paths: tuple[Path, ...], stop) -> bool:
        assert inbox_dir(repo) in paths
        watcher_ready.set()
        change_written.wait(timeout=1.0)
        return change_written.is_set() and not stop.is_set()

    monkeypatch.setattr(livebus, "_wait_for_change", observed_wait)
    session = LiveBusSession(
        connection,
        _callbacks(target=target, transcript=transcript),
    )

    try:
        session._handle_lane_subscribe(
            {"type": "lane.subscribe", "targetId": target.id, "query": {"limit": 5}}
        )
        assert connection.sent[0]["payload"]["pendingInboxCount"] == 0
        assert watcher_ready.wait(timeout=1.0)

        _write_inbox_item_from_subprocess(repo)
        change_written.set()

        pushed = _wait_for_watch_push(connection)
        assert pushed["payload"]["pendingInboxCount"] == 1
        assert pushed["payload"]["statusLine"]["pendingInboxCount"] == 1
        assert transcript.read_text(encoding="utf-8") == ""
    finally:
        change_written.set()
        session._teardown()


def test_lane_subscription_watch_wakes_stopped_agent_for_external_inbox_write(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(livebus, "LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S", 0.05)
    repo = tmp_path / "repo"
    repo.mkdir()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = WorktreeTarget(id="lane", repo_root=repo, name="repo", branch="main")
    state = ServeState(anchor_root=tmp_path)
    state.cached_targets = [target]
    state.team_store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    status = SimpleNamespace(
        running=False,
        thread_id=THREAD_ID,
        process_status="idle",
        pid=0,
        process_group_id=0,
        model="gpt-test",
        reasoning_effort="low",
        service_tier="",
        started_at="",
        log_path=None,
        prompt_skill_path=None,
    )
    monkeypatch.setattr(agentapi, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(
        identitypayload, "agent_status", lambda *_args, **_kwargs: status
    )
    monkeypatch.setattr(lanepayload, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(
        messagepayload, "agent_status", lambda *_args, **_kwargs: status
    )
    monkeypatch.setattr(
        worktreepayload, "agent_status", lambda *_args, **_kwargs: status
    )
    ensure_calls: list[dict[str, object]] = []

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        return {"ok": True, "threadId": THREAD_ID}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)
    connection = _Connection()
    watcher_ready = Event()
    change_written = Event()

    def observed_wait(paths: tuple[Path, ...], stop) -> bool:
        assert inbox_dir(repo) in paths
        watcher_ready.set()
        change_written.wait(timeout=1.0)
        return change_written.is_set() and not stop.is_set()

    monkeypatch.setattr(livebus, "_wait_for_change", observed_wait)
    session = LiveBusSession(
        connection,
        LiveBusCallbacks(
            resolve_target=lambda selector: target if selector == target.id else None,
            work_trees_payload=lambda: {},
            messages_payload=lambda bus_target, **kwargs: (
                messagepayload.messages_payload_for_worktree(
                    state, bus_target, **kwargs
                )
            ),
            send_payload=lambda _target, _payload: ({}, None),
            task_drain_payload=lambda _target, _payload: ({}, None),
            team_snapshot_payload=lambda _since_revision: {},
            team_command_payload=lambda _payload: ({}, None),
            metric_series_payload=lambda _query: {"ok": True, "points": []},
            thread_id=lambda _target: THREAD_ID,
            transcript_resolution=lambda _thread_id: _transcript_resolution(
                THREAD_ID, transcript
            ),
            lane_watch_paths=lambda bus_target, thread_id, transcript_path: (
                app.lane_watch_paths_for_target(
                    state, bus_target, thread_id, transcript_path
                )
            ),
            lane_signature=lambda bus_target, thread_id, transcript_path: (
                app.lane_signature_for_target(
                    state, bus_target, thread_id, transcript_path
                )
            ),
        ),
    )

    try:
        session._handle_lane_subscribe(
            {"type": "lane.subscribe", "targetId": target.id, "query": {"limit": 5}}
        )
        assert connection.sent[0]["payload"]["pendingInboxCount"] == 0
        assert watcher_ready.wait(timeout=1.0)

        _write_inbox_item_from_subprocess(repo)
        change_written.set()

        pushed = _wait_for_watch_push(connection)
        assert pushed["payload"]["pendingInboxCount"] == 1
        assert pushed["payload"]["statusLine"]["pendingInboxCount"] == 1
        assert pushed["payload"]["statusLine"]["pendingInboxLabel"] == "1"
        assert pushed["payload"]["agentEnsure"]["threadId"] == THREAD_ID
        assert ensure_calls == [
            {"target": target, "fast_mode": False, "force_new": False}
        ]
    finally:
        change_written.set()
        session._teardown()


def test_lane_subscription_suppresses_duplicate_push_for_unchanged_signature(
    tmp_path, monkeypatch
):
    repo = tmp_path / "repo"
    repo.mkdir()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=repo)
    connection = _Connection()
    waits = 0
    signature_calls = 0

    def fake_wait(_paths: tuple[Path, ...], stop) -> bool:
        nonlocal waits
        waits += 1
        if waits > 2:
            stop.set()
            return False
        return True

    def signature(_target, _thread_id, _transcript_path):
        nonlocal signature_calls
        signature_calls += 1
        return "initial" if signature_calls == 1 else "changed"

    monkeypatch.setattr(livebus, "_wait_for_change", fake_wait)
    session = LiveBusSession(
        connection,
        _callbacks(target=target, transcript=transcript, lane_signature=signature),
    )

    try:
        session._handle_lane_subscribe(
            {"type": "lane.subscribe", "targetId": target.id, "query": {"limit": 5}}
        )
        _wait_for_watch_push(connection)
        subscription = session.subscriptions[target.id]
        if subscription.thread is not None:
            subscription.thread.join(timeout=1.0)

        pushes = [
            payload for payload in connection.sent if payload.get("source") == "watch"
        ]
        assert len(pushes) == 1
        assert waits >= 2
    finally:
        session._teardown()


def _callbacks(
    *,
    target: _Target,
    transcript: Path,
    lane_signature=None,
) -> LiveBusCallbacks:
    def messages_payload(_target, **_kwargs):
        pending_identity = pending_inbox_identity_payload(target.repo_root)
        return {
            "messages": [],
            **pending_identity,
            "statusLine": pending_identity,
        }

    def watch_paths(_target, _thread_id, transcript):
        paths = [inbox_dir(target.repo_root)]
        if transcript is not None:
            paths.append(transcript.path)
        return tuple(paths)

    def signature(_target, _thread_id, transcript):
        pending_names = ()
        directory = inbox_dir(target.repo_root)
        if directory.is_dir():
            pending_names = tuple(sorted(path.name for path in directory.glob("*.txt")))
        transcript_size = transcript.path.stat().st_size if transcript else 0
        return (pending_names, transcript_size)

    return LiveBusCallbacks(
        resolve_target=lambda selector: target if selector == target.id else None,
        work_trees_payload=lambda: {},
        messages_payload=messages_payload,
        send_payload=lambda _target, _payload: ({}, None),
        task_drain_payload=lambda _target, _payload: ({}, None),
        team_snapshot_payload=lambda _since_revision: {},
        team_command_payload=lambda _payload: ({}, None),
        metric_series_payload=lambda _query: {"ok": True, "points": []},
        thread_id=lambda _target: "thread",
        transcript_resolution=lambda _thread_id: _transcript_resolution(
            "thread", transcript
        ),
        lane_watch_paths=watch_paths,
        lane_signature=lane_signature or signature,
    )


def _transcript_resolution(thread_id: str, path: Path) -> TranscriptResolution:
    return TranscriptResolution(
        thread_id=thread_id,
        path=path,
        owner_driver=CODEX_DRIVER,
    )


def _write_inbox_item_from_subprocess(repo: Path) -> None:
    env = os.environ.copy()
    env["PYTHONPATH"] = os.pathsep.join(
        entry for entry in (str(PROJECT_ROOT), env.get("PYTHONPATH", "")) if entry
    )
    subprocess.run(
        [
            sys.executable,
            "-c",
            (
                "from pathlib import Path\n"
                "from spice.mail.inbox import compose_inbox_text, write_inbox_item\n"
                "repo = Path(__import__('sys').argv[1])\n"
                "text = compose_inbox_text(body='external steering', priority=None, stop=False)\n"
                "write_inbox_item(repo, '20260101T000000000001Z.txt', text)\n"
            ),
            str(repo),
        ],
        check=True,
        env=env,
    )


def _wait_for_watch_push(
    connection: _Connection, *, timeout_seconds: float = 3.0
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        with connection.lock:
            pushes = [
                payload
                for payload in connection.sent
                if payload.get("source") == "watch"
            ]
        if pushes:
            return pushes[-1]
        time.sleep(0.02)
    pytest.fail(f"timed out waiting for watch push; sent={connection.sent!r}")
