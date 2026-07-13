"""The live bus: one WebSocket per browser, request/response plus push.

Requests carry a `requestId` the response echoes; pushes carry none. Verbs:
`bus.ping`, `targets.refresh`, `teams.refresh`, `teams.command`,
`lanes.subscribe`, `lane.configure`, `lane.unsubscribe`,
`lane.refresh`, `lane.history`, `lane.send`, `lane.taskDrain`,
`metrics.series`. A subscription tails the
agent's transcript and pushes `lane.payload` frames the moment new lines
land — kqueue watches the open file descriptor on macOS (FSEvents misses
appends through a held-open handle), watchfiles covers Linux/Windows.
"""

from __future__ import annotations

import json
import os
import select
import time
import uuid
from collections import deque
from concurrent.futures import Future, ThreadPoolExecutor, wait
from dataclasses import dataclass, field
from importlib import import_module
from pathlib import Path
from queue import Queue
from threading import Event, Lock, Thread, Timer
from typing import Any, Callable, cast

from spice.serve.messages import TranscriptResolution
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.submissions import SubmissionLifecycleTracker
from spice.serve.websocket import (
    WebSocketConnection,
    WebSocketDisconnect,
    WebSocketProtocolError,
    accept_websocket,
)

DEFAULT_BUS_MESSAGE_LIMIT = 50
INITIAL_BUS_MESSAGE_LIMIT = 25
# Batch payload fan-out width per session. Lane payload reads are already safe
# to overlap (per-client rollout cursors, locked team store); the bound keeps
# one browser from monopolizing transcript parsing when it opens many lanes.
LIVE_BUS_PAYLOAD_WORKERS = 8
PENDING_LANE_PAYLOAD_KEYS = (
    "pendingInboxCount",
    "pendingInboxKeys",
    "pendingInboxRevision",
    "pendingInboxVersion",
)


def _select_has_attrs(*names: str) -> bool:
    return all(hasattr(select, name) for name in names)


def _select_attr(name: str) -> Any:
    return getattr(select, name)


_HAVE_KQUEUE = _select_has_attrs(
    "kqueue",
    "kevent",
    "KQ_FILTER_VNODE",
    "KQ_EV_ADD",
    "KQ_EV_CLEAR",
    "KQ_NOTE_WRITE",
    "KQ_NOTE_EXTEND",
    "KQ_NOTE_DELETE",
    "KQ_NOTE_RENAME",
)
_KQUEUE_VNODE_FFLAGS: Any = 0
if _HAVE_KQUEUE:
    _KQUEUE_VNODE_FFLAGS = (
        _select_attr("KQ_NOTE_WRITE")
        | _select_attr("KQ_NOTE_EXTEND")
        | _select_attr("KQ_NOTE_DELETE")
        | _select_attr("KQ_NOTE_RENAME")
    )
# kqueue blocks until a vnode event arrives; this bounds how long a cancelled
# watcher waits before noticing its stop flag. It is a wakeup interval, not a
# filesystem poll.
LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S = 1.0
LIVE_BUS_WATCHER_JOIN_TIMEOUT_S = LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S + 0.5
LIVE_BUS_WATCHER_ACTIVATION_TIMEOUT_S = 5.0
LIVE_BUS_INITIAL_PAYLOAD_TIMEOUT_S = 15.0

# A connected client sends `bus.ping` heartbeats well inside this window; a
# whole interval with no frame means the peer is gone and the blocking read
# is unblocked so the session and its watchers are reaped.
LIVE_BUS_READ_TIMEOUT_S = 45.0
BACKGROUND_LANE_COALESCE_SECONDS = 0.25
_MS_PER_SECOND = 1000


@dataclass(frozen=True)
class LiveBusCallbacks:
    resolve_target: Callable[[str | None], Any | None]
    work_trees_payload: Callable[[], dict[str, Any]]
    messages_payload: Callable[..., dict[str, Any]]
    send_payload: Callable[[Any, dict[str, Any]], tuple[dict[str, Any], Any]]
    task_drain_payload: Callable[[Any, dict[str, Any]], tuple[dict[str, Any], Any]]
    team_snapshot_payload: Callable[[int | None], dict[str, Any]]
    team_command_payload: Callable[[dict[str, Any]], tuple[dict[str, Any], Any]]
    metric_series_payload: Callable[[dict[str, Any]], dict[str, Any]]
    thread_id: Callable[[Any], str | None]
    transcript_resolution: Callable[[str], TranscriptResolution | None]
    lane_watch_paths: Callable[
        [Any, str | None, TranscriptResolution | None], tuple[Path, ...]
    ]
    lane_signature: Callable[[Any, str | None, TranscriptResolution | None], Any]
    send_followup_payload: Callable[[Any, dict[str, Any]], dict[str, Any]] | None = None
    drop_client_cursors: Callable[[str], None] | None = None


@dataclass(frozen=True)
class LaneSignature:
    transcript: Any
    inbox: Any
    other: Any


@dataclass(frozen=True)
class FrameSendTiming:
    lock_wait_ms: float
    lock_hold_ms: float
    write_ms: float
    finished_at: float


