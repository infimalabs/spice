"""Target-scoped lifecycle decision scheduling for Serve.

The reconciler is deliberately an ephemeral coordination boundary.  Inputs name
one target and one source fact or operator intent; the authoritative inbox,
task, team, and agent state is read by the decision handler when it runs.
Nothing here persists or copies a lane snapshot.
"""

from __future__ import annotations

from collections import deque
from concurrent.futures import Future
from contextlib import contextmanager
from dataclasses import dataclass
from enum import StrEnum
from threading import Event, Lock, Thread
from time import monotonic
from typing import Any, Callable, Iterator, Protocol, TypeAlias

from spice.serve.payload.identity import (
    record_started_renewal_from_ensure,
    resolve_thread_id_for_target,
    serve_agent_identity_payload,
    team_actor_for_target,
    team_facts_for_target,
)
from spice.serve.worktree.target import WorktreeTarget

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


@dataclass(frozen=True, slots=True)
class AutomaticLifecycleDecision:
    """The compact result of one authoritative automatic lane evaluation."""

    thread_id: str
    predecessor_actor: str
    renewal_intent: bool
    agent_ensure: dict[str, Any] | None


LifecycleHandler: TypeAlias = Callable[
    [LifecycleInput, Event],
    LifecycleOutcome,
]


class ExplicitPendingInboxEnsure(Protocol):
    """The one explicit pending-inbox launch grant yielded by the authority."""

    def __call__(
        self, *, fast_mode: bool = False, force_new: bool = False
    ) -> dict[str, Any] | None: ...


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


def ensure_agent_for_pending_inbox(
    target: WorktreeTarget,
    **kwargs: Any,
) -> dict[str, Any] | None:
    """Call the pending-inbox actuator without creating an import cycle."""
    from spice.serve.agentapi import ensure_agent_for_pending_inbox as ensure

    return ensure(target, **kwargs)


def ensure_agent_for_available_work(
    target: WorktreeTarget,
    **kwargs: Any,
) -> dict[str, Any] | None:
    """Call the available-work actuator without creating an import cycle."""
    from spice.serve.agentapi import ensure_agent_for_available_work as ensure

    return ensure(target, **kwargs)


class LifecycleDecisionAuthority:
    """Own automatic lifecycle policy and its target-local attempt bookkeeping."""

    def __init__(self, state: Any) -> None:
        self._state = state
        self._lock = Lock()
        self._target_locks: dict[str, Lock] = {}
        self._attempt_cache: dict[str, float] = {}

    def handle(
        self,
        value: LifecycleInput,
        cancelled: Event,
    ) -> LifecycleOutcome:
        """Evaluate automatic wakes; later slices migrate explicit intents."""
        if not isinstance(value, AutomaticLifecycleWake):
            return _observe_input(value, cancelled)
        target = self._target(value.target_id)
        decision = self.evaluate_target(target)
        return LifecycleOutcome(
            target_id=value.target_id,
            input_identity=value.input_identity,
            input_kind=value.input_kind,
            status=LifecycleOutcomeStatus.OBSERVED,
            detail=_automatic_decision_detail(decision),
        )

    def evaluate_target(
        self,
        target: WorktreeTarget,
        *,
        thread_id: str | None = None,
    ) -> AutomaticLifecycleDecision:
        """Run the one pending-inbox-first automatic decision for ``target``."""
        target_lock = self._target_lock(target.id)
        with target_lock:
            return self._evaluate_target_locked(target, thread_id=thread_id)

    @contextmanager
    def explicit_pending_inbox(
        self, target: WorktreeTarget
    ) -> Iterator[ExplicitPendingInboxEnsure]:
        """Serialize publication and its one unthrottled explicit launch grant."""
        target_lock = self._target_lock(target.id)
        with target_lock:
            active = True
            used = False

            def ensure(
                *, fast_mode: bool = False, force_new: bool = False
            ) -> dict[str, Any] | None:
                nonlocal used
                if not active:
                    raise RuntimeError("explicit pending-inbox grant is out of scope")
                if used:
                    raise RuntimeError("explicit pending-inbox grant already used")
                used = True
                return ensure_agent_for_pending_inbox(
                    target,
                    retry_due=self._attempt_due,
                    retry_seconds=0.0,
                    fast_mode=fast_mode,
                    force_new=force_new,
                    automatic=False,
                )

            try:
                yield ensure
            finally:
                active = False

    def _evaluate_target_locked(
        self,
        target: WorktreeTarget,
        *,
        thread_id: str | None,
    ) -> AutomaticLifecycleDecision:
        bound_thread_id = (
            resolve_thread_id_for_target(self._state, target) or ""
            if thread_id is None
            else thread_id
        )
        store = self._state.team_store
        predecessor_actor = team_actor_for_target(
            store,
            target,
            bound_thread_id,
        )
        renewal_intent = bool(
            bound_thread_id
            and predecessor_actor
            and store.agent_renewal_active(predecessor_actor)
        )
        if renewal_intent:
            serve_agent_identity_payload(
                target,
                bound_thread_id,
                actor_id=predecessor_actor,
                store=store,
            )
        fast_mode = bool(store.global_fast_mode_enabled())
        ensure_kwargs: dict[str, Any] = {
            "retry_due": self._attempt_due,
            "fast_mode": fast_mode,
            "force_new": renewal_intent,
        }
        agent_ensure = ensure_agent_for_pending_inbox(target, **ensure_kwargs)
        team_facts = team_facts_for_target(store, target, bound_thread_id)
        if agent_ensure is None and team_facts.get("lifetime") == "Drain":
            agent_ensure = ensure_agent_for_available_work(
                target,
                thread_id=bound_thread_id,
                **ensure_kwargs,
            )
        ensured_thread_id = record_started_renewal_from_ensure(
            store,
            predecessor_agent_id=predecessor_actor,
            agent_ensure=agent_ensure,
        )
        return AutomaticLifecycleDecision(
            thread_id=ensured_thread_id or bound_thread_id,
            predecessor_actor=predecessor_actor,
            renewal_intent=renewal_intent,
            agent_ensure=agent_ensure,
        )

    def _attempt_due(self, target_id: str, retry_seconds: float) -> bool:
        now = monotonic()
        last_attempt = self._attempt_cache.get(target_id)
        if last_attempt is not None and now - last_attempt < retry_seconds:
            return False
        self._attempt_cache[target_id] = now
        return True

    def _target_lock(self, target_id: str) -> Lock:
        with self._lock:
            return self._target_locks.setdefault(target_id, Lock())

    def _target(self, target_id: str) -> WorktreeTarget:
        for target in self._state.worktree_targets():
            if target.id == target_id:
                return target
        raise RuntimeError(f"unknown lifecycle target: {target_id}")


