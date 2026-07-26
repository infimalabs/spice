"""Agent supervisor runtime, outcome, refusal, and claim-renewal contracts."""

import argparse
import json
import os
import subprocess
import time
from datetime import UTC, datetime
from pathlib import Path
from threading import Event, Thread

import pytest

from spice.agent import driver as agent_driver
from spice.agent import lifecycle, sidechannel, watchdog
from spice.agent.driver import (
    CODEX_DRIVER,
    RATE_LIMIT_HTTP_STATUS,
)
from spice.errors import SpiceError
from spice.tasks import claimstate
from tests.test_lifecyclehelpers import (
    FakeProcess as _FakeProcess,
    FakeThread as _FakeThread,
    status as _status,
)

SUPERVISOR_PID = 3333
SUPERVISED_AGENT_PID = 4444
# Long enough to be an unmistakable share of one 20-second beat.
RENEWAL_COST_SECONDS = 5.0
SPEND_LIMIT_RESET_EPOCH = 1784280000
STORM_DEATH_EPOCH = 1784269388  # 2026-07-17T06:23:08Z, the final storm launch death


@pytest.fixture(autouse=True)
def _git_worktree_tmp_path(request, tmp_path):
    if "tmp_path" in request.fixturenames:
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)


def test_run_agent_supervisor_writes_state_under_fakes(tmp_path, monkeypatch):
    log_path = tmp_path / "supervisor.log"
    skill_path = (tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH).resolve()
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=5)
    thread = _FakeThread()
    side_events: list[tuple[str, object]] = []
    spawned: list[dict[str, object]] = []
    thread_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    monkeypatch.setattr(lifecycle, "agent_environment", lambda repo_root: {"ENV": "1"})
    monkeypatch.setattr(
        lifecycle,
        "spawn_supervised_agent",
        lambda command, *, cwd, log_path, env: (
            spawned.append(
                {"command": command, "cwd": cwd, "log_path": log_path, "env": env}
            )
            or (process, thread)
        ),
    )
    monkeypatch.setattr(
        lifecycle,
        "require_started_process",
        lambda _process, _log_path, **_kwargs: None,
    )
    monkeypatch.setattr(
        lifecycle,
        "started_agent_thread_id",
        lambda _log_path, *, repo_root, fallback_thread_id: thread_id,
    )
    monkeypatch.setattr(
        sidechannel,
        "AgentSideChannelServer",
        lambda repo_root, **_kwargs: _FakeSideChannel(repo_root, side_events),
    )
    args = argparse.Namespace(
        repo_root=str(tmp_path),
        action="resume",
        model="gpt-test",
        reasoning_effort="high",
        service_tier="fast",
        resume_thread_id="resume-thread",
        log_path=str(log_path),
        fast_mode=True,
        command_json='["codex","exec","prompt"]',
        launch_claim_uuid="",
        launch_claim_actor="",
    )

    exit_code = lifecycle.run_agent_supervisor(args)
    state = lifecycle.read_agent_state(tmp_path)
    final_log_path = (
        tmp_path / ".git" / ".spice" / "agents" / thread_id / "logs" / log_path.name
    ).resolve()

    assert exit_code == 5
    assert side_events == [("enter", tmp_path), ("exit", tmp_path)]
    assert spawned == [
        {
            "command": ["codex", "exec", "prompt"],
            "cwd": tmp_path,
            "log_path": log_path,
            "env": {"ENV": "1"},
        }
    ]
    assert state["pid"] == SUPERVISED_AGENT_PID
    assert state["supervisor_pid"] == os.getpid()
    assert state["thread_id"] == thread_id
    assert state["log_path"] == str(final_log_path)
    assert state["prompt_skill_path"] == str(skill_path)
    assert state["fast_mode"] is True
    assert thread.joined_timeouts == [1.0]


