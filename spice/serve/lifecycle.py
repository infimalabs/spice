"""Target-scoped lifecycle decision scheduling for Serve.

The reconciler is deliberately an ephemeral coordination boundary.  Inputs name
one target and one source fact or operator intent; the authoritative inbox,
task, team, and agent state is read by the decision handler when it runs.
Nothing here persists or copies a lane snapshot.
"""

from __future__ import annotations

from collections import deque
from concurrent.futures import Future
from dataclasses import dataclass
from enum import StrEnum
from threading import Event, Lock, Thread
from time import monotonic
from typing import Any, Callable, TypeAlias

LIFECYCLE_RECONCILER_JOIN_SECONDS = 3.0
LIFECYCLE_RECONCILER_THREAD_PREFIX = "spice-serve-lifecycle"


class LifecycleWakeSource(StrEnum):
    """Durable fact families that can make an automatic decision relevant."""

    TASK = "task"
    TEAM = "team"
    INBOX = "inbox"


class LifecycleOutcomeStatus(StrEnum):
    """Terminal states surfaced by the reconciler result boundary."""

    OBSERVED = "observed"
    FAILED = "failed"


@dataclass(frozen=True, slots=True)
class AutomaticLifecycleWake:
    """A compact automatic signal tied to one durable source identity."""

    target_id: str
    source: LifecycleWakeSource
    source_identity: str

    def __post_init__(self) -> None:
        _require_identity("target_id", self.target_id)
        _require_identity("source_identity", self.source_identity)

    @property
    def input_identity(self) -> str:
        return f"{self.source.value}:{self.source_identity}"

    @property
    def input_kind(self) -> str:
        return f"automatic:{self.source.value}"


@dataclass(frozen=True, slots=True)
class ExplicitLifecycleIntent:
    """One operator decision request; explicit requests are never coalesced."""

    target_id: str
    intent_id: str
    kind: str

    def __post_init__(self) -> None:
        _require_identity("target_id", self.target_id)
        _require_identity("intent_id", self.intent_id)
        _require_identity("kind", self.kind)

    @property
    def input_identity(self) -> str:
        return self.intent_id

    @property
    def input_kind(self) -> str:
        return f"explicit:{self.kind}"


LifecycleInput: TypeAlias = AutomaticLifecycleWake | ExplicitLifecycleIntent


@dataclass(frozen=True, slots=True)
class LifecycleOutcome:
    """The latest compact observable result for one target."""

    target_id: str
    input_identity: str
    input_kind: str
    status: LifecycleOutcomeStatus
    detail: str = ""


LifecycleHandler: TypeAlias = Callable[
    [LifecycleInput, Event],
    LifecycleOutcome,
]


@dataclass(slots=True)
class _QueuedInput:
    value: LifecycleInput
    future: Future[LifecycleOutcome]
    automatic_key: tuple[str, LifecycleWakeSource, str] | None


def _require_identity(field: str, value: str) -> None:
    if not value.strip():
        raise ValueError(f"{field} must be non-empty")


def _observe_input(
    value: LifecycleInput,
    _cancelled: Event,
) -> LifecycleOutcome:
    """A no-actuation handler used until policy crosses in behind this runtime."""
    return LifecycleOutcome(
        target_id=value.target_id,
        input_identity=value.input_identity,
        input_kind=value.input_kind,
        status=LifecycleOutcomeStatus.OBSERVED,
    )