@dataclass
class _FrameTelemetry:
    count: int = 0
    bytes: int = 0
    lock_wait_total_ms: float = 0.0
    lock_wait_last_ms: float = 0.0
    lock_wait_max_ms: float = 0.0
    lock_hold_total_ms: float = 0.0
    lock_hold_last_ms: float = 0.0
    lock_hold_max_ms: float = 0.0

    def record(self, byte_count: int, timing: FrameSendTiming) -> None:
        self.count += 1
        self.bytes += byte_count
        self.lock_wait_total_ms += timing.lock_wait_ms
        self.lock_wait_last_ms = timing.lock_wait_ms
        self.lock_wait_max_ms = max(self.lock_wait_max_ms, timing.lock_wait_ms)
        self.lock_hold_total_ms += timing.lock_hold_ms
        self.lock_hold_last_ms = timing.lock_hold_ms
        self.lock_hold_max_ms = max(self.lock_hold_max_ms, timing.lock_hold_ms)

    def payload(self) -> dict[str, int | float]:
        return {
            "count": self.count,
            "bytes": self.bytes,
            "sendLockWaitMsTotal": self.lock_wait_total_ms,
            "sendLockWaitMsLast": self.lock_wait_last_ms,
            "sendLockWaitMsMax": self.lock_wait_max_ms,
            "sendLockHoldMsTotal": self.lock_hold_total_ms,
            "sendLockHoldMsLast": self.lock_hold_last_ms,
            "sendLockHoldMsMax": self.lock_hold_max_ms,
        }


@dataclass
class _LaneSubscription:
    target: Any
    query: dict[str, Any]
    generation: str
    stop: Event = field(default_factory=Event)
    watcher_activated: Event = field(default_factory=Event)
    initial_payload_sent: Event = field(default_factory=Event)
    thread: Thread | None = None
    lock: Lock = field(default_factory=Lock)
    last_signature: Any = None
    watcher_error: str | None = None
    background_dirty: bool = False


