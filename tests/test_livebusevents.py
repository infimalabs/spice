"""Live bus watcher and event-delivery integration coverage."""

from __future__ import annotations

import json
import os
import subprocess
from dataclasses import replace
from http import HTTPStatus
from pathlib import Path
from threading import Event, Lock, current_thread
from types import SimpleNamespace
from typing import Any

import pytest

from spice.agent import lifecycle
from spice.agent.driver import CODEX_DRIVER
from spice.mail.inbox import inbox_dir
from spice.mail.replies import append_reply_record, reply_log_path
from spice.serve import (
    agentapi,
    app,
    httpapi,
    livebus,
    livebuswatch,
    messages as message_reader,
)
from spice.serve.app import ServeState
from spice.serve.lifecycle import (
    LIFECYCLE_RECONCILER_THREAD_PREFIX,
    start_lifecycle_reconciler,
)
from spice.serve.livebus import LaneSignature, LiveBusCallbacks, LiveBusSession
from spice.serve.payload import identity, lane, message
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.team.store import ServeTeamStore
from spice.serve.workroutes import work_tree_send_accepted_response_payload
from spice.serve.worktree import inventory
from spice.serve.worktree.target import WorktreeTarget
from spice.tasks import config as task_config
from tests.test_livebus import (
    THREAD_ID,
    _callbacks,
    _Connection,
    _multi_lane_callbacks,
    _subscribe_lane,
    _Target,
    _transcript_resolution,
    _wait_for_reply,
    _wait_for_watch_push,
    _write_inbox_item_from_subprocess,
)
from tests.test_wirefixtures import valid_lane_payload, valid_live_bus_callback_payloads

# A deliberately blocked ensure only has to outlive the assertions made while it
# is parked; the release bound is the escape hatch for a test that stops early.
BLOCKED_ENSURE_ENTRY_SECONDS = 5.0
BLOCKED_ENSURE_RELEASE_SECONDS = 15.0


def _pending_lane_chrome(
    target_id: str = "lane",
    *,
    count: int = 1,
) -> dict[str, Any]:
    """Build the canonical inbox facet through the production assembler."""
    return lane.lane_chrome_payload(
        target_id=target_id,
        pending_identity={
            "pendingInboxCount": count,
            "pendingInboxLabel": str(count),
            "pendingInboxKeys": ["inbox-key"] if count else [],
            "pendingInboxVersion": 1,
        },
    )


def _single_change_wait(path: Path, ready: Event, changed: Event):
    """Deliver one latched test change exactly once, then park until stopped."""
    delivered = False

    def wait(paths: tuple[Path, ...], stop, watch=None, *, activated=None) -> bool:
        nonlocal delivered
        if activated is not None:
            activated.set()
        assert path in paths
        ready.set()
        if delivered:
            # One filesystem edge is one watcher iteration: once the change has
            # been reported, park on stop so teardown releases us instead of
            # spinning True on every re-wait.
            stop.wait()
            return False
        changed.wait()
        delivered = True
        return not stop.is_set()

    return wait


def test_existing_watch_paths_returns_existing_input_paths(tmp_path):
    parent = tmp_path / "parent"
    parent.mkdir()
    missing = parent / "missing.txt"

    assert livebuswatch._existing_watch_paths((parent, missing)) == (parent,)


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


def _team_move_lane_fixture(tmp_path: Path, lane_count: int):
    targets: list[_Target] = []
    transcripts: dict[str, Path] = {}
    task_config.set_backend(str(tmp_path / "task-backend"))
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    for index in range(lane_count):
        target_id = f"lane-{index:02d}"
        repo = tmp_path / f"repo-{target_id}"
        repo.mkdir()
        transcript = tmp_path / f"{target_id}.jsonl"
        transcript.write_text("", encoding="utf-8")
        targets.append(_Target(id=target_id, repo_root=repo))
        transcripts[f"thread-{target_id}"] = transcript
        store.create_team(
            team_id=f"team-{index:02d}",
            members=[f"target:{target_id}"],
        )
    destination = store.create_team(team_id="team-moved", members=[])
    return targets, transcripts, store, destination


