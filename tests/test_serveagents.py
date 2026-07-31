"""Serve agent startup, operator-inbox wakes, and restart-refusal contracts."""

from __future__ import annotations

import json
import threading
from datetime import UTC, datetime, timedelta
from http import HTTPStatus
from types import SimpleNamespace

import pytest

from spice.agent import lifecycle
from spice.mail.inbox import (
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    compose_inbox_text,
    inbox_item_key,
    inbox_request_body,
    pending_inbox_count,
    write_inbox_item,
)
from spice.serve import agentapi, launch, lifecycle as serve_lifecycle, workroutes
from spice.serve.workroutes import work_tree_send_response_payload
from tests.test_servehelpers import (
    THREAD_A,
    _patch_agent_status,
    _repo,
    _retry_gate,
    _serve_state,
    _target,
)
from tests.test_taskgitsync import _advance_upstream, _repo_with_upstream, _run

# The race below parks a publication mid-flight on purpose. Each ordered step is
# waited on with the same short bound -- long enough that a loaded machine still
# reaches the next moment, short enough that a genuine deadlock fails the test
# rather than hanging it.
EXPLICIT_SEND_STEP_SECONDS = 5.0
RECONCILER_JOIN_SECONDS = 5.0
# The parked publication only has to outlive the assertions made while the race
# is open; this bound is the escape hatch for a test that stops early.
EXPLICIT_SEND_RELEASE_SECONDS = 15.0


def test_work_tree_send_deadletters_message_after_generic_ensure_failure(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)

    def fake_ensure(ensured_target, **kwargs):
        assert ensured_target == target
        return {
            "ok": False,
            "error": "Could not ensure agent: invalid config",
        }, HTTPStatus.INTERNAL_SERVER_ERROR

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

    payload, status = work_tree_send_response_payload(
        state,
        target,
        {"text": "inspect this failure"},
    )

    assert status == HTTPStatus.OK
    assert payload["ok"] is True
    assert payload["requestText"] == "inspect this failure"
    pending = payload["chrome"]["pendingInbox"]
    assert pending["authority"] == "inbox"
    assert pending["order"]["revision"] > 0
    assert pending["value"] == {"count": 0, "label": "0", "keys": []}
    assert payload["agentEnsure"]["ok"] is False
    assert payload["agentEnsure"]["error"] == "Could not ensure agent: invalid config"
    assert payload["agentEnsure"]["deadletteredInboxKey"]
    assert payload["agentEnsure"]["deadletterRequeueCommand"] == (
        "spice agent requeue-deadletter "
        f"{payload['agentEnsure']['deadletteredInboxKey']}"
    )
    assert collect_inbox_items(repo) == []
    deadletters = collect_deadlettered_inbox_items(repo)
    assert len(deadletters) == 1
    assert inbox_request_body(deadletters[0].text) == "inspect this failure"


