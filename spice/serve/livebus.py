"""The live bus: one WebSocket per browser, request/response plus push.

Requests carry a `requestId` the response echoes; pushes carry none. Verbs:
`bus.ping`, `targets.refresh`, `teams.refresh`, `teams.command`,
`lane.subscribe`, `lane.configure`, `lane.unsubscribe`, `lane.refresh`,
`lane.history`, `lane.send`, `lane.taskDrain`, `metrics.series`. A subscription tails the
agent's transcript and pushes `lane.payload` frames the moment new lines
land — kqueue watches the open file descriptor on macOS (FSEvents misses
appends through a held-open handle), watchfiles covers Linux/Windows.
"""

from __future__ import annotations

import os
import select
from dataclasses import dataclass, field
from importlib import import_module
from pathlib import Path
from queue import Queue
from threading import Event, Lock, Thread
from typing import Any, Callable, cast

from spice.serve.messages import TranscriptResolution
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.websocket import (
    WebSocketConnection,
    WebSocketDisconnect,
    WebSocketProtocolError,
    accept_websocket,
)

DEFAULT_BUS_MESSAGE_LIMIT = 50
INITIAL_BUS_MESSAGE_LIMIT = 25
PENDING_LANE_PAYLOAD_KEYS = (
    "pendingInboxCount",
    "pendingInboxKeys",
    "pendingInboxRevision",
    "pendingInboxVersion",
)

_HAVE_KQUEUE = hasattr(select, "kqueue")
if _HAVE_KQUEUE:
    _KQUEUE_VNODE_FFLAGS = (
        select.KQ_NOTE_WRITE
        | select.KQ_NOTE_EXTEND
        | select.KQ_NOTE_DELETE
        | select.KQ_NOTE_RENAME
    )
# kqueue blocks until a vnode event arrives; this bounds how long a cancelled
# watcher waits before noticing its stop flag. It is a wakeup interval, not a
# filesystem poll.
LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S = 1.0
LIVE_BUS_WATCHER_JOIN_TIMEOUT_S = LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S + 0.5

# A connected client sends `bus.ping` heartbeats well inside this window; a
# whole interval with no frame means the peer is gone and the blocking read
# is unblocked so the session and its watchers are reaped.
LIVE_BUS_READ_TIMEOUT_S = 45.0
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


@dataclass(frozen=True)
class LaneSignature:
    transcript: Any
    inbox: Any
    other: Any


@dataclass
class _LaneSubscription:
    target: Any
    query: dict[str, Any]
    stop: Event = field(default_factory=Event)
    thread: Thread | None = None
    lock: Lock = field(default_factory=Lock)
    last_signature: Any = None