def _two_round_lane_event_waiter(
    event_path: Path,
    lane_count: int,
    changes: tuple[Event, Event],
    completions: tuple[Event, Event],
):
    visits: dict[Path, int] = {}
    completed: tuple[set[Path], set[Path]] = (set(), set())
    visit_lock = Lock()

    def wait(paths: tuple[Path, ...], stop, watch=None, *, activated=None) -> bool:
        del watch
        if activated is not None:
            activated.set()
        lane_path = next(path for path in paths if path != event_path)
        with visit_lock:
            visit = visits.get(lane_path, 0) + 1
            visits[lane_path] = visit
            completed_round = visit - 2
            if completed_round in (0, 1):
                completed[completed_round].add(lane_path)
                if len(completed[completed_round]) == lane_count:
                    completions[completed_round].set()
        if visit in (1, 2):
            return changes[visit - 1].wait(timeout=2.0) and not stop.is_set()
        stop.wait()
        return False

    return wait


def _watch_targets(connection: _Connection) -> list[str]:
    with connection.lock:
        return [
            frame["targetId"]
            for frame in connection.sent
            if frame.get("source") == "watch"
        ]


def test_team_wake_pushes_only_changed_lane_then_task_wake_pushes_every_lane(
    tmp_path, monkeypatch
):
    lane_count = 12
    targets, transcripts, store, destination = _team_move_lane_fixture(
        tmp_path, lane_count
    )
    moved_target_id = "lane-07"
    event_path = task_config.ensure_task_event_file()
    monkeypatch.setattr(httpapi, "_inbox_signature", lambda _repo_root: ())
    monkeypatch.setattr(
        httpapi,
        "_reply_log_signature",
        lambda _repo_root, _thread_id: ("", 0, 0),
    )
    monkeypatch.setattr(httpapi, "_agent_state_signature_path", lambda _repo_root: None)
    team_change, task_change = Event(), Event()
    team_round_complete, task_round_complete = Event(), Event()
    event_round_wait = _two_round_lane_event_waiter(
        event_path,
        lane_count,
        (team_change, task_change),
        (team_round_complete, task_round_complete),
    )
    callbacks = _multi_lane_callbacks(targets, transcripts)
    callbacks = replace(
        callbacks,
        thread_id=lambda _target: None,
        lane_watch_paths=lambda target, _thread_id, _transcript: (
            event_path,
            target.repo_root,
        ),
        lane_signature=lambda target, thread_id, transcript: (
            app.lane_signature_for_target(
                SimpleNamespace(team_store=store),
                target,
                thread_id,
                transcript,
            )
        ),
    )
    monkeypatch.setattr(livebus, "wait_for_change", event_round_wait)
    connection = _Connection()
    session = LiveBusSession(connection, callbacks)

    try:
        session._handle_lanes_subscribe(
            {
                "type": "lanes.subscribe",
                "requestId": "all-lanes",
                "entries": [
                    {"targetId": target.id, "query": {"limit": 5}} for target in targets
                ],
            }
        )
        _wait_for_reply(connection, request_id="all-lanes")

        store.assign_agent(destination.team_id, f"target:{moved_target_id}")
        team_change.set()
        assert team_round_complete.wait(timeout=2.0)
        team_watch_targets = _watch_targets(connection)
        assert team_watch_targets == [moved_target_id]

        task_config.mark_task_backend_changed("task")
        task_change.set()
        assert task_round_complete.wait(timeout=2.0)
        all_watch_targets = _watch_targets(connection)
        task_watch_targets = all_watch_targets[len(team_watch_targets) :]
        assert sorted(task_watch_targets) == sorted(target.id for target in targets)
    finally:
        team_change.set()
        task_change.set()
        session._teardown()
        task_config.set_backend(None)


