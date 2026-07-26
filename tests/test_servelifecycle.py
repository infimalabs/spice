"""The target-scoped Serve lifecycle reconciler runtime."""

from __future__ import annotations

import threading
import time
from argparse import Namespace
from dataclasses import fields
from types import SimpleNamespace

import pytest

from spice.serve import app as serve_app, lifecycle
from spice.serve.lifecycle import (
    AutomaticLifecycleWake,
    ExplicitLifecycleIntent,
    LifecycleDecisionAuthority,
    LifecycleOutcome,
    LifecycleOutcomeStatus,
    LifecycleReconciler,
    LifecycleWakeSource,
    lifecycle_decision_authority,
    start_lifecycle_reconciler,
    submit_explicit_send_intent,
    submit_inbox_wake,
)

RECONCILER_RETRY_AFTER_SECONDS = 17.5


def _outcome(
    value: AutomaticLifecycleWake | ExplicitLifecycleIntent,
) -> LifecycleOutcome:
    return LifecycleOutcome(
        target_id=value.target_id,
        input_identity=_identity(value),
        input_kind=_kind(value),
        status=LifecycleOutcomeStatus.OBSERVED,
    )


def _identity(value: AutomaticLifecycleWake | ExplicitLifecycleIntent) -> str:
    if isinstance(value, AutomaticLifecycleWake):
        return f"{value.source.value}:{value.source_identity}"
    return value.intent_id


def _kind(value: AutomaticLifecycleWake | ExplicitLifecycleIntent) -> str:
    if isinstance(value, AutomaticLifecycleWake):
        return f"automatic:{value.source.value}"
    return f"explicit:{value.kind}"


def test_compact_inputs_carry_identity_without_lane_snapshots() -> None:
    assert [field.name for field in fields(AutomaticLifecycleWake)] == [
        "target_id",
        "source",
        "source_identity",
    ]
    # The grant parameters are the request, not a lane snapshot: how this one
    # launch attempt should be spent, decided before the handler reads any state.
    assert [field.name for field in fields(ExplicitLifecycleIntent)] == [
        "target_id",
        "intent_id",
        "kind",
        "fast_mode",
        "force_new",
    ]


def test_active_mode_starts_one_reconciler_and_observer_mode_starts_none() -> None:
    active = SimpleNamespace(observer_mode=False, lifecycle_reconciler=None)
    reconciler = start_lifecycle_reconciler(active)
    try:
        assert reconciler is not None
        assert active.lifecycle_reconciler is reconciler
    finally:
        assert reconciler is not None
        reconciler.cancel()
        assert reconciler.join(timeout=1.0) is True

    observer = SimpleNamespace(observer_mode=True, lifecycle_reconciler=None)
    assert start_lifecycle_reconciler(observer) is None
    assert observer.lifecycle_reconciler is None


def test_target_chains_serialize_while_sibling_targets_run_concurrently() -> None:
    first_started = threading.Event()
    sibling_finished = threading.Event()
    release_first = threading.Event()
    calls: list[str] = []
    calls_lock = threading.Lock()

    def handle(value, _cancelled):
        with calls_lock:
            calls.append(_identity(value))
        if _identity(value) == "intent-a1":
            first_started.set()
            assert release_first.wait(timeout=2.0) is True
        if _identity(value) == "intent-b1":
            sibling_finished.set()
        return _outcome(value)

    reconciler = LifecycleReconciler(handle)
    reconciler.start()
    try:
        first = reconciler.submit_intent(
            ExplicitLifecycleIntent("lane-a", "intent-a1", "send")
        )
        assert first_started.wait(timeout=1.0) is True
        second = reconciler.submit_intent(
            ExplicitLifecycleIntent("lane-a", "intent-a2", "send")
        )
        sibling = reconciler.submit_intent(
            ExplicitLifecycleIntent("lane-b", "intent-b1", "send")
        )

        assert sibling_finished.wait(timeout=1.0) is True
        assert sibling.result(timeout=1.0).status is LifecycleOutcomeStatus.OBSERVED
        assert second.done() is False
        release_first.set()
        assert first.result(timeout=1.0).input_identity == "intent-a1"
        assert second.result(timeout=1.0).input_identity == "intent-a2"
    finally:
        reconciler.cancel()
        assert reconciler.join(timeout=1.0) is True

    assert calls == ["intent-a1", "intent-b1", "intent-a2"]


