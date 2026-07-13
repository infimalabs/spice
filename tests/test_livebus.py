"""Live bus lane subscriptions: push triggers beyond transcript appends."""

from __future__ import annotations

import os
import subprocess
import sys
import time
from dataclasses import dataclass, replace
from pathlib import Path
from threading import Barrier, Event, Lock, Thread
from typing import Any

import pytest

from spice.agent.driver import CODEX_DRIVER
from spice.mail.inbox import inbox_dir
from spice.serve import livebus
from spice.serve.livebus import LaneSignature, LiveBusCallbacks, LiveBusSession
from spice.serve.messages import TranscriptResolution
from spice.serve.pending import pending_inbox_identity_payload

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

    def encode_text_frame(self, payload: dict[str, Any]) -> dict[str, Any]:
        # The session encodes to a frame before taking its send lock; the fake
        # keeps the payload dict as its "frame" so assertions read it directly.
        return payload

    def send_frame(self, frame: dict[str, Any]) -> None:
        with self.lock:
            self.sent.append(frame)


def test_ping_pongs_while_a_slow_lane_refresh_is_still_computing(tmp_path):
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=tmp_path)
    connection = _Connection()
    refresh_started = Event()
    refresh_release = Event()

    def slow_payload(_target, **_kwargs):
        refresh_started.set()
        refresh_release.wait(timeout=2.0)
        return {"messages": [], "statusLine": {}}

    session = LiveBusSession(
        connection,
        _callbacks(target=target, transcript=transcript, messages_payload=slow_payload),
    )

    try:
        session._handle_lane_refresh(
            {
                "type": "lane.refresh",
                "requestId": "refresh-1",
                "targetId": target.id,
                "query": {"limit": 5},
            }
        )
        # The refresh is now parked on the pool thread inside slow_payload; the
        # dispatch thread is free, so a ping issued now pongs before the refresh
        # unblocks -- head-of-line blocking is gone.
        assert refresh_started.wait(timeout=1.0)
        session._handle_ping({"type": "bus.ping", "requestId": "ping-1"})
        pong = _wait_for_reply(connection, request_id="ping-1")
        assert pong["type"] == "bus.pong"

        refresh_release.set()
        refresh_reply = _wait_for_reply(connection, request_id="refresh-1")
        assert refresh_reply["type"] == "lane.payload"
        # The pong landed ahead of the refresh reply it was racing.
        with connection.lock:
            reply_order = [
                payload.get("requestId")
                for payload in connection.sent
                if payload.get("requestId")
            ]
        assert reply_order.index("ping-1") < reply_order.index("refresh-1")
    finally:
        refresh_release.set()
        session._teardown()


def test_two_refreshes_for_one_target_reply_in_request_order(tmp_path):
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=tmp_path)
    connection = _Connection()
    first_started = Event()
    release_first = Event()
    compute_order: list[str] = []

    def staggered_payload(_target, **kwargs):
        after = str(kwargs.get("after") or "")
        if after == "first":
            first_started.set()
            release_first.wait(timeout=2.0)
        compute_order.append(after)
        return {"messages": [], "statusLine": {"after": after}}

    session = LiveBusSession(
        connection,
        _callbacks(
            target=target, transcript=transcript, messages_payload=staggered_payload
        ),
    )

    try:
        session._handle_lane_refresh(
            {
                "type": "lane.refresh",
                "requestId": "r-first",
                "targetId": target.id,
                "query": {"limit": 5, "after": "first"},
            }
        )
        # Queue the second read for the same target while the first is still
        # stuck; per-target FIFO must hold it behind the first rather than let
        # the fast second compute overtake it.
        assert first_started.wait(timeout=1.0)
        session._handle_lane_refresh(
            {
                "type": "lane.refresh",
                "requestId": "r-second",
                "targetId": target.id,
                "query": {"limit": 5, "after": "second"},
            }
        )
        release_first.set()
        _wait_for_reply(connection, request_id="r-first")
        _wait_for_reply(connection, request_id="r-second")

        assert compute_order == ["first", "second"]
        with connection.lock:
            reply_order = [
                payload.get("requestId")
                for payload in connection.sent
                if payload.get("requestId")
            ]
        assert reply_order == ["r-first", "r-second"]
    finally:
        release_first.set()
        session._teardown()


