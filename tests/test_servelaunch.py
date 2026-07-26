"""The server-owned wake loop that starts stopped Drain lanes."""

from __future__ import annotations

from concurrent.futures import Future
import threading
from pathlib import Path
from types import SimpleNamespace


from spice.mail.inbox import write_inbox_item
from spice.serve import launch, livebuswatch
from spice.serve.lifecycle import (
    AutomaticLifecycleWake,
    LifecycleOutcome,
    LifecycleOutcomeStatus,
    LifecycleWakeSource,
)
from spice.serve.worktree.target import WorktreeTarget

# Spelled out here rather than read from the scheduler: reading the production
# constant would keep this bound green at any interval.
ESCAPE_REMAINING_AFTER_ONE_MINUTE = 2.0 * 60.0
# A deadline already behind us: the lane declining it has been waiting past its
# escape, so the only thing left to decide is how soon to act.
ESCAPE_ALREADY_EXPIRED_SECONDS = -20.0


def _target(name: str, tmp_path: Path) -> WorktreeTarget:
    repo = tmp_path / name
    repo.mkdir(exist_ok=True)
    return WorktreeTarget(id=name, repo_root=repo, name=name, branch="main")


def _state(
    targets: list[WorktreeTarget],
    *,
    reconcile=None,
) -> SimpleNamespace:
    state = SimpleNamespace(
        observer_mode=False,
        worktree_targets=lambda: list(targets),
        submitted_wakes=[],
    )

    def submit(wake):
        state.submitted_wakes.append(wake)
        future = Future()
        future.set_result(reconcile(wake) if reconcile else _outcome(wake))
        return future

    state.submit_lifecycle_wake = submit
    return state


def _events_file(tmp_path: Path) -> Path:
    events = tmp_path / "events"
    events.write_text("0 bootstrap\n", encoding="utf-8")
    return events


def _outcome(
    wake: AutomaticLifecycleWake,
    *,
    retry_after_seconds: float | None = None,
    status: LifecycleOutcomeStatus = LifecycleOutcomeStatus.OBSERVED,
    detail: str = "",
) -> LifecycleOutcome:
    return LifecycleOutcome(
        target_id=wake.target_id,
        input_identity=f"{wake.source.value}:{wake.source_identity}",
        input_kind=f"automatic:{wake.source.value}",
        status=status,
        detail=detail,
        retry_after_seconds=retry_after_seconds,
    )


def test_observer_mode_runs_no_available_work_watch():
    """Observer mode owns no lanes, so it has nothing to start."""
    watch = launch.start_available_work_watch(SimpleNamespace(observer_mode=True))

    assert watch is None


def test_evaluate_publishes_compact_wakes_and_waits_for_outcomes(tmp_path):
    state = _state([])
    watch = launch.AvailableWorkWatch(state, events_path=_events_file(tmp_path))
    wakes = (
        AutomaticLifecycleWake("lane-a", LifecycleWakeSource.TASK, "task-1"),
        AutomaticLifecycleWake("lane-b", LifecycleWakeSource.INBOX, "inbox-2"),
    )

    outcomes = watch.evaluate(wakes)

    assert state.submitted_wakes == list(wakes)
    assert [outcome.target_id for outcome in outcomes] == ["lane-a", "lane-b"]
    assert watch.next_timer_timeout() is None


def test_evaluate_schedules_one_timer_from_the_reconciler_outcome(
    tmp_path,
    monkeypatch,
):
    now = [100.0]
    monkeypatch.setattr(launch, "monotonic", lambda: now[0])
    state = _state(
        [],
        reconcile=lambda wake: _outcome(
            wake,
            retry_after_seconds=ESCAPE_REMAINING_AFTER_ONE_MINUTE,
        ),
    )
    watch = launch.AvailableWorkWatch(state, events_path=_events_file(tmp_path))
    wake = AutomaticLifecycleWake(
        "lane-a",
        LifecycleWakeSource.TASK,
        "task-1",
    )

    watch.evaluate((wake,))

    assert watch.next_timer_timeout() == ESCAPE_REMAINING_AFTER_ONE_MINUTE
    now[0] += ESCAPE_REMAINING_AFTER_ONE_MINUTE
    timer_wakes = watch.due_timer_wakes()
    assert len(timer_wakes) == 1
    assert timer_wakes[0].target_id == "lane-a"
    assert timer_wakes[0].source is LifecycleWakeSource.TIMER
    assert watch.next_timer_timeout() is None