def lifecycle_decision_authority(state: Any) -> LifecycleDecisionAuthority:
    """Return the one ephemeral lifecycle policy owner attached to Serve state."""
    authority = getattr(state, "lifecycle_decision_authority", None)
    if authority is None:
        authority = LifecycleDecisionAuthority(state)
        state.lifecycle_decision_authority = authority
    return authority


def evaluate_automatic_lifecycle(
    state: Any,
    target: WorktreeTarget,
    *,
    thread_id: str | None = None,
) -> AutomaticLifecycleDecision:
    """Transitional direct entry point shared by inventory and message callers."""
    return lifecycle_decision_authority(state).evaluate_target(
        target,
        thread_id=thread_id,
    )


def _automatic_decision_detail(decision: AutomaticLifecycleDecision) -> str:
    agent_ensure = decision.agent_ensure
    if agent_ensure is None:
        return "no-action"
    parts = [
        str(agent_ensure.get(field) or "")
        for field in ("trigger", "action", "reason", "failure")
    ]
    return ":".join(part for part in parts if part) or "agent-ensure"


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
            dropped: list[_QueuedInput] = []
            for queue in self._queues.values():
                dropped.extend(queue)
                queue.clear()
            for item in dropped:
                self._forget_automatic_locked(item)
        # Future.cancel runs done callbacks inline, so cancelling under the
        # lock would deadlock any callback that reads an outcome or submits.
        for item in dropped:
            item.future.cancel()

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
            # Publish before releasing the key so a wake landing on the
            # completion boundary coalesces onto this same Future instead of
            # running the handler a second time for one durable fact.  Both
            # calls stay outside the lock because set_result runs done
            # callbacks inline and those callbacks may re-enter this runtime.
            item.future.set_result(outcome)
            self._forget_automatic(item)

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
            self._forget_automatic_locked(item)

    def _forget_automatic_locked(self, item: _QueuedInput) -> None:
        """Release a key only while it still names this item's own Future.

        Cancellation and the completion boundary can both reach the same key,
        and a later wake may already own it by then; matching on the Future
        keeps one release from discarding the next wake's coalescing entry.
        """
        key = item.automatic_key
        if key is not None and self._automatic_futures.get(key) is item.future:
            del self._automatic_futures[key]


def start_lifecycle_reconciler(
    state: Any,
    *,
    handler: LifecycleHandler | None = None,
) -> LifecycleReconciler | None:
    """Start the one active-mode reconciler and attach it to Serve state."""
    if state.observer_mode:
        state.lifecycle_reconciler = None
        return None
    if state.lifecycle_reconciler is not None:
        raise RuntimeError("Serve state already owns a lifecycle reconciler")
    reconciler = LifecycleReconciler(
        handler or lifecycle_decision_authority(state).handle
    )
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