def test_lane_send_is_not_blocked_by_an_inflight_subscribe(tmp_path, monkeypatch):
    # The single serial dispatch thread used to run lanes.subscribe inline: it
    # parked on watcher activation and the full initial payload read, so a
    # lane.send arriving right behind it on that one thread waited out the whole
    # subscribe before the composer could clear -- the operator's several-second,
    # timing-dependent submit latency. The subscribe now completes off the
    # dispatch thread, so the send is dispatched and acked without waiting for it.
    monkeypatch.setattr(livebus, "_wait_for_change", _idle_wait)
    target = _Target(id="lane", repo_root=tmp_path)
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    connection = _Connection()
    initial_read_started = Event()
    release_initial_read = Event()

    def slow_initial_payload(_target, **kwargs):
        # Only the subscribe's initial full read blocks; append-only watch reads
        # stay fast so the watcher never wedges behind the gate.
        if kwargs.get("append_only"):
            return {"messages": [], "statusLine": {}}
        initial_read_started.set()
        # Parks well past the send ack's wait window: on the broken path the send
        # cannot slip through at an auto-release, it simply never arrives in time.
        release_initial_read.wait(timeout=5.0)
        return {"messages": [], "statusLine": {}}

    session = LiveBusSession(
        connection,
        _callbacks(
            target=target,
            transcript=transcript,
            messages_payload=slow_initial_payload,
        ),
    )

    def serial_dispatch() -> None:
        # Model the one per-connection dispatch thread: read+dispatch the
        # subscribe, then the send that follows it in the socket, strictly serial.
        session._dispatch(
            {
                "type": "lanes.subscribe",
                "requestId": "sub-1",
                "entries": [{"targetId": target.id, "query": {"limit": 5}}],
            }
        )
        session._dispatch(
            {
                "type": "lane.send",
                "requestId": "send-1",
                "targetId": target.id,
                "payload": {"text": "hello"},
            }
        )

    dispatcher = Thread(target=serial_dispatch, name="test-dispatch", daemon=True)
    try:
        dispatcher.start()
        # The subscribe's initial read is parked mid-flight; the send behind it
        # must still be dispatched and acked instead of waiting out the batch.
        assert initial_read_started.wait(timeout=1.0)
        send_result = _wait_for_reply(
            connection, request_id="send-1", timeout_seconds=1.5
        )
        assert send_result["type"] == "lane.sendResult"
        # And the ack landed while the subscribe was still parked -- proof it did
        # not queue behind the initial batch read on the dispatch thread.
        with connection.lock:
            assert not any(
                frame.get("requestId") == "sub-1" for frame in connection.sent
            )
    finally:
        release_initial_read.set()
        dispatcher.join(timeout=2.0)
        session._teardown()


def test_teardown_drains_an_inflight_read_before_shutting_the_pool(tmp_path):
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=tmp_path)
    connection = _Connection()
    compute_started = Event()
    compute_release = Event()

    def gated_payload(_target, **_kwargs):
        compute_started.set()
        compute_release.wait(timeout=2.0)
        return {"messages": [], "statusLine": {}}

    session = LiveBusSession(
        connection,
        _callbacks(
            target=target, transcript=transcript, messages_payload=gated_payload
        ),
    )

    session._handle_lane_refresh(
        {
            "type": "lane.refresh",
            "requestId": "read-1",
            "targetId": target.id,
            "query": {"limit": 5},
        }
    )
    assert compute_started.wait(timeout=1.0)
    # Release the compute, then tear down: teardown joins the read chain within
    # the watcher join budget, so the reply is delivered synchronously -- it is
    # already present the instant teardown returns, no post-teardown polling.
    compute_release.set()
    session._teardown()
    with connection.lock:
        replies = [
            payload
            for payload in connection.sent
            if payload.get("requestId") == "read-1"
        ]
    assert len(replies) == 1
    assert replies[0]["type"] == "lane.payload"