class LiveBusSession:
    def __init__(
        self, connection: WebSocketConnection, callbacks: LiveBusCallbacks
    ) -> None:
        self.connection = connection
        self.callbacks = callbacks
        # Per-connection identity so each client owns its rollout cursor; a
        # shared per-thread cursor let one tab/machine starve another's stream.
        self.client_id = uuid.uuid4().hex
        self._subscription_sequence = 0
        self.subscriptions: dict[str, _LaneSubscription] = {}
        self.send_lock = Lock()
        self._telemetry_lock = Lock()
        self._frame_telemetry: dict[str, _FrameTelemetry] = {}
        self._background_dirty_lock = Lock()
        self._background_dirty_lanes: dict[str, str] = {}
        self._background_dirty_timer: Timer | None = None
        self._closed = False
        # Metrics are read-only display data whose queries can be heavy; running
        # them inline would block interactive frames (lane.send, acks) on this
        # one socket. A dedicated worker drains them so the dispatch loop stays
        # responsive — replies still carry the requestId the client matches on.
        self._metrics_queue: Queue[dict[str, Any] | None] = Queue()
        self._metrics_worker: Thread | None = None
        self._send_followup_queue: Queue[tuple[Any, dict[str, Any]] | None] = Queue()
        self._send_followup_worker: Thread | None = None
        # A subscribe's blocking completion -- waiting out watcher activation and
        # reading the initial batch payload -- drains here off the dispatch
        # thread. Only the cheap bookkeeping (replacement, baseline signature,
        # watcher arm) stays inline, so a lane.send arriving right behind a
        # subscribe is dispatched and acked immediately instead of waiting out
        # the whole batch read on the single per-connection dispatch thread.
        self._subscribe_queue: Queue[
            tuple[dict[str, Any], list[_LaneSubscription]] | None
        ] = Queue()
        self._subscribe_worker: Thread | None = None
        # Batched subscribes fan their payload computes out here so N lanes
        # arrive together in one reply frame instead of trickling in serially.
        self._payload_pool: ThreadPoolExecutor | None = None
        # Single read-only verbs (refresh/history) route their
        # heavy messages_payload compute + reply onto that pool, one FIFO chain
        # per target: two queued reads for a lane apply in request order while
        # distinct lanes overlap up to the pool's width. Only one chain runs
        # per target at a time (guarded by _read_active), so the queue never
        # needs cross-thread reordering.
        self._read_lock = Lock()
        self._read_queues: dict[str, deque[Callable[[], None]]] = {}
        self._read_active: set[str] = set()
        self._read_futures: set[Future[None]] = set()
        self._submission_tracker = SubmissionLifecycleTracker()

    def run(self) -> None:
        self.connection.set_read_timeout(LIVE_BUS_READ_TIMEOUT_S)
        try:
            while True:
                try:
                    message = self.connection.read_json()
                except WebSocketProtocolError:
                    self._send({"type": "bus.error", "error": "protocol error"})
                    continue
                self._dispatch(message)
        except WebSocketDisconnect:
            return
        finally:
            self._teardown()

    def _teardown(self) -> None:
        with self._background_dirty_lock:
            self._closed = True
            timer = self._background_dirty_timer
            self._background_dirty_timer = None
            self._background_dirty_lanes.clear()
        if timer is not None:
            timer.cancel()
        for subscription in list(self.subscriptions.values()):
            self._stop_subscription(subscription)
        self.subscriptions.clear()
        if self.callbacks.drop_client_cursors is not None:
            self.callbacks.drop_client_cursors(self.client_id)
        if self._metrics_worker is not None:
            self._metrics_queue.put(None)
            self._metrics_worker.join(timeout=LIVE_BUS_WATCHER_JOIN_TIMEOUT_S)
            self._metrics_worker = None
        if self._send_followup_worker is not None:
            self._send_followup_queue.put(None)
            self._send_followup_worker.join(timeout=LIVE_BUS_WATCHER_JOIN_TIMEOUT_S)
            self._send_followup_worker = None
        if self._subscribe_worker is not None:
            # Stopping the subscriptions above set every watcher_activated, so a
            # parked completion unblocks; drain it before the pool it computes on
            # is torn down. The reply lands (or the peer is gone and the send is
            # swallowed) and the initial-payload gate releases within the join.
            self._subscribe_queue.put(None)
            self._subscribe_worker.join(timeout=LIVE_BUS_WATCHER_JOIN_TIMEOUT_S)
            self._subscribe_worker = None
        if self._payload_pool is not None:
            # The subscribe worker drained just above, so only detached single
            # read chains remain — one may be mid-compute at close. Drop the
            # queued-but-unstarted jobs, then wait out the running chains within
            # the same bounded budget the watcher joins use, so a slow transcript
            # parse cannot hang teardown. cancel_futures reaps anything still
            # queued.
            with self._read_lock:
                self._read_queues.clear()
            self._await_pending_reads(LIVE_BUS_WATCHER_JOIN_TIMEOUT_S)
            self._payload_pool.shutdown(wait=False, cancel_futures=True)
            self._payload_pool = None

    def diagnostics(self) -> dict[str, Any]:
        with self._telemetry_lock:
            frames = {
                kind: telemetry.payload()
                for kind, telemetry in sorted(self._frame_telemetry.items())
            }
        return {
            "clientId": self.client_id,
            "frames": frames,
            "totals": {
                "count": sum(int(frame["count"]) for frame in frames.values()),
                "bytes": sum(int(frame["bytes"]) for frame in frames.values()),
            },
        }

    def _send(
        self,
        payload: dict[str, Any],
        *,
        before_send: Callable[[float], None] | None = None,
    ) -> FrameSendTiming:
        # Encode the frame to bytes before taking send_lock so the lock's
        # critical section -- and the lock-hold/write timing below -- covers only
        # the socket write. A watcher thread encoding a bulk lane payload no
        # longer holds the lock through that encode, so a small lane.sendResult
        # ack acquires it and writes as soon as any in-flight write returns
        # rather than queuing behind the encode. byte_count is the JSON payload
        # length (matching the wire text), also computed outside the lock.
        frame = self.connection.encode_text_frame(payload)
        byte_count = len(json.dumps(payload, separators=(",", ":")).encode("utf-8"))
        wait_started_at = time.perf_counter()
        self.send_lock.acquire()
        acquired_at = time.perf_counter()
        lock_wait_ms = _elapsed_ms(wait_started_at, acquired_at)
        try:
            if before_send is not None:
                before_send(lock_wait_ms)
            write_started_at = time.perf_counter()
            self.connection.send_frame(frame)
            finished_at = time.perf_counter()
            timing = FrameSendTiming(
                lock_wait_ms=lock_wait_ms,
                lock_hold_ms=_elapsed_ms(acquired_at, finished_at),
                write_ms=_elapsed_ms(write_started_at, finished_at),
                finished_at=finished_at,
            )
            kind = str(payload.get("type") or "unknown")
            with self._telemetry_lock:
                self._frame_telemetry.setdefault(kind, _FrameTelemetry()).record(
                    byte_count, timing
                )
        finally:
            self.send_lock.release()
        return timing

    def _reply(
        self,
        message: dict[str, Any],
        payload: dict[str, Any],
        *,
        before_send: Callable[[float], None] | None = None,
    ) -> FrameSendTiming:
        request_id = message.get("requestId")
        if isinstance(request_id, str) and request_id:
            payload = {**payload, "requestId": request_id}
        return self._send(payload, before_send=before_send)

    def _dispatch(self, message: dict[str, Any]) -> None:
        kind = str(message.get("type") or "")
        try:
            handler = {
                "bus.ping": self._handle_ping,
                "targets.refresh": self._handle_targets_refresh,
                "teams.refresh": self._handle_teams_refresh,
                "teams.command": self._handle_teams_command,
                "lanes.subscribe": self._handle_lanes_subscribe,
                "lane.configure": self._handle_lane_configure,
                "lane.unsubscribe": self._handle_lane_unsubscribe,
                "lane.refresh": self._handle_lane_refresh,
                "lane.history": self._handle_lane_history,
                "lane.send": self._handle_lane_send,
                "lane.taskDrain": self._handle_lane_task_drain,
                "metrics.series": self._handle_metrics_series,
            }.get(kind)
            if handler is None:
                self._reply(
                    message,
                    {"type": "bus.error", "error": f"unknown message type {kind!r}"},
                )
                return
            handler(message)
        except WebSocketDisconnect:
            raise
        except Exception as exc:  # surface, never kill the session silently
            self._reply(message, {"type": "bus.error", "error": str(exc)})

    # ---- handlers ------------------------------------------------------

    def _handle_ping(self, message: dict[str, Any]) -> None:
        self._reply(
            message,
            {"type": "bus.pong", "diagnostics": self.diagnostics()},
        )

    def _handle_targets_refresh(self, message: dict[str, Any]) -> None:
        self._reply(
            message,
            {"type": "targets.payload", "payload": self.callbacks.work_trees_payload()},
        )

    def _handle_teams_refresh(self, message: dict[str, Any]) -> None:
        query = message.get("query") or {}
        since = query.get("sinceRevision")
        since_revision = since if isinstance(since, int) else None
        self._reply(
            message,
            {
                "type": "teams.payload",
                "payload": self.callbacks.team_snapshot_payload(since_revision),
            },
        )

    def _handle_teams_command(self, message: dict[str, Any]) -> None:
        result, _status = self.callbacks.team_command_payload(
            message.get("payload") or {}
        )
        self._reply(message, {"type": "teams.commandResult", "result": result})

    def _require_target(self, message: dict[str, Any]) -> Any | None:
        target = self.callbacks.resolve_target(str(message.get("targetId") or ""))
        if target is None:
            self._reply(message, {"type": "bus.error", "error": "work tree not found"})
        return target

    def _query_kwargs(self, message: dict[str, Any]) -> dict[str, Any]:
        return self._query_kwargs_from(message.get("query") or {})

    def _query_kwargs_from(self, query: dict[str, Any]) -> dict[str, Any]:
        kwargs: dict[str, Any] = {
            "limit": _bounded_int(query.get("limit"), DEFAULT_BUS_MESSAGE_LIMIT),
            "client_id": self.client_id,
        }
        for source_key, kwarg in (
            ("after", "after"),
            ("before", "before"),
            ("threadId", "expected_thread_id"),
        ):
            value = str(query.get(source_key) or "")
            if value:
                kwargs[kwarg] = value
        return kwargs

    def _handle_lanes_subscribe(self, message: dict[str, Any]) -> None:
        raw_entries = message.get("entries")
        if not isinstance(raw_entries, list) or not raw_entries:
            self._reply(
                message,
                {"type": "bus.error", "error": "lanes.subscribe requires entries"},
            )
            return
        # Validate the whole batch before touching subscription state so a bad
        # entry never leaves siblings half-replaced.
        entries: list[tuple[Any, dict[str, Any]]] = []
        seen_target_ids: set[str] = set()
        for raw_entry in raw_entries:
            entry = raw_entry if isinstance(raw_entry, dict) else {}
            target = self.callbacks.resolve_target(str(entry.get("targetId") or ""))
            if target is None:
                self._reply(
                    message, {"type": "bus.error", "error": "work tree not found"}
                )
                return
            if target.id in seen_target_ids:
                self._reply(
                    message,
                    {
                        "type": "bus.error",
                        "error": f"duplicate targetId {target.id!r} in lanes.subscribe",
                    },
                )
                return
            seen_target_ids.add(target.id)
            entries.append((target, dict(entry.get("query") or {})))
        # Bookkeeping stays inline on the dispatch thread: replacement and the
        # baseline signature precede watcher activation. Every watcher arms
        # before its initial payload read; watch pushes then wait behind the
        # reply gate, so a setup-racing edit is delivered by either that read or
        # the queued append-only push without overtaking the initial frame.
        subscriptions = [
            self._replace_subscription(target, query) for target, query in entries
        ]
        for subscription in subscriptions:
            self._start_watcher(subscription)
        self._enqueue_subscribe_completion(message, subscriptions)

    def _enqueue_subscribe_completion(
        self, message: dict[str, Any], subscriptions: list[_LaneSubscription]
    ) -> None:
        if self._subscribe_worker is None:
            self._subscribe_worker = Thread(
                target=self._subscribe_completion_loop,
                name="spice-live-bus-subscribe",
                daemon=True,
            )
            self._subscribe_worker.start()
        self._subscribe_queue.put((message, subscriptions))

    def _subscribe_completion_loop(self) -> None:
        while True:
            item = self._subscribe_queue.get()
            if item is None:
                return
            message, subscriptions = item
            self._complete_lanes_subscribe(message, subscriptions)

    def _complete_lanes_subscribe(
        self, message: dict[str, Any], subscriptions: list[_LaneSubscription]
    ) -> None:
        """Wait out watcher activation, read the batch payload, reply, release.

        Runs off the dispatch thread so a lane.send behind this subscribe is not
        head-of-line-blocked. The watcher armed inline before this ran, so the
        baseline signature and the initial read straddle registration and a
        setup-racing edit is delivered by exactly one of them. Payload computes
        still fan out across the pool so a batch overlaps its lanes; this waiter
        is a dedicated thread, not a pool worker, so those nested submits cannot
        starve it. initial_payload_sent releases the watcher only after the reply
        so a watch push can never overtake the initial frame.
        """
        try:
            for subscription in subscriptions:
                if not subscription.watcher_activated.wait(
                    timeout=LIVE_BUS_WATCHER_ACTIVATION_TIMEOUT_S
                ):
                    subscription.stop.set()
                    raise TimeoutError(
                        "lane watcher activation deadline exceeded "
                        f"target={subscription.target.id} "
                        f"budget={LIVE_BUS_WATCHER_ACTIVATION_TIMEOUT_S:g}s"
                    )
            futures: list[tuple[_LaneSubscription, Future[dict[str, Any]]]] = [
                (
                    subscription,
                    self._payload_executor().submit(
                        self._subscription_payload, subscription
                    ),
                )
                for subscription in subscriptions
            ]
            lanes = [
                {
                    "targetId": subscription.target.id,
                    "payload": self._initial_payload_result(subscription, future),
                    "subscriptionGeneration": subscription.generation,
                    "watcherActive": subscription.watcher_error is None,
                    "watcherError": subscription.watcher_error or "",
                }
                for subscription, future in futures
            ]
            frame: dict[str, Any] = {"type": "lanes.payload", "lanes": lanes}
        except Exception as exc:  # mirror _dispatch: surface, never wedge the loop
            frame = {"type": "bus.error", "error": str(exc)}
        try:
            self._reply(message, frame)
        except (OSError, WebSocketProtocolError, WebSocketDisconnect):
            pass  # peer vanished mid-subscribe; the gate below still releases
        finally:
            for subscription in subscriptions:
                subscription.initial_payload_sent.set()

    def _initial_payload_result(
        self,
        subscription: _LaneSubscription,
        future: Future[dict[str, Any]],
    ) -> dict[str, Any]:
        try:
            return future.result(timeout=LIVE_BUS_INITIAL_PAYLOAD_TIMEOUT_S)
        except TimeoutError as exc:
            raise TimeoutError(
                "lane initial payload deadline exceeded "
                f"target={subscription.target.id} "
                f"budget={LIVE_BUS_INITIAL_PAYLOAD_TIMEOUT_S:g}s"
            ) from exc

    def _replace_subscription(
        self, target: Any, query: dict[str, Any]
    ) -> _LaneSubscription:
        previous = self.subscriptions.pop(target.id, None)
        if previous is not None:
            self._stop_subscription(previous)
        self._subscription_sequence += 1
        subscription = _LaneSubscription(
            target=target,
            query=query,
            generation=f"{self.client_id}:{self._subscription_sequence}",
        )
        self.subscriptions[target.id] = subscription
        subscription.last_signature = self._lane_signature(subscription)
        return subscription

    def _subscription_payload(self, subscription: _LaneSubscription) -> dict[str, Any]:
        try:
            return self.callbacks.messages_payload(
                subscription.target, **self._query_kwargs_from(subscription.query)
            )
        except Exception as exc:  # a failed lane stays inside its batch slot
            return {"error": str(exc), "messages": [], "statusLine": {}}

    def _payload_executor(self) -> ThreadPoolExecutor:
        if self._payload_pool is None:
            self._payload_pool = ThreadPoolExecutor(
                max_workers=LIVE_BUS_PAYLOAD_WORKERS,
                thread_name_prefix="spice-live-bus-payload",
            )
        return self._payload_pool

    def _dispatch_read(self, target: Any, message: dict[str, Any]) -> None:
        """Queue a lane payload compute + reply on the target's FIFO read chain.

        The query is parsed inline (cheap, on the dispatch thread) and captured;
        only the heavy messages_payload read runs on the pool so one slow lane
        never blocks bus.ping or a sibling lane on the single dispatch thread.
        """
        kwargs = self._query_kwargs(message)

        def job() -> None:
            try:
                payload = self.callbacks.messages_payload(target, **kwargs)
                frame: dict[str, Any] = {"type": "lane.payload", "payload": payload}
            except Exception as exc:  # mirror _dispatch: surface, never wedge
                frame = {"type": "bus.error", "error": str(exc)}
            try:
                self._reply(message, frame)
            except (OSError, WebSocketProtocolError, WebSocketDisconnect):
                pass  # peer vanished mid-read; the chain still drains cleanly

        self._enqueue_read_job(target.id, job)

    def _enqueue_read_job(self, target_id: str, job: Callable[[], None]) -> None:
        with self._read_lock:
            self._read_queues.setdefault(target_id, deque()).append(job)
            if target_id in self._read_active:
                return  # a chain is already draining this target FIFO
            self._read_active.add(target_id)
            # The chain blocks on _read_lock on entry, so it cannot finish (and
            # fire the done callback inline) while this call still holds the lock.
            future = self._payload_executor().submit(self._drain_read_chain, target_id)
            self._read_futures.add(future)
            future.add_done_callback(self._forget_read_future)

    def _drain_read_chain(self, target_id: str) -> None:
        while True:
            with self._read_lock:
                queue = self._read_queues.get(target_id)
                if not queue:
                    self._read_queues.pop(target_id, None)
                    self._read_active.discard(target_id)
                    return
                job = queue.popleft()
            job()  # self-contained: computes, replies, swallows send errors

    def _forget_read_future(self, future: Future[None]) -> None:
        with self._read_lock:
            self._read_futures.discard(future)

    def _await_pending_reads(self, timeout: float | None = None) -> None:
        """Block until the detached read chains finish or `timeout` elapses.

        Snapshots the live futures under the read lock — each chain's done
        callback removes itself from the set, so waiting on a copy stays
        stable while they retire. An empty snapshot means every queued read
        already replied. With the default ``timeout=None`` the wait is
        unbounded and returns exactly when the futures complete, so callers
        that need the reply in hand block on the completion itself rather than
        a fixed deadline; ``_teardown`` still passes a bounded timeout so it
        cannot hang on a wedged pool thread.
        """
        with self._read_lock:
            pending = list(self._read_futures)
        if pending:
            wait(pending, timeout=timeout)

    def _handle_lane_configure(self, message: dict[str, Any]) -> None:
        target = self._require_target(message)
        if target is None:
            return
        subscription = self.subscriptions.get(target.id)
        if subscription is not None:
            with subscription.lock:
                subscription.query = dict(message.get("query") or {})
        self._reply(message, {"type": "lane.configured"})

    def coalesce_background_update(self, subscription: _LaneSubscription) -> bool:
        """Defer one background lane change into the aggregate dirty frame."""
        with subscription.lock:
            focused = subscription.query.get("focused") is not False
            if focused:
                subscription.background_dirty = False
                return False
            if subscription.background_dirty:
                return True
            subscription.background_dirty = True
        self._mark_background_lane_dirty(subscription)
        return True

    def _mark_background_lane_dirty(self, subscription: _LaneSubscription) -> None:
        with self._background_dirty_lock:
            if self._closed:
                return
            self._background_dirty_lanes[subscription.target.id] = (
                subscription.generation
            )
            if self._background_dirty_timer is not None:
                return
            timer = Timer(
                BACKGROUND_LANE_COALESCE_SECONDS,
                self._flush_background_lane_dirties,
            )
            timer.daemon = True
            self._background_dirty_timer = timer
            timer.start()

    def _flush_background_lane_dirties(self) -> None:
        with self._background_dirty_lock:
            self._background_dirty_timer = None
            if self._closed or not self._background_dirty_lanes:
                return
            lanes = [
                {"targetId": target_id, "subscriptionGeneration": generation}
                for target_id, generation in sorted(
                    self._background_dirty_lanes.items()
                )
            ]
            self._background_dirty_lanes.clear()
        try:
            self._send({"type": "lanes.dirty", "lanes": lanes})
        except (OSError, WebSocketProtocolError, WebSocketDisconnect):
            return

    def _handle_lane_unsubscribe(self, message: dict[str, Any]) -> None:
        target_id = str(message.get("targetId") or "")
        subscription = self.subscriptions.pop(target_id, None)
        if subscription is not None:
            self._stop_subscription(subscription)
        self._reply(message, {"type": "lane.unsubscribed"})

    def _handle_lane_refresh(self, message: dict[str, Any]) -> None:
        target = self._require_target(message)
        if target is None:
            return
        self._dispatch_read(target, message)

    def _handle_lane_history(self, message: dict[str, Any]) -> None:
        self._handle_lane_refresh(message)

    def _handle_lane_send(self, message: dict[str, Any]) -> None:
        received_at = time.perf_counter()
        target = self._require_target(message)
        if target is None:
            return
        target_resolved_at = time.perf_counter()
        payload = message.get("payload") or {}
        send_payload_started_at = time.perf_counter()
        result, _status = self.callbacks.send_payload(target, payload)
        send_payload_finished_at = time.perf_counter()
        server_timing = _lane_send_server_timing(
            received_at=received_at,
            target_resolved_at=target_resolved_at,
            send_payload_started_at=send_payload_started_at,
            send_payload_finished_at=send_payload_finished_at,
        )
        result = {
            **result,
            "serverTiming": server_timing,
        }
        submission_key = str(result.get("key") or "")
        if result.get("ok") is True and submission_key:
            result["submission"] = self._submission_tracker.accept(
                target_id=target.id,
                key=submission_key,
                evidence=str(result.get("path") or submission_key),
            )
        reply_timing = self._reply(
            message,
            {"type": "lane.sendResult", "result": result},
            before_send=lambda wait_ms: server_timing.update(
                {"replyLockWaitMs": wait_ms}
            ),
        )
        request_id = message.get("requestId")
        if isinstance(request_id, str) and request_id:
            self._send(
                {
                    "type": "lane.sendTiming",
                    "requestId": request_id,
                    "serverTiming": {
                        **server_timing,
                        "replyLockHoldMs": reply_timing.lock_hold_ms,
                        "replyWriteMs": reply_timing.write_ms,
                        "totalMs": _elapsed_ms(received_at, reply_timing.finished_at),
                    },
                }
            )
        if result.get("ok") is True:
            self._queue_lane_send_followup(target, payload)

    def _queue_lane_send_followup(self, target: Any, payload: dict[str, Any]) -> None:
        if self.callbacks.send_followup_payload is None:
            return
        if self._send_followup_worker is None:
            self._send_followup_worker = Thread(
                target=self._send_followup_loop,
                name="spice-live-bus-send-followup",
                daemon=True,
            )
            self._send_followup_worker.start()
        self._send_followup_queue.put((target, dict(payload)))

    def _send_followup_loop(self) -> None:
        send_followup_payload = self.callbacks.send_followup_payload
        if send_followup_payload is None:
            return
        while True:
            item = self._send_followup_queue.get()
            if item is None:
                return
            target, send_payload = item
            try:
                payload = send_followup_payload(target, send_payload)
            except Exception as exc:
                payload = {"error": str(exc), "messages": [], "statusLine": {}}
            try:
                self._send(
                    {
                        "type": "lane.payload",
                        "targetId": target.id,
                        "source": "send",
                        "payload": payload,
                    }
                )
            except (OSError, WebSocketProtocolError, WebSocketDisconnect):
                return

    def _handle_lane_task_drain(self, message: dict[str, Any]) -> None:
        target = self._require_target(message)
        if target is None:
            return
        result, _status = self.callbacks.task_drain_payload(
            target, message.get("payload") or {}
        )
        self._reply(message, {"type": "lane.taskDrainResult", "result": result})

    def _handle_metrics_series(self, message: dict[str, Any]) -> None:
        if self._metrics_worker is None:
            self._metrics_worker = Thread(
                target=self._metrics_loop,
                name="spice-live-bus-metrics",
                daemon=True,
            )
            self._metrics_worker.start()
        self._metrics_queue.put(message)

    def _metrics_loop(self) -> None:
        while True:
            message = self._metrics_queue.get()
            if message is None:
                return
            try:
                result = self.callbacks.metric_series_payload(
                    message.get("query") or {}
                )
                frame = {"type": "metrics.seriesResult", "result": result}
            except Exception as exc:  # surface, never kill the worker silently
                frame = {"type": "bus.error", "error": str(exc)}
            try:
                self._reply(message, frame)
            except (OSError, WebSocketProtocolError, WebSocketDisconnect):
                return

    # ---- watchers ------------------------------------------------------

    def _start_watcher(self, subscription: _LaneSubscription) -> None:
        thread = Thread(
            target=self._watch_subscription,
            args=(subscription,),
            name=f"spice-live-bus-watch-{subscription.target.id}",
            daemon=True,
        )
        subscription.thread = thread
        thread.start()

    def _stop_subscription(self, subscription: _LaneSubscription) -> None:
        subscription.stop.set()
        subscription.initial_payload_sent.set()
        if subscription.thread is not None:
            subscription.thread.join(timeout=LIVE_BUS_WATCHER_JOIN_TIMEOUT_S)

    def _watch_subscription(self, subscription: _LaneSubscription) -> None:
        target = subscription.target
        watch = _KqueueWatch()
        try:
            self._run_watch_loop(subscription, target, watch)
        except Exception as exc:
            subscription.watcher_error = str(exc)
        finally:
            subscription.watcher_activated.set()
            watch.close()

    def _run_watch_loop(
        self, subscription: _LaneSubscription, target: Any, watch: _KqueueWatch
    ) -> None:
        while not subscription.stop.is_set():
            thread_id, transcript = self._lane_context(target)
            watch_paths = self.callbacks.lane_watch_paths(target, thread_id, transcript)
            changed = _wait_for_change(
                watch_paths,
                subscription.stop,
                watch,
                activated=subscription.watcher_activated,
            )
            if subscription.stop.is_set():
                return
            if not changed:
                continue
            if not subscription.initial_payload_sent.wait(
                timeout=LIVE_BUS_INITIAL_PAYLOAD_TIMEOUT_S
            ):
                raise TimeoutError(
                    "lane initial payload deadline exceeded "
                    f"target={target.id} "
                    f"budget={LIVE_BUS_INITIAL_PAYLOAD_TIMEOUT_S:g}s"
                )
            if subscription.stop.is_set():
                return
            signature = self.callbacks.lane_signature(target, thread_id, transcript)
            previous_signature = subscription.last_signature
            if signature == previous_signature:
                continue
            subscription.last_signature = signature
            if self.coalesce_background_update(subscription):
                continue
            if pending_only_signature_change(previous_signature, signature):
                payload = _pending_lane_payload(target)
                try:
                    self._send(
                        {
                            "type": "lane.pending",
                            "targetId": target.id,
                            "source": "watch",
                            "subscriptionGeneration": subscription.generation,
                            "payload": payload,
                        }
                    )
                    self._push_submission_updates(
                        target,
                        payload,
                        subscription_generation=subscription.generation,
                        include_ack_state=True,
                    )
                except (OSError, WebSocketProtocolError):
                    return
                continue
            with subscription.lock:
                query = dict(subscription.query)
            kwargs: dict[str, Any] = {
                "limit": _bounded_int(query.get("limit"), DEFAULT_BUS_MESSAGE_LIMIT),
                "append_only": True,
                "client_id": self.client_id,
            }
            after = str(query.get("after") or "")
            if after:
                kwargs["after"] = after
            try:
                payload = self.callbacks.messages_payload(target, **kwargs)
            except Exception as exc:
                payload = {"error": str(exc), "messages": [], "statusLine": {}}
            try:
                self._send(
                    {
                        "type": "lane.payload",
                        "targetId": target.id,
                        "source": "watch",
                        "subscriptionGeneration": subscription.generation,
                        "payload": payload,
                    }
                )
                self._push_submission_updates(
                    target,
                    payload,
                    subscription_generation=subscription.generation,
                )
            except (OSError, WebSocketProtocolError):
                return

    def _push_submission_updates(
        self,
        target: Any,
        payload: dict[str, Any],
        *,
        subscription_generation: str,
        include_ack_state: bool = False,
    ) -> None:
        events = self._submission_tracker.advance(
            target_id=target.id,
            repo_root=getattr(target, "repo_root", None),
            payload=payload,
            include_ack_state=include_ack_state,
        )
        for submission in events:
            self._send(
                {
                    "type": "lane.submission",
                    "targetId": target.id,
                    "source": "watch",
                    "subscriptionGeneration": subscription_generation,
                    "submission": submission,
                }
            )

    def _lane_context(
        self, target: Any
    ) -> tuple[str | None, TranscriptResolution | None]:
        thread_id = self.callbacks.thread_id(target)
        transcript = (
            self.callbacks.transcript_resolution(thread_id) if thread_id else None
        )
        return thread_id, transcript

    def _lane_signature(self, subscription: _LaneSubscription) -> Any:
        thread_id, transcript = self._lane_context(subscription.target)
        return self.callbacks.lane_signature(subscription.target, thread_id, transcript)


