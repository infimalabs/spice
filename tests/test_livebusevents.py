"""Live bus watcher and event-delivery integration coverage."""

from __future__ import annotations

import json
import os
import subprocess
import time
from dataclasses import replace
from http import HTTPStatus
from pathlib import Path
from threading import Event
from types import SimpleNamespace
from typing import Any

import pytest

from spice.agent import lifecycle
from spice.agent.driver import CODEX_DRIVER
from spice.mail.inbox import inbox_dir
from spice.mail.replies import append_reply_record, reply_log_path
from spice.serve import agentapi, app, livebus, messages as message_reader
from spice.serve.app import ServeState
from spice.serve.livebus import LaneSignature, LiveBusCallbacks, LiveBusSession
from spice.serve.payload import identity, lane, message
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.team.store import ServeTeamStore
from spice.serve.worktree import inventory
from spice.serve.worktree.target import WorktreeTarget
from spice.tasks import config as task_config
from tests.test_livebus import (
    THREAD_ID,
    _callbacks,
    _Connection,
    _subscribe_lane,
    _Target,
    _transcript_resolution,
    _wait_for_reply,
    _wait_for_watch_push,
    _write_inbox_item_from_subprocess,
)


def test_existing_watch_paths_returns_existing_input_paths(tmp_path):
    parent = tmp_path / "parent"
    parent.mkdir()
    missing = parent / "missing.txt"

    assert livebus._existing_watch_paths((parent, missing)) == (parent,)


