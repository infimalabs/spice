"""Serve agent startup, automatic-work expansion, and restart contracts."""

from __future__ import annotations

import threading
import time
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
from spice.serve.payload import wire
from spice.serve.workroutes import work_tree_send_response_payload
from spice.tasks import identity
from spice.tasks.claimstate import ClaimReleaseResult
from tests.test_servehelpers import (
    THREAD_A,
    _patch_agent_status,
    _repo,
    _serve_state,
    _target,
)
from tests.test_taskgitsync import _advance_upstream, _repo_with_upstream, _run

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
# The race below parks a publication mid-flight on purpose. Each ordered step is
# waited on with the same short bound -- long enough that a loaded machine still
# reaches the next moment, short enough that a genuine deadlock fails the test
# rather than hanging it.
EXPLICIT_SEND_STEP_SECONDS = 5.0
RECONCILER_JOIN_SECONDS = 5.0
# The parked publication only has to outlive the assertions made while the race
# is open; this bound is the escape hatch for a test that stops early.
EXPLICIT_SEND_RELEASE_SECONDS = 15.0


def _retry_gate():
    attempts: dict[str, float] = {}

    def due(target_id: str, retry_seconds: float) -> bool:
        now = time.monotonic()
        last_attempt = attempts.get(target_id)
        if last_attempt is not None and now - last_attempt < retry_seconds:
            return False
        attempts[target_id] = now
        return True

    return due


def _ready_row(uuid: str, *, waiting_seconds: float = 0.0) -> dict[str, str]:
    """A ready row carrying its current durable queue-age origin."""
    ready_at = datetime.now(UTC) - timedelta(seconds=waiting_seconds)
    return {
        "uuid": uuid,
        "ready_at": ready_at.isoformat().replace("+00:00", "Z"),
        "project": "serve.queue",
    }


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
    assert payload["pendingInboxCount"] == 0
    assert payload["pendingInboxLabel"] == "0"
    assert payload["pendingInboxKeys"] == []
    assert payload["pendingInboxRevision"]
    assert payload["pendingInboxVersion"] > 0
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
    assert refused["pendingInboxCount"] == 0
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
        self.background_at_launch_lock = threading.Event()
        self.background_finished = threading.Event()
        self.agent_started = threading.Event()
        self.attempts: list[bool] = []
        self.attempt_pending_counts: list[int] = []
        self.direct_result: dict[str, object] = {}

    def install(self, monkeypatch) -> None:
        real_submit = workroutes.submit_steering_message
        real_launch_lock = agentapi._PENDING_INBOX_LAUNCH_LOCK
        race = self

        class ObservedPendingInboxLaunchLock:
            def __enter__(self):
                if threading.current_thread().name.startswith(
                    serve_lifecycle.LIFECYCLE_RECONCILER_THREAD_PREFIX
                ):
                    race.background_at_launch_lock.set()
                return real_launch_lock.__enter__()

            def __exit__(self, exc_type, exc_value, traceback):
                return real_launch_lock.__exit__(exc_type, exc_value, traceback)

        def pause_after_publication(**kwargs):
            sent = real_submit(**kwargs)
            race.published.set()
            race.release_direct_send.wait(timeout=EXPLICIT_SEND_RELEASE_SECONDS)
            return sent

        def status_after_explicit_start(*_args, **_kwargs):
            return SimpleNamespace(running=race.agent_started.is_set())

        monkeypatch.setattr(
            workroutes, "submit_steering_message", pause_after_publication
        )
        monkeypatch.setattr(agentapi, "agent_status", status_after_explicit_start)
        monkeypatch.setattr(agentapi, "agent_ensure_response_payload", self.ensure)
        monkeypatch.setattr(
            agentapi,
            "_PENDING_INBOX_LAUNCH_LOCK",
            ObservedPendingInboxLaunchLock(),
        )

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
    race.install(monkeypatch)
    reconciler = state.lifecycle_reconciler
    assert reconciler is not None

    def send_directly() -> None:
        race.direct_result["response"] = work_tree_send_response_payload(
            state, target, {"text": "use my explicit restart grant"}
        )

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
    # The watcher's decision is at the real launch boundary, blocked behind the
    # route's pre-publication acquisition, and its own evaluation has not
    # finished. Releasing the route publishes and reserves this send's grant as
    # one step, so the decision that wins the guard next reads a reservation only
    # the send's own intent may spend.
    assert (
        race.background_at_launch_lock.wait(timeout=EXPLICIT_SEND_STEP_SECONDS) is True
    )
    assert race.background_finished.is_set() is False
    race.release_direct_send.set()
    direct_thread.join(timeout=EXPLICIT_SEND_STEP_SECONDS)
    background_thread.join(timeout=EXPLICIT_SEND_STEP_SECONDS)

    response, status = race.direct_result["response"]
    assert status == HTTPStatus.OK
    assert response["agentEnsure"]["action"] == "start"
    # Exactly one launch decision, it is the send's own, and it ran against an
    # inbox that already holds the item -- publication precedes the attempt that
    # the item justifies, never the other way around.
    assert race.attempts == [False]
    assert race.attempt_pending_counts == [1]
    assert race.agent_started.is_set() is True
    assert race.background_at_launch_lock.is_set() is True
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
        service_tier="",
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