def test_run_agent_supervisor_records_launch_outcome_under_fakes(tmp_path, monkeypatch):
    log_path = tmp_path / "supervisor.log"
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=5)
    thread_id = "cccccccccccccccccccccccccccccccc"
    monkeypatch.setattr(lifecycle, "agent_environment", lambda repo_root: {"ENV": "1"})
    monkeypatch.setattr(
        lifecycle,
        "spawn_supervised_agent",
        lambda command, *, cwd, log_path, env: (process, _FakeThread()),
    )
    monkeypatch.setattr(
        lifecycle,
        "require_started_process",
        lambda _process, _log_path, **_kwargs: None,
    )
    monkeypatch.setattr(
        lifecycle,
        "started_agent_thread_id",
        lambda _log_path, *, repo_root, fallback_thread_id: thread_id,
    )
    monkeypatch.setattr(
        sidechannel,
        "AgentSideChannelServer",
        lambda repo_root, **_kwargs: _FakeSideChannel(repo_root, []),
    )
    args = argparse.Namespace(
        repo_root=str(tmp_path),
        action="resume",
        model="gpt-test",
        reasoning_effort="high",
        service_tier="",
        resume_thread_id="resume-thread",
        log_path=str(log_path),
        fast_mode=False,
        command_json='["codex","exec","prompt"]',
        launch_claim_uuid="",
        launch_claim_actor="",
    )

    exit_code = lifecycle.run_agent_supervisor(args)
    outcomes = lifecycle.read_launch_outcomes(tmp_path)
    final_log_path = (
        tmp_path / ".git" / ".spice" / "agents" / thread_id / "logs" / log_path.name
    ).resolve()

    assert exit_code == 5
    assert [outcome["exit_code"] for outcome in outcomes] == [5]
    assert outcomes[0]["thread_id"] == thread_id
    assert outcomes[0]["log_path"] == str(final_log_path)
    assert outcomes[0]["failure_kind"] == ""
    assert outcomes[0]["assistant_messages"] == 0
    assert outcomes[0]["tool_calls"] == 0
    assert outcomes[0]["lifetime_seconds"] >= 0.0
    assert outcomes[0]["ended_at"] >= outcomes[0]["started_at"]


def test_supervised_launch_outcome_replays_one_turn_429_stream_log(
    tmp_path, monkeypatch
):
    # The four structural lines a spend-limited claude launch actually
    # streamed on 2026-07-17: init succeeds, the rate limiter rejects with a
    # reset horizon, the synthetic assistant message carries the human-facing
    # text, and the result line flags the error — then the process exits 0.
    monkeypatch.setenv(agent_driver.SPICE_AGENT_DRIVER_ENV, "claude")
    log_path = tmp_path / "launch.log"
    session = "805282e9-dafc-4014-8523-e6e7ae0a4144"
    message = (
        "You've hit your monthly spend limit · raise it at claude.ai/settings/usage"
    )
    lines = [
        {"type": "system", "subtype": "init", "session_id": session},
        {"type": "system", "subtype": "status", "status": "requesting"},
        {
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": "rejected",
                "resetsAt": SPEND_LIMIT_RESET_EPOCH,
                "rateLimitType": "five_hour",
                "overageStatus": "rejected",
            },
            "session_id": session,
        },
        {
            "type": "assistant",
            "message": {
                "model": "<synthetic>",
                "role": "assistant",
                "content": [{"type": "text", "text": message}],
            },
            "error": "rate_limit",
            "session_id": session,
        },
        {
            "type": "result",
            "subtype": "success",
            "is_error": True,
            "api_error_status": RATE_LIMIT_HTTP_STATUS,
            "duration_ms": 751,
            "num_turns": 1,
            "result": message,
            "session_id": session,
        },
    ]
    log_path.write_text(
        "".join(f"{json.dumps(line)}\n" for line in lines), encoding="utf-8"
    )

    outcome = lifecycle.supervised_launch_outcome(
        tmp_path,
        thread_id=session.replace("-", ""),
        log_path=log_path,
        started_at="2026-07-17T06:21:10.042183Z",
        lifetime_seconds=2.6182,
        exit_code=0,
    )

    assert outcome["failure_kind"] == "out-of-credits"
    assert outcome["reset_epoch"] == SPEND_LIMIT_RESET_EPOCH
    assert outcome["exit_code"] == 0
    assert outcome["assistant_messages"] == 1
    assert outcome["tool_calls"] == 0
    assert outcome["lifetime_seconds"] == 2.618