def test_teardown_abandons_a_stuck_read_within_the_join_budget(tmp_path, monkeypatch):
    monkeypatch.setattr(livebus, "LIVE_BUS_WATCHER_JOIN_TIMEOUT_S", 0.2)
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=tmp_path)
    connection = _Connection()
    compute_started = Event()
    compute_release = Event()

    def stuck_payload(_target, **_kwargs):
        compute_started.set()
        compute_release.wait(timeout=2.0)
        return {"messages": [], "statusLine": {}}

    session = LiveBusSession(
        connection,
        _callbacks(
            target=target, transcript=transcript, messages_payload=stuck_payload
        ),
    )

    try:
        session._handle_lane_refresh(
            {
                "type": "lane.refresh",
                "requestId": "read-1",
                "targetId": target.id,
                "query": {"limit": 5},
            }
        )
        assert compute_started.wait(timeout=1.0)
        started_at = time.monotonic()
        session._teardown()
        elapsed = time.monotonic() - started_at
        # The compute would block a full 2s; teardown returns within the bounded
        # join budget instead of waiting it out.
        assert elapsed < 1.0
    finally:
        compute_release.set()


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


class _DeadConnection:
    """Peer that vanished: every write raises like a closed socket."""

    def __init__(self) -> None:
        self.attempts = 0
        self.lock = Lock()

    def encode_text_frame(self, payload: dict[str, Any]) -> dict[str, Any]:
        return payload

    def send_frame(self, frame: dict[str, Any]) -> None:
        with self.lock:
            self.attempts += 1
        raise BrokenPipeError("Broken pipe")


def test_metrics_worker_ends_quietly_when_peer_vanishes_mid_reply(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=repo)
    connection = _DeadConnection()
    seen: list[dict[str, Any]] = []

    def metric_series(query):
        seen.append(query)
        return {"ok": True, "points": []}

    callbacks = replace(
        _callbacks(target=target, transcript=transcript),
        metric_series_payload=metric_series,
    )
    session = LiveBusSession(connection, callbacks)

    session._handle_metrics_series(
        {"type": "metrics.series", "requestId": "r1", "query": {"series": "burndown"}}
    )
    session._teardown()

    assert seen == [{"series": "burndown"}]
    # One reply attempt, then the worker returns. The pre-guard shape wrote a
    # second bus.error frame to the same dead socket, so attempts would be 2
    # and the escaping raise would kill the thread with a printed stack.
    assert connection.attempts == 1


def test_metrics_worker_surfaces_compute_errors_as_bus_error(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=repo)
    connection = _Connection()

    def metric_series(query):
        raise ValueError("boom")

    callbacks = replace(
        _callbacks(target=target, transcript=transcript),
        metric_series_payload=metric_series,
    )
    session = LiveBusSession(connection, callbacks)

    session._handle_metrics_series(
        {"type": "metrics.series", "requestId": "r1", "query": {"series": "burndown"}}
    )
    session._teardown()

    errors = [m for m in connection.sent if m.get("type") == "bus.error"]
    assert len(errors) == 1
    assert errors[0]["error"] == "boom"
    assert errors[0]["requestId"] == "r1"


