"""Serve's automatic expansion onto available work: selection, settle, capacity."""

from __future__ import annotations

import threading
from datetime import UTC, datetime, timedelta
from http import HTTPStatus
from types import SimpleNamespace

import pytest

from spice.agent import lifecycle
from spice.serve import agentapi
from spice.serve.payload import wire
from spice.tasks import identity
from spice.tasks.claimstate import ClaimReleaseResult
from tests.test_servehelpers import (
    THREAD_A,
    _patch_agent_status,
    _repo,
    _retry_gate,
    _target,
)

# Spelled out here rather than read from `agentapi`: reading the production
# constant would keep these assertions green at any interval, and three minutes
# is the property under test. The remainder is what the watcher's countdown has
# left one minute into a candidate's wait.
LONE_TASK_ESCAPE_SECONDS = 3.0 * 60.0
ESCAPE_REMAINING_AFTER_ONE_MINUTE = 2.0 * 60.0
# The tolerance for a countdown measured against the real clock: these rows are
# stamped moments before the call that reads them.
ESCAPE_COUNTDOWN_TOLERANCE_SECONDS = 5.0
# Likewise literal: the shortest interval between two launch attempts for one
# lane, which is what keeps a launch that dies on arrival from being retried at
# the speed of the board it keeps re-dirtying.
CAPACITY_RETRY_SECONDS = 5.0
# The short settle a burst that clears the count threshold must let its chosen
# row sit READY before a new lane claims it -- spelled out, not read from the
# production constant, so it stays the property under test.
SETTLE_SECONDS = 3.0
# The settle countdown one second into the chosen row's wait: pins the ~3s
# production interval the way the escape remainder pins the three-minute one.
SETTLE_REMAINING_AFTER_ONE_SECOND = 2.0
# The tolerance for the settle countdown, read off a row stamped moments before
# the call. Wide enough for that stamp gap, tight enough to exclude both a spent
# interval (0s) and the whole starvation escape (180s).
SETTLE_COUNTDOWN_TOLERANCE_SECONDS = 1.0
# Aged past the settle above but short of the lone-row starvation escape: a row
# already settled, so its burst dispatches at once and the test exercises the
# claim/start path rather than the settle wait.
SETTLED_WAIT_SECONDS = 30.0


def _ready_row(uuid: str, *, waiting_seconds: float = 0.0) -> dict[str, str]:
    """A ready row carrying its current durable queue-age origin."""
    ready_at = datetime.now(UTC) - timedelta(seconds=waiting_seconds)
    return {
        "uuid": uuid,
        "ready_at": ready_at.isoformat().replace("+00:00", "Z"),
        "project": "serve.queue",
    }


def test_available_work_ensure_claims_as_bound_lane_before_start(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    trace: list[tuple[str, object]] = []
    # The chosen row is already settled, so this exercises the claim/start path
    # rather than the settle wait a fresh burst would take.
    candidates = [
        _ready_row("task-a", waiting_seconds=SETTLED_WAIT_SECONDS),
        _ready_row("task-spare"),
    ]

    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda actor: trace.append(("select", actor)) or candidates,
    )

    def fake_claim(task_uuid, actor, **kwargs):
        trace.append(("claim", (task_uuid, actor, kwargs)))
        return True

    monkeypatch.setattr(agentapi.claimstate, "do_claim", fake_claim)
    monkeypatch.setattr(
        agentapi,
        "git_read",
        lambda _repo, *args: "target-head" if args[0] == "rev-parse" else "main",
    )
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        lambda ensured_target, **_kwargs: (
            trace.append(("start", ensured_target.id))
            or ({"ok": True, "action": "start", "threadId": THREAD_A}, HTTPStatus.OK)
        ),
    )

    payload = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_due=_retry_gate(),
        retry_seconds=0.0,
    )

    claim_kwargs = trace[1][1][2]
    assert payload == {
        "ok": True,
        "action": "start",
        "threadId": THREAD_A,
        "trigger": "available-work",
        "taskHandle": identity.render_handle(candidates[0]),
    }
    assert [event[0] for event in trace] == ["select", "claim", "start"]
    assert trace[0] == ("select", THREAD_A)
    assert trace[1][1][:2] == ("task-a", THREAD_A)
    assert claim_kwargs == {
        "site": agentapi.claimstate.ClaimSite(repo.resolve(), "main", "target-head"),
        "context_thread": THREAD_A,
        "lease_seconds": lifecycle.SUPERVISOR_CLAIM_LEASE_SECONDS,
        "guard_unclaimed": True,
    }