def test_explicit_grant_and_automatic_decision_share_the_target_lock(
    monkeypatch,
) -> None:
    authority = LifecycleDecisionAuthority(SimpleNamespace())
    target_a = SimpleNamespace(id="lane-a")
    target_b = SimpleNamespace(id="lane-b")
    explicit_started = threading.Event()
    release_explicit = threading.Event()
    same_target_started = threading.Event()
    sibling_started = threading.Event()
    ensure_calls: list[dict[str, object]] = []

    def ensure_pending(_target, **kwargs):
        ensure_calls.append(kwargs)
        return None

    def evaluate_locked(_authority, target, *, thread_id):
        del thread_id
        if target.id == target_a.id:
            same_target_started.set()
        else:
            sibling_started.set()

    monkeypatch.setattr(lifecycle, "ensure_agent_for_pending_inbox", ensure_pending)
    monkeypatch.setattr(
        LifecycleDecisionAuthority,
        "_evaluate_target_locked",
        evaluate_locked,
    )

    def hold_explicit_grant() -> None:
        with authority.explicit_pending_inbox(target_a) as ensure:
            ensure(fast_mode=True, force_new=True)
            with pytest.raises(RuntimeError, match="already used"):
                ensure()
            explicit_started.set()
            assert release_explicit.wait(timeout=2.0) is True

    explicit = threading.Thread(target=hold_explicit_grant)
    same_target = threading.Thread(
        target=authority.evaluate_target,
        args=(target_a,),
    )
    sibling = threading.Thread(
        target=authority.evaluate_target,
        args=(target_b,),
    )
    explicit.start()
    assert explicit_started.wait(timeout=1.0) is True
    same_target.start()
    sibling.start()
    assert sibling_started.wait(timeout=1.0) is True
    assert same_target_started.is_set() is False
    release_explicit.set()
    for thread in (explicit, same_target, sibling):
        thread.join(timeout=1.0)
        assert thread.is_alive() is False
    assert same_target_started.is_set() is True

    assert len(ensure_calls) == 1
    ensure_call = ensure_calls[0]
    assert ensure_call["retry_seconds"] == 0.0
    assert ensure_call["automatic"] is False
    assert ensure_call["fast_mode"] is True
    assert ensure_call["force_new"] is True
    retry_due = ensure_call["retry_due"]
    assert callable(retry_due)
    assert retry_due(target_a.id, 5.0) is True
    assert retry_due(target_a.id, 5.0) is False
    assert retry_due(target_b.id, 5.0) is True


def _grant_release_lane(monkeypatch, *, ensure_calls: list[dict[str, object]]):
    """One lane whose automatic decision reaches a recorded pending-inbox ensure.

    The lane starts undiscoverable so an explicit intent for it fails where the
    reservation is already outstanding, and becomes discoverable again the way a
    pruned worktree or a recovered discovery does.
    """
    target = SimpleNamespace(id="lane-a", repo_root="/lane-a")
    discoverable: list[object] = []
    store = SimpleNamespace(
        agent_renewal_active=lambda _actor: False,
        global_fast_mode_enabled=lambda: False,
    )
    state = SimpleNamespace(
        observer_mode=False,
        lifecycle_reconciler=None,
        lifecycle_decision_authority=None,
        team_store=store,
        worktree_targets=lambda: list(discoverable),
    )

    def ensure_pending(_target, **kwargs):
        ensure_calls.append(kwargs)
        return {"ok": True, "trigger": "pending-inbox", "threadId": "successor"}

    monkeypatch.setattr(
        lifecycle, "resolve_thread_id_for_target", lambda *_args: "predecessor"
    )
    monkeypatch.setattr(
        lifecycle, "team_actor_for_target", lambda *_args: "thread:predecessor"
    )
    monkeypatch.setattr(lifecycle, "ensure_agent_for_pending_inbox", ensure_pending)
    monkeypatch.setattr(lifecycle, "team_facts_for_target", lambda *_args: {})
    monkeypatch.setattr(
        lifecycle,
        "record_started_renewal_from_ensure",
        lambda _store, **_kwargs: "successor",
    )
    reconciler = start_lifecycle_reconciler(state)
    assert reconciler is not None
    # ServeState hands both submissions to the reconciler it owns; the fake
    # stands in for exactly that, so the submit helpers under test run whole.
    state.submit_lifecycle_intent = reconciler.submit_intent
    state.submit_lifecycle_wake = reconciler.submit_automatic
    return SimpleNamespace(
        state=state,
        target=target,
        discoverable=discoverable,
        reconciler=reconciler,
    )