def test_lanes_subscribe_replies_once_with_activation_per_lane(tmp_path, monkeypatch):
    monkeypatch.setattr(livebus, "_wait_for_change", _idle_wait)
    targets, transcripts = _two_lane_fixture(tmp_path)
    batch_connection = _Connection()
    batch_session = LiveBusSession(
        batch_connection, _multi_lane_callbacks(targets, transcripts)
    )
    try:
        batch_session._handle_lanes_subscribe(
            {
                "type": "lanes.subscribe",
                "requestId": "batch-1",
                "entries": [
                    {"targetId": "lane-a", "query": {"limit": 5}},
                    {"targetId": "lane-b", "query": {"limit": 5}},
                ],
            }
        )
        frame = _wait_for_reply(batch_connection, request_id="batch-1")
        with batch_connection.lock:
            replies = [f for f in batch_connection.sent if f.get("source") is None]
        assert len(replies) == 1
        assert frame["type"] == "lanes.payload"
        assert frame["requestId"] == "batch-1"
        lane_ids = [lane_entry["targetId"] for lane_entry in frame["lanes"]]
        assert lane_ids == ["lane-a", "lane-b"]
        assert [entry["watcherActive"] for entry in frame["lanes"]] == [True, True]
        assert [entry["watcherError"] for entry in frame["lanes"]] == ["", ""]
        assert len({entry["subscriptionGeneration"] for entry in frame["lanes"]}) == 2
        assert [
            entry["payload"]["statusLine"]["targetId"] for entry in frame["lanes"]
        ] == ["lane-a", "lane-b"]
        assert frame["lanes"][0]["payload"]["ackContexts"] == [
            {"key": "20260101T000000000001Z", "targetId": "lane-a"}
        ]
        assert frame["lanes"][0]["payload"]["messages"][0]["kind"] == "task"
        assert set(batch_session.subscriptions) == {"lane-a", "lane-b"}
    finally:
        batch_session._teardown()


def test_lanes_subscribe_reports_watcher_activation_per_lane(tmp_path, monkeypatch):
    monkeypatch.setattr(livebus, "_wait_for_change", _idle_wait)
    targets, transcripts = _two_lane_fixture(tmp_path)
    callbacks = _multi_lane_callbacks(targets, transcripts)
    base_watch_paths = callbacks.lane_watch_paths

    def watch_paths(bus_target, thread_id, transcript):
        if bus_target.id == "lane-a":
            raise ValueError("lane-a watcher unavailable")
        return base_watch_paths(bus_target, thread_id, transcript)

    connection = _Connection()
    session = LiveBusSession(
        connection,
        replace(callbacks, lane_watch_paths=watch_paths),
    )

    try:
        session._handle_lanes_subscribe(
            {
                "type": "lanes.subscribe",
                "requestId": "batch-errors",
                "entries": [
                    {"targetId": "lane-a", "query": {"limit": 5}},
                    {"targetId": "lane-b", "query": {"limit": 5}},
                ],
            }
        )
        frame = _wait_for_reply(connection, request_id="batch-errors")
        entries = {entry["targetId"]: entry for entry in frame["lanes"]}
        assert [entries[target_id]["watcherActive"] for target_id in entries] == [
            False,
            True,
        ]
        assert entries["lane-a"]["watcherError"] == "lane-a watcher unavailable"
        assert entries["lane-b"]["watcherError"] == ""
        assert [
            entries[target_id]["payload"]["statusLine"]["targetId"]
            for target_id in entries
        ] == [
            "lane-a",
            "lane-b",
        ]
    finally:
        session._teardown()