def test_pending_inbox_ensure_ignores_automated_guidance(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    write_inbox_item(
        repo,
        "1jNJvRyn.txt",
        compose_inbox_text(body="automated maxim", priority="maxim", stop=False),
    )
    write_inbox_item(
        repo,
        "1jNJvRyp.txt",
        compose_inbox_text(
            body="automated review feedback", priority="review", stop=False
        ),
    )
    ensure_calls = 0
    retry_calls: list[tuple[str, float]] = []

    def fake_ensure(ensured_target, **kwargs):
        nonlocal ensure_calls
        ensure_calls += 1
        assert ensured_target == target
        return {"ok": True, "threadId": THREAD_A}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

    payload = agentapi.ensure_agent_for_pending_inbox(
        target,
        retry_due=lambda target_id, seconds: (
            retry_calls.append((target_id, seconds)) or True
        ),
        retry_seconds=0.0,
    )

    assert payload is None
    assert ensure_calls == 0
    assert retry_calls == []
    assert pending_inbox_count(repo) == 2


def test_pending_inbox_ensure_uses_first_operator_item_as_trigger(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    write_inbox_item(
        repo,
        "1jNJvRyn.txt",
        compose_inbox_text(body="automated maxim", priority="maxim", stop=False),
    )
    write_inbox_item(
        repo,
        "1jNJvRyp.txt",
        compose_inbox_text(
            body="automated review feedback", priority="review", stop=False
        ),
    )
    write_inbox_item(
        repo,
        "1jNJvRyq.txt",
        compose_inbox_text(body="operator steering", priority=None, stop=False),
    )

    def fake_ensure(ensured_target, **kwargs):
        assert ensured_target == target
        return {
            "ok": False,
            "error": "Could not ensure agent: invalid config",
        }, HTTPStatus.INTERNAL_SERVER_ERROR

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

    payload = agentapi.ensure_agent_for_pending_inbox(
        target,
        retry_due=_retry_gate(),
        retry_seconds=0.0,
    )

    assert payload["deadletteredInboxKey"] == "1jNJvRyq"
    assert [item.name for item in collect_inbox_items(repo)] == [
        "1jNJvRyn.txt",
        "1jNJvRyp.txt",
    ]
    assert [item.name for item in collect_deadlettered_inbox_items(repo)] == [
        "1jNJvRyq.txt"
    ]


@pytest.mark.parametrize("condition", ["dirty", "ahead", "diverged", "fetch-failed"])
def test_pending_inbox_ensure_starts_a_skipped_lane_without_deadletter(
    tmp_path, monkeypatch, condition
):
    repo = _repo_with_upstream(tmp_path)
    target = _target(repo)
    (repo / ".git" / "info" / "exclude").write_text(".spice/\n", encoding="utf-8")
    original_head = _run(repo, "git", "rev-parse", "HEAD").stdout.strip()
    if condition == "fetch-failed":
        _run(
            repo,
            "git",
            "remote",
            "set-url",
            "origin",
            str(tmp_path / "missing-remote.git"),
        )
    else:
        (repo / "local.txt").write_text("local work\n", encoding="utf-8")
        if condition in {"ahead", "diverged"}:
            _run(repo, "git", "add", "local.txt")
            _run(repo, "git", "commit", "-m", "local work")
            if condition == "diverged":
                _advance_upstream(tmp_path)
    write_inbox_item(
        repo,
        "1kLaunch.txt",
        compose_inbox_text(body="recover this lane", priority=None, stop=False),
    )
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    launch_notes: list[str] = []
    spawned: list[object] = []
    real_prepare = lifecycle.boundaries.fast_forward_if_safe

    def observed_prepare(repo_root):
        result = real_prepare(repo_root)
        launch_notes.extend(result.notes)
        return result

    def fake_supervisor(repo_root, **kwargs):
        spawned.append(SimpleNamespace(repo_root=repo_root, **kwargs))
        return SimpleNamespace(pid=4321)

    monkeypatch.setattr(lifecycle.boundaries, "fast_forward_if_safe", observed_prepare)
    monkeypatch.setattr(lifecycle, "ensure_origin_head", lambda _repo: None)
    monkeypatch.setattr(lifecycle, "spawn_agent_supervisor", fake_supervisor)
    monkeypatch.setattr(
        lifecycle, "require_supervisor_started", lambda *_args, **_kwargs: None
    )
    monkeypatch.setattr(
        lifecycle, "reap_process_when_done", lambda *_args, **_kwargs: None
    )

    payload = agentapi.ensure_agent_for_pending_inbox(
        target,
        retry_due=_retry_gate(),
        retry_seconds=0.0,
    )

    assert payload["ok"] is True
    assert payload["action"] == "start"
    assert launch_notes == [f"skipped:{condition}"]
    assert [item.name for item in collect_inbox_items(repo)] == ["1kLaunch.txt"]
    assert [(item.repo_root, item.action) for item in spawned] == [(repo, "start")]
    if condition == "fetch-failed":
        assert _run(repo, "git", "rev-parse", "HEAD").stdout.strip() == original_head
        assert _run(repo, "git", "status", "--porcelain").stdout.strip() == ""


def test_pending_inbox_ensure_stops_launching_after_rapid_death_storm(
    tmp_path, monkeypatch
):
    # Replays the 2026-07-17 spend-limit storm: every launch survives startup
    # and dies in under a second, pending operator items keep re-triggering
    # the ensure, and only the journal-backed refusal stops the loop.
    repo = _repo(tmp_path)
    target = _target(repo)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    write_inbox_item(
        repo,
        "1kDw6qsY.txt",
        compose_inbox_text(body="operator broadcast", priority=None, stop=False),
    )
    write_inbox_item(
        repo,
        "1kDw6qsZ.txt",
        compose_inbox_text(body="operator follow-up", priority=None, stop=False),
    )
    launches = 0

    def fake_start_agent(repo_root, **_kwargs):
        nonlocal launches
        launches += 1
        lifecycle.record_launch_outcome(
            repo_root,
            {
                "lifetime_seconds": 0.751,
                "exit_code": 0,
                "ended_at": lifecycle.utc_now(),
            },
        )
        return repo_root / "launch.log"

    monkeypatch.setattr(lifecycle, "start_agent", fake_start_agent)
    monkeypatch.setattr(lifecycle, "ensure_origin_head", lambda *_args: None)

    payloads = [
        agentapi.ensure_agent_for_pending_inbox(
            target, retry_due=_retry_gate(), retry_seconds=0.0
        )
        for _ in range(6)
    ]

    assert launches == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD
    assert [payload["action"] for payload in payloads[:3]] == [
        "start",
        "start",
        "start",
    ]
    refused = payloads[3]
    assert refused["failure"] == lifecycle.AGENT_FAILURE_RESTART_REFUSED
    assert (
        refused["restartRefusal"]["consecutive_rapid_deaths"]
        == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD
    )
    assert refused["deadletteredInboxKeys"] == [
        "1kDw6qsY",
        "1kDw6qsZ",
    ]
    assert payloads[4:] == [None, None]
    assert pending_inbox_count(repo) == 0
    # Deadletter listings read newest-first, like the archive preview.
    assert [item.name for item in collect_deadlettered_inbox_items(repo)] == [
        "1kDw6qsZ.txt",
        "1kDw6qsY.txt",
    ]

    # A fresh operator send is an explicit action: exactly one new attempt,
    # journal intact — and its rapid death re-arms the refusal.
    write_inbox_item(
        repo,
        "1kDwBqjF.txt",
        compose_inbox_text(body="operator retry", priority=None, stop=False),
    )
    granted = agentapi.ensure_agent_for_pending_inbox(
        target, retry_due=_retry_gate(), retry_seconds=0.0, automatic=False
    )
    reopened = agentapi.ensure_agent_for_pending_inbox(
        target, retry_due=_retry_gate(), retry_seconds=0.0
    )

    assert granted["action"] == "start"
    assert launches == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD + 1
    assert reopened["failure"] == lifecycle.AGENT_FAILURE_RESTART_REFUSED
    assert reopened["deadletteredInboxKeys"] == ["1kDwBqjF"]
    assert pending_inbox_count(repo) == 0
    outcomes = lifecycle.read_launch_outcomes(repo)
    assert len(outcomes) == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD + 1


def _age_recorded_deaths(repo, seconds: float) -> None:
    """Rewrite every recorded death as if it happened `seconds` ago.

    Moves the journal rather than the clock, so the production call path keeps
    reading real `time.time()` and the lapse under test is the one a lane
    actually gets when the hold runs out on its own.
    """
    path = lifecycle.launch_outcomes_path(repo)
    outcomes = json.loads(path.read_text(encoding="utf-8"))
    aged = (datetime.now(UTC) - timedelta(seconds=seconds)).isoformat()
    for outcome in outcomes:
        outcome["ended_at"] = aged
    path.write_text(json.dumps(outcomes), encoding="utf-8")


def _storm_launcher(monkeypatch, counter: list[int]):
    """Every launch reproduces one 2026-07-17 storm death: 0.751s, exit 0, idle.

    The activity counts are written out rather than left absent on purpose. A
    record missing them is treated as a death too, but by the guard's fallback
    for partial records -- so omitting them would pin that fallback instead of
    the storm, and keep passing if the storm's own shape stopped counting.
    """

    def fake_start_agent(repo_root, **_kwargs):
        counter[0] += 1
        lifecycle.record_launch_outcome(
            repo_root,
            {
                "lifetime_seconds": 0.751,
                "exit_code": 0,
                "assistant_messages": 0,
                "tool_calls": 0,
                "ended_at": lifecycle.utc_now(),
            },
        )
        return repo_root / "launch.log"

    monkeypatch.setattr(lifecycle, "start_agent", fake_start_agent)
    monkeypatch.setattr(lifecycle, "ensure_origin_head", lambda *_args: None)


def test_lapsed_refusal_returns_parked_operator_steering_to_the_inbox(
    tmp_path, monkeypatch
):
    # The parking is correct while the hold is armed and terminal afterwards:
    # nothing re-queues a deadlettered item, so a directed operator ask parked
    # here reads exactly like a delivered one on every surface an operator has.
    repo = _repo(tmp_path)
    target = _target(repo)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    write_inbox_item(
        repo,
        "1kJ3WK40.txt",
        compose_inbox_text(body="operator broadcast", priority=None, stop=False),
    )
    write_inbox_item(
        repo,
        "1kJ3Wz71.txt",
        compose_inbox_text(body="triage the oops items", priority=None, stop=False),
    )
    launches = [0]
    _storm_launcher(monkeypatch, launches)

    for _ in range(4):
        agentapi.ensure_agent_for_pending_inbox(
            target, retry_due=_retry_gate(), retry_seconds=0.0
        )

    assert launches[0] == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD
    assert pending_inbox_count(repo) == 0
    assert sorted(item.name for item in collect_deadlettered_inbox_items(repo)) == [
        "1kJ3WK40.txt",
        "1kJ3Wz71.txt",
    ]

    # A pass while the hold is still armed must restore nothing: the wake
    # condition the parking removed is exactly what the refusal is holding off.
    assert agentapi.requeue_lapsed_refusal_parking(target) == []
    assert pending_inbox_count(repo) == 0

    # The window is read from production because tracking it is the point --
    # the assertion is about redelivery, not about the interval's value.
    _age_recorded_deaths(repo, lifecycle.RAPID_DEATH_REFUSAL_WINDOW_SECONDS + 60)

    # Driven through the production path, not the helper: the restore is only
    # worth anything if an ordinary pass performs it without an operator.
    reopened = agentapi.ensure_agent_for_pending_inbox(
        target, retry_due=_retry_gate(), retry_seconds=0.0
    )

    assert reopened["action"] == "start"
    assert launches[0] == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD + 1
    assert pending_inbox_count(repo) == 2
    assert collect_deadlettered_inbox_items(repo) == []
    assert sorted(
        inbox_request_body(item.text) for item in collect_inbox_items(repo)
    ) == ["operator broadcast", "triage the oops items"]
    # The directed ask survives with its text intact, which is the whole point:
    # lane j's 1kJ3Wz71 was a triage instruction no other lane received.


def test_lapsed_refusal_redelivery_costs_one_launch_per_window(tmp_path, monkeypatch):
    # Redelivery must not re-open the storm it was parked to stop. The bound is
    # structural: the restore reads empty until the hold lapses, so a lane that
    # keeps dying young pays one launch per window instead of one per pass.
    repo = _repo(tmp_path)
    target = _target(repo)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    write_inbox_item(
        repo,
        "1kJ3c29H.txt",
        compose_inbox_text(body="operator steering", priority=None, stop=False),
    )
    launches = [0]
    _storm_launcher(monkeypatch, launches)

    for _ in range(4):
        agentapi.ensure_agent_for_pending_inbox(
            target, retry_due=_retry_gate(), retry_seconds=0.0
        )
    armed_launches = launches[0]

    assert armed_launches == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD
    assert [item.name for item in collect_deadlettered_inbox_items(repo)] == [
        "1kJ3c29H.txt"
    ]

    # Twenty passes under the armed hold: no restore, no launch, no growth in
    # the journal. This is the storm case, replayed against the new path.
    for _ in range(20):
        agentapi.ensure_agent_for_pending_inbox(
            target, retry_due=_retry_gate(), retry_seconds=0.0
        )

    assert launches[0] == armed_launches
    assert pending_inbox_count(repo) == 0

    # One lapse buys exactly one attempt, whose rapid death re-arms the hold and
    # parks the item again -- ready for the next window, not the next pass.
    _age_recorded_deaths(repo, lifecycle.RAPID_DEATH_REFUSAL_WINDOW_SECONDS + 60)
    reopened = agentapi.ensure_agent_for_pending_inbox(
        target, retry_due=_retry_gate(), retry_seconds=0.0
    )

    assert reopened["action"] == "start"
    assert launches[0] == armed_launches + 1

    parked_again = agentapi.ensure_agent_for_pending_inbox(
        target, retry_due=_retry_gate(), retry_seconds=0.0
    )

    assert parked_again["failure"] == lifecycle.AGENT_FAILURE_RESTART_REFUSED
    assert parked_again["deadletteredInboxKeys"] == ["1kJ3c29H"]
    assert launches[0] == armed_launches + 1
    assert agentapi.requeue_lapsed_refusal_parking(target) == []


class _ExplicitSendRace:
    """The observable moments of one explicit send racing a background wake.

    Held apart from the narrative below because the race is the subject:
    publication pauses on demand, every launch attempt is recorded with the inbox
    it saw, and the launch boundary reports when a decision other than the
    publishing route's own reaches it.
    """

    def __init__(self, repo, target) -> None:
        self.repo = repo
        self.target = target
        self.published = threading.Event()
        self.release_direct_send = threading.Event()
        self.direct_finished = threading.Event()
        self.background_submitted = threading.Event()
        self.background_finished = threading.Event()
        self.agent_started = threading.Event()
        self.attempts: list[bool] = []
        self.attempt_pending_counts: list[int] = []
        self.direct_result: dict[str, object] = {}

    def install(self, monkeypatch, state) -> None:
        real_submit = workroutes.submit_steering_message
        real_submit_wake = state.submit_lifecycle_wake
        race = self

        def pause_after_publication(**kwargs):
            sent = real_submit(**kwargs)
            race.published.set()
            race.release_direct_send.wait(timeout=EXPLICIT_SEND_RELEASE_SECONDS)
            return sent

        def observe_background_submission(wake):
            future = real_submit_wake(wake)
            race.background_submitted.set()
            return future

        def status_after_explicit_start(*_args, **_kwargs):
            return SimpleNamespace(running=race.agent_started.is_set())

        monkeypatch.setattr(
            workroutes, "submit_steering_message", pause_after_publication
        )
        monkeypatch.setattr(
            state,
            "submit_lifecycle_wake",
            observe_background_submission,
        )
        monkeypatch.setattr(agentapi, "agent_status", status_after_explicit_start)
        monkeypatch.setattr(agentapi, "agent_ensure_response_payload", self.ensure)

    def ensure(self, ensured_target, **kwargs):
        """Refuse every automatic restart and honor the one explicit grant."""
        assert ensured_target == self.target
        automatic = bool(kwargs["automatic"])
        self.attempts.append(automatic)
        self.attempt_pending_counts.append(pending_inbox_count(self.repo))
        if automatic:
            return {
                "ok": False,
                "failure": lifecycle.AGENT_FAILURE_RESTART_REFUSED,
                "restartRefusal": {"reason": "rapid-death"},
            }, HTTPStatus.TOO_MANY_REQUESTS
        self.agent_started.set()
        return {
            "ok": True,
            "action": "start",
            "threadId": THREAD_A,
        }, HTTPStatus.OK


def test_explicit_send_keeps_its_restart_grant_during_background_evaluation(
    tmp_path, monkeypatch
):
    """An already-awake watcher cannot consume a fresh explicit UI grant."""
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    race = _ExplicitSendRace(repo, target)
    race.install(monkeypatch, state)
    reconciler = state.lifecycle_reconciler
    assert reconciler is not None

    def send_directly() -> None:
        try:
            race.direct_result["response"] = work_tree_send_response_payload(
                state, target, {"text": "use my explicit restart grant"}
            )
        except BaseException as exc:
            race.direct_result["error"] = exc
        finally:
            race.direct_finished.set()

    def evaluate_in_background() -> None:
        watch = launch.AvailableWorkWatch(state, events_path=tmp_path / "task-events")
        watch.evaluate(
            (
                serve_lifecycle.AutomaticLifecycleWake(
                    target.id,
                    serve_lifecycle.LifecycleWakeSource.INBOX,
                    "explicit-send-race",
                ),
            )
        )
        race.background_finished.set()

    direct_thread = threading.Thread(target=send_directly, daemon=True)
    direct_thread.start()
    assert race.published.wait(timeout=EXPLICIT_SEND_STEP_SECONDS) is True
    background_thread = threading.Thread(
        target=evaluate_in_background,
        name="background-launch-evaluation",
        daemon=True,
    )
    background_thread.start()
    # The watcher has handed its real Future to the reconciler but its
    # same-target decision is blocked behind the route's publication boundary.
    # Releasing the route reserves this send's grant as the same target-scoped
    # step, so the queued automatic decision observes a reservation only the
    # send's own intent may spend.
    assert race.background_submitted.wait(timeout=EXPLICIT_SEND_STEP_SECONDS) is True
    assert race.background_finished.is_set() is False
    race.release_direct_send.set()
    assert race.direct_finished.wait(timeout=EXPLICIT_SEND_RELEASE_SECONDS) is True
    assert race.background_finished.wait(timeout=EXPLICIT_SEND_RELEASE_SECONDS) is True
    direct_thread.join()
    background_thread.join()
    error = race.direct_result.get("error")
    if isinstance(error, BaseException):
        raise error

    response, status = race.direct_result["response"]
    assert status == HTTPStatus.OK
    assert response["agentEnsure"]["action"] == "start"
    # Exactly one launch decision, it is the send's own, and it ran against an
    # inbox that already holds the item -- publication precedes the attempt that
    # the item justifies, never the other way around.
    assert race.attempts == [False]
    assert race.attempt_pending_counts == [1]
    assert race.agent_started.is_set() is True
    assert race.background_submitted.is_set() is True
    assert race.background_finished.is_set() is True
    reconciler.cancel()
    assert reconciler.join(timeout=RECONCILER_JOIN_SECONDS) is True
    assert [inbox_item_key(item.name) for item in collect_inbox_items(repo)] == [
        response["key"]
    ]


def test_agent_status_payload_surfaces_restart_refusal(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    for _ in range(lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD):
        lifecycle.record_launch_outcome(
            repo,
            {
                "lifetime_seconds": 0.751,
                "exit_code": 0,
                "ended_at": lifecycle.utc_now(),
            },
        )

    payload = agentapi.agent_status_payload(target)

    assert (
        payload["restartRefusal"]["consecutive_rapid_deaths"]
        == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD
    )
    assert payload["launchable"] is True


def test_agent_status_payload_distinguishes_starting_and_startup_stalled(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    status = SimpleNamespace(
        repo_root=repo,
        process_status="starting",
        pid=123,
        process_group_id=123,
        thread_id=THREAD_A,
        model="gpt-test",
        reasoning_effort="high",
        ready_at="",
        startup_failure="",
        running=True,
        command=("codex", "exec", "--cd", str(repo)),
    )
    monkeypatch.setattr(agentapi, "agent_status", lambda _repo: status)

    starting = agentapi.agent_status_payload(target)
    assert {
        "status": starting["status"],
        "launchable": starting["launchable"],
        "startupFailure": starting["startupFailure"],
    } == {
        "status": "starting",
        "launchable": False,
        "startupFailure": "",
    }

    status.process_status = lifecycle.AGENT_FAILURE_STARTUP_STALLED
    status.pid = 0
    status.process_group_id = 0
    status.startup_failure = "agent startup stalled after 120s"
    status.running = False
    stalled = agentapi.agent_status_payload(target)
    assert {
        "status": stalled["status"],
        "launchable": stalled["launchable"],
        "startupFailure": stalled["startupFailure"],
    } == {
        "status": "startup-stalled",
        "launchable": True,
        "startupFailure": "agent startup stalled after 120s",
    }
