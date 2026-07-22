"""Serve agent startup, automatic-work expansion, and restart contracts."""

from __future__ import annotations

import threading
from http import HTTPStatus
from types import SimpleNamespace

import pytest

from spice.agent import lifecycle
from spice.mail.inbox import (
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    compose_inbox_text,
    inbox_request_body,
    pending_inbox_count,
    write_inbox_item,
)
from spice.serve import agentapi
from spice.serve.workroutes import work_tree_send_response_payload
from tests.test_servehelpers import (
    THREAD_A,
    _patch_agent_status,
    _repo,
    _serve_state,
    _target,
)
from tests.test_taskgitsync import _advance_upstream, _repo_with_upstream, _run


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

    def fake_ensure(ensured_target, **kwargs):
        nonlocal ensure_calls
        ensure_calls += 1
        assert ensured_target == target
        return {"ok": True, "threadId": THREAD_A}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

    payload = agentapi.ensure_agent_for_pending_inbox(
        target,
        attempt_cache={},
        retry_seconds=0.0,
    )

    assert payload is None
    assert ensure_calls == 0
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
        attempt_cache={},
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


@pytest.mark.parametrize("condition", ["dirty", "ahead", "diverged"])
def test_pending_inbox_ensure_starts_a_skipped_lane_without_deadletter(
    tmp_path, monkeypatch, condition
):
    repo = _repo_with_upstream(tmp_path)
    target = _target(repo)
    (repo / ".git" / "info" / "exclude").write_text(".spice/\n", encoding="utf-8")
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
    real_prepare = lifecycle.gitsync.prepare_for_agent_launch

    def observed_prepare(repo_root):
        result = real_prepare(repo_root)
        launch_notes.extend(result.notes)
        return result

    def fake_supervisor(repo_root, **kwargs):
        spawned.append(SimpleNamespace(repo_root=repo_root, **kwargs))
        return SimpleNamespace(pid=4321)

    monkeypatch.setattr(lifecycle.gitsync, "prepare_for_agent_launch", observed_prepare)
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
        attempt_cache={},
        retry_seconds=0.0,
    )

    assert payload["ok"] is True
    assert payload["action"] == "start"
    assert launch_notes == [f"skipped:{condition}"]
    assert [item.name for item in collect_inbox_items(repo)] == ["1kLaunch.txt"]
    assert [(item.repo_root, item.action) for item in spawned] == [(repo, "start")]


def test_available_work_ensure_claims_as_bound_lane_before_start(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    trace: list[tuple[str, object]] = []
    candidates = [{"uuid": "task-a"}, {"uuid": "task-spare"}]

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
        attempt_cache={},
        retry_seconds=0.0,
    )

    claim_kwargs = trace[1][1][2]
    assert payload == {
        "ok": True,
        "action": "start",
        "threadId": THREAD_A,
        "trigger": "available-work",
        "taskHandle": "task-a",
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
    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: [{"uuid": "task-raced"}, {"uuid": "task-next"}],
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
        attempt_cache={},
        retry_seconds=0.0,
    )

    assert payload == {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "claim-lost",
        "taskHandle": "task-raced",
    }
    assert trace == ["claim-raced"]


def test_available_work_ensure_releases_confirmed_claim_after_start_failure(
    tmp_path, monkeypatch
):
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    trace: list[str] = []
    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: [{"uuid": "task-failed"}, {"uuid": "task-spare"}],
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
        lambda *_args, **_kwargs: trace.append("release") or True,
    )

    payload = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        attempt_cache={},
        retry_seconds=0.0,
    )

    assert trace == ["claim", "start", "release"]
    assert payload == {
        "ok": False,
        "error": "launch failed",
        "trigger": "available-work",
        "taskHandle": "task-failed",
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
    candidates = [{"uuid": "task-a"}, {"uuid": "task-b"}]
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
                    attempt_cache={},
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


def test_available_work_capacity_starts_with_two_ready_tasks(tmp_path, monkeypatch):
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    candidates = [{"uuid": "task-a"}]
    claims: list[str] = []
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
        lambda *_args, **_kwargs: (
            {"ok": True, "action": "start"},
            HTTPStatus.OK,
        ),
    )

    below_capacity = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_seconds=0.0,
    )
    candidates.append({"uuid": "task-b"})
    at_capacity = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        retry_seconds=0.0,
    )

    assert below_capacity == {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "capacity",
    }
    assert at_capacity == {
        "ok": True,
        "action": "start",
        "trigger": "available-work",
        "taskHandle": "task-a",
    }
    assert claims == ["task-a"]