def test_lanes_subscribe_generations_change_on_replacement_and_reconnect(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(livebus, "_wait_for_change", _idle_wait)
    targets, transcripts = _two_lane_fixture(tmp_path)
    callbacks = _multi_lane_callbacks(targets, transcripts)
    first_connection = _Connection()
    first_session = LiveBusSession(first_connection, callbacks)
    second_connection = _Connection()
    second_session = LiveBusSession(second_connection, callbacks)
    message = {
        "type": "lanes.subscribe",
        "entries": [{"targetId": "lane-a", "query": {"limit": 5}}],
    }

    try:
        first_session._handle_lanes_subscribe({**message, "requestId": "first"})
        first_subscription = first_session.subscriptions["lane-a"]
        first_reply = _wait_for_reply(first_connection, request_id="first")
        first_session._handle_lanes_subscribe({**message, "requestId": "replacement"})
        replacement_reply = _wait_for_reply(first_connection, request_id="replacement")
        second_session._handle_lanes_subscribe({**message, "requestId": "reconnect"})
        reconnect_reply = _wait_for_reply(second_connection, request_id="reconnect")

        generations = [
            first_reply["lanes"][0]["subscriptionGeneration"],
            replacement_reply["lanes"][0]["subscriptionGeneration"],
            reconnect_reply["lanes"][0]["subscriptionGeneration"],
        ]
        assert generations == [
            f"{first_session.client_id}:1",
            f"{first_session.client_id}:2",
            f"{second_session.client_id}:1",
        ]
        assert len(set(generations)) == 3
        assert first_subscription.stop.is_set() is True
    finally:
        first_session._teardown()
        second_session._teardown()


def test_lanes_subscribe_orders_initial_payload_before_setup_race_push(
    tmp_path, monkeypatch
):
    target = _Target(id="lane", repo_root=tmp_path / "repo")
    target.repo_root.mkdir()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    task_keys: list[str] = []
    delivered_keys: set[str] = set()
    state_lock = Lock()
    wait_lock = Lock()
    initial_read_started = Event()
    release_initial_read = Event()
    change_written = Event()
    watch_registered = Event()
    watch_push_sent = Event()
    wait_calls = 0

    class _ObservedConnection(_Connection):
        def send_frame(self, frame: dict[str, Any]) -> None:
            super().send_frame(frame)
            if frame.get("source") == "watch":
                watch_push_sent.set()

    def observed_wait(
        _paths: tuple[Path, ...], stop, watch=None, *, activated=None
    ) -> bool:
        nonlocal wait_calls
        watch_registered.set()
        if activated is not None:
            activated.set()
        with wait_lock:
            wait_calls += 1
            current_call = wait_calls
        if current_call == 1:
            changed = change_written.wait(timeout=1.0)
            return changed and not stop.is_set()
        stop.wait(timeout=1.0)
        return False

    def messages_payload(_target, **kwargs):
        append_only = bool(kwargs.get("append_only"))
        if not append_only:
            initial_read_started.set()
            release_initial_read.wait(timeout=1.0)
        with state_lock:
            snapshot = [
                key for key in task_keys if not append_only or key not in delivered_keys
            ]
            delivered_keys.update(snapshot)
        return {
            "messages": [{"key": key, "kind": "task"} for key in snapshot],
            "statusLine": {},
        }

    def signature(_target, _thread_id, _transcript):
        with state_lock:
            return tuple(task_keys)

    monkeypatch.setattr(livebus, "_wait_for_change", observed_wait)
    connection = _ObservedConnection()
    session = LiveBusSession(
        connection,
        _callbacks(
            target=target,
            transcript=transcript,
            lane_signature=signature,
            messages_payload=messages_payload,
        ),
    )
    subscribe_thread = Thread(
        target=session._handle_lanes_subscribe,
        args=(
            {
                "type": "lanes.subscribe",
                "requestId": "setup-race",
                "entries": [{"targetId": "lane", "query": {"limit": 5}}],
            },
        ),
    )

    try:
        subscribe_thread.start()
        assert initial_read_started.wait(timeout=1.0)
        assert watch_registered.is_set() is True
        with state_lock:
            task_keys.append("task-racing-setup")
        change_written.set()
        release_initial_read.set()
        subscribe_thread.join(timeout=1.0)
        assert watch_push_sent.wait(timeout=1.0)

        with connection.lock:
            frames = list(connection.sent)
        assert [(frame["type"], frame.get("source", "reply")) for frame in frames] == [
            ("lanes.payload", "reply"),
            ("lane.payload", "watch"),
        ]
        message_keys = [
            item["key"]
            for frame in frames
            for item in (
                frame["lanes"][0]["payload"]["messages"]
                if frame["type"] == "lanes.payload"
                else frame["payload"]["messages"]
            )
        ]
        assert message_keys == ["task-racing-setup"]
        assert (
            frames[0]["lanes"][0]["subscriptionGeneration"]
            == frames[1]["subscriptionGeneration"]
        )
    finally:
        release_initial_read.set()
        change_written.set()
        session._teardown()


def test_lane_send_ack_is_not_queued_behind_a_bulk_watch_push_encode(
    tmp_path, monkeypatch
):
    # A watcher push encodes its (potentially large) frame before the session
    # takes send_lock, so the lock's critical section is only the socket write.
    # A small lane.send racing that push on the same connection acquires
    # send_lock and acks as soon as any in-flight write returns rather than
    # queuing behind the bulk encode. This pins that ordering: the ack lands
    # while a watch push is still parked mid-encode. On the old path -- encode
    # inside send_lock -- the parked encode holds the lock and the ack never
    # arrives, so ack_landed times out below.
    target = _Target(id="lane", repo_root=tmp_path / "repo")
    target.repo_root.mkdir()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    change_written = Event()
    watch_encoding_started = Event()
    release_watch_encode = Event()

    class _GatedEncodeConnection(_Connection):
        def encode_text_frame(self, payload: dict[str, Any]) -> dict[str, Any]:
            if payload.get("source") == "watch":
                # Stand in for an expensive bulk-payload encode. With encoding
                # moved out of send_lock this parks holding no lock, so a
                # concurrent ack must not be serialized behind it. The park runs
                # well past the ack's wait window below: on the broken path the
                # ack cannot slip through at an auto-release, it simply never
                # arrives in time.
                watch_encoding_started.set()
                assert release_watch_encode.wait(timeout=5.0)
            return super().encode_text_frame(payload)

    def observed_wait(
        _paths: tuple[Path, ...], stop, watch=None, *, activated=None
    ) -> bool:
        if activated is not None:
            activated.set()
        if not change_written.is_set():
            changed = change_written.wait(timeout=2.0)
            return changed and not stop.is_set()
        stop.wait(timeout=2.0)
        return False

    def messages_payload(_target, **_kwargs):
        return {"messages": [], "statusLine": {}}

    def signature(_target, _thread_id, _transcript):
        # The post-change signature differs from the one captured at subscribe,
        # so exactly one watch push fires.
        return LaneSignature(
            transcript=1 if change_written.is_set() else 0,
            inbox=(),
            other=(),
        )

    monkeypatch.setattr(livebus, "_wait_for_change", observed_wait)
    connection = _GatedEncodeConnection()
    session = LiveBusSession(
        connection,
        _callbacks(
            target=target,
            transcript=transcript,
            lane_signature=signature,
            messages_payload=messages_payload,
        ),
    )

    ack_landed = Event()

    def issue_send() -> None:
        session._handle_lane_send(
            {
                "type": "lane.send",
                "requestId": "send-1",
                "targetId": target.id,
                "payload": {"body": "hi"},
            }
        )
        ack_landed.set()

    try:
        _subscribe_lane(session, target.id, limit=5)
        # Arm one watch push and let it park inside the gated encode with no
        # send_lock held.
        change_written.set()
        assert watch_encoding_started.wait(timeout=2.0)

        # Race a lane.send against the parked bulk encode.
        send_thread = Thread(target=issue_send, daemon=True)
        send_thread.start()
        assert ack_landed.wait(timeout=2.0)

        with connection.lock:
            acks = [
                frame
                for frame in connection.sent
                if frame.get("type") == "lane.sendResult"
            ]
            watch_frames = [
                frame for frame in connection.sent if frame.get("source") == "watch"
            ]
        assert [ack["requestId"] for ack in acks] == ["send-1"]
        # The bulk push is still parked mid-encode, so no watch frame has been
        # written yet: the ack overtook it rather than queuing behind it.
        assert watch_frames == []
        send_thread.join(timeout=2.0)
        assert not send_thread.is_alive()
    finally:
        release_watch_encode.set()
        change_written.set()
        session._teardown()


def test_lanes_subscribe_watch_pushes_exactly_the_changed_lane(tmp_path, monkeypatch):
    targets, transcripts = _two_lane_fixture(tmp_path)
    repo_a = targets[0].repo_root
    transcript_a = transcripts["thread-lane-a"]
    connection = _Connection()
    change_a = Event()

    def observed_wait(
        paths: tuple[Path, ...], stop, watch=None, *, activated=None
    ) -> bool:
        if activated is not None:
            activated.set()
        if inbox_dir(repo_a) in paths and not change_a.is_set():
            changed = change_a.wait(timeout=2.0)
            return changed and not stop.is_set()
        stop.wait(timeout=2.0)
        return False

    monkeypatch.setattr(livebus, "_wait_for_change", observed_wait)
    session = LiveBusSession(connection, _multi_lane_callbacks(targets, transcripts))

    try:
        session._handle_lanes_subscribe(
            {
                "type": "lanes.subscribe",
                "requestId": "batch-1",
                "entries": [
                    {"targetId": "lane-a", "query": {"limit": 5}},
                    {"targetId": "lane-b", "query": {"limit": 5}},
                ],
            }
        )
        transcript_a.write_text('{"kind":"message"}\n', encoding="utf-8")
        change_a.set()

        pushed = _wait_for_watch_push(connection)
        assert pushed["type"] == "lane.payload"
        assert pushed["targetId"] == "lane-a"
        time.sleep(0.15)
        with connection.lock:
            watch_targets = [
                payload["targetId"]
                for payload in connection.sent
                if payload.get("source") == "watch"
            ]
        assert watch_targets == ["lane-a"]
    finally:
        change_a.set()
        session._teardown()


def test_lanes_subscribe_computes_payloads_concurrently(tmp_path, monkeypatch):
    monkeypatch.setattr(livebus, "_wait_for_change", _idle_wait)
    targets, transcripts = _two_lane_fixture(tmp_path)
    connection = _Connection()
    barrier = Barrier(2)

    def overlapping_payload(bus_target, **_kwargs):
        # Each compute blocks until the other arrives: only overlapping
        # execution passes the barrier; serial execution breaks it and the
        # error payload below fails the equality assertion.
        barrier.wait(timeout=2.0)
        return {"messages": [], "statusLine": {"targetId": bus_target.id}}

    session = LiveBusSession(
        connection,
        _multi_lane_callbacks(
            targets, transcripts, messages_payload=overlapping_payload
        ),
    )

    try:
        started = time.perf_counter()
        session._handle_lanes_subscribe(
            {
                "type": "lanes.subscribe",
                "requestId": "batch-1",
                "entries": [
                    {"targetId": "lane-a", "query": {"limit": 5}},
                    {"targetId": "lane-b", "query": {"limit": 5}},
                ],
            }
        )
        frame = _wait_for_reply(connection, request_id="batch-1")
        elapsed = time.perf_counter() - started
        assert [entry["payload"] for entry in frame["lanes"]] == [
            {"messages": [], "statusLine": {"targetId": "lane-a"}},
            {"messages": [], "statusLine": {"targetId": "lane-b"}},
        ]
        assert elapsed < 2.0  # one overlapped rendezvous, well inside 2x the wait
    finally:
        session._teardown()


def test_lanes_subscribe_rejects_duplicate_target_ids(tmp_path):
    targets, transcripts = _two_lane_fixture(tmp_path)
    connection = _Connection()
    session = LiveBusSession(connection, _multi_lane_callbacks(targets, transcripts))

    try:
        session._handle_lanes_subscribe(
            {
                "type": "lanes.subscribe",
                "requestId": "dup-1",
                "entries": [
                    {"targetId": "lane-a", "query": {"limit": 5}},
                    {"targetId": "lane-a", "query": {"limit": 5}},
                ],
            }
        )
        assert connection.sent == [
            {
                "type": "bus.error",
                "error": "duplicate targetId 'lane-a' in lanes.subscribe",
                "requestId": "dup-1",
            }
        ]
        assert session.subscriptions == {}
    finally:
        session._teardown()


def _subscribe_lane(session: LiveBusSession, target_id: str, *, limit: int) -> None:
    session._handle_lanes_subscribe(
        {
            "type": "lanes.subscribe",
            "entries": [{"targetId": target_id, "query": {"limit": limit}}],
        }
    )
    # The blocking completion now runs off the dispatch thread; callers here rely
    # on the initial payload being read and the watcher armed before they write a
    # change and await its push, so block until the subscribe has settled. The
    # old inline handler blocked here unbounded; this generous cap is a hang guard
    # under xdist load, not an expected wait -- the gate is set in milliseconds.
    subscription = session.subscriptions.get(target_id)
    if subscription is not None:
        assert subscription.initial_payload_sent.wait(timeout=15.0)


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


def _idle_wait(_paths: tuple[Path, ...], stop, watch=None, *, activated=None) -> bool:
    if activated is not None:
        activated.set()
    stop.wait(0.05)
    return False


def _two_lane_fixture(tmp_path: Path) -> tuple[list[_Target], dict[str, Path]]:
    targets: list[_Target] = []
    transcripts: dict[str, Path] = {}
    for name in ("lane-a", "lane-b"):
        repo = tmp_path / f"repo-{name}"
        repo.mkdir()
        transcript = tmp_path / f"{name}.jsonl"
        transcript.write_text("", encoding="utf-8")
        targets.append(_Target(id=name, repo_root=repo))
        transcripts[f"thread-{name}"] = transcript
    return targets, transcripts


def _multi_lane_callbacks(
    targets: list[_Target],
    transcripts: dict[str, Path],
    messages_payload=None,
) -> LiveBusCallbacks:
    by_id = {target.id: target for target in targets}

    def default_messages_payload(bus_target, **_kwargs):
        return {
            "messages": [{"key": bus_target.id + "-m1", "kind": "task"}],
            "ackContexts": [
                {"key": "20260101T000000000001Z", "targetId": bus_target.id}
            ],
            "statusLine": {"targetId": bus_target.id},
        }

    def watch_paths(bus_target, _thread_id, transcript):
        paths = [inbox_dir(bus_target.repo_root)]
        if transcript is not None:
            paths.append(transcript.path)
        return tuple(paths)

    def signature(_bus_target, _thread_id, transcript):
        transcript_size = transcript.path.stat().st_size if transcript else 0
        return LaneSignature(transcript=transcript_size, inbox=(), other=())

    return LiveBusCallbacks(
        resolve_target=lambda selector: by_id.get(str(selector or "")),
        work_trees_payload=lambda: {},
        messages_payload=messages_payload or default_messages_payload,
        send_payload=lambda _target, _payload: ({}, None),
        task_drain_payload=lambda _target, _payload: ({}, None),
        team_snapshot_payload=lambda _since_revision: {},
        team_command_payload=lambda _payload: ({}, None),
        metric_series_payload=lambda _query: {"ok": True, "points": []},
        thread_id=lambda bus_target: "thread-" + bus_target.id,
        transcript_resolution=lambda thread_id: _transcript_resolution(
            thread_id, transcripts[thread_id]
        ),
        lane_watch_paths=watch_paths,
        lane_signature=signature,
    )


def _transcript_resolution(thread_id: str, path: Path) -> TranscriptResolution:
    return TranscriptResolution(
        thread_id=thread_id,
        path=path,
        owner_driver=CODEX_DRIVER,
    )


def _write_inbox_item_from_subprocess(repo: Path) -> None:
    env = os.environ.copy()  # env-policy: allow
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
            return pushes[0]
        time.sleep(0.02)
    pytest.fail(f"timed out waiting for watch push; sent={connection.sent!r}")


def _wait_for_reply(
    connection: _Connection,
    *,
    request_id: str | None = None,
    timeout_seconds: float = 3.0,
) -> dict[str, Any]:
    """Await a direct reply frame (no push `source`), optionally by requestId.

    Read-only verbs now compute their payload off the dispatch thread, so their
    reply lands asynchronously; tests await it here instead of reading sent[0].
    """
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        with connection.lock:
            for payload in connection.sent:
                if payload.get("source") is not None:
                    continue
                if request_id is not None and payload.get("requestId") != request_id:
                    continue
                return payload
        time.sleep(0.02)
    pytest.fail(f"timed out waiting for reply; sent={connection.sent!r}")
