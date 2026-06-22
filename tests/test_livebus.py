"""Live bus lane subscriptions: push triggers beyond transcript appends."""

from __future__ import annotations

import os
import subprocess
import sys
import time
from dataclasses import dataclass, replace
from http import HTTPStatus
from pathlib import Path
from threading import Event, Lock
from types import SimpleNamespace
from typing import Any

import pytest

from spice.agent.driver import CODEX_DRIVER
from spice.mail.inbox import inbox_dir
from spice.serve import agentapi, app, livebus
from spice.serve.worktree import inventory
from spice.serve.payload import identity, lane, message
from spice.serve.app import ServeState
from spice.serve.livebus import LaneSignature, LiveBusCallbacks, LiveBusSession
from spice.serve.messages import TranscriptResolution
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.team.store import ServeTeamStore
from spice.serve.worktree.target import WorktreeTarget

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

    def observed_wait(paths: tuple[Path, ...], stop, watch=None) -> bool:
        assert inbox_dir(repo) in paths
        watcher_ready.set()
        change_written.wait(timeout=1.0)
        return change_written.is_set() and not stop.is_set()

    monkeypatch.setattr(livebus, "_wait_for_change", observed_wait)
    message_payload_calls = 0

    def messages_payload(_target, **_kwargs):
        nonlocal message_payload_calls
        message_payload_calls += 1
        if message_payload_calls > 1:
            raise AssertionError("inbox-only change must not read messages payload")
        pending_identity = pending_inbox_identity_payload(target.repo_root)
        return {
            "messages": [],
            **pending_identity,
            "statusLine": pending_identity,
        }

    session = LiveBusSession(
        connection,
        _callbacks(
            target=target,
            transcript=transcript,
            messages_payload=messages_payload,
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
        assert pushed["type"] == "lane.pending"
        assert pushed["payload"]["pendingInboxCount"] == 1
        assert pushed["payload"]["pendingInboxKeys"] == ["20260101T000000000001Z"]
        assert pushed["payload"]["pendingInboxRevision"]
        assert pushed["payload"]["pendingInboxVersion"] > 0
        assert set(pushed["payload"]) == {
            "pendingInboxCount",
            "pendingInboxKeys",
            "pendingInboxRevision",
            "pendingInboxVersion",
        }
        assert message_payload_calls == 1
        assert transcript.read_text(encoding="utf-8") == ""
    finally:
        change_written.set()
        session._teardown()


def test_lane_subscription_pushes_pending_frame_for_stopped_agent_inbox_write(
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
    monkeypatch.setattr(identity, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(lane, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(message, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(inventory, "agent_status", lambda *_args, **_kwargs: status)
    ensure_calls: list[dict[str, object]] = []

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        return {"ok": True, "threadId": THREAD_ID}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)
    connection = _Connection()
    watcher_ready = Event()
    change_written = Event()

    def observed_wait(paths: tuple[Path, ...], stop, watch=None) -> bool:
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
                message.messages_payload_for_worktree(state, bus_target, **kwargs)
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
        assert pushed["type"] == "lane.pending"
        assert pushed["payload"]["pendingInboxCount"] == 1
        assert pushed["payload"]["pendingInboxKeys"] == ["20260101T000000000001Z"]
        assert "agentEnsure" not in pushed["payload"]
        assert ensure_calls == []
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

    def fake_wait(_paths: tuple[Path, ...], stop, watch=None) -> bool:
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


def test_lane_subscription_watch_requests_append_only_payload(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    repo.mkdir()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=repo)
    connection = _Connection()
    waits = 0
    payload_kwargs: list[dict[str, Any]] = []

    def fake_wait(_paths: tuple[Path, ...], stop, watch=None) -> bool:
        nonlocal waits
        waits += 1
        if waits > 1:
            stop.set()
            return False
        return True

    def messages_payload(_target, **kwargs):
        payload_kwargs.append(kwargs)
        return {"messages": [], "statusLine": {}}

    monkeypatch.setattr(livebus, "_wait_for_change", fake_wait)
    callbacks = replace(
        _callbacks(target=target, transcript=transcript),
        messages_payload=messages_payload,
        lane_signature=lambda *_args: object(),
    )
    session = LiveBusSession(connection, callbacks)

    try:
        session._handle_lane_subscribe(
            {"type": "lane.subscribe", "targetId": target.id, "query": {"limit": 5}}
        )
        _wait_for_watch_push(connection)
    finally:
        session._teardown()

    assert payload_kwargs[0] == {"limit": 5}
    assert payload_kwargs[1] == {"limit": 5, "append_only": True}


def test_kqueue_watch_rearms_only_when_watched_paths_change(tmp_path):
    if not livebus._HAVE_KQUEUE:
        pytest.skip("kqueue-only behavior")
    (tmp_path / "a").write_text("", encoding="utf-8")
    (tmp_path / "b").write_text("", encoding="utf-8")
    watch = livebus._KqueueWatch()
    try:
        watch._arm((tmp_path / "a",))
        armed = watch._kqueue
        assert armed is not None

        watch._arm((tmp_path / "a",))
        assert watch._kqueue is armed  # unchanged paths keep the same kqueue

        watch._arm((tmp_path / "a", tmp_path / "b"))
        assert watch._kqueue is not armed  # changed paths rebuild it
    finally:
        watch.close()
    assert watch._kqueue is None


def test_metrics_series_replies_from_worker_off_the_dispatch_loop(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=repo)
    connection = _Connection()
    seen: list[dict[str, Any]] = []

    def metric_series(query):
        seen.append(query)
        return {"ok": True, "points": [], "echo": query.get("series")}

    callbacks = replace(
        _callbacks(target=target, transcript=transcript),
        metric_series_payload=metric_series,
    )
    session = LiveBusSession(connection, callbacks)

    session._handle_metrics_series(
        {"type": "metrics.series", "requestId": "r1", "query": {"series": "burndown"}}
    )
    # Teardown enqueues the stop sentinel behind the request and joins the
    # worker, so the metrics reply is delivered deterministically — no polling.
    session._teardown()

    assert seen == [{"series": "burndown"}]
    results = [m for m in connection.sent if m.get("type") == "metrics.seriesResult"]
    assert len(results) == 1
    assert results[0]["requestId"] == "r1"
    assert results[0]["result"]["echo"] == "burndown"


def _callbacks(
    *,
    target: _Target,
    transcript: Path,
    lane_signature=None,
    messages_payload=None,
) -> LiveBusCallbacks:
    def default_messages_payload(_target, **_kwargs):
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
        return LaneSignature(
            transcript=transcript_size,
            inbox=pending_names,
            other=(),
        )

    return LiveBusCallbacks(
        resolve_target=lambda selector: target if selector == target.id else None,
        work_trees_payload=lambda: {},
        messages_payload=messages_payload or default_messages_payload,
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