def test_available_work_ensure_reports_unbound_lane(tmp_path):
    target = _target(_repo(tmp_path))

    payload = agentapi.ensure_agent_for_available_work(target, thread_id="")

    assert payload == {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "unbound",
    }


def test_available_work_ensure_reports_lost_claim_as_terminal_decision(
    tmp_path, monkeypatch
):
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    trace: list[str] = []
    # Settled chosen row: the pass reaches the claim it then loses, rather than
    # declining for the settle first.
    candidates = [
        _ready_row("task-raced", waiting_seconds=SETTLED_WAIT_SECONDS),
        _ready_row("task-next"),
    ]
    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: candidates,
    )
    monkeypatch.setattr(agentapi, "git_read", lambda *_args: "head")
    monkeypatch.setattr(
        agentapi.claimstate,
        "do_claim",
        lambda *_args, **_kwargs: trace.append("claim-raced") or False,
    )

    payload = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_due=_retry_gate(),
        retry_seconds=0.0,
    )

    assert payload == {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "claim-lost",
        "taskHandle": identity.render_handle(candidates[0]),
    }
    assert trace == ["claim-raced"]


def test_available_work_ensure_releases_confirmed_claim_after_start_failure(
    tmp_path, monkeypatch
):
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    trace: list[str] = []
    # Settled chosen row: the pass reaches the claim/start it must then release,
    # rather than declining for the settle first.
    candidates = [
        _ready_row("task-failed", waiting_seconds=SETTLED_WAIT_SECONDS),
        _ready_row("task-spare"),
    ]
    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: candidates,
    )
    monkeypatch.setattr(agentapi, "git_read", lambda *_args: "head")
    monkeypatch.setattr(
        agentapi.claimstate,
        "do_claim",
        lambda *_args, **_kwargs: trace.append("claim") or True,
    )
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        lambda *_args, **_kwargs: (
            trace.append("start") or {"ok": False, "error": "launch failed"},
            HTTPStatus.INTERNAL_SERVER_ERROR,
        ),
    )
    monkeypatch.setattr(
        agentapi.claimstate,
        "release_claim",
        lambda *_args, **_kwargs: (
            trace.append("release") or ClaimReleaseResult(released=True)
        ),
    )

    payload = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_due=_retry_gate(),
        retry_seconds=0.0,
    )

    assert trace == ["claim", "start", "release"]
    assert payload == {
        "ok": False,
        "error": "launch failed",
        "trigger": "available-work",
        "taskHandle": identity.render_handle(candidates[0]),
        "claimReleased": True,
    }


