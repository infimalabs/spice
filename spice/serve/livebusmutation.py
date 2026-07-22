"""Asynchronous, target-ordered live-bus mutation delivery."""

from __future__ import annotations

import time
from collections import deque
from concurrent.futures import Future, ThreadPoolExecutor, wait
from queue import Queue
from threading import Lock, Thread
from typing import TYPE_CHECKING, Any, Callable

from spice.serve.payload.wire import validate_wire_payload
from spice.serve.submissions import SubmissionLifecycleTracker
from spice.serve.websocket import WebSocketDisconnect, WebSocketProtocolError

# Interactive mutations can block on target discovery, durable inbox publication,
# and side-channel notification. They must not run on the socket's sole dispatch
# thread or share the payload pool, where eight slow lane reads could starve every
# send. Per-target FIFO chains below preserve request order while this bound keeps
# one browser from creating an unbounded number of mutation workers.
LIVE_BUS_MUTATION_WORKERS = 8
_MS_PER_SECOND = 1000


class LiveBusMutationMixin:
    """Keep slow lane publications off the connection's dispatch thread."""

    if TYPE_CHECKING:
        callbacks: Any
        _closed: bool
        _submission_tracker: SubmissionLifecycleTracker

        def _require_target(self, message: dict[str, Any]) -> Any | None: ...

        def _reply(
            self,
            message: dict[str, Any],
            payload: dict[str, Any],
            *,
            before_send: Callable[[float], None] | None = None,
        ) -> Any: ...

        def _send(self, payload: dict[str, Any]) -> Any: ...

    def _init_mutation_state(self) -> None:
        self._send_followup_queue: Queue[tuple[Any, dict[str, Any]] | None] = Queue()
        self._send_followup_worker: Thread | None = None
        self._mutation_pool: ThreadPoolExecutor | None = None
        self._mutation_lock = Lock()
        self._mutation_queues: dict[str, deque[Callable[[], None]]] = {}
        self._mutation_active: set[str] = set()
        self._mutation_futures: set[Future[None]] = set()

    def _teardown_mutations(self, timeout: float) -> None:
        if self._mutation_pool is not None:
            # Drop work that has not started and give running publications the
            # normal bounded teardown budget. A job that outlives the peer may
            # still finish its durable write, but _closed prevents it from
            # starting a post-send followup worker after teardown.
            with self._mutation_lock:
                self._mutation_queues.clear()
            self._await_pending_mutations(timeout)
            self._mutation_pool.shutdown(wait=False, cancel_futures=True)
            self._mutation_pool = None
        if self._send_followup_worker is not None:
            self._send_followup_queue.put(None)
            self._send_followup_worker.join(timeout=timeout)
            self._send_followup_worker = None

    def _handle_lane_send(self, message: dict[str, Any]) -> None:
        # Resolve + publish on a target-keyed FIFO chain. Target resolution can
        # reconcile every worktree and publication can wait on an inbox lock or
        # side-channel socket; neither belongs on the one dispatch thread shared
        # by every lane in this browser connection.
        received_at = time.perf_counter()
        target_key = str(message.get("targetId") or "")
        self._enqueue_mutation_job(
            target_key,
            lambda: self._complete_lane_send(message, received_at),
        )

    def _complete_lane_send(self, message: dict[str, Any], received_at: float) -> None:
        mutation_started_at = time.perf_counter()
        try:
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
                mutation_started_at=mutation_started_at,
                target_resolved_at=target_resolved_at,
                send_payload_started_at=send_payload_started_at,
                send_payload_finished_at=send_payload_finished_at,
            )
            result = {**result, "serverTiming": server_timing}
            submission_key = str(result.get("key") or "")
            if result.get("ok") is True and submission_key:
                tracker: SubmissionLifecycleTracker = self._submission_tracker
                result["submission"] = tracker.accept(
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
            self._send_lane_timing(message, server_timing, received_at, reply_timing)
            if result.get("ok") is True:
                self._queue_lane_send_followup(target, payload)
        except (OSError, WebSocketProtocolError, WebSocketDisconnect):
            pass  # peer vanished; the target chain still drains cleanly
        except Exception as exc:  # mirror _dispatch from the worker boundary
            try:
                self._reply(message, {"type": "bus.error", "error": str(exc)})
            except (OSError, WebSocketProtocolError, WebSocketDisconnect):
                pass

    def _send_lane_timing(
        self,
        message: dict[str, Any],
        server_timing: dict[str, float],
        received_at: float,
        reply_timing: Any,
    ) -> None:
        request_id = message.get("requestId")
        if not isinstance(request_id, str) or not request_id:
            return
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

    def _mutation_executor(self) -> ThreadPoolExecutor:
        if self._mutation_pool is None:
            self._mutation_pool = ThreadPoolExecutor(
                max_workers=LIVE_BUS_MUTATION_WORKERS,
                thread_name_prefix="spice-live-bus-mutation",
            )
        return self._mutation_pool

    def _enqueue_mutation_job(self, target_key: str, job: Callable[[], None]) -> None:
        with self._mutation_lock:
            self._mutation_queues.setdefault(target_key, deque()).append(job)
            if target_key in self._mutation_active:
                return
            self._mutation_active.add(target_key)
            future = self._mutation_executor().submit(
                self._drain_mutation_chain, target_key
            )
            self._mutation_futures.add(future)
            future.add_done_callback(self._forget_mutation_future)

    def _drain_mutation_chain(self, target_key: str) -> None:
        while True:
            with self._mutation_lock:
                queue = self._mutation_queues.get(target_key)
                if not queue:
                    self._mutation_queues.pop(target_key, None)
                    self._mutation_active.discard(target_key)
                    return
                job = queue.popleft()
            job()

    def _forget_mutation_future(self, future: Future[None]) -> None:
        with self._mutation_lock:
            self._mutation_futures.discard(future)

    def _await_pending_mutations(self, timeout: float | None = None) -> None:
        with self._mutation_lock:
            pending = list(self._mutation_futures)
        if pending:
            wait(pending, timeout=timeout)

    def _queue_lane_send_followup(self, target: Any, payload: dict[str, Any]) -> None:
        if self.callbacks.send_followup_payload is None or self._closed:
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


def _lane_send_server_timing(
    *,
    received_at: float,
    mutation_started_at: float,
    target_resolved_at: float,
    send_payload_started_at: float,
    send_payload_finished_at: float,
) -> dict[str, float]:
    payload = {
        "mutationQueueMs": _elapsed_ms(received_at, mutation_started_at),
        "targetResolveMs": _elapsed_ms(mutation_started_at, target_resolved_at),
        "sendPayloadMs": _elapsed_ms(send_payload_started_at, send_payload_finished_at),
        "totalBeforeReplyMs": _elapsed_ms(received_at, send_payload_finished_at),
    }
    return validate_wire_payload("ServerTiming", payload)


def _elapsed_ms(start: float, end: float) -> float:
    return max(0.0, (end - start) * _MS_PER_SECOND)
