"""Keyed livebus submission lifecycle integration coverage."""

from __future__ import annotations

import json
import subprocess
from datetime import datetime
from http import HTTPStatus
from pathlib import Path
from threading import Condition, Event, Semaphore
from typing import Any, Callable

import pytest

from spice.agent.driver import CODEX_DRIVER
from spice.agent.lifecycle import utc_now
from spice.mail.ackarchive import archive_ackd_inbox_items, archive_nackd_inbox_items
from spice.mail.inbox import (
    compose_inbox_text,
    inbox_dir,
    inbox_item_key,
    write_inbox_item,
)
from spice.mail.replies import (
    append_reply_record,
    ensure_reply_log,
    read_reply_records,
)
from spice.serve import livebus, messages as message_reader, submissions
from spice.serve.livebus import LaneSignature, LiveBusCallbacks, LiveBusSession
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.submissions import SubmissionLifecycleTracker
from spice.serve.websocket import EncodedTextFrame
from tests.test_livebus import _Target, _subscribe_lane, _transcript_resolution
from tests.test_wirefixtures import (
    valid_lane_payload,
    valid_live_bus_callback_payloads,
)

THREAD_ID = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


def test_submission_tracker_caps_mixed_lifecycle_states(monkeypatch) -> None:
    monkeypatch.setattr(submissions, "MAX_TRACKED_SUBMISSIONS", 3)
    tracker = SubmissionLifecycleTracker(now=lambda: "2026-07-10T07:00:00.000000Z")

    tracker.accept(target_id="lane", key="completed-old", evidence="old")
    tracker.advance(
        target_id="lane",
        repo_root=None,
        payload={
            "messages": [
                {
                    "ack_keys": ["completed-old"],
                    "key": "message-completed",
                    "kind": "final",
                }
            ]
        },
    )
    tracker.accept(target_id="lane", key="received-middle", evidence="middle")
    tracker.advance(
        target_id="lane",
        repo_root=None,
        payload={
            "messages": [
                {
                    "ack_keys": ["received-middle"],
                    "key": "message-received",
                    "kind": "assistant",
                }
            ]
        },
    )
    tracker.accept(target_id="lane", key="accepted-new", evidence="new")
    tracker.accept(target_id="lane", key="newest", evidence="newest")

    retained = [
        tracker.accept(target_id="lane", key=key, evidence="replacement")
        for key in ("received-middle", "accepted-new", "newest")
    ]
    reaccepted = tracker.accept(
        target_id="lane", key="completed-old", evidence="reaccepted"
    )

    assert [event["stages"]["accepted"]["evidence"] for event in retained] == [
        "middle",
        "new",
        "newest",
    ]
    assert reaccepted["stages"]["accepted"]["evidence"] == "reaccepted"