def test_record_launch_outcome_keeps_bounded_journal(tmp_path):
    for index in range(lifecycle.LAUNCH_OUTCOMES_LIMIT + 3):
        lifecycle.record_launch_outcome(tmp_path, {"exit_code": index})

    outcomes = lifecycle.read_launch_outcomes(tmp_path)

    assert len(outcomes) == lifecycle.LAUNCH_OUTCOMES_LIMIT
    assert outcomes[-1] == {"exit_code": lifecycle.LAUNCH_OUTCOMES_LIMIT + 2}
    assert outcomes[0] == {"exit_code": 3}


def _launch_death_stamp(epoch: float) -> str:
    return (
        datetime.fromtimestamp(epoch, tz=UTC)
        .isoformat(timespec="microseconds")
        .replace("+00:00", "Z")
    )


def _rapid_death(ended_epoch: float, **extra) -> dict:
    return {
        "lifetime_seconds": 0.751,
        "exit_code": 0,
        "ended_at": _launch_death_stamp(ended_epoch),
        **extra,
    }


def test_launch_refusal_opens_after_consecutive_rapid_deaths(tmp_path):
    probe = STORM_DEATH_EPOCH + 1.0
    for index in range(lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD - 1):
        lifecycle.record_launch_outcome(
            tmp_path, _rapid_death(STORM_DEATH_EPOCH - 9 + index)
        )

    assert lifecycle.launch_refusal(tmp_path, now=probe) is None

    lifecycle.record_launch_outcome(tmp_path, _rapid_death(STORM_DEATH_EPOCH))

    assert lifecycle.launch_refusal(tmp_path, now=probe) == {
        "consecutive_rapid_deaths": lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD,
        "hold_until_epoch": int(
            STORM_DEATH_EPOCH + lifecycle.RAPID_DEATH_REFUSAL_WINDOW_SECONDS
        ),
    }

    lifecycle.record_launch_outcome(
        tmp_path,
        {
            "lifetime_seconds": 300.0,
            "exit_code": 0,
            "ended_at": _launch_death_stamp(STORM_DEATH_EPOCH + 60),
        },
    )

    assert lifecycle.launch_refusal(tmp_path, now=probe) is None


def test_launch_refusal_window_expiry_yields_to_reset_epoch(tmp_path):
    for index in range(lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD):
        lifecycle.record_launch_outcome(
            tmp_path, _rapid_death(STORM_DEATH_EPOCH - 18 + 9 * index)
        )
    expired = float(
        STORM_DEATH_EPOCH + lifecycle.RAPID_DEATH_REFUSAL_WINDOW_SECONDS + 1
    )

    assert lifecycle.launch_refusal(tmp_path, now=expired) is None

    lifecycle.record_launch_outcome(
        tmp_path,
        _rapid_death(STORM_DEATH_EPOCH, reset_epoch=SPEND_LIMIT_RESET_EPOCH),
    )

    assert lifecycle.launch_refusal(tmp_path, now=expired) == {
        "consecutive_rapid_deaths": lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD + 1,
        "hold_until_epoch": SPEND_LIMIT_RESET_EPOCH,
        "reset_epoch": SPEND_LIMIT_RESET_EPOCH,
    }
    assert (
        lifecycle.launch_refusal(tmp_path, now=float(SPEND_LIMIT_RESET_EPOCH)) is None
    )