def test_a_failed_explicit_intent_gives_back_the_grant_it_reserved(
    monkeypatch,
) -> None:
    """A lane pruned before its decision runs still returns its reservation."""
    ensure_calls: list[dict[str, object]] = []
    lane = _grant_release_lane(monkeypatch, ensure_calls=ensure_calls)
    try:
        failed = submit_explicit_send_intent(
            lane.state, lane.target, "1kGwGJq0"
        ).result(timeout=1.0)
        lane.discoverable.append(lane.target)
        later = submit_inbox_wake(lane.state, lane.target, "inbox-revision-9").result(
            timeout=1.0
        )
    finally:
        lane.reconciler.cancel()
        assert lane.reconciler.join(timeout=1.0) is True

    assert failed.status is LifecycleOutcomeStatus.FAILED
    assert failed.detail == "unknown lifecycle target: lane-a"
    # The lane is back and its next automatic decision reaches its own ensure:
    # a grant that outlived its intent would decline this and every one after it.
    assert later.status is LifecycleOutcomeStatus.OBSERVED
    assert later.detail == "pending-inbox"
    retry_due = ensure_calls[0]["retry_due"]
    assert callable(retry_due)
    assert ensure_calls == [
        {"retry_due": retry_due, "fast_mode": False, "force_new": False}
    ]


def test_a_failed_intent_releases_only_the_grant_it_reserved(monkeypatch) -> None:
    """One send's failure cannot spend the grant another send is still owed."""
    ensure_calls: list[dict[str, object]] = []
    lane = _grant_release_lane(monkeypatch, ensure_calls=ensure_calls)
    # A second send has published and holds the launch attempt it reserved; only
    # the first send's intent fails.
    lifecycle_decision_authority(lane.state).reserve_explicit_grant(lane.target.id)
    try:
        failed = submit_explicit_send_intent(
            lane.state, lane.target, "1kGwGJq0"
        ).result(timeout=1.0)
        lane.discoverable.append(lane.target)
        later = submit_inbox_wake(lane.state, lane.target, "inbox-revision-9").result(
            timeout=1.0
        )
    finally:
        lane.reconciler.cancel()
        assert lane.reconciler.join(timeout=1.0) is True

    assert failed.status is LifecycleOutcomeStatus.FAILED
    # The surviving reservation still holds the lane for the send that owns it.
    assert later.status is LifecycleOutcomeStatus.OBSERVED
    assert later.detail == "no-action"
    assert ensure_calls == []


def test_duplicate_automatic_wakes_coalesce_but_explicit_intents_remain_distinct() -> (
    None
):
    started = threading.Event()
    release = threading.Event()
    calls: list[str] = []

    def handle(value, _cancelled):
        calls.append(_identity(value))
        if isinstance(value, AutomaticLifecycleWake):
            started.set()
            assert release.wait(timeout=2.0) is True
        return _outcome(value)

    reconciler = LifecycleReconciler(handle)
    reconciler.start()
    try:
        wake = AutomaticLifecycleWake(
            "lane-a",
            LifecycleWakeSource.TASK,
            "revision-7",
        )
        first = reconciler.submit_automatic(wake)
        assert started.wait(timeout=1.0) is True
        duplicate = reconciler.submit_automatic(wake)
        assert duplicate is first
        release.set()
        assert first.result(timeout=1.0).input_identity == "task:revision-7"

        explicit_one = reconciler.submit_intent(
            ExplicitLifecycleIntent("lane-a", "intent-1", "send")
        )
        explicit_two = reconciler.submit_intent(
            ExplicitLifecycleIntent("lane-a", "intent-2", "send")
        )
        assert explicit_one is not explicit_two
        assert explicit_one.result(timeout=1.0).input_identity == "intent-1"
        assert explicit_two.result(timeout=1.0).input_identity == "intent-2"
    finally:
        reconciler.cancel()
        assert reconciler.join(timeout=1.0) is True

    assert calls == ["task:revision-7", "intent-1", "intent-2"]