def test_evaluate_keeps_a_floor_under_an_expired_deadline(
    tmp_path,
    monkeypatch,
):
    now = [100.0]
    monkeypatch.setattr(launch, "monotonic", lambda: now[0])
    state = _state(
        [],
        reconcile=lambda wake: _outcome(
            wake,
            retry_after_seconds=ESCAPE_ALREADY_EXPIRED_SECONDS,
        ),
    )
    watch = launch.AvailableWorkWatch(state, events_path=_events_file(tmp_path))

    watch.evaluate(
        (
            AutomaticLifecycleWake(
                "lane-a",
                LifecycleWakeSource.TASK,
                "task-1",
            ),
        )
    )

    assert watch.next_timer_timeout() == launch.AVAILABLE_WORK_WATCH_MIN_SECONDS


def test_removing_a_target_cancels_its_pending_timer(tmp_path, monkeypatch):
    now = [100.0]
    monkeypatch.setattr(launch, "monotonic", lambda: now[0])
    target = _target("lane-a", tmp_path)
    state = _state(
        [target],
        reconcile=lambda wake: _outcome(
            wake,
            retry_after_seconds=ESCAPE_REMAINING_AFTER_ONE_MINUTE,
        ),
    )
    watch = launch.AvailableWorkWatch(state, events_path=_events_file(tmp_path))
    wake = AutomaticLifecycleWake(
        target.id,
        LifecycleWakeSource.TASK,
        "task-1",
    )
    watch.evaluate((wake,))
    assert watch.next_timer_timeout() == ESCAPE_REMAINING_AFTER_ONE_MINUTE

    watch.observe_events([])

    assert watch.next_timer_timeout() is None


def test_watch_looks_again_when_its_deadline_arrives_with_no_board_change(
    tmp_path, monkeypatch
):
    """The lone-task escape needs no second task and no client refresh."""
    target = _target("timer-lane", tmp_path)
    timer_observed = threading.Event()
    sources: list[LifecycleWakeSource] = []

    def reconcile(wake):
        sources.append(wake.source)
        if wake.source is LifecycleWakeSource.TIMER:
            timer_observed.set()
            return _outcome(wake)
        return _outcome(wake, retry_after_seconds=0.05)

    monkeypatch.setattr(launch, "AVAILABLE_WORK_WATCH_MIN_SECONDS", 0.01)
    watch = launch.AvailableWorkWatch(
        _state([target], reconcile=reconcile),
        events_path=_events_file(tmp_path),
    )
    watch.start()
    try:
        assert timer_observed.wait(timeout=15.0) is True
    finally:
        watch.cancel()
        watch.join()

    assert sources == [LifecycleWakeSource.TASK, LifecycleWakeSource.TIMER]


def test_watch_looks_again_when_the_task_board_changes(tmp_path):
    """A task entering the board wakes the decision without waiting out a deadline."""
    events = _events_file(tmp_path)
    target = _target("task-lane", tmp_path)
    second_wake = threading.Event()
    wakes: list[AutomaticLifecycleWake] = []

    def reconcile(wake):
        wakes.append(wake)
        if len(wakes) >= 2:
            second_wake.set()
        return _outcome(wake)

    watch = launch.AvailableWorkWatch(
        _state([target], reconcile=reconcile),
        events_path=events,
    )
    watch.start()
    try:
        assert watch.armed.wait(timeout=15.0) is True
        events.write_text("1 task\n", encoding="utf-8")
        assert second_wake.wait(timeout=15.0) is True
    finally:
        watch.cancel()
        watch.join()

    assert [(wake.source, wake.source_identity) for wake in wakes] == [
        (LifecycleWakeSource.TASK, "0"),
        (LifecycleWakeSource.TASK, "1"),
    ]