def test_ensure_agent_automatic_refuses_while_explicit_start_is_granted(
    tmp_path, monkeypatch
):
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr(lifecycle, "agent_status", lambda *_args, **_kwargs: _status())
    for index in range(lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD):
        lifecycle.record_launch_outcome(tmp_path, _rapid_death(time.time() - index))
    journal = lifecycle.read_launch_outcomes(tmp_path)

    with pytest.raises(lifecycle.AgentRestartRefusedError) as excinfo:
        lifecycle.ensure_agent(tmp_path, dry_run=True, automatic=True)

    assert (
        excinfo.value.refusal["consecutive_rapid_deaths"]
        == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD
    )

    explicit = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert explicit.action == "would-start"
    assert lifecycle.read_launch_outcomes(tmp_path) == journal


def test_supervisor_lane_watch_periodically_renews_claim(tmp_path, monkeypatch):
    log_path = tmp_path / "supervisor.log"
    renewals: list[tuple[Path, str, Path]] = []
    nudges: list[tuple[Path, str, Path]] = []
    stop = _StopAfterOneIteration()
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=None)
    # A renewal costs real wall clock on a loaded host. The beat is paced from
    # its own start, so that cost comes out of the idle budget rather than
    # being added on top of it.
    clock = _AdvancingClock(cost=RENEWAL_COST_SECONDS)
    monkeypatch.setattr(lifecycle.time, "monotonic", clock.monotonic)
    monkeypatch.setattr(
        lifecycle,
        "_renew_supervised_claim",
        lambda repo_root, thread_id, log_path, _reported, _cursors, _held: (
            renewals.append((repo_root, thread_id, log_path)),
            clock.spend(),
        )[0],
    )
    monkeypatch.setattr(
        lifecycle,
        "_flag_uncaptured_lane",
        lambda repo_root, thread_id, log_path: nudges.append(
            (repo_root, thread_id, log_path)
        ),
    )

    lifecycle._watch_supervised_lane(tmp_path, "thread-a", log_path, process, stop)

    assert renewals == [(tmp_path, "thread-a", log_path)]
    assert nudges == [(tmp_path, "thread-a", log_path)]
    assert stop.waits[0] == (
        lifecycle.SUPERVISOR_CLAIM_RENEWAL_SECONDS - RENEWAL_COST_SECONDS
    )
    assert (
        lifecycle.SUPERVISOR_CLAIM_RENEWAL_SECONDS
        == lifecycle.SUPERVISOR_LANE_WATCH_SECONDS
    )
    assert lifecycle.SUPERVISOR_CLAIM_LEASE_SECONDS == (
        3.0 * lifecycle.SUPERVISOR_CLAIM_RENEWAL_SECONDS
    )


def test_supervisor_lane_signal_keeps_notification_that_precedes_wait():
    signal = lifecycle.SupervisorLaneSignal()
    consumed = Event()
    signal.notify()

    def wait_for_signal() -> None:
        signal.wait_for_event(30.0)
        consumed.set()

    waiter = Thread(target=wait_for_signal, daemon=True)
    waiter.start()

    assert consumed.wait(2.0)
    waiter.join(timeout=2.0)


def test_supervisor_claim_renewal_uses_owned_actor(tmp_path, monkeypatch):
    calls: list[dict[str, object]] = []

    def fake_renew_claim(handle=None, *, actor=None, lease_seconds=None):
        calls.append(
            {
                "handle": handle,
                "actor": actor,
                "lease_seconds": lease_seconds,
            }
        )
        return claimstate.ClaimRenewalResult(
            True,
            "renewed",
            handle="TASK-00000000",
            claim_until="2026-07-09T06:00:00.000000Z",
            uuid="11111111-1111-1111-1111-111111111111",
        )

    monkeypatch.setattr(claimstate, "renew_claim", fake_renew_claim)

    lifecycle._renew_supervised_claim(
        tmp_path, "thread-a", tmp_path / "supervisor.log", {}, {}, {}
    )

    assert calls == [
        {
            "handle": None,
            "actor": "thread-a",
            "lease_seconds": lifecycle.SUPERVISOR_CLAIM_LEASE_SECONDS,
        }
    ]