def test_automatic_authority_evaluates_pending_before_drain_work(
    monkeypatch,
) -> None:
    calls: list[object] = []
    pending_calls: list[dict[str, object]] = []
    target = SimpleNamespace(id="lane-a", repo_root="/lane-a")
    store = SimpleNamespace(
        agent_renewal_active=lambda actor: calls.append(("renewal", actor)) or True,
        global_fast_mode_enabled=lambda: calls.append("fast-mode") or True,
    )
    state = SimpleNamespace(
        observer_mode=False,
        lifecycle_reconciler=None,
        lifecycle_decision_authority=None,
        team_store=store,
        worktree_targets=lambda: [target],
    )
    pending_result = {
        "ok": True,
        "trigger": "pending-inbox",
        "threadId": "successor",
    }

    monkeypatch.setattr(
        lifecycle,
        "resolve_thread_id_for_target",
        lambda _state, _target: calls.append("resolve-thread") or "predecessor",
    )
    monkeypatch.setattr(
        lifecycle,
        "team_actor_for_target",
        lambda _store, _target, thread_id: (
            calls.append(("team-actor", thread_id)) or "thread:predecessor"
        ),
    )
    monkeypatch.setattr(
        lifecycle,
        "serve_agent_identity_payload",
        lambda *_args, **_kwargs: calls.append("renewal-identity") or {},
    )

    def ensure_pending(_target, **kwargs):
        pending_calls.append(kwargs)
        calls.append(("pending", kwargs))
        return pending_result

    monkeypatch.setattr(lifecycle, "ensure_agent_for_pending_inbox", ensure_pending)
    monkeypatch.setattr(
        lifecycle,
        "team_facts_for_target",
        lambda _store, _target, thread_id: (
            calls.append(("team-facts", thread_id)) or {"lifetime": "Drain"}
        ),
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_available_work",
        lambda *_args, **_kwargs: calls.append("available-work"),
    )
    monkeypatch.setattr(
        lifecycle,
        "record_started_renewal_from_ensure",
        lambda _store, **kwargs: (
            calls.append(("record-renewal", kwargs)) or "successor"
        ),
    )

    reconciler = start_lifecycle_reconciler(state)
    assert reconciler is not None
    try:
        outcome = reconciler.submit_automatic(
            AutomaticLifecycleWake(
                "lane-a",
                LifecycleWakeSource.INBOX,
                "inbox-revision-4",
            )
        ).result(timeout=1.0)
    finally:
        reconciler.cancel()
        assert reconciler.join(timeout=1.0) is True

    authority = state.lifecycle_decision_authority
    assert authority is not None
    assert not hasattr(authority, "attempt_cache")
    retry_due = pending_calls[0]["retry_due"]
    assert callable(retry_due)
    assert outcome.detail == "pending-inbox"
    assert "available-work" not in calls
    assert pending_calls == [
        {
            "retry_due": retry_due,
            "fast_mode": True,
            "force_new": True,
        }
    ]
    assert calls == [
        "resolve-thread",
        ("team-actor", "predecessor"),
        ("renewal", "thread:predecessor"),
        "renewal-identity",
        "fast-mode",
        (
            "pending",
            {
                "retry_due": retry_due,
                "fast_mode": True,
                "force_new": True,
            },
        ),
        ("team-facts", "predecessor"),
        (
            "record-renewal",
            {
                "predecessor_agent_id": "thread:predecessor",
                "agent_ensure": pending_result,
            },
        ),
    ]


