"""Focus ordering while a batched live-bus subscription reply is pending."""

from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from pathlib import Path
from queue import Empty, Queue
from threading import Condition, Event, Lock, Thread
from typing import Any

from spice.agent.driver import CODEX_DRIVER
from spice.serve import livebus
from spice.serve.livebus import LaneSignature, LiveBusCallbacks, LiveBusSession
from spice.serve.messages import TranscriptResolution
from spice.serve.websocket import EncodedTextFrame
from tests.test_wirefixtures import (
    valid_lane_payload,
    valid_live_bus_callback_payloads,
)


@dataclass(frozen=True)
class _Target:
    id: str
    repo_root: Path


class _Connection:
    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []
        self.lock = Lock()
        # Publish every appended frame through a Condition over the same lock
        # that guards `sent`, so watch helpers block on arrival instead of
        # polling the shared list.
        self.arrival = Condition(self.lock)

    def encode_text_frame(self, payload: dict[str, Any]) -> EncodedTextFrame:
        # Keep the payload dict as the "frame" for direct assertions, and report
        # the real wire-text length so send telemetry stays exact.
        text_bytes = len(json.dumps(payload, separators=(",", ":")).encode("utf-8"))
        return EncodedTextFrame(payload, text_bytes)

    def send_frame(self, frame: dict[str, Any]) -> None:
        with self.arrival:
            self.sent.append(frame)
            self.arrival.notify_all()


class _HeldSubscribeConnection(_Connection):
    def __init__(self, response_started: Event, release_response: Event) -> None:
        super().__init__()
        self.response_started = response_started
        self.release_response = release_response

    def send_frame(self, frame: dict[str, Any]) -> None:
        if frame.get("requestId") == "focus-pending":
            self.response_started.set()
            self.release_response.wait(timeout=2.0)
        super().send_frame(frame)


@dataclass
class _PendingFocusRace:
    prior_id: str
    selected_id: str
    connection: _Connection
    session: LiveBusSession
    response_started: Event
    release_response: Event
    change_queues: dict[str, Queue[Any]]
    change_dequeued: dict[str, Event]
    signature_seen: dict[str, Event]
    revisions: dict[str, int]
    dirty_callbacks: list[Any]
    configure_threads: list[Thread] = field(default_factory=list)

    def run(self) -> dict[str, Any]:
        response_held = self._subscribe()
        queued_before_release = self._queue_changes_before_release()
        configured_focus = self._configure_pending_focus()
        self.release_response.set()
        self._join_configure_threads()
        processed_after_release = [
            self.signature_seen[target_id].wait(timeout=1.0)
            for target_id in (self.prior_id, self.selected_id)
        ]
        burst_processed = self._queue_background_burst()
        direct_push = _wait_for_watch_push(self.connection)
        timer_count = len(self.dirty_callbacks)
        if self.dirty_callbacks:
            self.dirty_callbacks[0]()
        return self._outcome(
            response_held=response_held,
            queued_before_release=queued_before_release,
            configured_focus=configured_focus,
            processed_after_release=processed_after_release,
            burst_processed=burst_processed,
            direct_push=direct_push,
            timer_count=timer_count,
        )

    def close(self) -> None:
        self.release_response.set()
        self._join_configure_threads()
        self.session._teardown()

    def _subscribe(self) -> bool:
        self.session._handle_lanes_subscribe(
            {
                "type": "lanes.subscribe",
                "requestId": "focus-pending",
                "entries": [
                    {"targetId": self.prior_id, "query": {"limit": 5, "focused": True}},
                    {
                        "targetId": self.selected_id,
                        "query": {"limit": 5, "focused": False},
                    },
                ],
            }
        )
        return self.response_started.wait(timeout=1.0)

    def _queue_changes_before_release(self) -> list[bool]:
        queued: list[bool] = []
        for target_id in (self.prior_id, self.selected_id):
            self.signature_seen[target_id].clear()
            self.revisions[target_id] = 1
            self.change_queues[target_id].put(True)
            queued.append(self.change_dequeued[target_id].wait(timeout=1.0))
        return queued

    def _configure_pending_focus(self) -> tuple[bool, bool] | None:
        self.configure_threads = [
            Thread(target=self._configure, args=(self.prior_id, False), daemon=True),
            Thread(target=self._configure, args=(self.selected_id, True), daemon=True),
        ]
        for thread in self.configure_threads:
            thread.start()
        deadline = time.monotonic() + 1.0
        configured: tuple[bool, bool] | None = None
        while time.monotonic() < deadline:
            configured = self._configured_focus()
            if configured == (False, True):
                break
            time.sleep(0.01)
        return configured

    def _configure(self, target_id: str, focused: bool) -> None:
        self.session._handle_lane_configure(
            {
                "type": "lane.configure",
                "requestId": f"focus-{target_id}",
                "targetId": target_id,
                "query": {"limit": 5, "focused": focused},
            }
        )

    def _configured_focus(self) -> tuple[bool, bool]:
        with self.session.subscriptions[self.prior_id].lock:
            prior = self.session.subscriptions[self.prior_id].query.get("focused")
        with self.session.subscriptions[self.selected_id].lock:
            selected = self.session.subscriptions[self.selected_id].query.get("focused")
        return (prior, selected)

    def _join_configure_threads(self) -> None:
        for thread in self.configure_threads:
            thread.join(timeout=1.0)

    def _queue_background_burst(self) -> list[bool]:
        processed: list[bool] = []
        for revision in (2, 3):
            self.signature_seen[self.prior_id].clear()
            self.revisions[self.prior_id] = revision
            self.change_queues[self.prior_id].put(True)
            processed.append(self.signature_seen[self.prior_id].wait(timeout=1.0))
        return processed

    def _outcome(self, **evidence: Any) -> dict[str, Any]:
        with self.connection.lock:
            direct_frames = [
                frame
                for frame in self.connection.sent
                if frame.get("type") == "lane.payload"
                and frame.get("source") == "watch"
            ]
            dirty_frames = [
                frame
                for frame in self.connection.sent
                if frame.get("type") == "lanes.dirty"
            ]
        direct_push = evidence.pop("direct_push")
        return {
            **evidence,
            "direct_push_target": direct_push.get("targetId"),
            "direct_targets": [frame.get("targetId") for frame in direct_frames],
            "dirty_frame_count": len(dirty_frames),
            "dirty_targets": [
                lane["targetId"] for frame in dirty_frames for lane in frame["lanes"]
            ],
            "prior_burst_revision": self.revisions[self.prior_id],
        }