class LifecycleReconciler:
    """Serialize decisions per target while independent targets run concurrently."""

    def __init__(self, handler: LifecycleHandler = _observe_input) -> None:
        self._handler = handler
        self._lock = Lock()
        self._cancelled = Event()
        self._started = False
        self._closing = False
        self._queues: dict[str, deque[_QueuedInput]] = {}
        self._workers: dict[str, Thread] = {}
        self._automatic_futures: dict[
            tuple[str, LifecycleWakeSource, str],
            Future[LifecycleOutcome],
        ] = {}
        self._latest_outcomes: dict[str, LifecycleOutcome] = {}

    def start(self) -> None:
        with self._lock:
            if self._started:
                raise RuntimeError("lifecycle reconciler is already started")
            self._started = True

    def submit_automatic(
        self,
        wake: AutomaticLifecycleWake,
    ) -> Future[LifecycleOutcome]:
        key = (wake.target_id, wake.source, wake.source_identity)
        with self._lock:
            self._require_running()
            existing = self._automatic_futures.get(key)
            if existing is not None:
                return existing
            future: Future[LifecycleOutcome] = Future()
            self._automatic_futures[key] = future
            self._enqueue_locked(_QueuedInput(wake, future, key))
            return future

    def submit_intent(
        self,
        intent: ExplicitLifecycleIntent,
    ) -> Future[LifecycleOutcome]:
        with self._lock:
            self._require_running()
            future: Future[LifecycleOutcome] = Future()
            self._enqueue_locked(_QueuedInput(intent, future, None))
            return future

    def latest_outcome(self, target_id: str) -> LifecycleOutcome | None:
        with self._lock:
            return self._latest_outcomes.get(target_id)

    def cancel(self) -> None:
        with self._lock:
            if not self._started or self._closing:
                return
            self._closing = True
            self._cancelled.set()
            for queue in self._queues.values():
                while queue:
                    item = queue.popleft()
                    item.future.cancel()
                    if item.automatic_key is not None:
                        self._automatic_futures.pop(item.automatic_key, None)

    def join(self, timeout: float = LIFECYCLE_RECONCILER_JOIN_SECONDS) -> bool:
        """Wait within ``timeout`` and report whether every target worker exited."""
        deadline = monotonic() + max(timeout, 0.0)
        with self._lock:
            workers = tuple(self._workers.values())
        for worker in workers:
            worker.join(timeout=max(deadline - monotonic(), 0.0))
        return all(not worker.is_alive() for worker in workers)

    def _require_running(self) -> None:
        if not self._started:
            raise RuntimeError("lifecycle reconciler is not started")
        if self._closing:
            raise RuntimeError("lifecycle reconciler is shutting down")

    def _enqueue_locked(self, item: _QueuedInput) -> None:
        target_id = item.value.target_id
        self._queues.setdefault(target_id, deque()).append(item)
        if target_id in self._workers:
            return
        worker = Thread(
            target=self._drain_target,
            args=(target_id,),
            name=f"{LIFECYCLE_RECONCILER_THREAD_PREFIX}-{target_id}",
            daemon=True,
        )
        self._workers[target_id] = worker
        worker.start()

    def _drain_target(self, target_id: str) -> None:
        while True:
            with self._lock:
                queue = self._queues.get(target_id)
                if not queue:
                    self._queues.pop(target_id, None)
                    self._workers.pop(target_id, None)
                    return
                item = queue.popleft()
            if not item.future.set_running_or_notify_cancel():
                self._forget_automatic(item)
                continue
            outcome = self._run_input(item.value)
            with self._lock:
                self._latest_outcomes[target_id] = outcome
                if item.automatic_key is not None:
                    self._automatic_futures.pop(item.automatic_key, None)
            item.future.set_result(outcome)

    def _run_input(self, value: LifecycleInput) -> LifecycleOutcome:
        try:
            return self._handler(value, self._cancelled)
        except Exception as exc:
            outcome = LifecycleOutcome(
                target_id=value.target_id,
                input_identity=value.input_identity,
                input_kind=value.input_kind,
                status=LifecycleOutcomeStatus.FAILED,
                detail=str(exc),
            )
            print(
                "spice serve: lifecycle reconciliation failed "
                f"target={value.target_id}: {exc}"
            )
            return outcome

    def _forget_automatic(self, item: _QueuedInput) -> None:
        if item.automatic_key is None:
            return
        with self._lock:
            self._automatic_futures.pop(item.automatic_key, None)


def start_lifecycle_reconciler(
    state: Any,
    *,
    handler: LifecycleHandler = _observe_input,
) -> LifecycleReconciler | None:
    """Start the one active-mode reconciler and attach it to Serve state."""
    if state.observer_mode:
        state.lifecycle_reconciler = None
        return None
    if state.lifecycle_reconciler is not None:
        raise RuntimeError("Serve state already owns a lifecycle reconciler")
    reconciler = LifecycleReconciler(handler)
    reconciler.start()
    state.lifecycle_reconciler = reconciler
    return reconciler


def cancel_lifecycle_reconciler(reconciler: LifecycleReconciler) -> None:
    """Stop accepting work and signal every running target decision."""
    reconciler.cancel()


def join_lifecycle_reconciler(
    reconciler: LifecycleReconciler,
    timeout: float | None = None,
) -> bool:
    """Join target workers within Serve's bounded shutdown budget."""
    if timeout is None:
        return reconciler.join()
    return reconciler.join(timeout=timeout)
