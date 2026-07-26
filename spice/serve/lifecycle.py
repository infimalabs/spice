"""Target-scoped lifecycle decision scheduling for Serve.

The reconciler is deliberately an ephemeral coordination boundary.  Inputs name
one target and one source fact or operator intent; the authoritative inbox,
task, team, and agent state is read by the decision handler when it runs.
Nothing here persists or copies a lane snapshot.
"""

from __future__ import annotations

from collections import deque
from collections.abc import Iterator
from concurrent.futures import Future
from contextlib import contextmanager
from dataclasses import dataclass
from enum import StrEnum
from threading import Event, Lock, Thread
from time import monotonic
from typing import Any, Callable, TypeAlias

from spice.errors import SpiceError
from spice.serve.payload.identity import (
    projected_team_actor_for_target,
    record_started_renewal_from_ensure,
    record_serve_agent_identity,
    resolve_thread_id_for_target,
    target_bound_actor,
    team_actor_for_target,
    team_facts_for_target,
)
from spice.serve.worktree.target import WorktreeTarget

LIFECYCLE_RECONCILER_JOIN_SECONDS = 3.0
LIFECYCLE_RECONCILER_THREAD_PREFIX = "spice-serve-lifecycle"
# The one explicit intent kind: an operator send asking this lane to be awake.
LIFECYCLE_INTENT_SEND = "send"
# A queued decision is one ensure against a lane that is already publishing its
# item, and waiting on it is what lets a send reply with the launch it caused.
# The bound only has to outlast that decision, never long enough to hide a stuck
# one.
LIFECYCLE_DECISION_WAIT_SECONDS = 30.0


class LifecycleWakeSource(StrEnum):
    """Compact signals that can make an automatic decision relevant."""

    TASK = "task"
    TEAM = "team"
    INBOX = "inbox"
    TIMER = "timer"


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
    fast_mode: bool = False
    force_new: bool = False

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
class LifecycleDecision:
    """The compact result of one authoritative lane evaluation.

    Automatic wakes and explicit intents both resolve to this record: they differ
    in what may act, not in what a decision is.
    """

    thread_id: str
    predecessor_actor: str
    renewal_intent: bool
    agent_ensure: dict[str, Any] | None


@dataclass(frozen=True, slots=True)
class LifecycleProjection:
    """Current durable lane facts plus the reconciler's observable result."""

    thread_id: str
    predecessor_actor: str
    renewal_intent: bool
    agent_ensure: dict[str, Any] | None


@dataclass(frozen=True, slots=True)
class LifecycleOutcome:
    """The latest compact observable result for one target."""

    target_id: str
    input_identity: str
    input_kind: str
    status: LifecycleOutcomeStatus
    detail: str = ""
    retry_after_seconds: float | None = None
    # The route that submitted an intent needs the decision itself, not just its
    # rendered detail: the send response reports agentEnsure, the ensured thread,
    # and renewal intent straight off this record.
    decision: LifecycleDecision | None = None


LifecycleHandler: TypeAlias = Callable[
    [LifecycleInput, Event],
    LifecycleOutcome,
]