def test_automatic_authority_falls_through_to_drain_work(monkeypatch) -> None:
    target = SimpleNamespace(id="lane-a", repo_root="/lane-a")
    store = SimpleNamespace(
        agent_renewal_active=lambda _actor: False,
        global_fast_mode_enabled=lambda: True,
    )
    state = SimpleNamespace(
        observer_mode=False,
        lifecycle_reconciler=None,
        lifecycle_decision_authority=None,
        team_store=store,
        worktree_targets=lambda: [target],
    )
    calls: list[tuple[str, object]] = []
    available_result = {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "claim-lost",
        "taskHandle": "LIFECYC-example",
        "retryAfterSeconds": RECONCILER_RETRY_AFTER_SECONDS,
    }
    monkeypatch.setattr(
        lifecycle,
        "resolve_thread_id_for_target",
        lambda _state, _target: "bound-thread",
    )
    monkeypatch.setattr(
        lifecycle,
        "team_actor_for_target",
        lambda _store, _target, thread_id: (
            calls.append(("actor", thread_id)) or "thread:bound-thread"
        ),
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_pending_inbox",
        lambda _target, **kwargs: calls.append(("pending", kwargs)),
    )
    monkeypatch.setattr(
        lifecycle,
        "team_facts_for_target",
        lambda _store, _target, thread_id: (
            calls.append(("team", thread_id)) or {"lifetime": "Drain"}
        ),
    )

    def ensure_available(_target, **kwargs):
        calls.append(("available", kwargs))
        return available_result

    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_available_work",
        ensure_available,
    )
    monkeypatch.setattr(
        lifecycle,
        "record_started_renewal_from_ensure",
        lambda _store, **kwargs: calls.append(("record", kwargs)) or "",
    )

    reconciler = start_lifecycle_reconciler(state)
    assert reconciler is not None
    try:
        outcome = reconciler.submit_automatic(
            AutomaticLifecycleWake(
                "lane-a",
                LifecycleWakeSource.TASK,
                "task-revision-9",
            )
        ).result(timeout=1.0)
    finally:
        reconciler.cancel()
        assert reconciler.join(timeout=1.0) is True

    authority = state.lifecycle_decision_authority
    assert authority is not None
    pending_kwargs = calls[1][1]
    assert isinstance(pending_kwargs, dict)
    retry_due = pending_kwargs["retry_due"]
    assert callable(retry_due)
    ensure_kwargs = {
        "retry_due": retry_due,
        "fast_mode": True,
        "force_new": False,
    }
    assert outcome.detail == "available-work:skipped:claim-lost"
    assert outcome.retry_after_seconds == RECONCILER_RETRY_AFTER_SECONDS
    assert calls == [
        ("actor", "bound-thread"),
        ("pending", ensure_kwargs),
        ("team", "bound-thread"),
        (
            "available",
            {
                "thread_id": "bound-thread",
                **ensure_kwargs,
            },
        ),
        (
            "record",
            {
                "predecessor_agent_id": "thread:bound-thread",
                "agent_ensure": available_result,
            },
        ),
    ]
    available_kwargs = calls[3][1]
    assert isinstance(available_kwargs, dict)
    assert available_kwargs["retry_due"] is retry_due
    assert retry_due("lane-a", 5.0) is True
    assert retry_due("lane-a", 5.0) is False
    assert retry_due("lane-b", 5.0) is True


def test_duplicate_automatic_policy_wake_performs_lifecycle_writes_once(
    monkeypatch,
) -> None:
    started = threading.Event()
    release = threading.Event()
    writes: list[str] = []
    target = SimpleNamespace(id="lane-a", repo_root="/lane-a")
    store = SimpleNamespace(
        agent_renewal_active=lambda _actor: True,
        global_fast_mode_enabled=lambda: False,
    )
    state = SimpleNamespace(
        observer_mode=False,
        lifecycle_reconciler=None,
        lifecycle_decision_authority=None,
        team_store=store,
        worktree_targets=lambda: [target],
    )
    monkeypatch.setattr(
        lifecycle,
        "resolve_thread_id_for_target",
        lambda _state, _target: "predecessor",
    )
    monkeypatch.setattr(
        lifecycle,
        "team_actor_for_target",
        lambda _store, _target, _thread: "thread:predecessor",
    )
    monkeypatch.setattr(
        lifecycle,
        "serve_agent_identity_payload",
        lambda *_args, **_kwargs: writes.append("renewal-identity") or {},
    )

    def ensure_pending(_target, **_kwargs):
        writes.append("launch-or-deadletter")
        started.set()
        assert release.wait(timeout=2.0) is True
        return {
            "ok": True,
            "trigger": "pending-inbox",
            "threadId": "successor",
            "deadletteredInboxKey": "inbox-1",
        }

    monkeypatch.setattr(lifecycle, "ensure_agent_for_pending_inbox", ensure_pending)
    monkeypatch.setattr(
        lifecycle,
        "team_facts_for_target",
        lambda *_args: {"lifetime": "Drain"},
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_available_work",
        lambda *_args, **_kwargs: writes.append("available-work"),
    )
    monkeypatch.setattr(
        lifecycle,
        "record_started_renewal_from_ensure",
        lambda *_args, **_kwargs: writes.append("renewal-start") or "successor",
    )

    reconciler = start_lifecycle_reconciler(state)
    assert reconciler is not None
    try:
        wake = AutomaticLifecycleWake(
            "lane-a",
            LifecycleWakeSource.INBOX,
            "inbox-revision-4",
        )
        first = reconciler.submit_automatic(wake)
        assert started.wait(timeout=1.0) is True
        duplicate = reconciler.submit_automatic(wake)
        assert duplicate is first
        release.set()
        assert first.result(timeout=1.0).detail == "pending-inbox"
    finally:
        release.set()
        reconciler.cancel()
        assert reconciler.join(timeout=1.0) is True

    assert writes == [
        "renewal-identity",
        "launch-or-deadletter",
        "renewal-start",
    ]