def test_supervisor_claim_renewal_is_silent_without_active_claim(tmp_path, monkeypatch):
    feedback: list[tuple[str, dict[str, object]]] = []
    log_path = tmp_path / "supervisor.log"
    monkeypatch.setattr(
        claimstate,
        "renew_claim",
        lambda **_kwargs: claimstate.ClaimRenewalResult(False, "no_active_claim"),
    )
    monkeypatch.setattr(
        watchdog,
        "publish_supervisor_feedback",
        lambda _repo, _log, kind, **fields: feedback.append((kind, fields)),
    )

    lifecycle._renew_supervised_claim(tmp_path, "thread-a", log_path, {}, {}, {})

    assert feedback == []
    assert not log_path.exists()


@pytest.mark.parametrize(
    "result",
    [
        claimstate.ClaimRenewalResult(
            False, "claimed_by_other", "TASK-peer", detail="peer"
        ),
        claimstate.ClaimRenewalResult(False, "missing", "TASK-missing"),
        claimstate.ClaimRenewalResult(False, "deleted", "TASK-deleted"),
    ],
)
def test_supervisor_claim_renewal_reports_bounded_noop_reasons(
    tmp_path, monkeypatch, result
):
    feedback: list[tuple[str, dict[str, object]]] = []
    log_path = tmp_path / "supervisor.log"
    reported: dict[str, str] = {}
    monkeypatch.setattr(claimstate, "renew_claim", lambda **_kwargs: result)
    monkeypatch.setattr(
        watchdog,
        "publish_supervisor_feedback",
        lambda _repo, _log, kind, **fields: feedback.append((kind, fields)),
    )

    lifecycle._renew_supervised_claim(tmp_path, "thread-a", log_path, reported, {}, {})
    lifecycle._renew_supervised_claim(tmp_path, "thread-a", log_path, reported, {}, {})

    log_text = log_path.read_text(encoding="utf-8")
    assert log_text.count(f"reason={result.reason}") == 1
    assert feedback == [
        (
            "claim.renewal-skipped",
            {
                "reason": result.reason,
                "handle": result.handle,
                "detail": result.detail,
            },
        )
    ]


def test_supervisor_claim_renewal_reports_backend_failure(tmp_path, monkeypatch):
    feedback: list[tuple[str, dict[str, object]]] = []
    log_path = tmp_path / "supervisor.log"
    reported: dict[str, str] = {}
    result = claimstate.ClaimRenewalResult(
        False, "backend_error", handle="TASK-failed", detail="backend offline"
    )
    monkeypatch.setattr(claimstate, "renew_claim", lambda **_kwargs: result)
    monkeypatch.setattr(
        watchdog,
        "publish_supervisor_feedback",
        lambda _repo, _log, kind, **fields: feedback.append((kind, fields)),
    )

    lifecycle._renew_supervised_claim(tmp_path, "thread-a", log_path, reported, {}, {})
    lifecycle._renew_supervised_claim(tmp_path, "thread-a", log_path, reported, {}, {})

    assert log_path.read_text(encoding="utf-8") == (
        "spice claim renewal failed: "
        "reason=backend_error handle=TASK-failed detail=backend offline\n"
    )
    assert feedback == [
        (
            "claim.renewal-failed",
            {
                "reason": "backend_error",
                "handle": "TASK-failed",
                "detail": "backend offline",
            },
        )
    ]