def test_lane_watch_paths_include_agent_state_file(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    target = WorktreeTarget(id="lane", repo_root=repo, name="repo", branch="main")
    state = ServeState(anchor_root=tmp_path)

    paths = app.lane_watch_paths_for_target(state, target, THREAD_ID, None)

    assert lifecycle.agent_state_path(repo) in paths


def test_lane_signature_changes_when_agent_state_file_changes(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    target = WorktreeTarget(id="lane", repo_root=repo, name="repo", branch="main")
    state = ServeState(
        anchor_root=tmp_path,
        team_store=ServeTeamStore(path=tmp_path / "teams.sqlite3"),
    )
    state_path = lifecycle.agent_state_path(repo)
    state_path.parent.mkdir(parents=True, exist_ok=True)
    state_path.write_text("{}", encoding="utf-8")

    before = app.lane_signature_for_target(state, target, THREAD_ID, None)
    os.utime(state_path, ns=(2_000_000_000, 2_000_000_000))
    after = app.lane_signature_for_target(state, target, THREAD_ID, None)

    assert before != after


def test_lane_subscription_pushes_structural_final_status(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    repo.mkdir()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=repo)
    status = SimpleNamespace(
        repo_root=repo,
        running=True,
        thread_id="thread",
        process_status="running",
        pid=123,
        process_group_id=123,
        model="gpt-test",
        reasoning_effort="low",
        service_tier="",
        started_at="",
        log_path=None,
        prompt_skill_path=None,
        command=(),
    )
    monkeypatch.setattr(lane, "agent_status", lambda _repo: status)
    connection = _Connection()
    watcher_ready = Event()
    change_written = Event()

    def observed_wait(
        paths: tuple[Path, ...], stop, watch=None, *, activated=None
    ) -> bool:
        if activated is not None:
            activated.set()
        assert transcript in paths
        watcher_ready.set()
        change_written.wait(timeout=1.0)
        return change_written.is_set() and not stop.is_set()

    def messages_payload(_target, **_kwargs):
        items = message_reader.read_assistant_messages(
            transcript, limit=5, driver=CODEX_DRIVER
        )
        pending = pending_inbox_identity_payload(repo)
        return {
            "messages": [item.to_payload() for item in items],
            **pending,
            "statusLine": lane.status_line_payload(
                SimpleNamespace(),
                target,
                items=items,
                error=None,
                pending_identity=pending,
            ),
        }

    monkeypatch.setattr(livebus, "_wait_for_change", observed_wait)
    session = LiveBusSession(
        connection,
        _callbacks(
            target=target,
            transcript=transcript,
            messages_payload=messages_payload,
        ),
    )

    try:
        _subscribe_lane(session, target.id, limit=5)
        assert watcher_ready.wait(timeout=1.0)

        transcript.write_text(
            json.dumps(
                {
                    "timestamp": "2026-01-01T00:00:01.000000Z",
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "phase": "final_answer",
                        "content": [
                            {"type": "output_text", "text": "Confirmed fixed."}
                        ],
                    },
                },
                separators=(",", ":"),
            )
            + "\n",
            encoding="utf-8",
        )
        change_written.set()

        pushed = _wait_for_watch_push(connection)
        status_line = pushed["payload"]["statusLine"]
        assert pushed["type"] == "lane.payload"
        assert status_line["latestActivityKind"] == "final"
        assert status_line["latestActivityPreview"] == "Confirmed fixed."
        assert status_line["agentProcessStatus"] == "running"
        assert status_line["agentVisualStatus"] == "idle"
        assert status_line["pendingInboxCount"] == 0
    finally:
        change_written.set()
        session._teardown()


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

    def observed_wait(
        paths: tuple[Path, ...], stop, watch=None, *, activated=None
    ) -> bool:
        if activated is not None:
            activated.set()
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
        _subscribe_lane(session, target.id, limit=5)
        assert (
            _wait_for_reply(connection)["lanes"][0]["payload"]["pendingInboxCount"] == 0
        )
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
    # Initial lane payloads include task filter/review metadata. Give this
    # integration its own backend so xdist workers never serialize here on the
    # operator board's bootstrap lock while the subscription deadline runs.
    isolated_task_backend = tmp_path / "task-backend"
    task_config.set_backend(str(isolated_task_backend))
    monkeypatch.setattr(livebus, "LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S", 0.05)
    repo = tmp_path / "repo"
    repo.mkdir()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = WorktreeTarget(id="lane", repo_root=repo, name="repo", branch="main")
    state = ServeState(
        anchor_root=tmp_path,
        team_store=ServeTeamStore(path=tmp_path / "teams.sqlite3"),
    )
    state.cached_targets = [target]
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

    def observed_wait(
        paths: tuple[Path, ...], stop, watch=None, *, activated=None
    ) -> bool:
        if activated is not None:
            activated.set()
        assert inbox_dir(repo) in paths
        watcher_ready.set()
        change_written.wait(timeout=1.0)
        return change_written.is_set() and not stop.is_set()

    def pending_signature(_target, _thread_id, _transcript_path):
        pending_names = tuple(
            sorted(path.name for path in inbox_dir(repo).glob("*.txt"))
        )
        return LaneSignature(
            transcript=transcript.stat().st_size,
            inbox=pending_names,
            other=(),
        )

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
            lane_signature=pending_signature,
        ),
    )

    try:
        _subscribe_lane(session, target.id, limit=5)
        assert (
            _wait_for_reply(connection)["lanes"][0]["payload"]["pendingInboxCount"] == 0
        )
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
        task_config.set_backend(None)

    assert isolated_task_backend.is_dir()


