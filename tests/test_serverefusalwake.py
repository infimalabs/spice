"""The wake that returns steering a restart refusal parked.

Parking empties the lane's inbox, and an empty inbox is not a wake. These cover
the gap that leaves: whether a lane nobody else disturbs ever looks again.

The proofs drive serve's own wake path -- ``observe_events`` reading the real
event files, and ``evaluate`` submitting through the reconciler to the lifecycle
authority. Only the setup that reaches the parked state calls an ensure
directly, because that state is the precondition under test rather than the
claim being made about it.
"""

from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta
from pathlib import Path

from spice.agent import lifecycle
from spice.agent.launchhistory import (
    lapsed_refusal_parked_keys,
    refusal_parking_retry_seconds,
)
from spice.mail.inbox import (
    collect_deadlettered_inbox_items,
    compose_inbox_text,
    pending_inbox_count,
    write_inbox_item,
)
from spice.serve import agentapi, launch
from spice.serve.lifecycle import AutomaticLifecycleWake, LifecycleWakeSource
from spice.serve.worktree.target import WorktreeTarget
from tests.test_servehelpers import (
    THREAD_A,
    _patch_agent_status,
    _repo,
    _retry_gate,
    _serve_state,
    _target,
)

PARKED_KEY = "1kJ3Wz71"
# The task event file carries a digit token; only its movement is a board wake.
QUIET_BOARD_TOKEN = "7"
MOVED_BOARD_TOKEN = "8"
# Far enough past the hold that a lapse cannot be confused with a clock edge.
LAPSE_OVERSHOOT_SECONDS = 60.0
# One 2026-07-17 spend-limit-storm death: clean exit, no work, gone in under a second.
STORM_LIFETIME_SECONDS = 0.751
# Enough passes to show the bound holds across wakes, not just on the first one.
REPEATED_WAKE_COUNT = 3


class _Lane:
    """One discoverable lane plus the watch and counters these cases read."""

    def __init__(
        self,
        repo: Path,
        target: WorktreeTarget,
        watch: launch.AvailableWorkWatch,
        events_path: Path,
        launches: list[int],
    ) -> None:
        self.repo = repo
        self.target = target
        self.watch = watch
        self.events_path = events_path
        self.launches = launches

    def observe(self) -> tuple[AutomaticLifecycleWake, ...]:
        return self.watch.observe_events([self.target])

    def reconcile(self) -> None:
        """Drive one full wake through the reconciler, as the watch loop does."""
        self.watch.evaluate(
            (
                AutomaticLifecycleWake(
                    target_id=self.target.id,
                    source=LifecycleWakeSource.INBOX,
                    source_identity=QUIET_BOARD_TOKEN,
                ),
            )
        )

    def move_board(self) -> None:
        self.events_path.write_text(MOVED_BOARD_TOKEN, encoding="utf-8")


def _lane(monkeypatch, tmp_path) -> _Lane:
    """A real lane whose every launch reproduces one storm death."""
    repo = _repo(tmp_path)
    target = _target(repo)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    launches = [0]

    def fake_start_agent(repo_root, **_kwargs):
        launches[0] += 1
        lifecycle.record_launch_outcome(
            repo_root,
            {
                "lifetime_seconds": STORM_LIFETIME_SECONDS,
                "exit_code": 0,
                "assistant_messages": 0,
                "tool_calls": 0,
                "ended_at": lifecycle.utc_now(),
            },
        )
        return repo_root / "launch.log"

    monkeypatch.setattr(lifecycle, "start_agent", fake_start_agent)
    monkeypatch.setattr(lifecycle, "ensure_origin_head", lambda *_args: None)

    events_path = tmp_path / "task-events"
    events_path.write_text(QUIET_BOARD_TOKEN, encoding="utf-8")
    state = _serve_state(tmp_path, target)
    watch = launch.AvailableWorkWatch(state, events_path=events_path)
    return _Lane(repo, target, watch, events_path, launches)


def _send_steering(lane: _Lane) -> None:
    """Publish the one operator item every case here is about."""
    write_inbox_item(
        lane.repo,
        f"{PARKED_KEY}.txt",
        compose_inbox_text(body="triage the oops items", priority=None, stop=False),
    )


def _park_the_steering(lane: _Lane) -> None:
    """Reach the state a refused ensure leaves behind: held item, empty inbox.

    Setup, not proof. The ensure is called here because repeated launches are
    what arm the refusal, and the authority's own attempt gate would throttle
    them apart.
    """
    gate = _retry_gate()
    for _ in range(lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD + 1):
        agentapi.ensure_agent_for_pending_inbox(
            lane.target, retry_due=gate, retry_seconds=0.0
        )
    assert [item.name for item in collect_deadlettered_inbox_items(lane.repo)] == [
        f"{PARKED_KEY}.txt"
    ]