def test_pending_focus_configures_before_subscribe_releases_queued_changes(
    tmp_path, monkeypatch
):
    race = _pending_focus_race(tmp_path, monkeypatch)
    try:
        outcome = race.run()
    finally:
        race.close()

    assert outcome == {
        "response_held": True,
        "queued_before_release": [True, True],
        "configured_focus": (False, True),
        "processed_after_release": [True, True],
        "burst_processed": [True, True],
        "timer_count": 1,
        "direct_push_target": race.selected_id,
        "direct_targets": [race.selected_id],
        "dirty_frame_count": 1,
        "dirty_targets": [race.prior_id],
        "prior_burst_revision": 3,
    }


def _pending_focus_race(tmp_path: Path, monkeypatch) -> _PendingFocusRace:
    targets, transcripts = _two_lane_fixture(tmp_path)
    prior_id, selected_id = (target.id for target in targets)
    response_started = Event()
    release_response = Event()
    change_queues = {target.id: Queue() for target in targets}
    change_dequeued = {target.id: Event() for target in targets}
    signature_seen = {target.id: Event() for target in targets}
    revisions = {target.id: 0 for target in targets}
    dirty_callbacks: list[Any] = []

    class DeferredTimer:
        def __init__(self, _seconds, callback):
            self.callback = callback
            self.daemon = False

        def start(self):
            dirty_callbacks.append(self.callback)

        def cancel(self):
            return None

    monkeypatch.setattr(
        livebus,
        "wait_for_change",
        _observed_wait(change_queues, change_dequeued),
    )
    monkeypatch.setattr(livebus, "Timer", DeferredTimer)
    callbacks = _focus_callbacks(targets, transcripts, revisions, signature_seen)
    connection = _HeldSubscribeConnection(response_started, release_response)
    return _PendingFocusRace(
        prior_id=prior_id,
        selected_id=selected_id,
        connection=connection,
        session=LiveBusSession(connection, callbacks),
        response_started=response_started,
        release_response=release_response,
        change_queues=change_queues,
        change_dequeued=change_dequeued,
        signature_seen=signature_seen,
        revisions=revisions,
        dirty_callbacks=dirty_callbacks,
    )


def _observed_wait(change_queues, change_dequeued):
    def wait(paths: tuple[Path, ...], stop, watch=None, *, activated=None) -> bool:
        if activated is not None:
            activated.set()
        target_id = paths[0].name
        try:
            change_queues[target_id].get(timeout=0.1)
        except Empty:
            stop.wait(timeout=0.02)
            return False
        change_dequeued[target_id].set()
        return not stop.is_set()

    return wait


def _focus_callbacks(
    targets: list[_Target],
    transcripts: dict[str, Path],
    revisions: dict[str, int],
    signature_seen: dict[str, Event],
) -> LiveBusCallbacks:
    by_id = {target.id: target for target in targets}

    def signature(target, _thread_id, _transcript):
        signature_seen[target.id].set()
        return LaneSignature(transcript=revisions[target.id], inbox=(), other=())

    def messages_payload(target, **_kwargs):
        return valid_lane_payload(
            messages=[{"key": f"{target.id}-{revisions[target.id]}", "kind": "task"}],
            statusLine={"preview": target.id},
        )

    return LiveBusCallbacks(
        resolve_target=lambda selector: by_id.get(str(selector or "")),
        **valid_live_bus_callback_payloads(messages_payload=messages_payload),
        thread_id=lambda target: "thread-" + target.id,
        transcript_resolution=lambda thread_id: TranscriptResolution(
            thread_id=thread_id,
            path=transcripts[thread_id],
            owner_driver=CODEX_DRIVER,
        ),
        lane_watch_paths=lambda target, _thread_id, _transcript: (Path(target.id),),
        lane_signature=signature,
    )


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


def _wait_for_watch_push(
    connection: _Connection, *, timeout_seconds: float = 3.0
) -> dict[str, Any]:
    def first_push() -> dict[str, Any] | None:
        for frame in connection.sent:
            if frame.get("source") == "watch":
                return frame
        return None

    with connection.arrival:
        push = connection.arrival.wait_for(first_push, timeout=timeout_seconds)
    if push is not None:
        return push
    return {"targetId": "timed-out"}