def test_available_work_concurrent_lane_decisions_start_one_expansion(
    tmp_path, monkeypatch
):
    repo_a = tmp_path / "repo-a"
    repo_b = tmp_path / "repo-b"
    repo_a.mkdir()
    repo_b.mkdir()
    targets = [
        agentapi.WorktreeTarget("lane-a", repo_a, "lane-a", "main-a"),
        agentapi.WorktreeTarget("lane-b", repo_b, "lane-b", "main-b"),
    ]
    actors = [THREAD_A, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]
    # The chosen row is already settled, so exactly one lane claims and starts it;
    # the losing lane sees a single fresh row left and declines for capacity.
    candidates = [
        _ready_row("task-a", waiting_seconds=SETTLED_WAIT_SECONDS),
        _ready_row("task-b"),
    ]
    claims: dict[str, str] = {}
    starts: list[str] = []
    results: list[dict[str, object] | None] = []
    monkeypatch.setattr(
        agentapi,
        "agent_status",
        lambda repo_root: SimpleNamespace(running=False, repo_root=repo_root),
    )
    monkeypatch.setattr(agentapi, "git_read", lambda *_args: "head")
    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: [row for row in candidates if row["uuid"] not in claims],
    )

    def fake_claim(task_uuid, actor, **_kwargs):
        claims[task_uuid] = actor
        return True

    def fake_start(target, **_kwargs):
        starts.append(target.id)
        return {"ok": True, "action": "start"}, HTTPStatus.OK

    monkeypatch.setattr(agentapi.claimstate, "do_claim", fake_claim)
    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_start)

    threads = [
        threading.Thread(
            target=lambda lane=target, actor=actor: results.append(
                agentapi.ensure_agent_for_available_work(
                    lane,
                    thread_id=actor,
                    retry_due=_retry_gate(),
                    retry_seconds=0.0,
                )
            )
        )
        for target, actor in zip(targets, actors, strict=True)
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    assert set(claims) == {"task-a"}
    assert len(starts) == 1
    assert sorted(
        result.get("reason", "started") for result in results if result is not None
    ) == ["capacity", "started"]


def test_second_ready_task_settles_before_dispatching_a_new_lane(tmp_path, monkeypatch):
    """A fresh burst waits out the settle, yet a decline never spends an interval.

    The lone first row declines for capacity with its whole starvation interval
    left. A second, fresh row clears the count threshold, but the chosen row is
    still freshly READY -- so the burst declines again for the short settle
    rather than beating a running agent's own done+next cycle to it. Once the
    chosen row has sat READY past the settle, the next pass dispatches: neither
    decline spent an interval, because both are read off the rows.
    """
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    candidates = [_ready_row("task-a")]
    claims: list[str] = []
    launch_policies: list[str] = []
    retry_calls: list[tuple[str, float]] = []

    def retry_due(target_id: str, seconds: float) -> bool:
        retry_calls.append((target_id, seconds))
        return True

    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: list(candidates),
    )
    monkeypatch.setattr(agentapi, "git_read", lambda *_args: "head")
    monkeypatch.setattr(
        agentapi.claimstate,
        "do_claim",
        lambda task_uuid, *_args, **_kwargs: claims.append(task_uuid) or True,
    )
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        lambda *_args, **kwargs: (
            launch_policies.append(
                "restart-held" if kwargs["automatic"] else "queue-immediate"
            )
            or ({"ok": True, "action": "start"}, HTTPStatus.OK)
        ),
    )

    below_capacity = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_due=retry_due,
    )
    # A second, fresh row clears the count threshold, but the chosen row is still
    # freshly READY, so the burst holds for the settle instead of starting.
    candidates.append(_ready_row("task-b"))
    settling = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_due=retry_due,
    )
    # Once the chosen row has sat READY past the settle, the pass dispatches it.
    candidates[0] = _ready_row("task-a", waiting_seconds=SETTLED_WAIT_SECONDS)
    at_capacity = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_due=retry_due,
    )

    assert below_capacity == {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "capacity",
        # The decline carries what it is waiting for: a freshly filed task has
        # its whole interval left before the escape opens.
        "retryAfterSeconds": pytest.approx(
            LONE_TASK_ESCAPE_SECONDS, abs=ESCAPE_COUNTDOWN_TOLERANCE_SECONDS
        ),
    }
    assert settling == {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "settle",
        # A freshly READY chosen row has the whole short settle left to sit.
        "retryAfterSeconds": pytest.approx(
            SETTLE_SECONDS, abs=SETTLE_COUNTDOWN_TOLERANCE_SECONDS
        ),
    }
    assert at_capacity == {
        "ok": True,
        "action": "start",
        "trigger": "available-work",
        "taskHandle": identity.render_handle(candidates[0]),
    }
    # One claim, taken on the dispatching pass -- never on the capacity or settle
    # decline, each of which reschedules the watcher rather than reserving early.
    assert claims == ["task-a"]
    assert launch_policies == ["restart-held"]
    assert retry_calls == [(target.id, agentapi.AVAILABLE_WORK_ENSURE_RETRY_SECONDS)]


def test_available_work_fresh_burst_settles_before_starting_a_lane(
    tmp_path, monkeypatch
):
    """Two freshly-READY rows hold for the settle before any lane is claimed.

    The count threshold is clear, so the old zero-wait path would have started a
    lane at once. Both rows are still fresh, so the burst instead declines with
    the settle remainder for the watcher to reschedule on -- a timer it acts on
    when the row has settled, not a busy poll -- and it takes no claim meanwhile.
    """
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    claims: list[str] = []
    fresh_burst = [_ready_row("task-fresh-a"), _ready_row("task-fresh-b")]
    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: list(fresh_burst),
    )
    monkeypatch.setattr(
        agentapi.claimstate,
        "do_claim",
        lambda task_uuid, *_args, **_kwargs: claims.append(task_uuid) or True,
    )

    settling = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_seconds=0.0,
    )

    assert settling == {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "settle",
        "retryAfterSeconds": pytest.approx(
            SETTLE_SECONDS, abs=SETTLE_COUNTDOWN_TOLERANCE_SECONDS
        ),
    }
    # The wait is a countdown the watcher reschedules on, not a claim reserved
    # early and held: nothing was claimed while the chosen row settled.
    assert claims == []
    # Like every decline, the settle skip rides into the worktree inventory as
    # `agentEnsure`; that schema must accept its reason and countdown.
    assert wire.validate_wire_payload("AgentEnsurePayload", settling) == settling