def test_automatic_key_stays_coalescible_until_its_future_completes() -> None:
    started = threading.Event()
    release = threading.Event()
    boundary_submitted = threading.Event()
    boundary: list[object] = []
    calls: list[str] = []

    def handle(value, _cancelled):
        calls.append(_identity(value))
        if isinstance(value, AutomaticLifecycleWake):
            started.set()
            assert release.wait(timeout=2.0) is True
        return _outcome(value)

    wake = AutomaticLifecycleWake("lane-a", LifecycleWakeSource.TASK, "revision-7")
    reconciler = LifecycleReconciler(handle)
    reconciler.start()
    try:

        def resubmit_on_completion(_future) -> None:
            boundary.append(reconciler.submit_automatic(wake))
            boundary_submitted.set()

        first = reconciler.submit_automatic(wake)
        # Registering while the handler is parked puts the duplicate wake
        # exactly on the completion boundary: set_result runs this callback
        # inline, between publishing the result and releasing the key.
        assert started.wait(timeout=1.0) is True
        first.add_done_callback(resubmit_on_completion)
        release.set()

        assert first.result(timeout=1.0).input_identity == "task:revision-7"
        assert boundary_submitted.wait(timeout=1.0) is True
        assert boundary[0] is first

        # The lane is FIFO, so a boundary duplicate that had escaped
        # coalescing would have to run before this sentinel completes.
        sentinel = reconciler.submit_intent(
            ExplicitLifecycleIntent("lane-a", "sentinel", "send")
        )
        assert sentinel.result(timeout=1.0).input_identity == "sentinel"
        assert calls == ["task:revision-7", "sentinel"]

        again = reconciler.submit_automatic(wake)
        assert again is not first
        assert again.result(timeout=1.0).input_identity == "task:revision-7"
    finally:
        release.set()
        reconciler.cancel()
        assert reconciler.join(timeout=1.0) is True

    assert calls == ["task:revision-7", "sentinel", "task:revision-7"]


def test_cancellation_callbacks_may_reenter_the_reconciler_during_cancel() -> None:
    started = threading.Event()
    release = threading.Event()
    cancel_returned = threading.Event()
    observed: list[str] = []

    def handle(value, _cancelled):
        if _identity(value) == "running":
            started.set()
            assert release.wait(timeout=2.0) is True
        return _outcome(value)

    reconciler = LifecycleReconciler(handle)
    reconciler.start()
    try:
        settled = reconciler.submit_intent(
            ExplicitLifecycleIntent("lane-a", "settled", "send")
        )
        assert settled.result(timeout=1.0).input_identity == "settled"

        running = reconciler.submit_intent(
            ExplicitLifecycleIntent("lane-a", "running", "send")
        )
        assert started.wait(timeout=1.0) is True
        queued = reconciler.submit_intent(
            ExplicitLifecycleIntent("lane-a", "queued", "send")
        )

        def reenter_on_cancel(_future) -> None:
            outcome = reconciler.latest_outcome("lane-a")
            observed.append(outcome.input_identity if outcome else "")
            try:
                reconciler.submit_intent(
                    ExplicitLifecycleIntent("lane-a", "reentrant", "send")
                )
            except RuntimeError as exc:
                observed.append(str(exc))

        def cancel_and_flag() -> None:
            reconciler.cancel()
            cancel_returned.set()

        queued.add_done_callback(reenter_on_cancel)
        # A daemon canceller keeps a regression here a bounded failure rather
        # than a wedged suite: the deadlock it guards would hold the lock.
        canceller = threading.Thread(target=cancel_and_flag, daemon=True)
        canceller.start()

        assert cancel_returned.wait(timeout=2.0) is True
        assert observed == ["settled", "lifecycle reconciler is shutting down"]
        assert queued.cancelled() is True

        release.set()
        assert running.result(timeout=1.0).input_identity == "running"
        assert reconciler.join(timeout=1.0) is True
    finally:
        release.set()


