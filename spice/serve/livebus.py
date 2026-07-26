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

import time
import uuid
from collections import deque
from concurrent.futures import Future, ThreadPoolExecutor, wait
from dataclasses import dataclass, field
from pathlib import Path
from queue import Queue
from threading import Event, Lock, Thread, Timer
from typing import Any, Callable

from spice.serve.messages import TranscriptResolution
from spice.serve.livebusmutation import LiveBusMutationMixin
from spice.serve.livebuswatch import (
    LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S,
    _KqueueWatch,
    wait_for_change,
)
from spice.serve.payload.lane import lane_chrome_payload
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.payload.wire import validate_live_bus_frame, validate_wire_payload
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
    lane_metrics_payload: Callable[[Any], dict[str, Any]]
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


class LiveBusSession(LiveBusMutationMixin):
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
        self._metrics_queue: Queue[tuple[dict[str, Any], Any] | None] = Queue()
        self._metrics_worker: Thread | None = None
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
        # Heavy read-only verbs route their compute + reply onto that pool:
        # target inventory uses one connection-wide FIFO chain, while lane
        # refresh/history use one chain per target. Two reads on a chain apply
        # in request order while independent chains overlap up to the pool's
        # width. Only one worker drains each chain at a time (guarded by
        # _read_active), so the queue never needs cross-thread reordering.
        self._read_lock = Lock()
        self._read_queues: dict[tuple[str, str], deque[Callable[[], None]]] = {}
        self._read_active: set[tuple[str, str]] = set()
        self._read_futures: set[Future[None]] = set()
        self._init_mutation_state()
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
        self._teardown_mutations(LIVE_BUS_WATCHER_JOIN_TIMEOUT_S)
        if self._metrics_worker is not None:
            self._metrics_queue.put(None)
            self._metrics_worker.join(timeout=LIVE_BUS_WATCHER_JOIN_TIMEOUT_S)
            self._metrics_worker = None
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
        payload = {
            "clientId": self.client_id,
            "frames": frames,
            "totals": {
                "count": sum(int(frame["count"]) for frame in frames.values()),
                "bytes": sum(int(frame["bytes"]) for frame in frames.values()),
            },
        }
        return validate_wire_payload("LiveBusDiagnostics", payload)

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
        # length (matching the wire text), carried back from that one encode so
        # the frame is never serialized a second time just to be measured.
        validate_live_bus_frame(payload)
        encoded = self.connection.encode_text_frame(payload)
        byte_count = encoded.payload_bytes
        wait_started_at = time.perf_counter()
        self.send_lock.acquire()
        acquired_at = time.perf_counter()
        lock_wait_ms = _elapsed_ms(wait_started_at, acquired_at)
        try:
            if before_send is not None:
                before_send(lock_wait_ms)
            write_started_at = time.perf_counter()
            self.connection.send_frame(encoded.frame)
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
                "metrics.summary": self._handle_metrics_summary,
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
        # A diagnostic client (the latency probe) can zero the per-frame
        # telemetry after reading it so each measurement window owns its own
        # counters -- totals AND maxima. Resetting after the reply drops the
        # pong just recorded too, so the next window starts genuinely empty.
        if message.get("reset") is True:
            with self._telemetry_lock:
                self._frame_telemetry.clear()

    def _handle_targets_refresh(self, message: dict[str, Any]) -> None:
        def job() -> None:
            try:
                frame: dict[str, Any] = {
                    "type": "targets.payload",
                    "payload": self.callbacks.work_trees_payload(),
                }
            except Exception as exc:  # mirror _dispatch: surface, never wedge
                frame = {"type": "bus.error", "error": str(exc)}
            try:
                self._reply(message, frame)
            except (OSError, WebSocketProtocolError, WebSocketDisconnect):
                pass  # peer vanished mid-inventory; the chain still drains

        # work_trees_payload can traverse every lane and start pending agents.
        # Keep that inventory off the socket's sole dispatch thread so later
        # team mutations and lane sends are received and acknowledged while it
        # computes. Repeated inventories remain FIFO on their own chain.
        self._enqueue_read_job(("global", "targets"), job)

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

        self._enqueue_read_job(("target", target.id), job)

    def _enqueue_read_job(
        self, chain_key: tuple[str, str], job: Callable[[], None]
    ) -> None:
        with self._read_lock:
            self._read_queues.setdefault(chain_key, deque()).append(job)
            if chain_key in self._read_active:
                return  # a worker is already draining this chain's FIFO
            self._read_active.add(chain_key)
            # The chain blocks on _read_lock on entry, so it cannot finish (and
            # fire the done callback inline) while this call still holds the lock.
            future = self._payload_executor().submit(self._drain_read_chain, chain_key)
            self._read_futures.add(future)
            future.add_done_callback(self._forget_read_future)

    def _drain_read_chain(self, chain_key: tuple[str, str]) -> None:
        while True:
            with self._read_lock:
                queue = self._read_queues.get(chain_key)
                if not queue:
                    self._read_queues.pop(chain_key, None)
                    self._read_active.discard(chain_key)
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

    def _handle_lane_task_drain(self, message: dict[str, Any]) -> None:
        target = self._require_target(message)
        if target is None:
            return
        result, _status = self.callbacks.task_drain_payload(
            target, message.get("payload") or {}
        )
        self._reply(message, {"type": "lane.taskDrainResult", "result": result})

    def _ensure_metrics_worker(self) -> None:
        if self._metrics_worker is None:
            self._metrics_worker = Thread(
                target=self._metrics_loop,
                name="spice-live-bus-metrics",
                daemon=True,
            )
            self._metrics_worker.start()

    def _handle_metrics_series(self, message: dict[str, Any]) -> None:
        self._ensure_metrics_worker()
        self._metrics_queue.put((message, None))

    def _handle_metrics_summary(self, message: dict[str, Any]) -> None:
        # Resolve the lane inline (cheap) so a bad selector is refused on the
        # dispatch thread; only the heavy metrics build is offloaded, matching
        # every other per-target handler.
        target = self._require_target(message)
        if target is None:
            return
        self._ensure_metrics_worker()
        self._metrics_queue.put((message, target))

    def _metrics_loop(self) -> None:
        while True:
            item = self._metrics_queue.get()
            if item is None:
                return
            message, target = item
            try:
                frame = self._metrics_frame(message, target)
            except Exception as exc:  # surface, never kill the worker silently
                frame = {"type": "bus.error", "error": str(exc)}
            try:
                self._reply(message, frame)
            except (OSError, WebSocketProtocolError, WebSocketDisconnect):
                return

    def _metrics_frame(self, message: dict[str, Any], target: Any) -> dict[str, Any]:
        if str(message.get("type") or "") == "metrics.summary":
            result = self.callbacks.lane_metrics_payload(target)
            return {"type": "metrics.summaryResult", "result": result}
        result = self.callbacks.metric_series_payload(message.get("query") or {})
        return {"type": "metrics.seriesResult", "result": result}

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
            changed = wait_for_change(
                watch_paths,
                subscription.stop,
                watch,
                activated=subscription.watcher_activated,
            )
            if subscription.stop.is_set():
                return
            if not changed:
                continue
            change_detected_perf = time.perf_counter()
            change_detected_wall = time.time()
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
            signature_started_perf = time.perf_counter()
            signature = self.callbacks.lane_signature(target, thread_id, transcript)
            signature_done_perf = time.perf_counter()
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
            payload_started_perf = time.perf_counter()
            try:
                payload = self.callbacks.messages_payload(target, **kwargs)
            except Exception as exc:
                payload = {"error": str(exc), "messages": [], "statusLine": {}}
            payload_done_perf = time.perf_counter()
            # Emit-to-card trace: wall marks bridge the server clock to the
            # browser's Date.now() (same host) so a load harness can decompose
            # watcher detection and socket delivery from static inference; the
            # perf deltas time the CPU-bound signature and payload compute.
            watch_timing = {
                "changeDetectedWallMs": change_detected_wall * 1000.0,
                "preSendWallMs": time.time() * 1000.0,
                "detectToSendMs": _elapsed_ms(change_detected_perf, payload_done_perf),
                "signatureMs": _elapsed_ms(signature_started_perf, signature_done_perf),
                "payloadMs": _elapsed_ms(payload_started_perf, payload_done_perf),
            }
            try:
                self._send(
                    {
                        "type": "lane.payload",
                        "targetId": target.id,
                        "source": "watch",
                        "subscriptionGeneration": subscription.generation,
                        "payload": payload,
                        "watchTiming": watch_timing,
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
    payload = {
        key: pending_identity[key]
        for key in PENDING_LANE_PAYLOAD_KEYS
        if key in pending_identity
    }
    # A pending-only change saw the inbox and nothing else, so the frame names
    # exactly one facet. Whatever the client holds for team configuration, the
    # task board, or activity is left standing rather than restated from a pass
    # that never looked at them.
    payload["chrome"] = lane_chrome_payload(
        target_id=target.id, pending_identity=pending_identity
    )
    return validate_wire_payload("PendingLanePayload", payload)


def _elapsed_ms(start: float, end: float) -> float:
    return max(0.0, (end - start) * _MS_PER_SECOND)


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