def test_lane_send_replies_before_send_followup_payload_completes(tmp_path):
    target = _Target(id="lane", repo_root=tmp_path)
    connection = _Connection()
    followup_entered = Event()
    followup_continue = Event()
    calls: list[tuple[str, dict[str, Any]]] = []

    def send_payload(_target, payload):
        calls.append(("send", payload))
        return {"ok": True, "key": "inbox-key"}, HTTPStatus.OK

    def send_followup_payload(_target, payload):
        calls.append(("followup", payload))
        followup_entered.set()
        followup_continue.wait(timeout=1.0)
        return {"messages": [], "statusLine": {"pendingInboxCount": 1}}

    session = LiveBusSession(
        connection,
        LiveBusCallbacks(
            resolve_target=lambda selector: target if selector == target.id else None,
            work_trees_payload=lambda: {},
            messages_payload=lambda _target, **_kwargs: {},
            send_payload=send_payload,
            task_drain_payload=lambda _target, _payload: ({}, None),
            team_snapshot_payload=lambda _since_revision: {},
            team_command_payload=lambda _payload: ({}, None),
            metric_series_payload=lambda _query: {"ok": True, "points": []},
            thread_id=lambda _target: "thread",
            transcript_resolution=lambda _thread_id: None,
            lane_watch_paths=lambda *_args: (),
            lane_signature=lambda *_args: (),
            send_followup_payload=send_followup_payload,
        ),
    )

    try:
        session._handle_lane_send(
            {
                "type": "lane.send",
                "requestId": "send-1",
                "targetId": "lane",
                "payload": {"text": "hello"},
            }
        )

        assert followup_entered.wait(timeout=1.0)
        with connection.lock:
            assert len(connection.sent) == 2
            send_result = connection.sent[0]
            send_timing = connection.sent[1]
        assert send_result["type"] == "lane.sendResult"
        assert send_result["requestId"] == "send-1"
        assert send_result["result"]["ok"] is True
        assert send_result["result"]["key"] == "inbox-key"
        assert set(send_result["result"]["serverTiming"]) == {
            "targetResolveMs",
            "sendPayloadMs",
            "totalBeforeReplyMs",
            "replyLockWaitMs",
        }
        assert all(
            value >= 0.0 for value in send_result["result"]["serverTiming"].values()
        )
        assert send_timing["type"] == "lane.sendTiming"
        assert send_timing["requestId"] == "send-1"
        assert set(send_timing["serverTiming"]) == {
            "targetResolveMs",
            "sendPayloadMs",
            "totalBeforeReplyMs",
            "replyLockWaitMs",
            "replyLockHoldMs",
            "replyWriteMs",
            "totalMs",
        }
        assert all(value >= 0.0 for value in send_timing["serverTiming"].values())
        followup_continue.set()
        deadline = time.monotonic() + 1.0
        while time.monotonic() < deadline:
            with connection.lock:
                send_pushes = [
                    payload
                    for payload in connection.sent
                    if payload.get("source") == "send"
                ]
            if send_pushes:
                break
            time.sleep(0.02)
        else:
            pytest.fail(
                f"timed out waiting for send followup; sent={connection.sent!r}"
            )
        assert send_pushes == [
            {
                "type": "lane.payload",
                "targetId": "lane",
                "source": "send",
                "payload": {
                    "messages": [],
                    "statusLine": {"pendingInboxCount": 1},
                },
            }
        ]
        assert calls == [
            ("send", {"text": "hello"}),
            ("followup", {"text": "hello"}),
        ]
    finally:
        followup_continue.set()
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

    def fake_wait(
        _paths: tuple[Path, ...], stop, watch=None, *, activated=None
    ) -> bool:
        nonlocal waits
        if activated is not None:
            activated.set()
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
        _subscribe_lane(session, target.id, limit=5)
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
    watcher_ready = Event()
    configured = Event()
    payload_kwargs: list[dict[str, Any]] = []

    def fake_wait(
        _paths: tuple[Path, ...], stop, watch=None, *, activated=None
    ) -> bool:
        nonlocal waits
        if activated is not None:
            activated.set()
        waits += 1
        if waits > 1:
            stop.set()
            return False
        watcher_ready.set()
        configured.wait(timeout=1.0)
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
        _subscribe_lane(session, target.id, limit=5)
        assert watcher_ready.wait(timeout=1.0)
        # The initial batch reply has completed, so its kwargs land before the
        # reconfigure drives the append-only watch read.
        _wait_for_reply(connection)
        session._handle_lane_configure(
            {
                "type": "lane.configure",
                "targetId": target.id,
                "query": {"limit": 5, "after": "newest-key"},
            }
        )
        configured.set()
        _wait_for_watch_push(connection)
    finally:
        configured.set()
        session._teardown()

    # Every payload read carries the session's stable per-connection client id
    # so the rollout cursor is owned per client, not shared per thread.
    client_ids = [kw.pop("client_id", None) for kw in payload_kwargs]
    assert all(isinstance(cid, str) and cid for cid in client_ids)
    assert len(set(client_ids)) == 1
    assert payload_kwargs[0] == {"limit": 5}
    assert payload_kwargs[1] == {
        "limit": 5,
        "append_only": True,
        "after": "newest-key",
    }