def test_available_work_storm_stops_at_the_rapid_death_refusal(tmp_path, monkeypatch):
    """Queue pressure starts a lane; it never retries one whose launches die."""
    # The available-work analogue of the pending-inbox storm below, and the
    # worse one: the trigger re-arms itself. Every launch dies in under a
    # second and hands its reservation straight back, so the two ready rows
    # that caused the launch are the board again the moment it fails. The
    # debounce is disabled here on purpose, leaving the journal-backed refusal
    # as the only thing that can end the loop.
    repo = _repo(tmp_path)
    target = _target(repo)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    launches = 0
    # Settled chosen row: every pass reaches the launch that dies on arrival,
    # isolating the rapid-death refusal from the settle wait.
    candidates = [
        _ready_row("task-a", waiting_seconds=SETTLED_WAIT_SECONDS),
        _ready_row("task-b"),
    ]

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
    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: candidates,
    )
    monkeypatch.setattr(agentapi, "git_read", lambda *_args: "head")
    monkeypatch.setattr(agentapi.claimstate, "do_claim", lambda *_args, **_kwargs: True)
    monkeypatch.setattr(
        agentapi.claimstate,
        "release_claim",
        lambda *_args: ClaimReleaseResult(released=True),
    )

    payloads = [
        agentapi.ensure_agent_for_available_work(
            target,
            thread_id=THREAD_A,
            retry_due=_retry_gate(),
            retry_seconds=0.0,
        )
        for _ in range(6)
    ]

    assert launches == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD
    assert [payload["action"] for payload in payloads[:3]] == [
        "start",
        "start",
        "start",
    ]
    held = payloads[3:]
    assert [payload["failure"] for payload in held] == (
        [lifecycle.AGENT_FAILURE_RESTART_REFUSED] * 3
    )
    # Refusing costs the board nothing: each held pass hands its reservation
    # back, so the row stays ready for whoever can actually run it.
    assert [payload["claimReleased"] for payload in held] == [True, True, True]
    assert [payload["taskHandle"] for payload in held] == [
        identity.render_handle(candidates[0])
    ] * 3
    assert (
        held[0]["restartRefusal"]["consecutive_rapid_deaths"]
        == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD
    )


def test_capacity_dispatch_records_its_attempt_against_the_next_pass(
    tmp_path, monkeypatch
):
    """A reservation handed straight back is not a fresh reason to launch."""
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    retry_due = _retry_gate()
    starts: list[str] = []
    # Settled chosen row: the first pass dispatches, so the second pass exercises
    # the recorded-attempt debounce rather than declining for the settle.
    candidates = [
        _ready_row("task-a", waiting_seconds=SETTLED_WAIT_SECONDS),
        _ready_row("task-b"),
    ]
    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: candidates,
    )
    monkeypatch.setattr(agentapi, "git_read", lambda *_args: "head")
    monkeypatch.setattr(agentapi.claimstate, "do_claim", lambda *_args, **_kwargs: True)
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        lambda *_args, **_kwargs: (
            starts.append("start") or ({"ok": True, "action": "start"}, HTTPStatus.OK)
        ),
    )

    started = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_due=retry_due,
    )
    immediately_again = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_due=retry_due,
    )

    assert agentapi.AVAILABLE_WORK_ENSURE_RETRY_SECONDS == CAPACITY_RETRY_SECONDS
    assert started == {
        "ok": True,
        "action": "start",
        "trigger": "available-work",
        "taskHandle": identity.render_handle(candidates[0]),
    }
    assert immediately_again == {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "retry-wait",
    }
    assert starts == ["start"]