class LiveBusSession:
    def __init__(
        self, connection: WebSocketConnection, callbacks: LiveBusCallbacks
    ) -> None:
        self.connection = connection
        self.callbacks = callbacks
        self.subscriptions: dict[str, _LaneSubscription] = {}
        self.send_lock = Lock()
        # Metrics are read-only display data whose queries can be heavy; running
        # them inline would block interactive frames (lane.send, acks) on this
        # one socket. A dedicated worker drains them so the dispatch loop stays
        # responsive — replies still carry the requestId the client matches on.
        self._metrics_queue: Queue[dict[str, Any] | None] = Queue()
        self._metrics_worker: Thread | None = None

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
        for subscription in list(self.subscriptions.values()):
            self._stop_subscription(subscription)
        self.subscriptions.clear()
        if self._metrics_worker is not None:
            self._metrics_queue.put(None)
            self._metrics_worker.join(timeout=LIVE_BUS_WATCHER_JOIN_TIMEOUT_S)
            self._metrics_worker = None

    def _send(self, payload: dict[str, Any]) -> None:
        with self.send_lock:
            self.connection.send_json(payload)

    def _reply(self, message: dict[str, Any], payload: dict[str, Any]) -> None:
        request_id = message.get("requestId")
        if isinstance(request_id, str) and request_id:
            payload = {**payload, "requestId": request_id}
        self._send(payload)

    def _dispatch(self, message: dict[str, Any]) -> None:
        kind = str(message.get("type") or "")
        try:
            handler = {
                "bus.ping": self._handle_ping,
                "targets.refresh": self._handle_targets_refresh,
                "teams.refresh": self._handle_teams_refresh,
                "teams.command": self._handle_teams_command,
                "lane.subscribe": self._handle_lane_subscribe,
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
        self._reply(message, {"type": "bus.pong"})

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
        query = message.get("query") or {}
        kwargs: dict[str, Any] = {
            "limit": _bounded_int(query.get("limit"), DEFAULT_BUS_MESSAGE_LIMIT)
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

    def _handle_lane_subscribe(self, message: dict[str, Any]) -> None:
        target = self._require_target(message)
        if target is None:
            return
        previous = self.subscriptions.pop(target.id, None)
        if previous is not None:
            self._stop_subscription(previous)
        subscription = _LaneSubscription(
            target=target, query=dict(message.get("query") or {})
        )
        self.subscriptions[target.id] = subscription
        subscription.last_signature = self._lane_signature(subscription)
        payload = self.callbacks.messages_payload(target, **self._query_kwargs(message))
        self._reply(message, {"type": "lane.payload", "payload": payload})
        self._start_watcher(subscription)

    def _handle_lane_configure(self, message: dict[str, Any]) -> None:
        target = self._require_target(message)
        if target is None:
            return
        subscription = self.subscriptions.get(target.id)
        if subscription is not None:
            with subscription.lock:
                subscription.query = dict(message.get("query") or {})
        self._reply(message, {"type": "lane.configured"})

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
        payload = self.callbacks.messages_payload(target, **self._query_kwargs(message))
        self._reply(message, {"type": "lane.payload", "payload": payload})

    def _handle_lane_history(self, message: dict[str, Any]) -> None:
        self._handle_lane_refresh(message)

    def _handle_lane_send(self, message: dict[str, Any]) -> None:
        target = self._require_target(message)
        if target is None:
            return
        result, _status = self.callbacks.send_payload(
            target, message.get("payload") or {}
        )
        self._reply(message, {"type": "lane.sendResult", "result": result})

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
                self._reply(message, {"type": "metrics.seriesResult", "result": result})
            except WebSocketDisconnect:
                return
            except Exception as exc:  # surface, never kill the worker silently
                self._reply(message, {"type": "bus.error", "error": str(exc)})

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
        if subscription.thread is not None:
            subscription.thread.join(timeout=LIVE_BUS_WATCHER_JOIN_TIMEOUT_S)

    def _watch_subscription(self, subscription: _LaneSubscription) -> None:
        target = subscription.target
        watch = _KqueueWatch()
        try:
            self._run_watch_loop(subscription, target, watch)
        finally:
            watch.close()

    def _run_watch_loop(
        self, subscription: _LaneSubscription, target: Any, watch: _KqueueWatch
    ) -> None:
        while not subscription.stop.is_set():
            thread_id, transcript = self._lane_context(target)
            watch_paths = self.callbacks.lane_watch_paths(target, thread_id, transcript)
            changed = _wait_for_change(watch_paths, subscription.stop, watch)
            if subscription.stop.is_set():
                return
            if not changed:
                continue
            signature = self.callbacks.lane_signature(target, thread_id, transcript)
            previous_signature = subscription.last_signature
            if signature == previous_signature:
                continue
            subscription.last_signature = signature
            if _pending_only_signature_change(previous_signature, signature):
                try:
                    self._send(
                        {
                            "type": "lane.pending",
                            "targetId": target.id,
                            "source": "watch",
                            "payload": _pending_lane_payload(target),
                        }
                    )
                except (OSError, WebSocketProtocolError):
                    return
                continue
            with subscription.lock:
                query = dict(subscription.query)
            kwargs: dict[str, Any] = {
                "limit": _bounded_int(query.get("limit"), DEFAULT_BUS_MESSAGE_LIMIT)
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
                        "payload": payload,
                    }
                )
            except (OSError, WebSocketProtocolError):
                return

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


def _pending_only_signature_change(previous: Any, current: Any) -> bool:
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


def _wait_for_change(
    paths: tuple[Path, ...], stop: Event, watch: _KqueueWatch | None = None
) -> bool:
    """Block until a watched path changes or `stop` is set.

    A `watch` keeps the kqueue armed across calls so a change that fires
    between calls — e.g. while the caller is pushing a payload — is kernel
    queued and delivered on the next call instead of being lost in a reopen
    gap. Without one (or off kqueue) the watch is opened per call.
    """
    watch_paths = _existing_watch_paths(paths)
    if not watch_paths:
        stop.wait(LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S)
        return False
    if _HAVE_KQUEUE:
        if watch is not None:
            return watch.wait(watch_paths, stop)
        return _wait_for_change_kqueue(watch_paths, stop)
    return _wait_for_change_watchfiles(watch_paths, stop)


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

    def wait(self, paths: tuple[Path, ...], stop: Event) -> bool:
        self._arm(paths)
        if not self._events:
            stop.wait(LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S)
            return False
        while not stop.is_set():
            triggered = self._kqueue.control(
                self._events, len(self._events), LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S
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
        self._kqueue = select.kqueue()
        self._events = [
            select.kevent(
                descriptor,
                filter=select.KQ_FILTER_VNODE,
                flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR,
                fflags=_KQUEUE_VNODE_FFLAGS,
            )
            for descriptor in descriptors
        ]

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


def _wait_for_change_kqueue(paths: tuple[Path, ...], stop: Event) -> bool:
    import os

    descriptors: list[int] = []
    try:
        for path in paths:
            try:
                descriptors.append(os.open(path, os.O_RDONLY))
            except OSError:
                continue
        if not descriptors:
            stop.wait(LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S)
            return False
        kqueue = select.kqueue()
        try:
            events = [
                select.kevent(
                    descriptor,
                    filter=select.KQ_FILTER_VNODE,
                    flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR,
                    fflags=_KQUEUE_VNODE_FFLAGS,
                )
                for descriptor in descriptors
            ]
            while not stop.is_set():
                triggered = kqueue.control(
                    events, len(events), LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S
                )
                if triggered:
                    return True
            return False
        finally:
            kqueue.close()
    finally:
        for descriptor in descriptors:
            os.close(descriptor)


def _wait_for_change_watchfiles(paths: tuple[Path, ...], stop: Event) -> bool:
    module = import_module("watchfiles")
    watch = cast(Callable[..., Any], getattr(module, "watch"))

    for _changes in watch(
        *paths,
        stop_event=stop,
        rust_timeout=int(LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S * _MS_PER_SECOND),
    ):
        return True
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