def test_available_work_starvation_escape_starts_old_ready_task(tmp_path, monkeypatch):
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    ready_since: dict[tuple[str, str], float] = {}
    observed_times = iter([0.0, agentapi.AVAILABLE_WORK_STARVATION_SECONDS])
    monkeypatch.setattr(agentapi.time, "monotonic", lambda: next(observed_times))
    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: [{"uuid": "task-old"}],
    )
    monkeypatch.setattr(agentapi, "git_read", lambda *_args: "head")
    monkeypatch.setattr(agentapi.claimstate, "do_claim", lambda *_args, **_kwargs: True)
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        lambda *_args, **_kwargs: (
            {"ok": True, "action": "start"},
            HTTPStatus.OK,
        ),
    )

    initial = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        ready_since_cache=ready_since,
        retry_seconds=0.0,
    )
    starved = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        ready_since_cache=ready_since,
        retry_seconds=0.0,
    )

    assert initial == {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "capacity",
    }
    assert starved == {
        "ok": True,
        "action": "start",
        "trigger": "available-work",
        "taskHandle": "task-old",
    }


def test_available_work_ready_age_resets_after_task_leaves_backlog(
    tmp_path, monkeypatch
):
    target = _target(_repo(tmp_path))
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    candidates = [{"uuid": "task-returned"}]
    ready_since: dict[tuple[str, str], float] = {}
    reappeared_at = agentapi.AVAILABLE_WORK_STARVATION_SECONDS + 1.0
    observed_times = iter([0.0, reappeared_at])
    monkeypatch.setattr(agentapi.time, "monotonic", lambda: next(observed_times))
    monkeypatch.setattr(
        agentapi.alloc,
        "ordered_visible_ready_rows",
        lambda _actor: list(candidates),
    )

    initial = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        ready_since_cache=ready_since,
        retry_seconds=0.0,
    )
    candidates.clear()
    absent = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        ready_since_cache=ready_since,
        retry_seconds=0.0,
    )
    candidates.append({"uuid": "task-returned"})
    reappeared = agentapi.ensure_agent_for_available_work(
        target,
        thread_id=THREAD_A,
        ready_since_cache=ready_since,
        retry_seconds=0.0,
    )

    capacity_skip = {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": "capacity",
    }
    assert initial == capacity_skip
    assert absent is None
    assert reappeared == capacity_skip
    assert ready_since[(THREAD_A, "task-returned")] == reappeared_at


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
            target, attempt_cache={}, retry_seconds=0.0
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
        target, attempt_cache={}, retry_seconds=0.0, automatic=False
    )
    reopened = agentapi.ensure_agent_for_pending_inbox(
        target, attempt_cache={}, retry_seconds=0.0
    )

    assert granted["action"] == "start"
    assert launches == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD + 1
    assert reopened["failure"] == lifecycle.AGENT_FAILURE_RESTART_REFUSED
    assert reopened["deadletteredInboxKeys"] == ["1kDwBqjF"]
    assert pending_inbox_count(repo) == 0
    outcomes = lifecycle.read_launch_outcomes(repo)
    assert len(outcomes) == lifecycle.RAPID_DEATH_REFUSAL_THRESHOLD + 1


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