@dataclass(frozen=True, slots=True)
class _TargetObservation:
    """The lane identity facts every decision and every payload starts from."""

    thread_id: str
    predecessor_actor: str
    renewal_intent: bool


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
    """The sole production owner of Serve lifecycle decisions and actuation.

    The reconciler already gives each target a FIFO worker. The target locks
    retained here have one additional job: a synchronous route publishes outside
    that worker, so its publication and explicit reservation must exclude an
    automatic decision for the same target. They never coordinate sibling lanes.

    ``_coordination_lock`` protects only the lock registry and reservation
    counters; it is never held across discovery, durable I/O, or launch. The
    attempt cache is read only under its target lock and records the last actual
    attempt, preserving retry throttling without becoming a second policy owner.
    """

    def __init__(self, state: Any) -> None:
        self._state = state
        self._coordination_lock = Lock()
        self._target_locks: dict[str, Lock] = {}
        self._attempt_cache: dict[str, float] = {}
        self._explicit_grants: dict[str, int] = {}

    @contextmanager
    def explicit_publication(self, target_id: str) -> Iterator[None]:
        """Exclude same-target decisions while a route publishes and reserves."""
        with self._target_lock(target_id):
            yield

    def reserve_explicit_grant(self, target_id: str) -> None:
        """Reserve the next launch attempt on ``target_id`` for an explicit intent.

        The publishing route holds this authority's target-scoped publication
        guard across both its publication and this reservation, so no automatic
        decision for this lane can observe the new item before the grant exists.
        A sibling lane remains entirely independent.
        """
        with self._coordination_lock:
            outstanding = self._explicit_grants.get(target_id, 0)
            self._explicit_grants[target_id] = outstanding + 1

    def handle(
        self,
        value: LifecycleInput,
        _cancelled: Event,
    ) -> LifecycleOutcome:
        """Decide one reconciler input against current lane facts."""
        if isinstance(value, ExplicitLifecycleIntent):
            decision = self._spend_reserved_grant(value)
        else:
            target = self._target(value.target_id)
            with self._target_lock(target.id):
                decision = self._evaluate_target_locked(target)
        return LifecycleOutcome(
            target_id=value.target_id,
            input_identity=value.input_identity,
            input_kind=value.input_kind,
            status=LifecycleOutcomeStatus.OBSERVED,
            detail=_decision_detail(decision),
            retry_after_seconds=_decision_retry_after_seconds(decision),
            decision=decision,
        )

    def _spend_reserved_grant(
        self, intent: ExplicitLifecycleIntent
    ) -> LifecycleDecision:
        """Spend the one grant ``intent`` reserved, and give it back either way.

        The release is keyed on the intent's own target id and covers resolving
        the target too, so a lane that stopped being discoverable between its
        publication and this decision still returns its reservation. A grant that
        outlived the intent that reserved it would decline every later automatic
        decision for that lane -- the lane it exists to protect would be the one
        that never starts again.
        """
        try:
            target = self._target(intent.target_id)
        except Exception:
            self._release_explicit_grant(intent.target_id)
            raise
        with self._target_lock(target.id):
            try:
                # Observe before the launch, not after. A force-new renewal
                # rebinds the lane to the successor, so a later read would name
                # the successor as its own predecessor and close the lineage on
                # itself.
                observed = self._observe_target_locked(target)
                agent_ensure = ensure_agent_for_pending_inbox(
                    target,
                    retry_due=self._attempt_due,
                    retry_seconds=0.0,
                    fast_mode=intent.fast_mode,
                    force_new=intent.force_new,
                    automatic=False,
                )
                ensured_thread_id = self._converge_renewal_locked(
                    observed=observed,
                    agent_ensure=agent_ensure,
                    # A send that does not force a successor is the one that
                    # carried the handoff request, so it is the send whose
                    # renewal settles as pending.
                    handoff_asked=not intent.force_new,
                )
            finally:
                # Release before the target lock: a later queued decision may
                # acquire that lock immediately and must not mistake a spent
                # grant for a reservation that still owns the inbox item.
                self._release_explicit_grant(intent.target_id)
        return LifecycleDecision(
            thread_id=ensured_thread_id or observed.thread_id,
            predecessor_actor=observed.predecessor_actor,
            renewal_intent=observed.renewal_intent,
            agent_ensure=agent_ensure,
        )

    def _observe_target_locked(
        self,
        target: WorktreeTarget,
    ) -> _TargetObservation:
        bound_thread_id = resolve_thread_id_for_target(self._state, target) or ""
        store = self._state.team_store
        predecessor_actor = team_actor_for_target(
            store,
            target,
            bound_thread_id,
        )
        return _TargetObservation(
            thread_id=bound_thread_id,
            predecessor_actor=predecessor_actor,
            renewal_intent=bool(
                bound_thread_id
                and predecessor_actor
                and store.agent_renewal_active(predecessor_actor)
            ),
        )

    def _release_explicit_grant(self, target_id: str) -> None:
        with self._coordination_lock:
            outstanding = self._explicit_grants.get(target_id, 0) - 1
            if outstanding > 0:
                self._explicit_grants[target_id] = outstanding
            else:
                self._explicit_grants.pop(target_id, None)

    def _explicit_grant_outstanding(self, target_id: str) -> bool:
        with self._coordination_lock:
            return target_id in self._explicit_grants

    def _evaluate_target_locked(
        self,
        target: WorktreeTarget,
    ) -> LifecycleDecision:
        observed = self._observe_target_locked(target)
        bound_thread_id = observed.thread_id
        predecessor_actor = observed.predecessor_actor
        renewal_intent = observed.renewal_intent
        store = self._state.team_store
        if self._explicit_grant_outstanding(target.id):
            # An operator send published its item and reserved the attempt it is
            # owed. Spending it here is what would dead-letter that steering
            # against the refusal the grant exists to bypass. The target lock
            # closes the publication-to-reservation gap for this lane only.
            return LifecycleDecision(
                thread_id=bound_thread_id,
                predecessor_actor=predecessor_actor,
                renewal_intent=renewal_intent,
                agent_ensure=None,
            )
        return self._decide_automatic_locked(
            target,
            observed=observed,
            store=store,
        )

    def _decide_automatic_locked(
        self,
        target: WorktreeTarget,
        *,
        observed: _TargetObservation,
        store: Any,
    ) -> LifecycleDecision:
        bound_thread_id = observed.thread_id
        predecessor_actor = observed.predecessor_actor
        renewal_intent = observed.renewal_intent
        if renewal_intent:
            record_serve_agent_identity(
                store,
                target,
                bound_thread_id,
                actor_id=predecessor_actor,
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
        ensured_thread_id = self._converge_renewal_locked(
            observed=observed, agent_ensure=agent_ensure
        )
        decision_thread_id = ensured_thread_id or bound_thread_id
        self._converge_automatic_identity_locked(target, decision_thread_id)
        return LifecycleDecision(
            thread_id=decision_thread_id,
            predecessor_actor=predecessor_actor,
            renewal_intent=renewal_intent,
            agent_ensure=agent_ensure,
        )

    def _converge_automatic_identity_locked(
        self,
        target: WorktreeTarget,
        thread_id: str,
    ) -> None:
        """Persist the real actor an automatic observation or launch resolved."""
        if not thread_id:
            return
        store = self._state.team_store
        actor = target_bound_actor(target, thread_id)
        if projected_team_actor_for_target(store, target, thread_id) != actor:
            actor = team_actor_for_target(store, target, thread_id)
        record_serve_agent_identity(
            store,
            target,
            thread_id,
            actor_id=actor,
        )

    def _converge_renewal_locked(
        self,
        *,
        observed: _TargetObservation,
        agent_ensure: dict[str, Any] | None,
        handoff_asked: bool = False,
    ) -> str:
        """Settle one active renewal against what the launch attempt just did.

        This is the only place a renewal fact is written. Both decision paths
        reach it, so a send, an inbox wake, and a task wake all converge on the
        same durable row rather than each carrying their own bookkeeping; the
        store's own transactions make repeats idempotent.

        A successor thread means the renewal started, and the store transfers
        the team slot and lineage with it. A renewal only turns pending when the
        send carrying the handoff request produced no successor: that request is
        what the predecessor answers, so an automatic wake -- which delivers no
        such text -- leaves the request standing for the next send.
        """
        store = self._state.team_store
        ensured_thread_id = record_started_renewal_from_ensure(
            store,
            predecessor_agent_id=observed.predecessor_actor,
            agent_ensure=agent_ensure,
        )
        if ensured_thread_id or not handoff_asked or not observed.renewal_intent:
            return ensured_thread_id
        # Only a still-requested renewal converges to pending. Reading the
        # request rather than the broader active state is what keeps a lane that
        # is sent to repeatedly during a handoff from restamping the same fact.
        if not store.agent_renewal_requested(observed.predecessor_actor):
            return ensured_thread_id
        try:
            store.record_pending_renewal(
                agent_id=observed.predecessor_actor,
                ancestor_thread_id=observed.thread_id,
            )
        except SpiceError:
            pass  # renewal bookkeeping requires a team; steering still lands
        return ensured_thread_id

    def _attempt_due(self, target_id: str, retry_seconds: float) -> bool:
        now = monotonic()
        last_attempt = self._attempt_cache.get(target_id)
        if last_attempt is not None and now - last_attempt < retry_seconds:
            return False
        self._attempt_cache[target_id] = now
        return True

    def _target_lock(self, target_id: str) -> Lock:
        with self._coordination_lock:
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


@contextmanager
def explicit_send_publication(
    state: Any,
    target: WorktreeTarget,
) -> Iterator[None]:
    """Make one send's publication and grant reservation target-atomic."""
    with lifecycle_decision_authority(state).explicit_publication(target.id):
        yield


def submit_explicit_send_intent(
    state: Any,
    target: WorktreeTarget,
    intent_id: str,
    *,
    fast_mode: bool = False,
    force_new: bool = False,
) -> Future[LifecycleOutcome]:
    """Reserve this send's launch attempt and queue the decision that spends it.

    Callers hold the authority's target-scoped publication guard across their
    publication and this call, so the reservation is in place before any
    automatic decision for the lane can reach the item. Submitting is only an
    enqueue: the caller releases the guard and then awaits the real Future.
    """
    authority = lifecycle_decision_authority(state)
    authority.reserve_explicit_grant(target.id)
    try:
        return state.submit_lifecycle_intent(
            ExplicitLifecycleIntent(
                target_id=target.id,
                intent_id=intent_id,
                kind=LIFECYCLE_INTENT_SEND,
                fast_mode=fast_mode,
                force_new=force_new,
            )
        )
    except Exception:
        # A reconciler shutting down can reject the enqueue after publication.
        # No Future exists to release this reservation, so return it here.
        authority._release_explicit_grant(target.id)
        raise


def submit_inbox_wake(
    state: Any,
    target: WorktreeTarget,
    source_identity: str,
) -> Future[LifecycleOutcome]:
    """Queue the automatic decision a durable inbox publication makes relevant."""
    return state.submit_lifecycle_wake(
        AutomaticLifecycleWake(
            target_id=target.id,
            source=LifecycleWakeSource.INBOX,
            source_identity=source_identity,
        )
    )


def project_lifecycle(
    state: Any,
    target: WorktreeTarget,
    *,
    thread_id: str | None = None,
    decision: LifecycleDecision | None = None,
    latest_outcome: bool = True,
    prefer_outcome_thread: bool = False,
) -> LifecycleProjection:
    """Project one lane without evaluating or otherwise actuating lifecycle.

    Normal inventory and message reads use the latest settled reconciler
    decision only for its observable ensure result. A send follow-up supplies
    the exact decision it queued and opts out of the latest-outcome lookup so a
    timeout cannot accidentally report an older decision.
    """
    resolved_thread_id = (
        resolve_thread_id_for_target(state, target) or ""
        if thread_id is None
        else thread_id
    )
    selected = decision
    if selected is None and latest_outcome:
        reconciler = getattr(state, "lifecycle_reconciler", None)
        outcome = (
            LifecycleReconciler.latest_outcome(reconciler, target.id)
            if reconciler is not None
            else None
        )
        selected = outcome.decision if outcome is not None else None
    if (
        selected is not None
        and selected.thread_id
        and (decision is not None or prefer_outcome_thread)
    ):
        resolved_thread_id = selected.thread_id
    actor = projected_team_actor_for_target(
        state.team_store,
        target,
        resolved_thread_id,
    )
    predecessor_actor = (
        selected.predecessor_actor
        if selected is not None and selected.predecessor_actor
        else actor
    )
    renewal_intent = (
        selected.renewal_intent
        if selected is not None
        else bool(
            resolved_thread_id
            and actor
            and state.team_store.agent_renewal_active(actor)
        )
    )
    return LifecycleProjection(
        thread_id=resolved_thread_id,
        predecessor_actor=predecessor_actor,
        renewal_intent=renewal_intent,
        agent_ensure=selected.agent_ensure if selected is not None else None,
    )


def await_lane_lifecycle(
    state: Any,
    target: WorktreeTarget,
) -> LifecycleDecision | None:
    """Report the decision behind ``target``'s queued lifecycle work, once run."""
    outcome = state.await_lifecycle_outcome(target.id)
    return outcome.decision if outcome is not None else None


def _decision_detail(decision: LifecycleDecision) -> str:
    agent_ensure = decision.agent_ensure
    if agent_ensure is None:
        return "no-action"
    parts = [
        str(agent_ensure.get(field) or "")
        for field in ("trigger", "action", "reason", "failure")
    ]
    return ":".join(part for part in parts if part) or "agent-ensure"


def _decision_retry_after_seconds(
    decision: LifecycleDecision,
) -> float | None:
    agent_ensure = decision.agent_ensure
    if agent_ensure is None:
        return None
    value = agent_ensure.get("retryAfterSeconds")
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return float(value)


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
        self._settled: dict[str, Event] = {}

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

    def await_target(
        self,
        target_id: str,
        timeout: float = LIFECYCLE_DECISION_WAIT_SECONDS,
    ) -> LifecycleOutcome | None:
        """Wait out the work already queued for ``target_id`` and report the last.

        A caller that publishes into a lane and then renders it needs the queued
        decisions to have run: rendering first shows the lane exactly as it was
        before the publication being reported, which for a send that starts a
        lane is an idle lane with no thread.
        """
        with self._lock:
            settled = self._settled.get(target_id)
        if settled is not None:
            settled.wait(timeout)
        return self.latest_outcome(target_id)

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
        self._settled.setdefault(target_id, Event()).clear()
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
                    # Release waiters from inside the lock that guards the queue
                    # they are waiting on, so a wake enqueued in this instant
                    # clears the event again rather than losing to this set.
                    settled = self._settled.get(target_id)
                    if settled is not None:
                        settled.set()
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