def test_available_work_single_fresh_task_leaves_the_board_alone(tmp_path, monkeypatch):
    """A long-planned task freshly READY starts its clock at that transition."""
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    claims: list[str] = []
    fresh = {
        **_ready_row("task-only"),
        # This valid historical identity predates the READY transition by
        # years; using inception would start the lane immediately.
        "incepted": "1k4vPpg5",
    }
    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: [fresh],
    )
    monkeypatch.setattr(
        agentapi.claimstate,
        "do_claim",
        lambda task_uuid, *_args, **_kwargs: claims.append(task_uuid) or True,
    )

    skipped = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_seconds=0.0,
    )

    assert agentapi.AVAILABLE_WORK_START_THRESHOLD == 2
    assert skipped == {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "capacity",
        "retryAfterSeconds": pytest.approx(
            LONE_TASK_ESCAPE_SECONDS, abs=ESCAPE_COUNTDOWN_TOLERANCE_SECONDS
        ),
    }
    assert claims == []
    # The decline is not an internal return value: it rides into the worktree
    # inventory as `agentEnsure`, and that schema rejects any field it has not
    # declared -- so a countdown the browser has never heard of would fail the
    # whole lane payload rather than this call.
    assert wire.validate_wire_payload("AgentEnsurePayload", skipped) == skipped


def test_available_work_lone_task_starts_a_lane_after_three_minutes(
    tmp_path, monkeypatch
):
    """The escape fires at three minutes with nothing else on the board."""
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    # Literal seconds, not the constant: reading the constant would keep this
    # green at any interval, and three minutes is the property under test.
    waited = _ready_row("task-alone", waiting_seconds=180.0)
    monkeypatch.setattr(
        agentapi.alloc, "ordered_visible_ready_rows", lambda _actor: [waited]
    )
    monkeypatch.setattr(agentapi, "git_read", lambda *_args: "head")
    monkeypatch.setattr(agentapi.claimstate, "do_claim", lambda *_args, **_kwargs: True)
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        lambda *_args, **_kwargs: ({"ok": True, "action": "start"}, HTTPStatus.OK),
    )

    starved = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_seconds=0.0,
    )

    assert agentapi.AVAILABLE_WORK_STARVATION_SECONDS == LONE_TASK_ESCAPE_SECONDS
    assert starved == {
        "ok": True,
        "action": "start",
        "trigger": "available-work",
        "taskHandle": identity.render_handle(waited),
    }


def test_available_work_age_outlives_the_process_that_first_saw_the_task(
    tmp_path, monkeypatch
):
    """A server that has never seen this task still reads it as starving.

    The age used to be observed rather than read: serve kept a first-seen
    `time.monotonic()` per candidate in `ServeState`, so a restart dropped both
    the observation and the clock it was measured against. A task that had
    waited an hour looked freshly ready to the next process and had to serve
    the whole interval over again -- for as long as serve kept restarting, a
    lone task could never reach its escape. Nothing reachable from this call
    has ever seen the row before: the caches are empty and the wait is entirely
    the row's own.
    """
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    long_waited = _ready_row("task-outlived", waiting_seconds=LONE_TASK_ESCAPE_SECONDS)
    monkeypatch.setattr(
        agentapi.alloc, "ordered_visible_ready_rows", lambda _actor: [long_waited]
    )
    monkeypatch.setattr(agentapi, "git_read", lambda *_args: "head")
    monkeypatch.setattr(agentapi.claimstate, "do_claim", lambda *_args, **_kwargs: True)
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        lambda *_args, **_kwargs: ({"ok": True, "action": "start"}, HTTPStatus.OK),
    )

    started = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_due=_retry_gate(),
        retry_seconds=0.0,
    )

    assert started == {
        "ok": True,
        "action": "start",
        "trigger": "available-work",
        "taskHandle": identity.render_handle(long_waited),
    }