def test_supervisor_claim_contract_watch_reports_one_actionable_error(
    tmp_path, monkeypatch
):
    feedback: list[tuple[str, dict[str, object]]] = []
    log_path = tmp_path / "supervisor.log"
    reported: dict[str, str] = {}
    result = claimstate.ClaimRenewalResult(
        True,
        "renewed",
        handle="TASK-watch",
        uuid="11111111-1111-1111-1111-111111111111",
    )
    detail = (
        "unsupported TaskChampion operations log at /tmp/taskchampion.sqlite3: "
        "operations table is missing"
    )
    monkeypatch.setattr(claimstate, "renew_claim", lambda **_kwargs: result)

    def schema_failure(*_args, **_kwargs):
        raise SpiceError(detail)

    monkeypatch.setattr(lifecycle, "_notice_contract_mutations", schema_failure)
    monkeypatch.setattr(
        watchdog,
        "publish_supervisor_feedback",
        lambda _repo, _log, kind, **fields: feedback.append((kind, fields)),
    )

    lifecycle._renew_supervised_claim(tmp_path, "thread-a", log_path, reported, {}, {})
    lifecycle._renew_supervised_claim(tmp_path, "thread-a", log_path, reported, {}, {})

    assert log_path.read_text(encoding="utf-8") == (
        f"spice claim contract watch failed: TASK-watch {detail}\n"
    )
    assert feedback == [
        (
            "claim.contract-watch-error",
            {"handle": "TASK-watch", "detail": detail},
        )
    ]
    assert reported == {"contract_watch": detail}


def test_require_supervisor_started_accepts_thread_settled_log_path(
    tmp_path, monkeypatch
):
    thread_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    log_path = lifecycle.next_agent_log_path(tmp_path)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.write_text("starting\n", encoding="utf-8")
    final_log_path = lifecycle.settle_agent_log_path(tmp_path, log_path, thread_id)
    lifecycle.write_agent_state(
        tmp_path,
        {
            "pid": SUPERVISED_AGENT_PID,
            "thread_id": thread_id,
            "log_path": str(final_log_path),
            "mode": "start",
            "started_at": "2026-01-02T03:04:05Z",
            "prompt_skill_path": str(tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH),
        },
    )
    monkeypatch.setattr(lifecycle, "SUPERVISOR_STARTUP_TIMEOUT_SECONDS", 0)
    monkeypatch.setattr(
        lifecycle,
        "process_id_is_running",
        lambda pid: pid == SUPERVISED_AGENT_PID,
    )
    process = _FakeProcess(pid=SUPERVISOR_PID, returncode=None)

    lifecycle.require_supervisor_started(process, repo_root=tmp_path, log_path=log_path)
    assert process.wait_calls == 0


def test_require_started_process_distinguishes_codex_credit_failure(
    tmp_path, monkeypatch
):
    log_path = tmp_path / "agent.log"
    log_path.write_text(
        "ERROR: You've hit your usage limit. Visit "
        "https://chatgpt.com/codex/settings/usage to purchase more credits "
        "or try again at 4:36 PM.\n",
        encoding="utf-8",
    )
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=1)

    monkeypatch.setattr(lifecycle, "STARTUP_GRACE_SECONDS", 0)
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CODEX_DRIVER)

    with pytest.raises(lifecycle.AgentOutOfCreditsError, match="hit your usage limit"):
        lifecycle.require_started_process(process, log_path, repo_root=tmp_path)


class _StopAfterOneIteration:
    def __init__(self) -> None:
        self.waits: list[float] = []

    def wait_for_event(self, seconds: float) -> bool:
        self.waits.append(seconds)
        return True


class _AdvancingClock:
    """A monotonic clock that only moves when the watched work spends it."""

    def __init__(self, cost: float) -> None:
        self.cost = cost
        self.reading = 0.0

    def monotonic(self) -> float:
        return self.reading

    def spend(self) -> None:
        self.reading += self.cost


class _FakeSideChannel:
    def __init__(self, repo_root, events) -> None:
        self.repo_root = repo_root
        self.events = events

    def __enter__(self):
        self.events.append(("enter", self.repo_root))
        return self

    def __exit__(self, *_exc):
        self.events.append(("exit", self.repo_root))