def test_lane_subscription_pushes_structural_final_status(tmp_path, monkeypatch):
    # status_line_payload includes claimed-task metadata. Keep that read on a
    # per-test backend so xdist load cannot block this watcher on the operator
    # board's shared Git/bootstrap path.
    isolated_task_backend = tmp_path / "task-backend"
    task_config.set_backend(str(isolated_task_backend))
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
        started_at="",
        log_path=None,
        prompt_skill_path=None,
        command=(),
    )
    monkeypatch.setattr(lane, "agent_status", lambda _repo: status)
    connection = _Connection()
    watcher_ready = Event()
    change_written = Event()

    def messages_payload(_target, **_kwargs):
        items = message_reader.read_assistant_messages(
            transcript, limit=5, driver=CODEX_DRIVER
        )
        pending = pending_inbox_identity_payload(repo)
        return valid_lane_payload(
            messages=[item.to_payload() for item in items],
            statusLine=lane.status_line_payload(
                SimpleNamespace(),
                target,
                items=items,
                error=None,
            ),
            chrome=lane.lane_chrome_payload(
                target_id=target.id,
                pending_identity=pending,
                last_assistant_at=lane.lane_activity_at(items),
            ),
        )

    monkeypatch.setattr(
        livebus,
        "wait_for_change",
        _single_change_wait(transcript, watcher_ready, change_written),
    )
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
        assert pushed["payload"]["chrome"]["pendingInbox"]["value"]["count"] == 0
    finally:
        change_written.set()
        session._teardown()
        task_config.set_backend(None)

    assert isolated_task_backend.is_dir()


def test_lane_subscription_pushes_when_external_inbox_write_changes_pending_count(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(livebuswatch, "LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S", 0.05)
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".spice").mkdir()
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    target = _Target(id="lane", repo_root=repo)
    connection = _Connection()
    watcher_ready = Event()
    change_written = Event()
    monkeypatch.setattr(
        livebus,
        "wait_for_change",
        _single_change_wait(inbox_dir(repo), watcher_ready, change_written),
    )
    message_payload_calls = 0

    def messages_payload(_target, **_kwargs):
        nonlocal message_payload_calls
        message_payload_calls += 1
        if message_payload_calls > 1:
            raise AssertionError("inbox-only change must not read messages payload")
        pending_identity = pending_inbox_identity_payload(target.repo_root)
        return valid_lane_payload(
            messages=[],
            chrome=lane.lane_chrome_payload(
                target_id=target.id,
                pending_identity=pending_identity,
            ),
        )

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
            _wait_for_reply(connection)["lanes"][0]["payload"]["chrome"][
                "pendingInbox"
            ]["value"]["count"]
            == 0
        )
        assert watcher_ready.wait(timeout=1.0)

        _write_inbox_item_from_subprocess(repo)
        change_written.set()

        pushed = _wait_for_watch_push(connection)
        assert pushed["type"] == "lane.pending"
        assert set(pushed["payload"]) == {"chrome"}
        chrome = pushed["payload"]["chrome"]
        assert set(chrome) == {"targetId", "pendingInbox"}
        assert chrome["targetId"] == target.id
        assert chrome["pendingInbox"]["authority"] == "inbox"
        assert chrome["pendingInbox"]["value"]["count"] == 1
        assert chrome["pendingInbox"]["value"]["keys"] == ["1jN54zJK"]
        assert chrome["pendingInbox"]["order"]["revision"] > 0
        assert message_payload_calls == 1
        assert transcript.read_text(encoding="utf-8") == ""
    finally:
        change_written.set()
        session._teardown()


def _agent_status(*, running: bool, pid: int) -> SimpleNamespace:
    return SimpleNamespace(
        running=running,
        thread_id=THREAD_ID,
        process_status="idle",
        pid=pid,
        process_group_id=pid,
        model="gpt-test",
        reasoning_effort="low",
        started_at="",
        log_path=None,
        prompt_skill_path=None,
    )


def _patch_agent_status(monkeypatch, status: SimpleNamespace) -> None:
    for module in (agentapi, identity, lane, message, inventory):
        monkeypatch.setattr(module, "agent_status", lambda *_args, **_kwargs: status)