def test_kqueue_watch_rearms_only_when_watched_paths_change(tmp_path, monkeypatch):
    (tmp_path / "a").write_text("", encoding="utf-8")
    (tmp_path / "b").write_text("", encoding="utf-8")
    activated = Event()
    stop = Event()
    queues = []

    class RecordingKqueue:
        def __init__(self):
            self.calls = []
            self.closed = False

        def control(self, changelist, max_events, timeout):
            self.calls.append(
                {
                    "changelist": list(changelist),
                    "maxEvents": max_events,
                    "timeout": timeout,
                    "activated": activated.is_set(),
                }
            )
            if max_events:
                stop.set()
            return []

        def close(self):
            self.closed = True

    def select_attr(name):
        if name == "kqueue":
            return lambda: queues.append(RecordingKqueue()) or queues[-1]
        if name == "kevent":
            return lambda descriptor, **_kwargs: ("watch", descriptor)
        if name.startswith("KQ_"):
            return 1
        raise AssertionError(f"unexpected select attribute {name}")

    monkeypatch.setattr(livebus, "_select_attr", select_attr)
    watch = livebus._KqueueWatch()
    try:
        assert watch.wait((tmp_path / "a",), stop, activated=activated) is False
        armed = watch._kqueue
        assert armed is not None
        assert [
            (
                len(call["changelist"]),
                call["maxEvents"],
                call["timeout"],
                call["activated"],
            )
            for call in queues[0].calls
        ] == [
            (1, 0, 0, False),
            (0, 1, livebus.LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S, True),
        ]

        watch._arm((tmp_path / "a",))
        assert watch._kqueue is armed  # unchanged paths keep the same kqueue

        watch._arm((tmp_path / "a", tmp_path / "b"))
        assert watch._kqueue is not armed  # changed paths rebuild it
        assert queues[0].closed is True
        assert queues[1].calls[0]["maxEvents"] == 0
    finally:
        watch.close()
    assert watch._kqueue is None


def test_kqueue_watch_rearms_atomic_replacement_before_return(tmp_path, monkeypatch):
    watched = tmp_path / "task-event"
    watched.write_text("first", encoding="utf-8")
    queues = []

    class InvalidatingEvent:
        fflags = 8

    class RecordingKqueue:
        def __init__(self, ordinal):
            self.ordinal = ordinal
            self.calls = []
            self.closed = False

        def control(self, changelist, max_events, timeout):
            self.calls.append((list(changelist), max_events, timeout))
            if self.ordinal == 0 and max_events:
                return [InvalidatingEvent()]
            return []

        def close(self):
            self.closed = True

    constants = {
        "KQ_NOTE_WRITE": 1,
        "KQ_NOTE_EXTEND": 2,
        "KQ_NOTE_DELETE": 4,
        "KQ_NOTE_RENAME": 8,
        "KQ_FILTER_VNODE": 16,
        "KQ_EV_ADD": 32,
        "KQ_EV_CLEAR": 64,
    }

    def select_attr(name):
        if name == "kqueue":
            return lambda: queues.append(RecordingKqueue(len(queues))) or queues[-1]
        if name == "kevent":
            return lambda descriptor, **_kwargs: ("watch", descriptor)
        return constants[name]

    monkeypatch.setattr(livebus, "_select_attr", select_attr)
    monkeypatch.setattr(livebus, "_KQUEUE_VNODE_FFLAGS", 15)
    monkeypatch.setattr(livebus, "_KQUEUE_INVALIDATING_FFLAGS", 12)
    watch = livebus._KqueueWatch()
    try:
        assert watch.wait((watched,), Event()) is True
        assert len(queues) == 2
        assert queues[0].closed is True
        assert queues[1].calls[0][1] == 0
    finally:
        watch.close()


