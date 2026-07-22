"""The server-owned wake loop that starts stopped Drain lanes."""

from __future__ import annotations

import threading
from pathlib import Path
from types import SimpleNamespace

from spice.mail.inbox import pending_operator_inbox_items, write_inbox_item
from spice.serve import launch, livebuswatch
from spice.serve.worktree import inventory
from spice.serve.worktree.target import WorktreeTarget

# Spelled out here rather than read from the scheduler: reading the production
# constant would keep these bounds green at any interval, and three minutes is
# the property under test. The remainder is what the countdown has left one
# minute into a candidate's wait.
LONE_TASK_ESCAPE_SECONDS = 3.0 * 60.0
ESCAPE_REMAINING_AFTER_ONE_MINUTE = 2.0 * 60.0
# A deadline already behind us: the lane declining it has been waiting past its
# escape, so the only thing left to decide is how soon to act.
ESCAPE_ALREADY_EXPIRED_SECONDS = -20.0


def _target(name: str, tmp_path: Path) -> WorktreeTarget:
    repo = tmp_path / name
    repo.mkdir(exist_ok=True)
    return WorktreeTarget(id=name, repo_root=repo, name=name, branch="main")


def _state(targets: list[WorktreeTarget]) -> SimpleNamespace:
    return SimpleNamespace(
        observer_mode=False,
        worktree_targets=lambda: list(targets),
        team_store=SimpleNamespace(global_fast_mode_enabled=lambda: False),
        pending_agent_ensure_attempts={},
    )


def _events_file(tmp_path: Path) -> Path:
    events = tmp_path / "events"
    events.write_text("0 bootstrap\n", encoding="utf-8")
    return events


def _patch_lanes(
    monkeypatch,
    lifetimes: dict[str, str],
    ensured: list[tuple],
    *,
    declined: dict | None = None,
) -> None:
    monkeypatch.setattr(
        launch,
        "resolve_thread_id_for_target",
        lambda _state, target: f"thread-{target.id}",
    )
    monkeypatch.setattr(
        inventory,
        "team_actor_for_target",
        lambda _store, _target, _thread: "",
    )
    monkeypatch.setattr(
        inventory,
        "ensure_agent_for_pending_inbox",
        lambda _target, **_kwargs: None,
    )
    monkeypatch.setattr(
        inventory,
        "team_facts_for_target",
        lambda _store, target, _thread: {"lifetime": lifetimes[target.id]},
    )

    def ensure(target, **kwargs):
        ensured.append((target.id, kwargs["thread_id"]))
        return declined

    monkeypatch.setattr(inventory, "ensure_agent_for_available_work", ensure)


def _capacity_decline(retry_after_seconds: float) -> dict:
    """The refusal a lane returns while its oldest candidate is still waiting."""
    return {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "capacity",
        "retryAfterSeconds": retry_after_seconds,
    }


def test_observer_mode_runs_no_available_work_watch():
    """Observer mode owns no lanes, so it has nothing to start."""
    watch = launch.start_available_work_watch(SimpleNamespace(observer_mode=True))

    assert watch is None


def test_evaluate_offers_work_to_drain_lanes_only(tmp_path, monkeypatch):
    """A lane whose team drains the board is the only lane this may start."""
    drain = _target("drain", tmp_path)
    burst = _target("burst", tmp_path)
    ensured: list[tuple] = []
    _patch_lanes(monkeypatch, {"drain": "Drain", "burst": "Burst"}, ensured)
    watch = launch.AvailableWorkWatch(
        _state([drain, burst]), events_path=_events_file(tmp_path)
    )

    remaining = watch.evaluate()

    assert ensured == [("drain", "thread-drain")]
    # No lane declined for capacity, so nothing is waiting on an age to arrive
    # and the next look is a whole interval out.
    assert remaining == LONE_TASK_ESCAPE_SECONDS


def test_evaluate_shortens_its_bound_to_the_oldest_candidates_escape(
    tmp_path, monkeypatch
):
    """The next wake lands when the oldest candidate reaches three minutes.

    The lane that declined already read its rows under the claim lock, so the
    countdown rides back out with the refusal rather than costing this thread a
    second look at the same board.
    """
    drain = _target("drain", tmp_path)
    ensured: list[tuple] = []
    _patch_lanes(
        monkeypatch,
        {"drain": "Drain"},
        ensured,
        declined=_capacity_decline(ESCAPE_REMAINING_AFTER_ONE_MINUTE),
    )
    watch = launch.AvailableWorkWatch(
        _state([drain]), events_path=_events_file(tmp_path)
    )

    remaining = watch.evaluate()

    assert remaining == ESCAPE_REMAINING_AFTER_ONE_MINUTE


def test_evaluate_keeps_a_floor_under_an_expired_escape(tmp_path, monkeypatch):
    """A candidate already past its escape still leaves room to act on it."""
    drain = _target("drain", tmp_path)
    ensured: list[tuple] = []
    _patch_lanes(
        monkeypatch,
        {"drain": "Drain"},
        ensured,
        declined=_capacity_decline(ESCAPE_ALREADY_EXPIRED_SECONDS),
    )
    watch = launch.AvailableWorkWatch(
        _state([drain]), events_path=_events_file(tmp_path)
    )

    remaining = watch.evaluate()

    assert remaining == launch.AVAILABLE_WORK_WATCH_MIN_SECONDS