def pending_only_signature_change(previous: Any, current: Any) -> bool:
    if not isinstance(previous, LaneSignature) or not isinstance(
        current, LaneSignature
    ):
        return False
    return (
        previous.inbox != current.inbox
        and previous.transcript == current.transcript
        and previous.other == current.other
    )


def _pending_lane_payload(target: Any) -> dict[str, Any]:
    pending_identity = pending_inbox_identity_payload(
        getattr(target, "repo_root", None)
    )
    return {
        key: pending_identity[key]
        for key in PENDING_LANE_PAYLOAD_KEYS
        if key in pending_identity
    }


def _lane_send_server_timing(
    *,
    received_at: float,
    target_resolved_at: float,
    send_payload_started_at: float,
    send_payload_finished_at: float,
) -> dict[str, float]:
    return {
        "targetResolveMs": _elapsed_ms(received_at, target_resolved_at),
        "sendPayloadMs": _elapsed_ms(send_payload_started_at, send_payload_finished_at),
        "totalBeforeReplyMs": _elapsed_ms(received_at, send_payload_finished_at),
    }


def _elapsed_ms(start: float, end: float) -> float:
    return max(0.0, (end - start) * _MS_PER_SECOND)


def _wait_for_change(
    paths: tuple[Path, ...],
    stop: Event,
    watch: _KqueueWatch | None = None,
    *,
    activated: Event | None = None,
) -> bool:
    """Block until a watched path changes or `stop` is set.

    A `watch` keeps the kqueue armed across calls so a change that fires
    between calls — e.g. while the caller is pushing a payload — is kernel
    queued and delivered on the next call instead of being lost in a reopen
    gap. Without one (or off kqueue) the watch is opened per call. `activated`
    is published only after the selected watcher has accepted observable paths.
    """
    watch_paths = _existing_watch_paths(paths)
    if not watch_paths:
        raise RuntimeError("lane watcher has no observable paths")
    if _HAVE_KQUEUE:
        if watch is not None:
            return watch.wait(watch_paths, stop, activated=activated)
        return _wait_for_change_kqueue(watch_paths, stop, activated=activated)
    return _wait_for_change_watchfiles(watch_paths, stop, activated=activated)