def test_watch_looks_again_when_pending_inbox_is_published(tmp_path):
    """A non-HTTP publish starts an off lane without an inventory request."""
    target = _target("off-lane", tmp_path)
    inbox_wake = threading.Event()
    wakes: list[AutomaticLifecycleWake] = []

    def reconcile(wake):
        wakes.append(wake)
        if wake.source is LifecycleWakeSource.INBOX:
            inbox_wake.set()
        return _outcome(wake)

    watch = launch.AvailableWorkWatch(
        _state([target], reconcile=reconcile),
        events_path=_events_file(tmp_path),
    )

    watch.start()
    try:
        assert watch.armed.wait(timeout=15.0) is True
        write_inbox_item(target.repo_root, "operator.txt", "start this lane")
        assert inbox_wake.wait(timeout=2.0) is True
    finally:
        watch.cancel()
        watch.join()

    assert wakes[0].source is LifecycleWakeSource.TASK
    assert wakes[-1].source is LifecycleWakeSource.INBOX
    assert wakes[-1].source_identity != "0"


def test_watch_preserves_a_task_event_written_during_scheduler_evaluation(
    tmp_path,
):
    """Native registration stays live while the scheduler reads the board."""
    events = _events_file(tmp_path)
    target = _target("task-lane", tmp_path)
    looked_twice = threading.Event()
    wakes: list[AutomaticLifecycleWake] = []

    def reconcile(wake):
        wakes.append(wake)
        if len(wakes) == 1:
            # This is the old observe-before-arm gap: evaluation has begun,
            # but the subsequent wait has not.
            events.write_text("1 changed-during-evaluation\n", encoding="utf-8")
        if len(wakes) >= 2:
            looked_twice.set()
        return _outcome(wake)

    watch = launch.AvailableWorkWatch(
        _state([target], reconcile=reconcile),
        events_path=events,
    )
    watch.start()
    try:
        assert looked_twice.wait(timeout=15.0) is True
    finally:
        watch.cancel()
        watch.join()

    assert [wake.source_identity for wake in wakes] == ["0", "1"]


def test_watchfiles_stays_armed_while_scheduler_evaluates(tmp_path, monkeypatch):
    """The non-kqueue backend uses one native iterator across evaluations."""
    events = _events_file(tmp_path)
    target = _target("task-lane", tmp_path)
    looked_twice = threading.Event()
    native_calls: list[dict] = []
    wakes: list[AutomaticLifecycleWake] = []

    def native_watch(*paths, **options):
        native_calls.append({"paths": paths, **options})
        yield set()  # native registration is now live
        yield {(1, str(events))}  # the write made by the first evaluation
        options["stop_event"].wait(timeout=15.0)

    def reconcile(wake):
        wakes.append(wake)
        if len(wakes) == 1:
            events.write_text("1 watchfiles-evaluation\n", encoding="utf-8")
        if len(wakes) >= 2:
            looked_twice.set()
        return _outcome(wake)

    monkeypatch.setattr(livebuswatch, "_HAVE_KQUEUE", False)
    monkeypatch.setattr(
        livebuswatch,
        "import_module",
        lambda name: (
            SimpleNamespace(watch=native_watch) if name == "watchfiles" else None
        ),
    )
    watch = launch.AvailableWorkWatch(
        _state([target], reconcile=reconcile),
        events_path=events,
    )
    watch.start()
    try:
        assert looked_twice.wait(timeout=15.0) is True
    finally:
        watch.cancel()
        watch.join()

    assert len(native_calls) == 1
    assert [wake.source_identity for wake in wakes] == ["0", "1"]