def test_available_work_refused_launch_starts_a_new_ready_interval(
    tmp_path, monkeypatch
):
    """A refused launch returns the row through a fresh READY transition."""
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    released: list[tuple[str, str]] = []
    broke = _ready_row("task-broke", waiting_seconds=LONE_TASK_ESCAPE_SECONDS)
    monkeypatch.setattr(
        agentapi.alloc, "ordered_visible_ready_rows", lambda _actor: [broke]
    )
    monkeypatch.setattr(agentapi, "git_read", lambda *_args: "head")
    monkeypatch.setattr(agentapi.claimstate, "do_claim", lambda *_args, **_kwargs: True)

    def release_with_fresh_ready_stamp(task_uuid, actor):
        released.append((task_uuid, actor))
        broke["ready_at"] = datetime.now(UTC).isoformat().replace("+00:00", "Z")
        return ClaimReleaseResult(released=True)

    monkeypatch.setattr(
        agentapi.claimstate, "release_claim", release_with_fresh_ready_stamp
    )
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        lambda *_args, **_kwargs: (
            {
                "ok": False,
                "failure": lifecycle.AGENT_FAILURE_OUT_OF_CREDITS,
                "error": "Could not ensure agent: out of credits",
            },
            HTTPStatus.PAYMENT_REQUIRED,
        ),
    )

    refused = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_seconds=0.0,
    )
    retried = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_seconds=0.0,
    )

    # Credit exhaustion is legible as itself: a launch was attempted, named the
    # provider's failure, and handed the task back.
    assert refused == {
        "ok": False,
        "failure": lifecycle.AGENT_FAILURE_OUT_OF_CREDITS,
        "error": "Could not ensure agent: out of credits",
        "trigger": "available-work",
        "taskHandle": identity.render_handle(broke),
        "claimReleased": True,
    }
    assert released == [("task-broke", THREAD_A)]
    assert retried == {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "capacity",
        "retryAfterSeconds": pytest.approx(
            LONE_TASK_ESCAPE_SECONDS, abs=ESCAPE_COUNTDOWN_TOLERANCE_SECONDS
        ),
    }


def test_available_work_next_deadline_counts_down_from_the_oldest_candidate():
    """The watcher's bound is what is left of the oldest candidate's interval."""
    now = datetime(2026, 7, 22, 12, 0, tzinfo=UTC)
    aged = [
        {"ready_at": (now - age).isoformat().replace("+00:00", "Z")}
        for age in (timedelta(minutes=1), timedelta(seconds=5))
    ]

    empty = agentapi.available_work_next_deadline([], now=now)
    watched = agentapi.available_work_next_deadline(aged, now=now)

    assert empty == LONE_TASK_ESCAPE_SECONDS
    assert watched == ESCAPE_REMAINING_AFTER_ONE_MINUTE


def test_available_work_settle_remaining_counts_down_from_the_chosen_row():
    """The settle bound is what is left of the first ordered row's interval."""
    now = datetime(2026, 7, 22, 12, 0, tzinfo=UTC)
    # The chosen row is first; an older row behind it does not shorten the wait,
    # so the settle tracks the row a burst would actually claim.
    rows = [
        {"ready_at": (now - age).isoformat().replace("+00:00", "Z")}
        for age in (timedelta(seconds=1), timedelta(seconds=90))
    ]

    empty = agentapi.available_work_settle_remaining([], now=now)
    watched = agentapi.available_work_settle_remaining(rows, now=now)

    # No candidates leaves nothing to wait on; one second in, the short settle
    # has its remainder left -- pinning the production interval.
    assert empty == 0.0
    assert watched == SETTLE_REMAINING_AFTER_ONE_SECOND


def test_available_work_age_refreshes_when_task_rejoins_the_backlog(
    tmp_path, monkeypatch
):
    """A task's later READY interval does not inherit its earlier queue age."""
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    returning = _ready_row("task-returned", waiting_seconds=LONE_TASK_ESCAPE_SECONDS)
    candidates = [returning]
    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: list(candidates),
    )
    monkeypatch.setattr(agentapi, "git_read", lambda *_args: "head")
    monkeypatch.setattr(agentapi.claimstate, "do_claim", lambda *_args, **_kwargs: True)
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        lambda *_args, **_kwargs: ({"ok": True, "action": "start"}, HTTPStatus.OK),
    )

    first_interval = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_seconds=0.0,
    )
    candidates.clear()
    absent = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_seconds=0.0,
    )
    returning["ready_at"] = datetime.now(UTC).isoformat().replace("+00:00", "Z")
    candidates.append(returning)
    rejoined = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_seconds=0.0,
    )

    assert first_interval == {
        "ok": True,
        "action": "start",
        "trigger": "available-work",
        "taskHandle": identity.render_handle(returning),
    }
    assert absent is None
    assert rejoined["reason"] == "capacity"
    assert rejoined["retryAfterSeconds"] == pytest.approx(
        LONE_TASK_ESCAPE_SECONDS, abs=ESCAPE_COUNTDOWN_TOLERANCE_SECONDS
    )
