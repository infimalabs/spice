"""Live bus dispatch, mutation, and worker concurrency regressions."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from dataclasses import dataclass, replace
from pathlib import Path
from threading import Condition, Event, Lock, Thread
from typing import Any

import pytest

from spice.agent.driver import CODEX_DRIVER
from spice.mail.inbox import inbox_dir
from spice.serve import livebus
from spice.serve.livebus import LaneSignature, LiveBusCallbacks, LiveBusSession
from spice.serve.messages import TranscriptResolution
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.websocket import EncodedTextFrame
from tests.test_wirefixtures import (
    valid_lane_payload,
    valid_live_bus_callback_payloads,
    valid_metric_series_payload,
)

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
        # Publish every appended frame through a Condition over the same lock
        # that guards `sent`, so reply/watch helpers block on arrival instead
        # of polling the shared list.
        self.arrival = Condition(self.lock)

    def encode_text_frame(self, payload: dict[str, Any]) -> EncodedTextFrame:
        # The session encodes to a frame before taking its send lock; the fake
        # keeps the payload dict as its "frame" so assertions read it directly,
        # and reports the real wire-text length so send telemetry stays exact.
        text_bytes = len(json.dumps(payload, separators=(",", ":")).encode("utf-8"))
        return EncodedTextFrame(payload, text_bytes)

    def send_frame(self, frame: dict[str, Any]) -> None:
        with self.arrival:
            self.sent.append(frame)
            self.arrival.notify_all()


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
        assert pong["diagnostics"] == {
            "clientId": session.client_id,
            "frames": {},
            "totals": {"count": 0, "bytes": 0},
        }

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


@pytest.mark.parametrize("lane_count", (1, 8))
def test_interactive_mutations_reply_while_target_inventory_is_computing(
    tmp_path, lane_count
):
    targets: list[_Target] = []
    transcripts: dict[str, Path] = {}
    for index in range(lane_count):
        target_id = f"lane-{index}"
        repo = tmp_path / target_id
        repo.mkdir()
        transcript = tmp_path / f"{target_id}.jsonl"
        transcript.write_text("", encoding="utf-8")
        targets.append(_Target(id=target_id, repo_root=repo))
        transcripts[f"thread-{target_id}"] = transcript
    base_callbacks = _multi_lane_callbacks(targets, transcripts)
    inventory_started = Event()
    inventory_release = Event()
    dispatch_finished = Event()

    def slow_work_trees_payload():
        inventory_started.set()
        inventory_release.wait(timeout=5.0)
        return base_callbacks.work_trees_payload()

    connection = _Connection()
    session = LiveBusSession(
        connection,
        replace(base_callbacks, work_trees_payload=slow_work_trees_payload),
    )

    def dispatch_frames() -> None:
        session._dispatch({"type": "targets.refresh", "requestId": "targets-1"})
        session._dispatch(
            {
                "type": "teams.command",
                "requestId": "team-1",
                "payload": {},
            }
        )
        session._dispatch(
            {
                "type": "lane.send",
                "requestId": "send-1",
                "targetId": targets[-1].id,
                "payload": {"text": "interactive steering"},
            }
        )
        dispatch_finished.set()

    dispatch_thread = Thread(target=dispatch_frames, daemon=True)
    dispatch_thread.start()
    try:
        assert inventory_started.wait(timeout=1.0) is True
        team_reply = _wait_for_reply(
            connection, request_id="team-1", timeout_seconds=1.0
        )
        send_reply = _wait_for_reply(
            connection, request_id="send-1", timeout_seconds=1.0
        )

        assert team_reply["type"] == "teams.commandResult"
        assert send_reply["type"] == "lane.sendResult"
        assert dispatch_finished.wait(timeout=1.0) is True

        inventory_release.set()
        targets_reply = _wait_for_reply(connection, request_id="targets-1")
        assert targets_reply["type"] == "targets.payload"
    finally:
        inventory_release.set()
        dispatch_thread.join(timeout=2.0)
        session._teardown()


def test_slow_lane_send_does_not_block_sibling_send_or_ping(tmp_path):
    targets, transcripts = _two_lane_fixture(tmp_path)
    base_callbacks = _multi_lane_callbacks(targets, transcripts)
    first_started = Event()
    first_release = Event()
    dispatch_finished = Event()

    def send_payload(target, payload):
        if target.id == targets[0].id:
            first_started.set()
            first_release.wait(timeout=5.0)
        return base_callbacks.send_payload(target, payload)

    connection = _Connection()
    session = LiveBusSession(
        connection,
        replace(base_callbacks, send_payload=send_payload),
    )

    def dispatch_frames() -> None:
        session._dispatch(
            {
                "type": "lane.send",
                "requestId": "send-blocked",
                "targetId": targets[0].id,
                "payload": {"text": "blocked publication"},
            }
        )
        session._dispatch(
            {
                "type": "lane.send",
                "requestId": "send-sibling",
                "targetId": targets[1].id,
                "payload": {"text": "sibling publication"},
            }
        )
        session._dispatch({"type": "bus.ping", "requestId": "ping-1"})
        dispatch_finished.set()

    dispatcher = Thread(target=dispatch_frames, daemon=True)
    dispatcher.start()
    try:
        assert first_started.wait(timeout=1.0) is True
        sibling = _wait_for_reply(
            connection, request_id="send-sibling", timeout_seconds=1.0
        )
        pong = _wait_for_reply(connection, request_id="ping-1", timeout_seconds=1.0)
        assert sibling["type"] == "lane.sendResult"
        assert pong["type"] == "bus.pong"
        assert dispatch_finished.wait(timeout=1.0) is True

        first_release.set()
        blocked = _wait_for_reply(connection, request_id="send-blocked")
        assert blocked["type"] == "lane.sendResult"
    finally:
        first_release.set()
        dispatcher.join(timeout=2.0)
        session._teardown()


def test_lane_sends_for_one_target_keep_fifo_publication_order(tmp_path):
    target = _Target(id="lane", repo_root=tmp_path)
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    base_callbacks = _callbacks(target=target, transcript=transcript)
    first_started = Event()
    first_release = Event()
    publication_order: list[str] = []

    def send_payload(bus_target, payload):
        text = str(payload.get("text") or "")
        publication_order.append(text + "-start")
        if text == "first":
            first_started.set()
            first_release.wait(timeout=5.0)
        publication_order.append(text + "-finish")
        return base_callbacks.send_payload(bus_target, payload)

    connection = _Connection()
    session = LiveBusSession(
        connection,
        replace(base_callbacks, send_payload=send_payload),
    )
    try:
        session._handle_lane_send(
            {
                "type": "lane.send",
                "requestId": "send-first",
                "targetId": target.id,
                "payload": {"text": "first"},
            }
        )
        assert first_started.wait(timeout=1.0) is True
        session._handle_lane_send(
            {
                "type": "lane.send",
                "requestId": "send-second",
                "targetId": target.id,
                "payload": {"text": "second"},
            }
        )
        first_release.set()
        _wait_for_reply(connection, request_id="send-first")
        _wait_for_reply(connection, request_id="send-second")

        assert publication_order == [
            "first-start",
            "first-finish",
            "second-start",
            "second-finish",
        ]
        with connection.lock:
            reply_order = [
                frame.get("requestId")
                for frame in connection.sent
                if frame.get("type") == "lane.sendResult"
            ]
        assert reply_order == ["send-first", "send-second"]
    finally:
        first_release.set()
        session._teardown()


def test_session_diagnostics_measure_frame_bytes_and_send_lock_timing(
    tmp_path, monkeypatch
):
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=tmp_path)
    connection = _Connection()
    session = LiveBusSession(
        connection,
        _callbacks(target=target, transcript=transcript),
    )
    ticks = iter((1.0, 1.004, 1.005, 1.011))
    monkeypatch.setattr(livebus.time, "perf_counter", lambda: next(ticks))
    payload = {
        "type": "lane.payload",
        "payload": valid_lane_payload(messages=[]),
    }

    timing = session._send(payload)
    diagnostics = session.diagnostics()

    expected_bytes = len(json.dumps(payload, separators=(",", ":")).encode("utf-8"))
    frame = diagnostics["frames"]["lane.payload"]
    assert timing.lock_wait_ms == pytest.approx(4.0)
    assert timing.lock_hold_ms == pytest.approx(7.0)
    assert timing.write_ms == pytest.approx(6.0)
    assert frame["count"] == 1
    assert frame["bytes"] == expected_bytes
    assert frame["sendLockWaitMsTotal"] == pytest.approx(4.0)
    assert frame["sendLockWaitMsLast"] == pytest.approx(4.0)
    assert frame["sendLockWaitMsMax"] == pytest.approx(4.0)
    assert frame["sendLockHoldMsTotal"] == pytest.approx(7.0)
    assert frame["sendLockHoldMsLast"] == pytest.approx(7.0)
    assert frame["sendLockHoldMsMax"] == pytest.approx(7.0)
    assert diagnostics["totals"] == {"count": 1, "bytes": expected_bytes}


def test_send_sizes_telemetry_from_the_single_encode(tmp_path):
    # _send records frame telemetry bytes from encode_text_frame's reported
    # payload length, so each frame is serialized exactly once. A fake that
    # reports a sentinel length distinct from the real wire size makes any
    # second json.dumps observable: a re-encode would size telemetry from the
    # true wire length instead of the sentinel the single encode returned.
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=tmp_path)
    payload = {"type": "lane.payload", "payload": valid_lane_payload(messages=[])}
    real_bytes = len(json.dumps(payload, separators=(",", ":")).encode("utf-8"))
    sentinel_bytes = real_bytes + 1

    class _SentinelEncodeConnection(_Connection):
        def encode_text_frame(self, payload: dict[str, Any]) -> EncodedTextFrame:
            return replace(
                super().encode_text_frame(payload), payload_bytes=sentinel_bytes
            )

    connection = _SentinelEncodeConnection()
    session = LiveBusSession(
        connection,
        _callbacks(target=target, transcript=transcript),
    )

    session._send(payload)

    diagnostics = session.diagnostics()
    assert diagnostics["frames"]["lane.payload"]["bytes"] == sentinel_bytes
    assert diagnostics["totals"]["bytes"] == sentinel_bytes


def test_ping_reset_clears_frame_telemetry_between_windows(tmp_path):
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=tmp_path)
    connection = _Connection()
    session = LiveBusSession(
        connection,
        _callbacks(target=target, transcript=transcript),
    )
    try:
        # First measurement window: one frame lands in the counters.
        session._send(
            {"type": "lane.payload", "payload": valid_lane_payload(messages=[])}
        )
        assert session.diagnostics()["totals"]["count"] == 1

        # A diagnostic client (the latency probe) resets telemetry with the same
        # ping it reads the pong from. The pong still reports the window it just
        # measured...
        session._handle_ping(
            {"type": "bus.ping", "requestId": "ping-reset", "reset": True}
        )
        pong = _wait_for_reply(connection, request_id="ping-reset")
        assert pong["type"] == "bus.pong"
        assert pong["diagnostics"]["totals"]["count"] == 1

        # ...and the reset lands after the reply, so the next window starts
        # genuinely empty -- totals AND per-frame maxima both gone.
        assert session.diagnostics() == {
            "clientId": session.client_id,
            "frames": {},
            "totals": {"count": 0, "bytes": 0},
        }

        # A frame sent in the fresh window counts from one, proving the window
        # owns its counters rather than inheriting the prior run's high-water.
        session._send(
            {"type": "lane.payload", "payload": valid_lane_payload(messages=[])}
        )
        assert session.diagnostics()["totals"]["count"] == 1
    finally:
        session._teardown()


@pytest.mark.parametrize("background_count", (1, 100))
def test_background_lane_burst_coalesces_before_focused_delivery(
    tmp_path, monkeypatch, background_count
):
    callbacks: list[Any] = []

    class DeferredTimer:
        def __init__(self, _seconds, callback):
            self.callback = callback
            self.daemon = False

        def start(self):
            callbacks.append(self.callback)

        def cancel(self):
            return None

    monkeypatch.setattr(livebus, "Timer", DeferredTimer)
    target = _Target(id="focused", repo_root=tmp_path)
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    connection = _Connection()
    session = LiveBusSession(
        connection, _callbacks(target=target, transcript=transcript)
    )
    for index in range(background_count):
        subscription = livebus._LaneSubscription(
            target=_Target(id=f"background-{index}", repo_root=tmp_path),
            query={"focused": False},
            generation=f"generation-{index}",
        )
        for _change in range(10):
            assert session.coalesce_background_update(subscription) is True

    session._send(
        {
            "type": "lane.payload",
            "targetId": target.id,
            "payload": valid_lane_payload(),
        }
    )
    assert [frame["type"] for frame in connection.sent] == ["lane.payload"]
    assert len(callbacks) == 1

    callbacks[0]()
    assert [frame["type"] for frame in connection.sent] == [
        "lane.payload",
        "lanes.dirty",
    ]
    assert len(connection.sent[1]["lanes"]) == background_count
    diagnostics = session.diagnostics()["frames"]
    assert diagnostics["lane.payload"]["count"] == 1
    assert diagnostics["lanes.dirty"]["count"] == 1


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
        return {"messages": [], "statusLine": {"preview": after}}

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
    monkeypatch.setattr(livebus, "wait_for_change", _idle_wait)
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
        return valid_metric_series_payload(metric=str(query["series"]))

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
    assert results[0]["result"] == valid_metric_series_payload(metric="burndown")


class _DeadConnection:
    """Peer that vanished: every write raises like a closed socket."""

    def __init__(self) -> None:
        self.attempts = 0
        self.lock = Lock()

    def encode_text_frame(self, payload: dict[str, Any]) -> EncodedTextFrame:
        text_bytes = len(json.dumps(payload, separators=(",", ":")).encode("utf-8"))
        return EncodedTextFrame(payload, text_bytes)

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
        return valid_metric_series_payload(metric=str(query["series"]))

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

    raw_messages_payload = messages_payload or default_messages_payload

    def wire_messages_payload(bus_target, **kwargs):
        return valid_lane_payload(**raw_messages_payload(bus_target, **kwargs))

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
        **valid_live_bus_callback_payloads(messages_payload=wire_messages_payload),
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
            "ackContexts": [{"key": "1jN54zJK", "text": bus_target.id}],
            "statusLine": {"preview": bus_target.id},
        }

    raw_messages_payload = messages_payload or default_messages_payload

    def wire_messages_payload(bus_target, **kwargs):
        return valid_lane_payload(**raw_messages_payload(bus_target, **kwargs))

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
        **valid_live_bus_callback_payloads(messages_payload=wire_messages_payload),
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
                "write_inbox_item(repo, '1jN54zJK.txt', text)\n"
            ),
            str(repo),
        ],
        check=True,
        env=env,
    )


def _wait_for_watch_push(
    connection: _Connection, *, timeout_seconds: float = 3.0
) -> dict[str, Any]:
    def first_push() -> dict[str, Any] | None:
        for payload in connection.sent:
            if payload.get("source") == "watch":
                return payload
        return None

    with connection.arrival:
        push = connection.arrival.wait_for(first_push, timeout=timeout_seconds)
    if push is not None:
        return push
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

    def first_reply() -> dict[str, Any] | None:
        for payload in connection.sent:
            if payload.get("source") is not None:
                continue
            if request_id is not None and payload.get("requestId") != request_id:
                continue
            return payload
        return None

    with connection.arrival:
        reply = connection.arrival.wait_for(first_reply, timeout=timeout_seconds)
    if reply is not None:
        return reply
    pytest.fail(f"timed out waiting for reply; sent={connection.sent!r}")