@pytest.mark.parametrize("lane_state", ["running", "idle"])
def test_submission_lifecycle_follows_real_watcher_events(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, lane_state: str
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    reply_log = ensure_reply_log(repo, THREAD_ID)
    assert reply_log is not None
    target = _Target(id="lane", repo_root=repo)
    connection = _FrameConnection()
    gate = _WatchGate()
    monkeypatch.setattr(livebus, "wait_for_change", gate.wait)
    session = LiveBusSession(
        connection,
        _submission_callbacks(
            target=target, transcript=transcript, reply_log=reply_log
        ),
    )

    try:
        _subscribe_lane(session, target.id, limit=10)
        assert gate.ready.wait(timeout=1.0)

        session._handle_lane_send(
            {
                "type": "lane.send",
                "requestId": "send-1",
                "targetId": target.id,
                "payload": {"text": f"{lane_state} steering"},
            }
        )
        accepted_frame = _wait_for_frame(
            connection,
            lambda frame: (
                frame.get("type") == "lane.sendResult"
                and frame.get("requestId") == "send-1"
            ),
        )
        accepted = accepted_frame["result"]["submission"]
        key = accepted["key"]
        gate.wake()
        pending = _wait_for_frame(
            connection,
            lambda frame: (
                frame.get("type") == "lane.pending"
                and key in frame.get("payload", {}).get("pendingInboxKeys", [])
            ),
        )
        assert pending["payload"]["pendingInboxCount"] == 1

        if lane_state == "running":
            ack_text = f"ACK {key}: received and working"
            _append_codex_message(transcript, ack_text, phase="commentary")
            assert archive_ackd_inbox_items(repo, [key], ack_text=ack_text) == [key]
            gate.wake()
            received = _wait_for_submission_stage(connection, "received")
            _append_codex_message(transcript, "Work completed.", phase="final_answer")
        else:
            nack_text = f"NACK {key}: request cannot be applied"
            assert archive_nackd_inbox_items(repo, [key], nack_text=nack_text) == [key]
            gate.wake()
            received = _wait_for_submission_stage(connection, "received")
            append_reply_record(
                repo,
                THREAD_ID,
                timestamp=utc_now(),
                text=nack_text,
                ack_keys=[],
                nack_keys=[key],
            )
        gate.wake()
        completed = _wait_for_submission_stage(connection, "completed")

        events = [accepted, received["submission"], completed["submission"]]
        assert [event["stage"] for event in events] == [
            "accepted",
            "received",
            "completed",
        ]
        assert [event["key"] for event in events] == [key, key, key]
        assert _frame_positions(
            connection, [accepted_frame, received, completed]
        ) == sorted(_frame_positions(connection, [accepted_frame, received, completed]))
        stages = completed["submission"]["stages"]
        assert list(stages) == ["accepted", "received", "completed"]
        assert stages["accepted"]["source"] == "inbox-write"
        assert stages["received"]["source"] == (
            "transcript" if lane_state == "running" else "ack-state"
        )
        assert stages["completed"]["source"] == (
            "transcript" if lane_state == "running" else "reply-log"
        )
        assert completed["submission"]["disposition"] == (
            "acked" if lane_state == "running" else "refused"
        )
        assert [_parse_iso(stages[name]["at"]) for name in stages] == sorted(
            _parse_iso(stages[name]["at"]) for name in stages
        )
        durations = completed["submission"]["durationsMs"]
        assert set(durations) == {
            "acceptedToReceived",
            "acceptedToCompleted",
            "receivedToCompleted",
        }
        assert all(value >= 0.0 for value in durations.values())
    finally:
        gate.wake()
        session._teardown()


class _WatchGate:
    def __init__(self) -> None:
        self.ready = Event()
        self._changes = Semaphore(0)

    def wait(
        self,
        _paths: tuple[Path, ...],
        stop: Event,
        watch: Any = None,
        *,
        activated: Event | None = None,
    ) -> bool:
        if activated is not None:
            activated.set()
        self.ready.set()
        while not stop.is_set():
            if self._changes.acquire(timeout=0.05):
                return True
        return False

    def wake(self) -> None:
        self._changes.release()


class _FrameConnection:
    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []
        self.lock = Condition()

    def encode_text_frame(self, payload: dict[str, Any]) -> EncodedTextFrame:
        # The session encodes to a frame before taking its send lock; the fake
        # keeps the payload dict as its "frame" so assertions read it directly,
        # and reports the real wire-text length so send telemetry stays exact.
        text_bytes = len(json.dumps(payload, separators=(",", ":")).encode("utf-8"))
        return EncodedTextFrame(payload, text_bytes)

    def send_frame(self, frame: dict[str, Any]) -> None:
        with self.lock:
            self.sent.append(frame)
            self.lock.notify_all()


def _submission_callbacks(
    *, target: _Target, transcript: Path, reply_log: Path
) -> LiveBusCallbacks:
    def messages_payload(_target: _Target, **_kwargs: Any) -> dict[str, Any]:
        items = message_reader.read_assistant_messages(
            transcript, limit=50, driver=CODEX_DRIVER
        )
        transcript_size = transcript.stat().st_size
        for index, record in enumerate(read_reply_records(target.repo_root, THREAD_ID)):
            items.append(
                message_reader.reply_card_message(
                    f"reply:{record['timestamp']}#{index}",
                    transcript_size + index,
                    str(record["timestamp"]),
                    str(record.get("text") or ""),
                )
            )
        items.sort(key=lambda item: (item.timestamp, item.index))
        pending = pending_inbox_identity_payload(target.repo_root)
        return valid_lane_payload(
            messages=[item.to_payload() for item in items],
            **pending,
            statusLine=pending,
        )

    def send_payload(
        _target: _Target, payload: dict[str, Any]
    ) -> tuple[dict[str, Any], HTTPStatus]:
        path = write_inbox_item(
            target.repo_root,
            None,
            compose_inbox_text(
                body=str(payload.get("text") or ""), priority=None, stop=False
            ),
        )
        return {
            "ok": True,
            "key": inbox_item_key(path.name),
            "path": str(path),
            **pending_inbox_identity_payload(target.repo_root),
        }, HTTPStatus.OK

    def signature(
        _target: _Target, _thread_id: str | None, _transcript: Any
    ) -> LaneSignature:
        pending = tuple(
            sorted(path.name for path in inbox_dir(target.repo_root).glob("*.txt"))
        )
        return LaneSignature(
            transcript=_path_signature(transcript),
            inbox=pending,
            other=_path_signature(reply_log),
        )

    return LiveBusCallbacks(
        resolve_target=lambda selector: target if selector == target.id else None,
        **valid_live_bus_callback_payloads(
            messages_payload=messages_payload,
            send_payload=send_payload,
        ),
        thread_id=lambda _target: THREAD_ID,
        transcript_resolution=lambda _thread_id: _transcript_resolution(
            THREAD_ID, transcript
        ),
        lane_watch_paths=lambda *_args: (
            inbox_dir(target.repo_root),
            transcript,
            reply_log,
        ),
        lane_signature=signature,
    )


def _append_codex_message(transcript: Path, text: str, *, phase: str) -> None:
    record = {
        "timestamp": utc_now(),
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "phase": phase,
            "content": [{"type": "output_text", "text": text}],
        },
    }
    with transcript.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, separators=(",", ":")) + "\n")


def _path_signature(path: Path) -> tuple[int, int]:
    stat = path.stat()
    return stat.st_mtime_ns, stat.st_size


def _wait_for_submission_stage(
    connection: _FrameConnection, stage: str
) -> dict[str, Any]:
    return _wait_for_frame(
        connection,
        lambda frame: (
            frame.get("type") == "lane.submission"
            and frame.get("submission", {}).get("stage") == stage
        ),
    )


def _wait_for_frame(
    connection: _FrameConnection,
    predicate: Callable[[dict[str, Any]], bool],
    *,
    timeout: float = 2.0,
) -> dict[str, Any]:
    matched: list[dict[str, Any]] = []

    def frame_delivered() -> bool:
        matched[:] = [frame for frame in connection.sent if predicate(frame)]
        return bool(matched)

    with connection.lock:
        if connection.lock.wait_for(frame_delivered, timeout=timeout):
            return matched[0]
        sent = list(connection.sent)
    pytest.fail(f"timed out waiting for livebus frame; sent={sent!r}")


def _frame_positions(
    connection: _FrameConnection, frames: list[dict[str, Any]]
) -> list[int]:
    with connection.lock:
        return [connection.sent.index(frame) for frame in frames]


def _parse_iso(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))