def _pending_signature(repo: Path, transcript: Path):
    def signature(_target, _thread_id, _transcript_path):
        pending_names = tuple(
            sorted(path.name for path in inbox_dir(repo).glob("*.txt"))
        )
        return LaneSignature(
            transcript=transcript.stat().st_size,
            inbox=pending_names,
            other=(),
        )

    return signature


def _live_message_session(
    connection: _Connection,
    state: ServeState,
    target: WorktreeTarget,
    transcript: Path,
    lane_signature,
) -> LiveBusSession:
    return LiveBusSession(
        connection,
        LiveBusCallbacks(
            resolve_target=lambda selector: target if selector == target.id else None,
            **valid_live_bus_callback_payloads(
                messages_payload=lambda bus_target, **kwargs: (
                    message.messages_payload_for_worktree(state, bus_target, **kwargs)
                )
            ),
            thread_id=lambda _target: THREAD_ID,
            transcript_resolution=lambda _thread_id: _transcript_resolution(
                THREAD_ID, transcript
            ),
            lane_watch_paths=lambda bus_target, thread_id, transcript_path: (
                app.lane_watch_paths_for_target(
                    state, bus_target, thread_id, transcript_path
                )
            ),
            lane_signature=lane_signature,
        ),
    )


def test_lane_subscription_pushes_pending_frame_for_stopped_agent_inbox_write(
    tmp_path, monkeypatch
):
    # Initial lane payloads include task filter/review metadata. Give this
    # integration its own backend so xdist workers never serialize here on the
    # operator board's bootstrap lock while the subscription deadline runs.
    isolated_task_backend = tmp_path / "task-backend"
    task_config.set_backend(str(isolated_task_backend))
    monkeypatch.setattr(livebuswatch, "LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S", 0.05)
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
    _patch_agent_status(monkeypatch, _agent_status(running=False, pid=0))
    ensure_calls: list[dict[str, object]] = []

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        return {"ok": True, "threadId": THREAD_ID}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)
    connection = _Connection()
    watcher_ready = Event()
    change_written = Event()
    monkeypatch.setattr(
        livebus,
        "wait_for_change",
        _single_change_wait(inbox_dir(repo), watcher_ready, change_written),
    )
    session = _live_message_session(
        connection,
        state,
        target,
        transcript,
        _pending_signature(repo, transcript),
    )

    try:
        _subscribe_lane(session, target.id, limit=5)
        assert (
            _wait_for_reply(connection)["lanes"][0]["payload"]["chrome"][
                "pendingInbox"
            ]["value"]["count"]
            == 0
        )
        assert watcher_ready.wait(timeout=1.0)

        _write_inbox_item_from_subprocess(repo)
        change_written.set()

        pushed = _wait_for_watch_push(connection)
        assert pushed["type"] == "lane.pending"
        assert set(pushed["payload"]) == {"chrome"}
        assert pushed["payload"]["chrome"]["pendingInbox"]["value"]["count"] == 1
        assert pushed["payload"]["chrome"]["pendingInbox"]["value"]["keys"] == [
            "1jN54zJK"
        ]
        assert "agentEnsure" not in pushed["payload"]
        assert ensure_calls == []
    finally:
        change_written.set()
        session._teardown()
        task_config.set_backend(None)

    assert isolated_task_backend.is_dir()


def _assert_send_ack_and_timing(connection: _Connection) -> None:
    with connection.lock:
        assert len(connection.sent) == 2
        send_result, send_timing = connection.sent
    assert send_result["type"] == "lane.sendResult"
    assert send_result["requestId"] == "send-1"
    assert send_result["result"]["ok"] is True
    assert send_result["result"]["key"] == "inbox-key"
    assert set(send_result["result"]["serverTiming"]) == {
        "mutationQueueMs",
        "targetResolveMs",
        "sendPayloadMs",
        "totalBeforeReplyMs",
        "replyLockWaitMs",
    }
    assert all(value >= 0.0 for value in send_result["result"]["serverTiming"].values())
    assert send_timing["type"] == "lane.sendTiming"
    assert send_timing["requestId"] == "send-1"
    assert set(send_timing["serverTiming"]) == {
        "mutationQueueMs",
        "targetResolveMs",
        "sendPayloadMs",
        "totalBeforeReplyMs",
        "replyLockWaitMs",
        "replyLockHoldMs",
        "replyWriteMs",
        "totalMs",
    }
    assert all(value >= 0.0 for value in send_timing["serverTiming"].values())