class _KqueueWatch:
    """A kqueue VNODE watch kept armed across waits.

    The fd set and kqueue are rebuilt only when the watched paths change;
    otherwise the same armed kqueue is reused, so vnode events that fire while
    the caller is between waits stay queued in the kernel and surface on the
    next wait. Not a poll: each wait blocks on `kqueue.control`.
    """

    def __init__(self) -> None:
        self._paths: tuple[Path, ...] = ()
        self._descriptors: list[int] = []
        self._kqueue: Any = None
        self._events: list[Any] = []

    def wait(
        self, paths: tuple[Path, ...], stop: Event, *, activated: Event | None = None
    ) -> bool:
        self._arm(paths)
        if not self._events:
            raise RuntimeError("lane watcher could not open observable paths")
        if activated is not None:
            activated.set()
        while not stop.is_set():
            triggered = self._kqueue.control(
                [], len(self._events), LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S
            )
            if triggered:
                return True
        return False

    def _arm(self, paths: tuple[Path, ...]) -> None:
        if paths == self._paths and self._kqueue is not None:
            return
        self.close()
        self._paths = paths
        descriptors: list[int] = []
        for path in paths:
            try:
                descriptors.append(os.open(path, os.O_RDONLY))
            except OSError:
                continue
        if not descriptors:
            return
        self._descriptors = descriptors
        self._kqueue = _select_attr("kqueue")()
        self._events = [
            _select_attr("kevent")(
                descriptor,
                filter=_select_attr("KQ_FILTER_VNODE"),
                flags=_select_attr("KQ_EV_ADD") | _select_attr("KQ_EV_CLEAR"),
                fflags=_KQUEUE_VNODE_FFLAGS,
            )
            for descriptor in descriptors
        ]
        # Submit the changelist separately from the first blocking wait. Only
        # after this call returns can the activation marker truthfully promise
        # that subsequent filesystem edits are observable by the kernel queue.
        self._kqueue.control(self._events, 0, 0)

    def close(self) -> None:
        if self._kqueue is not None:
            self._kqueue.close()
            self._kqueue = None
        for descriptor in self._descriptors:
            try:
                os.close(descriptor)
            except OSError:
                pass
        self._descriptors = []
        self._events = []
        self._paths = ()