def test_watch_looks_again_when_its_deadline_arrives_with_no_board_change(
    tmp_path, monkeypatch
):
    """The lone-task escape needs no second task and no client refresh."""
    watch = launch.AvailableWorkWatch(_state([]), events_path=_events_file(tmp_path))
    looked_twice = threading.Event()
    looks: list[int] = []

    def evaluate() -> float:
        looks.append(len(looks))
        if len(looks) >= 2:
            looked_twice.set()
        # A deadline just out of reach of the first wait; nothing will be
        # written to the event token for the rest of this test.
        return 0.05

    monkeypatch.setattr(watch, "evaluate", evaluate)
    watch.start()
    try:
        assert looked_twice.wait(timeout=15.0) is True
    finally:
        watch.cancel()
        watch.join()

    assert len(looks) >= 2


def test_watch_looks_again_when_the_task_board_changes(tmp_path, monkeypatch):
    """A task entering the board wakes the decision without waiting out a deadline."""
    events = _events_file(tmp_path)
    watch = launch.AvailableWorkWatch(_state([]), events_path=events)
    looked_twice = threading.Event()
    looks: list[int] = []

    def evaluate() -> float:
        looks.append(len(looks))
        if len(looks) >= 2:
            looked_twice.set()
        # Far past this test: only a board change can produce a second look.
        return 3600.0

    monkeypatch.setattr(watch, "evaluate", evaluate)
    watch.start()
    try:
        assert watch.armed.wait(timeout=15.0) is True
        events.write_text("1 task\n", encoding="utf-8")
        assert looked_twice.wait(timeout=15.0) is True
    finally:
        watch.cancel()
        watch.join()

    assert len(looks) >= 2


def test_watch_looks_again_when_pending_inbox_is_published(tmp_path, monkeypatch):
    """A non-HTTP publish starts an off lane without an inventory request."""
    target = _target("off-lane", tmp_path)
    _patch_lanes(monkeypatch, {target.id: "Burst"}, [])
    watch = launch.AvailableWorkWatch(
        _state([target]), events_path=_events_file(tmp_path)
    )
    launch_attempted = threading.Event()
    checks: list[int] = []

    def ensure_pending(_target, **_kwargs):
        checks.append(len(checks))
        if pending_operator_inbox_items(target.repo_root):
            launch_attempted.set()
            return {}
        return None

    monkeypatch.setattr(inventory, "ensure_agent_for_pending_inbox", ensure_pending)
    watch.start()
    try:
        assert watch.armed.wait(timeout=15.0) is True
        write_inbox_item(target.repo_root, "operator.txt", "start this lane")
        assert launch_attempted.wait(timeout=2.0) is True
    finally:
        watch.cancel()
        watch.join()

    assert len(checks) >= 2


def test_watch_preserves_a_task_event_written_during_scheduler_evaluation(
    tmp_path, monkeypatch
):
    """Native registration stays live while the scheduler reads the board."""
    events = _events_file(tmp_path)
    watch = launch.AvailableWorkWatch(_state([]), events_path=events)
    looked_twice = threading.Event()
    looks: list[int] = []

    def evaluate() -> float:
        looks.append(len(looks))
        if len(looks) == 1:
            # This is the old observe-before-arm gap: evaluation has begun,
            # but the subsequent wait has not.
            events.write_text("1 changed-during-evaluation\n", encoding="utf-8")
        if len(looks) >= 2:
            looked_twice.set()
        return 3600.0

    monkeypatch.setattr(watch, "evaluate", evaluate)
    watch.start()
    try:
        assert looked_twice.wait(timeout=15.0) is True
    finally:
        watch.cancel()
        watch.join()

    assert len(looks) >= 2


def test_watchfiles_stays_armed_while_scheduler_evaluates(tmp_path, monkeypatch):
    """The non-kqueue backend uses one native iterator across evaluations."""
    events = _events_file(tmp_path)
    watch = launch.AvailableWorkWatch(_state([]), events_path=events)
    looked_twice = threading.Event()
    native_calls: list[dict] = []
    looks: list[int] = []

    def native_watch(*paths, **options):
        native_calls.append({"paths": paths, **options})
        yield set()  # native registration is now live
        yield {(1, str(events))}  # the write made by the first evaluation
        options["stop_event"].wait(timeout=15.0)

    def evaluate() -> float:
        looks.append(len(looks))
        if len(looks) == 1:
            events.write_text("1 watchfiles-evaluation\n", encoding="utf-8")
        if len(looks) >= 2:
            looked_twice.set()
        return 3600.0

    monkeypatch.setattr(livebuswatch, "_HAVE_KQUEUE", False)
    monkeypatch.setattr(
        livebuswatch,
        "import_module",
        lambda name: (
            SimpleNamespace(watch=native_watch) if name == "watchfiles" else None
        ),
    )
    monkeypatch.setattr(watch, "evaluate", evaluate)
    watch.start()
    try:
        assert looked_twice.wait(timeout=15.0) is True
    finally:
        watch.cancel()
        watch.join()

    assert len(native_calls) == 1
    assert len(looks) >= 2


def test_cancel_ends_the_watch(tmp_path, monkeypatch):
    """Serve shutdown reclaims the thread out of its blocking wait."""
    watch = launch.AvailableWorkWatch(_state([]), events_path=_events_file(tmp_path))
    monkeypatch.setattr(watch, "evaluate", lambda: 3600.0)
    watch.start()
    assert watch.armed.wait(timeout=15.0) is True

    watch.cancel()
    watch.join()

    assert watch.error == ""
    assert threading.active_count() >= 1