def _wait_for_send_followup(connection: _Connection) -> dict[str, Any]:
    def first_followup() -> dict[str, Any] | None:
        for payload in connection.sent:
            if payload.get("source") == "send":
                return payload
        return None

    with connection.arrival:
        followup = connection.arrival.wait_for(
            first_followup,
            timeout=BLOCKED_ENSURE_ENTRY_SECONDS,
        )
    if followup is not None:
        return followup
    pytest.fail(f"timed out waiting for send followup; sent={connection.sent!r}")


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
        return valid_lane_payload(
            messages=[],
            chrome=_pending_lane_chrome(),
        )

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
            lane_metrics_payload=lambda _target: {},
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
        _assert_send_ack_and_timing(connection)
        followup_continue.set()
        assert _wait_for_send_followup(connection) == {
            "type": "lane.payload",
            "targetId": "lane",
            "source": "send",
            "payload": valid_lane_payload(
                messages=[],
                chrome=_pending_lane_chrome(),
            ),
        }
        assert calls == [
            ("send", {"text": "hello"}),
            ("followup", {"text": "hello"}),
        ]
    finally:
        followup_continue.set()
        session._teardown()


def test_lane_send_acks_before_its_blocked_lifecycle_decision_and_follows_up(
    tmp_path, monkeypatch
):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    target = WorktreeTarget(id="lane", repo_root=repo, name="repo", branch="main")
    state = ServeState(
        anchor_root=tmp_path,
        team_store=ServeTeamStore(path=tmp_path / "teams.sqlite3"),
    )
    state.cached_targets = [target]
    created = state.team_store.create_team(members=[f"target:{target.id}"])
    # Active-mode Serve owns the reconciler every lane decision is submitted to.
    start_lifecycle_reconciler(state)
    _patch_agent_status(monkeypatch, _agent_status(running=False, pid=0))
    ensure_calls: list[dict[str, object]] = []
    ensure_threads: list[str] = []
    ensure_entered = Event()
    release_ensure = Event()

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        ensure_threads.append(current_thread().name)
        ensure_entered.set()
        release_ensure.wait(timeout=BLOCKED_ENSURE_RELEASE_SECONDS)
        return {"ok": True, "threadId": THREAD_ID}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)
    connection = _Connection()
    session = LiveBusSession(
        connection,
        LiveBusCallbacks(
            resolve_target=lambda selector: target if selector == target.id else None,
            **valid_live_bus_callback_payloads(
                send_payload=lambda bus_target, payload: (
                    work_tree_send_accepted_response_payload(state, bus_target, payload)
                )
            ),
            thread_id=lambda _target: THREAD_ID,
            transcript_resolution=lambda _thread_id: None,
            lane_watch_paths=lambda *_args: (),
            lane_signature=lambda *_args: (),
            send_followup_payload=lambda bus_target, _payload: (
                message.send_followup_messages_payload(state, bus_target, limit=5)
            ),
        ),
    )

    try:
        session._handle_lane_send(
            {
                "type": "lane.send",
                "requestId": "send-1",
                "targetId": "lane",
                "payload": {"text": "wake this lane"},
            }
        )

        ack = _wait_for_reply(connection, request_id="send-1")
        # The lane start this send scheduled is still parked inside the ensure,
        # so the acknowledgement is proof the route never waits for it.
        assert ensure_entered.wait(timeout=BLOCKED_ENSURE_ENTRY_SECONDS) is True
        assert ack["type"] == "lane.sendResult"
        assert ack["result"]["ok"] is True
        assert ack["result"]["agentEnsure"] == {}
        assert ack["result"]["chrome"]["pendingInbox"]["value"]["count"] == 1
        release_ensure.set()

        followup = _wait_for_send_followup(connection)
        # One decision for the send, and a follow-up that reports it: the render
        # reads the outcome instead of starting the lane a second time. The
        # thread names the owner -- this lane start came from the reconciler the
        # send submitted to, not from whoever rendered the lane next.
        assert ensure_calls == [
            {
                "target": target,
                "fast_mode": False,
                "force_new": False,
                "automatic": True,
            }
        ]
        assert ensure_threads == [f"{LIFECYCLE_RECONCILER_THREAD_PREFIX}-{target.id}"]
        assert followup["type"] == "lane.payload"
        assert followup["targetId"] == target.id
        assert followup["payload"]["agentEnsure"]["threadId"] == THREAD_ID
        assert followup["payload"]["targetIdentity"]["thread"] == {
            "state": "bound",
            "threadId": THREAD_ID,
        }
        actor = f"thread:{THREAD_ID}"
        assert [
            member.agent_id
            for member in state.team_store.team_state(created.team_id).members
        ] == [actor]
        recorded = state.team_store.agent_identity_for_actor(actor)
        assert recorded is not None
        assert recorded.thread_id == THREAD_ID

        def reject(*_args, **_kwargs):
            raise AssertionError("send follow-up projection attempted a durable write")

        monkeypatch.setattr(state.team_store, "assign_agent", reject)
        monkeypatch.setattr(state.team_store, "record_agent_identity", reject)
        monkeypatch.setattr(state.team_store, "record_pending_renewal", reject)
        monkeypatch.setattr(state.team_store, "record_started_renewal", reject)
        repeated = message.send_followup_messages_payload(state, target, limit=5)
        assert repeated["agentEnsure"] == followup["payload"]["agentEnsure"]
        assert len(ensure_calls) == 1
    finally:
        release_ensure.set()
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

    monkeypatch.setattr(livebus, "wait_for_change", fake_wait)
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
        return valid_lane_payload(messages=[], statusLine={})

    monkeypatch.setattr(livebus, "wait_for_change", fake_wait)
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

    monkeypatch.setattr(livebuswatch, "_select_attr", select_attr)
    watch = livebuswatch._KqueueWatch()
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
            (0, 1, livebuswatch.LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S, True),
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

    monkeypatch.setattr(livebuswatch, "_select_attr", select_attr)
    monkeypatch.setattr(livebuswatch, "_KQUEUE_VNODE_FFLAGS", 15)
    monkeypatch.setattr(livebuswatch, "_KQUEUE_INVALIDATING_FFLAGS", 12)
    watch = livebuswatch._KqueueWatch()
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
        livebuswatch,
        "import_module",
        lambda name: SimpleNamespace(watch=watch) if name == "watchfiles" else None,
    )

    assert (
        livebuswatch._wait_for_change_watchfiles(
            (watched,), Event(), activated=activated
        )
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
    monkeypatch.setattr(livebuswatch, "LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S", 0.05)
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
    _patch_agent_status(monkeypatch, _agent_status(running=True, pid=123))
    connection = _Connection()
    watcher_ready = Event()
    change_written = Event()
    reply_log = reply_log_path(repo, THREAD_ID)
    monkeypatch.setattr(
        livebus,
        "wait_for_change",
        _single_change_wait(reply_log, watcher_ready, change_written),
    )
    task_config.set_backend(str(tmp_path / "task-backend"))
    session = _live_message_session(
        connection,
        state,
        target,
        transcript,
        lambda bus_target, thread_id, transcript_path: app.lane_signature_for_target(
            state, bus_target, thread_id, transcript_path
        ),
    )

    try:
        _subscribe_lane(session, target.id, limit=5)
        assert watcher_ready.wait(timeout=1.0)

        append_reply_record(
            repo,
            THREAD_ID,
            timestamp="2026-01-01T00:00:01.000000Z",
            text="ACK 1jN54zJK: applied",
            ack_keys=["1jN54zJK"],
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