def test_handler_failure_is_retained_and_does_not_kill_the_target_worker() -> None:
    def handle(value, _cancelled):
        if _identity(value) == "broken":
            raise RuntimeError("launch failed visibly")
        return _outcome(value)

    reconciler = LifecycleReconciler(handle)
    reconciler.start()
    try:
        failed = reconciler.submit_intent(
            ExplicitLifecycleIntent("lane-a", "broken", "send")
        )
        recovered = reconciler.submit_intent(
            ExplicitLifecycleIntent("lane-a", "recovered", "send")
        )

        failure = failed.result(timeout=1.0)
        assert failure.status is LifecycleOutcomeStatus.FAILED
        assert failure.detail == "launch failed visibly"
        assert recovered.result(timeout=1.0).status is LifecycleOutcomeStatus.OBSERVED
        assert reconciler.latest_outcome("lane-a") == recovered.result(timeout=1.0)
    finally:
        reconciler.cancel()
        assert reconciler.join(timeout=1.0) is True


def test_cancel_drops_queued_work_and_join_is_bounded_during_running_work() -> None:
    started = threading.Event()
    release = threading.Event()

    def handle(value, _cancelled):
        started.set()
        assert release.wait(timeout=2.0) is True
        return _outcome(value)

    reconciler = LifecycleReconciler(handle)
    reconciler.start()
    running = reconciler.submit_intent(
        ExplicitLifecycleIntent("lane-a", "running", "send")
    )
    assert started.wait(timeout=1.0) is True
    queued = reconciler.submit_intent(
        ExplicitLifecycleIntent("lane-a", "queued", "send")
    )

    before = time.monotonic()
    reconciler.cancel()
    assert reconciler.join(timeout=0.01) is False
    assert time.monotonic() - before < 0.2
    assert queued.cancelled() is True
    release.set()
    assert running.result(timeout=1.0).input_identity == "running"
    assert reconciler.join(timeout=1.0) is True


def test_run_serve_owns_reconciler_start_cancel_and_join(monkeypatch) -> None:
    events: list[str] = []

    class FakeServer:
        server_address = ("127.0.0.1", 4321)

        def serve_forever(self) -> None:
            events.append("serve")

        def server_close(self) -> None:
            events.append("server-close")

    class FakeReconciler:
        def cancel(self) -> None:
            events.append("reconciler-cancel")

        def join(self) -> None:
            events.append("reconciler-join")

    def start_reconciler(state):
        assert state.observer_mode is False
        events.append("reconciler-start")
        return FakeReconciler()

    monkeypatch.setattr(serve_app, "_ServeHttpServer", lambda *_args: FakeServer())
    monkeypatch.setattr(
        serve_app, "start_exit_file_watch", lambda *_args, **_kwargs: None
    )
    monkeypatch.setattr(serve_app, "start_available_work_watch", lambda _state: None)
    monkeypatch.setattr(serve_app, "start_lifecycle_reconciler", start_reconciler)

    result = serve_app.run_serve(
        Namespace(
            host="127.0.0.1",
            port=0,
            until=None,
            backend=None,
            task_backend=None,
            observer_mode=False,
        )
    )

    assert result == 0
    assert events == [
        "reconciler-start",
        "serve",
        "reconciler-cancel",
        "server-close",
        "reconciler-join",
    ]