def _existing_watch_paths(paths: tuple[Path, ...]) -> tuple[Path, ...]:
    result: list[Path] = []
    seen: set[Path] = set()
    for path in paths:
        if not path.exists() or path in seen:
            continue
        seen.add(path)
        result.append(path)
    return tuple(result)


def _wait_for_change_kqueue(
    paths: tuple[Path, ...], stop: Event, *, activated: Event | None = None
) -> bool:
    import os

    descriptors: list[int] = []
    try:
        for path in paths:
            try:
                descriptors.append(os.open(path, os.O_RDONLY))
            except OSError:
                continue
        if not descriptors:
            raise RuntimeError("lane watcher could not open observable paths")
        kqueue = _select_attr("kqueue")()
        try:
            events = [
                _select_attr("kevent")(
                    descriptor,
                    filter=_select_attr("KQ_FILTER_VNODE"),
                    flags=_select_attr("KQ_EV_ADD") | _select_attr("KQ_EV_CLEAR"),
                    fflags=_KQUEUE_VNODE_FFLAGS,
                )
                for descriptor in descriptors
            ]
            kqueue.control(events, 0, 0)
            if activated is not None:
                activated.set()
            while not stop.is_set():
                triggered = kqueue.control(
                    [], len(events), LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S
                )
                if triggered:
                    return True
            return False
        finally:
            kqueue.close()
    finally:
        for descriptor in descriptors:
            os.close(descriptor)


def _wait_for_change_watchfiles(
    paths: tuple[Path, ...], stop: Event, *, activated: Event | None = None
) -> bool:
    module = import_module("watchfiles")
    watch = cast(Callable[..., Any], getattr(module, "watch"))

    changes = watch(
        *paths,
        stop_event=stop,
        rust_timeout=int(LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S * _MS_PER_SECOND),
        yield_on_timeout=True,
    )
    for observed in changes:
        if activated is not None and not activated.is_set():
            # The generator creates RustNotify before its first event/timeout;
            # it stays open across yields, so this is a native-ready signal and
            # does not introduce a reopen interval before subsequent changes.
            activated.set()
        if observed:
            return True
    if activated is not None and not activated.is_set():
        raise RuntimeError("lane watcher stopped before native registration")
    return False


def _bounded_int(value: Any, default: int) -> int:
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        return default
    return parsed if parsed > 0 else default


def serve_live_bus(handler: Any, callbacks: LiveBusCallbacks) -> None:
    connection = accept_websocket(handler)
    if connection is None:
        return
    LiveBusSession(connection, callbacks).run()