def test_watchfiles_activation_follows_native_ready_yield(tmp_path, monkeypatch):
    watched = tmp_path / "watched"
    watched.mkdir()
    activated = Event()
    activation_during_yields = []
    received_options = []

    def watch(*paths, **options):
        received_options.append({"paths": paths, **options})
        activation_during_yields.append(activated.is_set())
        yield set()
        activation_during_yields.append(activated.is_set())
        yield {(1, str(watched / "task-event"))}

    monkeypatch.setattr(
        livebus,
        "import_module",
        lambda name: SimpleNamespace(watch=watch) if name == "watchfiles" else None,
    )

    assert (
        livebus._wait_for_change_watchfiles((watched,), Event(), activated=activated)
        is True
    )
    assert activation_during_yields == [False, True]
    assert received_options[0]["paths"] == (watched,)
    assert received_options[0]["yield_on_timeout"] is True


def test_lane_subscription_pushes_reply_card_without_a_followup_message(
    tmp_path, monkeypatch
):
    """`spice agent reply` while the agent is idle must surface immediately.

    The reply log is the only path that changes when an idle agent replies —
    no transcript append follows — so the lane watcher must wake on it and
    push the full messages payload carrying the reply card (UI-1kBrG3Lw).
    """
    monkeypatch.setattr(livebus, "LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S", 0.05)
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = WorktreeTarget(id="lane", repo_root=repo, name="repo", branch="main")
    state = ServeState(
        anchor_root=tmp_path,
        team_store=ServeTeamStore(path=tmp_path / "teams.sqlite3"),
    )
    state.cached_targets = [target]
    status = SimpleNamespace(
        running=True,
        thread_id=THREAD_ID,
        process_status="idle",
        pid=123,
        process_group_id=123,
        model="gpt-test",
        reasoning_effort="low",
        service_tier="",
        started_at="",
        log_path=None,
        prompt_skill_path=None,
    )
    for module in (agentapi, identity, lane, message, inventory):
        monkeypatch.setattr(module, "agent_status", lambda *_args, **_kwargs: status)
    connection = _Connection()
    watcher_ready = Event()
    change_written = Event()
    reply_log = reply_log_path(repo, THREAD_ID)

    def observed_wait(
        paths: tuple[Path, ...], stop, watch=None, *, activated=None
    ) -> bool:
        if activated is not None:
            activated.set()
        assert reply_log in paths
        watcher_ready.set()
        change_written.wait(timeout=1.0)
        return change_written.is_set() and not stop.is_set()

    monkeypatch.setattr(livebus, "_wait_for_change", observed_wait)
    task_config.set_backend(str(tmp_path / "task-backend"))
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
        _subscribe_lane(session, target.id, limit=5)
        assert watcher_ready.wait(timeout=1.0)

        append_reply_record(
            repo,
            THREAD_ID,
            timestamp="2026-01-01T00:00:01.000000Z",
            text="ACK 20260101T000000000001Z: applied",
            ack_keys=["20260101T000000000001Z"],
            nack_keys=[],
        )
        change_written.set()

        pushed = _wait_for_watch_push(connection)
        assert pushed["type"] == "lane.payload"
        reply_cards = [
            item
            for item in pushed["payload"]["messages"]
            if item.get("kind") == "reply"
        ]
        assert len(reply_cards) == 1
        assert "applied" in reply_cards[0]["text"]
        # The agent emitted nothing after the reply: the card surfaced with no
        # follow-up transcript append.
        assert transcript.read_text(encoding="utf-8") == ""
    finally:
        change_written.set()
        session._teardown()
        task_config.set_backend(None)