def _age_recorded_deaths(repo: Path, seconds: float) -> None:
    """Rewrite every recorded death as if it happened ``seconds`` ago."""
    path = lifecycle.launch_outcomes_path(repo)
    outcomes = json.loads(path.read_text(encoding="utf-8"))
    aged = (datetime.now(UTC) - timedelta(seconds=seconds)).isoformat()
    for outcome in outcomes:
        outcome["ended_at"] = aged
    path.write_text(json.dumps(outcomes), encoding="utf-8")


def test_quiet_lane_produces_no_wake_once_parking_empties_its_inbox(
    monkeypatch, tmp_path
):
    """The event stream goes silent exactly when the restore needs it.

    Parking is itself an inbox write, so it emits one last wake -- and that wake
    is spent while the hold is still armed, on a pass that now finds nothing
    pending. After it the inbox never moves again, so the lane's own event
    stream is finished, and an untouched board leaves the task branch nothing to
    report either. Whatever returns the steering later, it cannot be an event
    this lane generates.
    """
    lane = _lane(monkeypatch, tmp_path)
    _send_steering(lane)
    pending_wakes = lane.observe()

    _park_the_steering(lane)
    parking_wakes = lane.observe()

    assert [wake.source for wake in pending_wakes] == [LifecycleWakeSource.INBOX]
    assert [wake.source for wake in parking_wakes] == [LifecycleWakeSource.INBOX]
    assert pending_inbox_count(lane.repo) == 0
    assert lane.observe() == ()


def test_unrelated_board_traffic_is_what_masks_the_silent_lane(monkeypatch, tmp_path):
    """A peer's board write wakes the lane, which is the wrong thing to rely on.

    Same parked lane, same empty inbox; only another lane's task activity
    intervenes. That it alone produces a wake is why the gap hides in a busy
    fleet and strands a quiet one.
    """
    lane = _lane(monkeypatch, tmp_path)
    _send_steering(lane)
    _park_the_steering(lane)
    lane.observe()
    quiet_wakes = lane.observe()

    lane.move_board()
    busy_wakes = lane.observe()

    assert quiet_wakes != busy_wakes
    assert [wake.source for wake in busy_wakes] == [LifecycleWakeSource.TASK]


def test_parked_lane_keeps_a_scheduled_timer_instead_of_going_dark(
    monkeypatch, tmp_path
):
    """A lane holding parked steering stays on the watch loop's schedule.

    The loop blocks on ``next_timer_timeout()``, so a lane with no timer waits
    on file events that will never come. Reconciling a parked lane leaves the
    remaining hold as that timeout, which is what turns the lapse itself into
    the wake.
    """
    lane = _lane(monkeypatch, tmp_path)
    _send_steering(lane)
    _park_the_steering(lane)

    lane.reconcile()

    assert refusal_parking_retry_seconds(lane.repo) > 0.0
    assert lane.watch.next_timer_timeout() is not None


def test_a_lane_with_nothing_parked_schedules_nothing(monkeypatch, tmp_path):
    """The schedule is owed to parked steering, not handed to every idle lane.

    Without this the change would keep every quiet lane permanently timed, so
    the passing case above would say nothing about parking at all.
    """
    lane = _lane(monkeypatch, tmp_path)

    lane.reconcile()

    assert refusal_parking_retry_seconds(lane.repo) is None
    assert lane.watch.next_timer_timeout() is None


def test_lapsed_hold_returns_the_steering_on_the_wake_it_scheduled(
    monkeypatch, tmp_path
):
    """The timer the lane kept is the one that hands its steering back.

    Reaching zero is the whole point: the restore runs off the lapse rather than
    off traffic from another lane, and the operator's ask returns to the inbox
    it was taken from.
    """
    lane = _lane(monkeypatch, tmp_path)
    _send_steering(lane)
    _park_the_steering(lane)
    assert pending_inbox_count(lane.repo) == 0

    _age_recorded_deaths(
        lane.repo,
        lifecycle.RAPID_DEATH_REFUSAL_WINDOW_SECONDS + LAPSE_OVERSHOOT_SECONDS,
    )
    assert refusal_parking_retry_seconds(lane.repo) == 0.0
    lane.reconcile()

    assert pending_inbox_count(lane.repo) == 1
    assert collect_deadlettered_inbox_items(lane.repo) == []


def test_an_armed_hold_still_withholds_the_wake_it_would_answer(monkeypatch, tmp_path):
    """The storm bound survives: the schedule waits out the window it names.

    A lane that keeps dying young holds its steering for the remaining window
    rather than retrying on every pass, so repeated wakes buy no extra launches
    and the item stays parked until the hold actually lapses.
    """
    lane = _lane(monkeypatch, tmp_path)
    _send_steering(lane)
    _park_the_steering(lane)
    spent = lane.launches[0]

    for _ in range(REPEATED_WAKE_COUNT):
        lane.reconcile()

    assert lane.launches[0] == spent
    assert refusal_parking_retry_seconds(lane.repo) > 0.0
    assert lapsed_refusal_parked_keys(lane.repo) == []