def test_event_identities_collapse_duplicate_bursts_and_prefer_inbox(
    tmp_path,
):
    target = _target("lane-a", tmp_path)
    events = _events_file(tmp_path)
    watch = launch.AvailableWorkWatch(
        _state([target]),
        events_path=events,
    )
    initial_wakes = watch.observe_events([target])

    assert len(initial_wakes) == 1
    assert watch.observe_events([target]) == ()

    events.write_text("1 task\n", encoding="utf-8")
    write_inbox_item(target.repo_root, "operator.txt", "wake this lane")
    changed_wakes = watch.observe_events([target])

    assert len(changed_wakes) == 1
    assert changed_wakes[0].source is LifecycleWakeSource.INBOX
    assert watch.observe_events([target]) == ()


def test_reconfiguration_arms_new_inbox_before_evaluating_its_identity(
    tmp_path,
):
    events = _events_file(tmp_path)
    targets: list[WorktreeTarget] = []
    inbox_wake = threading.Event()
    pre_discovery_refresh = threading.Event()
    wakes: list[AutomaticLifecycleWake] = []

    def reconcile(wake):
        wakes.append(wake)
        if wake.source is LifecycleWakeSource.INBOX:
            inbox_wake.set()
        return _outcome(wake)

    state = _state(targets, reconcile=reconcile)
    target_reads = 0

    def worktree_targets():
        nonlocal target_reads
        target_reads += 1
        if target_reads == 2:
            pre_discovery_refresh.set()
        return [] if target_reads <= 2 else list(targets)

    state.worktree_targets = worktree_targets
    watch = launch.AvailableWorkWatch(state, events_path=events)
    watch.start()
    try:
        assert watch.armed.wait(timeout=15.0) is True
        assert pre_discovery_refresh.wait(timeout=15.0) is True
        discovered = _target("new-lane", tmp_path)
        targets.append(discovered)
        # This publication lands before the watch loop discovers and arms the
        # new lane path. The task event wakes the old registration; the first
        # post-arm snapshot must still preserve the inbox identity.
        write_inbox_item(
            discovered.repo_root,
            "operator.txt",
            "published before lane registration",
        )
        events.write_text("1 task\n", encoding="utf-8")
        assert inbox_wake.wait(timeout=15.0) is True
    finally:
        watch.cancel()
        watch.join()

    assert len(wakes) == 1
    assert wakes[0].target_id == "new-lane"
    assert wakes[0].source is LifecycleWakeSource.INBOX


def test_reconciler_failure_is_visible_on_the_watcher(tmp_path):
    target = _target("failed-lane", tmp_path)
    state = _state(
        [target],
        reconcile=lambda wake: _outcome(
            wake,
            status=LifecycleOutcomeStatus.FAILED,
            detail="launch failed visibly",
        ),
    )
    watch = launch.AvailableWorkWatch(
        state,
        events_path=_events_file(tmp_path),
    )

    watch.start()
    watch.join()

    assert "launch failed visibly" in watch.error


def test_cancel_ends_the_watch(tmp_path):
    """Serve shutdown reclaims the thread out of its blocking wait."""
    watch = launch.AvailableWorkWatch(_state([]), events_path=_events_file(tmp_path))
    watch.start()
    try:
        assert watch.armed.wait(timeout=15.0) is True
    finally:
        watch.cancel()
        watch.join()

    assert watch.error == ""
    assert threading.active_count() >= 1


def test_available_work_watch_leak_guard_joins_the_owning_test_thread(
    tmp_path,
    _available_work_watch_leak_guard,
):
    watch = launch.AvailableWorkWatch(_state([]), events_path=_events_file(tmp_path))
    watch.start()
    assert watch.armed.wait(timeout=15.0) is True

    leaked = _available_work_watch_leak_guard()
    active_watch_threads = [
        thread.name
        for thread in threading.enumerate()
        if thread.name == launch.AVAILABLE_WORK_WATCH_THREAD_NAME and thread.is_alive()
    ]

    assert leaked == [launch.AVAILABLE_WORK_WATCH_THREAD_NAME]
    assert active_watch_threads == []
