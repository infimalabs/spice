"""Serve work-route, live-bus, and static-route contracts."""

from __future__ import annotations

import threading
from http import HTTPStatus
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from spice.agent.renewal import (
    RENEWAL_HANDOFF_REQUEST_SUFFIX,
    renewal_rehydration_text,
)
from spice.mail.ackstate import (
    ack_state_database_path,
    directive_history_records_from_database,
)
from spice.mail.inbox import (
    INBOX_CONTROL_DRAIN_QUEUE,
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    compose_inbox_text,
    inbox_event_path,
    inbox_payload_rows,
    inbox_request_body,
    parse_inbox_payload,
    pending_inbox_count,
    write_inbox_item,
)
from spice.serve import agentapi, app as serve_app, web as serve_web
from spice.serve.payload import identity, lane, message
from spice.serve.app import (
    team_command_response_payload,
    team_snapshot_response_payload,
)
from spice.serve.livebus import (
    LIVE_BUS_WATCHER_JOIN_TIMEOUT_S,
    LiveBusCallbacks,
    LiveBusSession,
)
from spice.serve.web import STATIC_ROOT, render_index_html, send_static_asset
from spice.serve.workroutes import (
    work_tree_send_response_payload,
    work_tree_send_accepted_response_payload,
    work_tree_task_drain_response_payload,
)
from spice.serve.worktree.target import WorktreeTarget
from tests.test_teamstorehelpers import store_global_revision
from tests.test_wirefixtures import (
    valid_lane_payload,
    valid_live_bus_callback_payloads,
    valid_metric_series_payload,
    valid_wire_payload,
)
from tests.test_servehelpers import (
    ACTOR_A,
    ACTOR_B,
    IMAGE_DATA_URL,
    THREAD_A,
    THREAD_B,
    _BusTarget,
    _Connection,
    _StaticHandler,
    _patch_agent_status,
    _record_identity,
    _repo,
    _serve_state,
    _target,
    _transcript_resolution,
)

# A blocked ensure only has to outlive the assertions made while it is parked;
# the release bound is the failure escape hatch for a test that stops early.
BLOCKED_ENSURE_ENTRY_SECONDS = 5.0
BLOCKED_ENSURE_RELEASE_SECONDS = 15.0
# Short enough that a parked decision is reported as absent while the test still
# holds the launch, long enough not to race a decision that does arrive.
PARKED_DECISION_WAIT_SECONDS = 0.5


def test_work_tree_send_drive_keeps_control_out_of_request_text(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)

    payload, status = work_tree_send_response_payload(
        state, target, {"text": "keep draining", "lifetime": "Drive"}
    )
    empty_payload, empty_status = work_tree_send_response_payload(
        state, target, {"text": "   "}
    )
    item = collect_inbox_items(repo)[0]
    parsed = parse_inbox_payload(item.text)
    readout = "\n".join(inbox_payload_rows([item]))

    assert status == HTTPStatus.OK
    assert payload["requestText"] == "keep draining"
    assert "DRAIN QUEUE ASAP" not in payload["requestText"]
    assert payload["requestControls"] == [INBOX_CONTROL_DRAIN_QUEUE]
    assert parsed.body == "keep draining"
    assert parsed.controls == (INBOX_CONTROL_DRAIN_QUEUE,)
    assert f"Control: {INBOX_CONTROL_DRAIN_QUEUE}" in item.text
    assert "control=drive-drain-queue: DRAIN QUEUE ASAP: spice task next" in readout
    assert empty_status == HTTPStatus.BAD_REQUEST
    assert empty_payload == {"ok": False, "error": "Message text is required."}


def test_work_tree_send_accepted_response_schedules_ensure_without_waiting(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    ensure_calls: list[dict[str, object]] = []
    ensure_entered = threading.Event()
    release_ensure = threading.Event()

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        ensure_entered.set()
        release_ensure.wait(timeout=BLOCKED_ENSURE_RELEASE_SECONDS)
        return {"ok": True, "threadId": THREAD_A}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

    try:
        payload, status = work_tree_send_accepted_response_payload(
            state,
            target,
            {
                "text": "> > quoted context\n> > with newline\n\nwake this lane",
                "attachments": [
                    {
                        "name": "paste.png",
                        "contentType": "image/png",
                        "dataUrl": IMAGE_DATA_URL,
                    }
                ],
            },
        )

        items = collect_inbox_items(repo)
        assert status == HTTPStatus.OK
        assert payload["ok"] is True
        assert (
            payload["requestText"]
            == "> > quoted context\n> > with newline\n\nwake this lane"
        )
        assert payload["requestHtml"] == (
            "<blockquote><blockquote><p>quoted context<br>with newline</p>"
            "</blockquote></blockquote><p>wake this lane</p>"
        )
        assert payload["attachments"][0]["name"] == "paste.png"
        assert payload["attachments"][0]["contentType"] == "image/png"
        # The reply is complete while the launch it scheduled is still parked
        # inside the ensure below: this route reports the publication, never the
        # lane start.
        assert payload["agentEnsure"] == {}
        assert payload["pendingInboxCount"] == 1
        assert payload["pendingInboxLabel"] == "1"
        assert payload["pendingInboxKeys"] == [payload["key"]]
        assert payload["pendingInboxRevision"]
        assert payload["pendingInboxVersion"] > 0
        assert inbox_event_path(repo).read_text(encoding="utf-8").endswith(" inbox\n")
        assert inbox_request_body(items[0].text) == (
            "> > quoted context\n> > with newline\n\nwake this lane"
        )
        assert ensure_entered.wait(timeout=BLOCKED_ENSURE_ENTRY_SECONDS) is True
        # One decision, and an automatic one: this route reserves no explicit
        # restart grant, which is what still separates it from the synchronous
        # send.
        assert ensure_calls == [
            {
                "target": target,
                "fast_mode": False,
                "force_new": False,
                "automatic": True,
            }
        ]
    finally:
        release_ensure.set()


def test_send_attributes_its_publication_when_no_decision_arrives(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[ACTOR_A])
    _record_identity(state, target, ACTOR_A, THREAD_A)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    release_ensure = threading.Event()

    def fake_ensure(_target, **_kwargs):
        release_ensure.wait(timeout=BLOCKED_ENSURE_RELEASE_SECONDS)
        return {"ok": True, "threadId": THREAD_A}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)
    monkeypatch.setattr(
        agentapi,
        "LIFECYCLE_DECISION_WAIT_SECONDS",
        PARKED_DECISION_WAIT_SECONDS,
    )

    try:
        payload, status = work_tree_send_response_payload(
            state,
            target,
            {"text": "steer this lane"},
        )

        directive = directive_history_records_from_database(
            ack_state_database_path(repo)
        )[0]
        assert status == HTTPStatus.OK
        assert payload["ok"] is True
        # The decision is still parked inside the launch, so the reply reports
        # the durable publication against the lane it landed in rather than
        # dropping its attribution with the decision.
        assert payload["agentEnsure"] == {}
        assert payload["route"]["actor"] == ACTOR_A
        assert (directive.target_actor, directive.team_id) == (ACTOR_A, created.team_id)
    finally:
        release_ensure.set()


def test_running_requested_renewal_sends_handoff_and_marks_pending(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    state.team_store.create_team(members=[ACTOR_A])
    _record_identity(state, target, ACTOR_A, THREAD_A)
    state.team_store.set_agent_renewal_request(ACTOR_A, requested=True)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)
    monkeypatch.setattr(
        identity,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "gpt-next", "effort": "high"},
    )

    payload, status = work_tree_send_response_payload(
        state,
        target,
        {
            "text": "wrap this up",
            "attachments": [
                {
                    "name": "paste.png",
                    "contentType": "image/png",
                    "dataUrl": IMAGE_DATA_URL,
                }
            ],
        },
    )

    item = collect_inbox_items(repo)[0]
    with state.team_store.connect() as connection:
        renewal = connection.execute(
            "SELECT state, ancestor_thread_id, successor_agent_id "
            "FROM renewals WHERE agent_id = ?",
            (ACTOR_A,),
        ).fetchone()
    assert status == HTTPStatus.OK
    assert payload["agentEnsure"] == {}
    assert payload["requestText"] == "wrap this up"
    assert payload["requestHtml"] == "<p>wrap this up</p>"
    assert payload["attachments"][0]["name"] == "paste.png"
    assert RENEWAL_HANDOFF_REQUEST_SUFFIX in inbox_request_body(item.text)
    assert payload["renewalIntent"]["requested"] is False
    assert payload["renewalIntent"]["state"] == "pending"
    assert payload["renewalIntent"]["successorThreadId"] == ""
    assert payload["renewalIntent"]["teamSlot"] == 0
    assert payload["renewalIntent"]["predecessorIdentity"]["actualModel"] == (
        "gpt-test"
    )
    assert payload["renewalIntent"]["successorIdentity"]["desiredModel"] == ("gpt-next")
    assert renewal["state"] == "pending"
    assert renewal["ancestor_thread_id"] == THREAD_A
    assert renewal["successor_agent_id"] == ""


def test_stopped_requested_renewal_starts_successor_and_moves_team_membership(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[ACTOR_A])
    _record_identity(state, target, ACTOR_A, THREAD_A)
    state.team_store.set_agent_renewal_request(ACTOR_A, requested=True)
    ensure_calls: list[dict[str, object]] = []
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    monkeypatch.setattr(
        identity,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "gpt-next", "effort": "high"},
    )

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        return {"ok": True, "threadId": THREAD_B}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)
    state.team_store.set_global_fast_mode_enabled(True)

    payload, status = work_tree_send_response_payload(
        state,
        target,
        {"text": "continue from handoff"},
    )

    body = inbox_request_body(collect_inbox_items(repo)[0].text)
    assert status == HTTPStatus.OK
    assert payload["agentEnsure"]["threadId"] == THREAD_B
    assert payload["renewalIntent"]["requested"] is False
    assert payload["renewalIntent"]["state"] == "started"
    assert payload["renewalIntent"]["successorThreadId"] == THREAD_B
    assert payload["renewalIntent"]["teamSlot"] == 0
    assert payload["renewalIntent"]["successorIdentity"]["actorId"] == ACTOR_B
    assert payload["renewalIntent"]["successorIdentity"]["threadId"] == THREAD_B
    assert renewal_rehydration_text(THREAD_A) in body
    assert ensure_calls == [
        {
            "target": target,
            "fast_mode": True,
            "force_new": True,
            # An operator-requested renewal send is explicit steering.
            "automatic": False,
        }
    ]
    assert state.team_store.current_team_for_agent(ACTOR_A) is None
    assert state.team_store.current_team_for_agent(ACTOR_B) == created.team_id


def test_task_drain_replaces_filters_and_creates_route_team(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)

    payload, status = work_tree_task_drain_response_payload(
        state,
        target,
        {
            "replaceTaskFilters": True,
            "taskFilters": ["serve", "", "task.review"],
            "lifetime": "Drive",
        },
    )

    team_id = state.team_store.current_team_for_agent(ACTOR_A)
    assert status == HTTPStatus.OK
    assert payload["route"]["actor"] == ACTOR_A
    assert payload["route"]["teamIdentity"]["teamId"] == team_id
    assert payload["route"]["taskFilters"] == ["serve", "task.review"]
    assert payload["route"]["lifetime"] == "Drive"
    assert payload["route"]["memberAgents"] == [ACTOR_A]


def test_team_command_payloads_reject_stale_expected_revision(
    tmp_path,
):
    state = _serve_state(tmp_path, _target(_repo(tmp_path)))
    created, create_status = team_command_response_payload(
        state,
        {
            "command": "createTeam",
            "members": [ACTOR_A],
            "config": {"lifetime": "Steer"},
        },
    )
    team_id = created["snapshot"]["teams"][0]["teamId"]
    first_revision = created["revision"]
    advanced, _advanced_status = team_command_response_payload(
        state,
        {
            "command": "updateTeamConfig",
            "teamId": team_id,
            "configPatch": {"lifetime": "Drive"},
            "expectedRevision": first_revision,
        },
    )
    stale, stale_status = team_command_response_payload(
        state,
        {
            "command": "updateTeamConfig",
            "teamId": team_id,
            "configPatch": {"lifetime": "Steer"},
            "expectedRevision": first_revision,
        },
    )
    fresh_snapshot = team_snapshot_response_payload(
        state, since_revision=advanced["revision"]
    )

    assert create_status == HTTPStatus.OK
    assert stale_status == HTTPStatus.CONFLICT
    assert stale["ok"] is False
    assert "stale team command" in stale["error"]
    assert fresh_snapshot == {
        "ok": True,
        "revision": advanced["revision"],
        "changed": False,
    }
    current = team_snapshot_response_payload(state, since_revision=None)
    assert current["snapshot"]["teams"][0]["config"]["lifetime"] == "Drive"
    assert sorted(current["snapshot"]["teams"][0]["config"]) == [
        "lifetime",
        "revision",
        "shellSettings",
        "taskFilterEntries",
        "taskFilters",
    ]
    unchanged = team_snapshot_response_payload(
        state, since_revision=advanced["revision"]
    )
    assert unchanged == {
        "ok": True,
        "revision": advanced["revision"],
        "changed": False,
    }


def test_team_topology_responses_emit_complete_per_team_differentials(tmp_path):
    state = _serve_state(tmp_path, _target(_repo(tmp_path)))
    first = state.team_store.create_team(
        team_id="team-first",
        members=[ACTOR_A, ACTOR_B],
    )
    second = state.team_store.create_team(team_id="team-second")
    baseline = state.team_store.team_snapshot().global_revision

    moved, moved_status = team_command_response_payload(
        state,
        {
            "command": "moveAgentToTeam",
            "teamId": second.team_id,
            "agentId": ACTOR_A,
            "expectedRevision": baseline,
        },
    )
    refreshed = team_snapshot_response_payload(state, since_revision=baseline)
    closed, closed_status = team_command_response_payload(
        state,
        {
            "command": "closeTeam",
            "teamId": second.team_id,
            "expectedRevision": moved["revision"],
        },
    )

    assert moved_status == HTTPStatus.OK
    assert moved["differential"] is True
    assert moved["snapshot"]["teamCount"] == 2
    assert [team["teamId"] for team in moved["snapshot"]["teams"]] == [
        first.team_id,
        second.team_id,
    ]
    assert moved["snapshot"]["removedTeamIds"] == []
    assert refreshed == {
        "ok": True,
        "revision": moved["revision"],
        "changed": True,
        "differential": True,
        "snapshot": moved["snapshot"],
    }
    assert closed_status == HTTPStatus.OK
    assert closed["differential"] is True
    assert closed["snapshot"]["teamCount"] == 1
    assert closed["snapshot"]["teams"] == []
    assert closed["snapshot"]["removedTeamIds"] == [second.team_id]


def test_team_command_payload_preserves_explicit_actor_ids(tmp_path):
    target = _target(_repo(tmp_path))
    state = _serve_state(tmp_path, target)
    target_actor = f"target:{target.id}"

    created, create_status = team_command_response_payload(
        state,
        {
            "command": "createTeam",
            "members": [target_actor, ACTOR_A],
        },
    )
    team_id = created["snapshot"]["teams"][0]["teamId"]
    reorder, reorder_status = team_command_response_payload(
        state,
        {
            "command": "reorderTeamAgents",
            "teamId": team_id,
            "agents": [{"agentId": ACTOR_A}, {"agentId": target_actor}],
        },
    )

    members = [
        member["agentId"] for member in reorder["snapshot"]["teams"][0]["members"]
    ]
    assert create_status == HTTPStatus.OK
    assert reorder_status == HTTPStatus.OK
    assert members == [ACTOR_A, target_actor]


def test_team_snapshot_payload_preserves_typed_agent_identity_facts(tmp_path):
    target = _target(_repo(tmp_path))
    state = _serve_state(tmp_path, target)
    state.team_store.create_team(members=[ACTOR_A])
    state.team_store.record_agent_identity(
        actor_id=ACTOR_A,
        target_id=target.id,
        thread_id=THREAD_A,
        renewal_state="pending",
        renewal_revision=7,
    )

    payload = team_snapshot_response_payload(state, since_revision=None)
    facts = payload["snapshot"]["teams"][0]["members"][0]["agentFacts"]

    assert {
        "actorId": facts["actorId"],
        "targetId": facts["targetId"],
        "threadId": facts["threadId"],
        "renewalState": facts["renewalState"],
        "renewalRevision": facts["renewalRevision"],
    } == {
        "actorId": ACTOR_A,
        "targetId": target.id,
        "threadId": THREAD_A,
        "renewalState": "pending",
        "renewalRevision": 7,
    }
    assert isinstance(facts["updatedAt"], float)


def test_messages_refresh_wakes_stopped_agent_for_cli_written_inbox(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    write_inbox_item(
        repo,
        "1jN54zJK.txt",
        compose_inbox_text(body="external steering", priority=None, stop=False),
    )
    ensure_calls: list[dict[str, object]] = []

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        return {"ok": True, "threadId": THREAD_A}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

    payload = message.messages_payload_for_worktree(state, target, limit=5)

    assert payload["pendingInboxCount"] == 1
    assert payload["agentEnsure"]["threadId"] == THREAD_A
    assert ensure_calls == [
        {"target": target, "fast_mode": False, "force_new": False, "automatic": True}
    ]


def test_global_fast_mode_command_drives_two_lane_agent_ensure(tmp_path, monkeypatch):
    root_a = tmp_path / "lane-a"
    root_b = tmp_path / "lane-b"
    root_a.mkdir()
    root_b.mkdir()
    repo_a = _repo(root_a)
    repo_b = _repo(root_b)
    target_a = WorktreeTarget(
        id="target-a", repo_root=repo_a, name=repo_a.name, branch="main"
    )
    target_b = WorktreeTarget(
        id="target-b", repo_root=repo_b, name=repo_b.name, branch="main"
    )
    state = _serve_state(tmp_path, target_a)
    state.cached_targets = [target_a, target_b]
    _patch_agent_status(monkeypatch, thread_id="", running=False)
    write_inbox_item(
        repo_a,
        "1jN54zJK.txt",
        compose_inbox_text(body="lane a", priority=None, stop=False),
    )
    write_inbox_item(
        repo_b,
        "1jN54zJL.txt",
        compose_inbox_text(body="lane b", priority=None, stop=False),
    )
    ensure_calls: list[dict[str, object]] = []

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target.id, **kwargs})
        thread_id = THREAD_A if ensured_target.id == target_a.id else THREAD_B
        return {"ok": True, "threadId": thread_id}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

    command_payload, command_status = team_command_response_payload(
        state,
        {
            "command": "setGlobalFastMode",
            "expectedRevision": store_global_revision(state.team_store),
            "fastMode": True,
        },
    )
    payload_a = message.messages_payload_for_worktree(state, target_a, limit=5)
    payload_b = message.messages_payload_for_worktree(state, target_b, limit=5)

    assert command_status == HTTPStatus.OK
    assert command_payload["snapshot"]["globalSettings"] == {"fastMode": True}
    assert state.team_store.global_fast_mode_enabled() is True
    assert payload_a["agentEnsure"]["threadId"] == THREAD_A
    assert payload_b["agentEnsure"]["threadId"] == THREAD_B
    assert ensure_calls == [
        {
            "target": target_a.id,
            "fast_mode": True,
            "force_new": False,
            "automatic": True,
        },
        {
            "target": target_b.id,
            "fast_mode": True,
            "force_new": False,
            "automatic": True,
        },
    ]


def test_pending_inbox_deadletters_after_credit_failure(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    write_inbox_item(
        repo,
        "1jN54zJK.txt",
        compose_inbox_text(body="external steering", priority=None, stop=False),
    )
    ensure_calls = 0

    def fake_ensure(ensured_target, **kwargs):
        nonlocal ensure_calls
        ensure_calls += 1
        assert ensured_target == target
        return {
            "ok": False,
            "failure": agentapi.AGENT_FAILURE_OUT_OF_CREDITS,
            "error": "Could not ensure agent: usage limit reached",
        }, HTTPStatus.PAYMENT_REQUIRED

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

    payload = message.messages_payload_for_worktree(state, target, limit=5)

    assert ensure_calls == 1
    assert payload["agentEnsure"]["deadletteredInboxKey"] == "1jN54zJK"
    assert (
        payload["agentEnsure"]["deadletterRequeueCommand"]
        == "spice agent requeue-deadletter 1jN54zJK"
    )
    assert payload["pendingInboxCount"] == 0
    assert payload["statusLine"]["pendingInboxCount"] == 0
    assert payload["statusLine"]["pendingInboxLabel"] == "0"
    assert payload["pendingInboxKeys"] == []
    assert payload["statusLine"]["pendingInboxKeys"] == []
    assert payload["agentEnsure"]["pendingInboxCount"] == 0
    assert payload["agentEnsure"]["pendingInboxKeys"] == []
    assert pending_inbox_count(repo) == 0
    assert [item.name for item in collect_deadlettered_inbox_items(repo)] == [
        "1jN54zJK.txt"
    ]


def test_pending_inbox_deadletters_after_generic_ensure_failure(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    write_inbox_item(
        repo,
        "1jN54zJL.txt",
        compose_inbox_text(body="external steering", priority=None, stop=False),
    )
    ensure_calls = 0

    def fake_ensure(ensured_target, **kwargs):
        nonlocal ensure_calls
        ensure_calls += 1
        assert ensured_target == target
        return {
            "ok": False,
            "error": "Could not ensure agent: invalid config",
        }, HTTPStatus.INTERNAL_SERVER_ERROR

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

    payload = message.messages_payload_for_worktree(state, target, limit=5)

    assert ensure_calls == 1
    assert payload["agentEnsure"]["ok"] is False
    assert payload["agentEnsure"]["error"] == "Could not ensure agent: invalid config"
    assert "failure" not in payload["agentEnsure"]
    assert payload["agentEnsure"]["deadletteredInboxKey"] == "1jN54zJL"
    assert (
        payload["agentEnsure"]["deadletterRequeueCommand"]
        == "spice agent requeue-deadletter 1jN54zJL"
    )
    assert payload["agentEnsure"]["pendingInboxCount"] == 0
    assert payload["agentEnsure"]["pendingInboxLabel"] == "0"
    assert payload["agentEnsure"]["pendingInboxKeys"] == []
    assert payload["pendingInboxCount"] == 0
    assert payload["statusLine"]["pendingInboxCount"] == 0
    assert payload["statusLine"]["pendingInboxLabel"] == "0"
    assert payload["pendingInboxKeys"] == []
    assert payload["statusLine"]["pendingInboxKeys"] == []
    assert pending_inbox_count(repo) == 0
    assert [item.name for item in collect_deadlettered_inbox_items(repo)] == [
        "1jN54zJL.txt"
    ]


def test_status_line_reports_stale_agent_launch_cwd(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    other = tmp_path / "other"
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    status = SimpleNamespace(
        repo_root=repo,
        running=False,
        thread_id=THREAD_A,
        process_status="idle",
        pid=0,
        process_group_id=0,
        model="gpt-test",
        reasoning_effort="low",
        service_tier="",
        started_at="",
        log_path=None,
        prompt_skill_path=repo / ".agents" / "skills" / "spice" / "SKILL.md",
        command=["codex", "exec", "--cd", str(other)],
    )
    monkeypatch.setattr(lane, "agent_status", lambda *_args, **_kwargs: status)

    line = lane.status_line_payload(state, target, items=[], error=None)

    assert line["bindingStatus"] == "mismatch"
    assert "launch cwd" in line["bindingError"]
    assert str(other.resolve()) in line["error"]
    assert line["rolloutStatus"] == "error"


def test_status_line_ignores_stale_prompt_skill_path(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    other = tmp_path / "other"
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    stale_skill = other / ".agents" / "skills" / "spice" / "SKILL.md"
    status = SimpleNamespace(
        repo_root=repo,
        running=False,
        thread_id=THREAD_A,
        process_status="idle",
        pid=0,
        process_group_id=0,
        model="gpt-test",
        reasoning_effort="low",
        service_tier="",
        started_at="",
        log_path=None,
        prompt_skill_path=stale_skill,
        command=["codex", "exec", "--cd", str(repo)],
    )
    monkeypatch.setattr(lane, "agent_status", lambda *_args, **_kwargs: status)

    line = lane.status_line_payload(state, target, items=[], error=None)

    assert line["bindingStatus"] == "bound"
    assert line["bindingError"] == ""
    assert line["rolloutStatus"] == "ok"


def _route_test_livebus_callbacks(target, calls, messages_payload):
    def wire_messages_payload(bus_target, **kwargs):
        return valid_lane_payload(**messages_payload(bus_target, **kwargs))

    return LiveBusCallbacks(
        resolve_target=lambda selector: target if selector == target.id else None,
        **valid_live_bus_callback_payloads(
            messages_payload=wire_messages_payload,
            send_payload=lambda _target, payload: (
                calls.append(("send", payload)) or {"ok": True, "key": "inbox-key"},
                HTTPStatus.OK,
            ),
            task_drain_payload=lambda _target, payload: (
                calls.append(("taskDrain", payload))
                or valid_wire_payload("TaskDrainResult"),
                HTTPStatus.OK,
            ),
            team_snapshot_payload=lambda since_revision: valid_wire_payload(
                "TeamSnapshotResponse",
                revision=since_revision or 0,
            ),
            team_command_payload=lambda payload: (
                calls.append(("teamCommand", payload))
                or valid_wire_payload("TeamCommandResponse", revision=2),
                HTTPStatus.OK,
            ),
        ),
        thread_id=lambda _target: "thread",
        transcript_resolution=lambda _thread_id: _transcript_resolution(
            "thread", Path("rollout.jsonl")
        ),
        lane_watch_paths=lambda *_args: (),
        lane_signature=lambda *_args: (),
    )


def test_livebus_mutation_adapters_preserve_live_routes_without_override(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    calls: list[tuple[str, dict[str, Any]]] = []

    monkeypatch.setattr(
        serve_app,
        "work_tree_send_accepted_response_payload",
        lambda _state, _target, payload: (
            calls.append(("send", payload)) or {"ok": True, "key": "inbox-key"},
            HTTPStatus.OK,
        ),
    )
    monkeypatch.setattr(
        serve_app,
        "work_tree_task_drain_response_payload",
        lambda _state, _target, payload: (
            calls.append(("taskDrain", payload)) or {"ok": True, "route": {}},
            HTTPStatus.OK,
        ),
    )

    send_result = serve_app._live_bus_send_payload(
        state, target, {"text": "keep draining"}
    )
    drain_result = serve_app._live_bus_task_drain_payload(
        state, target, {"replaceTaskFilters": True}
    )

    assert send_result == ({"ok": True, "key": "inbox-key"}, HTTPStatus.OK)
    assert drain_result == ({"ok": True, "route": {}}, HTTPStatus.OK)
    assert calls == [
        ("send", {"text": "keep draining"}),
        ("taskDrain", {"replaceTaskFilters": True}),
    ]


def _dispatch_livebus_route_requests(session: LiveBusSession) -> None:
    session._handle_lane_send(
        {
            "type": "lane.send",
            "requestId": "send-1",
            "targetId": "lane",
            "payload": {"text": "hello"},
        }
    )
    session._handle_lane_task_drain(
        {
            "type": "lane.taskDrain",
            "requestId": "drain-1",
            "targetId": "lane",
            "payload": {"replaceTaskFilters": True},
        }
    )
    session._handle_teams_command(
        {
            "type": "teams.command",
            "requestId": "team-1",
            "payload": {"command": "createTeam"},
        }
    )
    session._handle_lane_history(
        {
            "type": "lane.history",
            "requestId": "history-1",
            "targetId": "lane",
            "query": {"limit": 9, "before": "oldest", "threadId": "thread"},
        }
    )


def _assert_livebus_send_completion(connection: _Connection) -> dict[str, Any]:
    send_result = next(
        frame for frame in connection.sent if frame.get("type") == "lane.sendResult"
    )
    send_timing = send_result["result"].pop("serverTiming")
    submission = send_result["result"].pop("submission")
    completed_timing = next(
        frame for frame in connection.sent if frame.get("type") == "lane.sendTiming"
    )
    assert list(send_timing) == [
        "mutationQueueMs",
        "targetResolveMs",
        "sendPayloadMs",
        "totalBeforeReplyMs",
        "replyLockWaitMs",
    ]
    assert all(isinstance(value, float) for value in send_timing.values())
    assert all(value >= 0.0 for value in send_timing.values())
    assert submission["stage"] == "accepted"
    assert submission["stages"]["accepted"]["source"] == "inbox-write"
    assert completed_timing["requestId"] == "send-1"
    assert set(completed_timing["serverTiming"]) == {
        "mutationQueueMs",
        "targetResolveMs",
        "sendPayloadMs",
        "totalBeforeReplyMs",
        "replyLockWaitMs",
        "replyLockHoldMs",
        "replyWriteMs",
        "totalMs",
    }
    assert all(value >= 0.0 for value in completed_timing["serverTiming"].values())
    assert send_result == {
        "type": "lane.sendResult",
        "result": {"ok": True, "key": "inbox-key"},
        "requestId": "send-1",
    }
    return send_result


def _assert_livebus_route_results(
    connection: _Connection, send_result: dict[str, Any]
) -> None:
    frames_by_request = {
        frame["requestId"]: frame
        for frame in connection.sent
        if frame.get("type") != "lane.sendTiming"
    }
    assert frames_by_request == {
        "send-1": send_result,
        "drain-1": {
            "type": "lane.taskDrainResult",
            "result": valid_wire_payload("TaskDrainResult"),
            "requestId": "drain-1",
        },
        "team-1": {
            "type": "teams.commandResult",
            "result": {"ok": True, "revision": 2},
            "requestId": "team-1",
        },
        "history-1": {
            "type": "lane.payload",
            "payload": valid_lane_payload(
                messages=[{"key": "m1"}],
                statusLine={},
            ),
            "requestId": "history-1",
        },
    }


def _assert_livebus_route_calls(calls: list[tuple[str, dict[str, Any]]]) -> None:
    history_kwargs = next(kw for kind, kw in calls if kind == "messages")
    assert isinstance(history_kwargs.pop("client_id", None), str)
    assert {kind: payload for kind, payload in calls} == {
        "send": {"text": "hello"},
        "taskDrain": {"replaceTaskFilters": True},
        "teamCommand": {"command": "createTeam"},
        "messages": {
            "limit": 9,
            "before": "oldest",
            "expected_thread_id": "thread",
        },
    }


def test_livebus_routes_send_task_drain_team_command_and_history_requests():
    target = _BusTarget(id="lane")
    connection = _Connection()
    calls: list[tuple[str, dict[str, Any]]] = []

    def messages_payload(_target, **kwargs):
        calls.append(("messages", kwargs))
        return {"messages": [{"key": "m1"}], "statusLine": {}}

    session = LiveBusSession(
        connection,
        _route_test_livebus_callbacks(target, calls, messages_payload),
    )
    _dispatch_livebus_route_requests(session)
    # Read and mutation responses use independent pools; drain both real
    # completion surfaces before checking correlated frames.
    session._await_pending_reads(LIVE_BUS_WATCHER_JOIN_TIMEOUT_S)
    session._await_pending_mutations(LIVE_BUS_WATCHER_JOIN_TIMEOUT_S)
    assert len(connection.sent) == 5

    send_result = _assert_livebus_send_completion(connection)
    _assert_livebus_route_results(connection, send_result)
    _assert_livebus_route_calls(calls)


def test_livebus_routes_metric_series_requests():
    query = {"metric": "activity", "agentId": "agent-a", "start": 0, "end": 60}
    connection = _Connection()
    calls: list[dict[str, Any]] = []
    callbacks = LiveBusCallbacks(
        resolve_target=lambda _selector: None,
        **valid_live_bus_callback_payloads(
            metric_series_payload=lambda payload: (
                calls.append(payload)
                or valid_metric_series_payload(metric=str(payload["metric"]))
            )
        ),
        thread_id=lambda _target: "thread",
        transcript_resolution=lambda _thread_id: None,
        lane_watch_paths=lambda *_args: (),
        lane_signature=lambda *_args: (),
    )

    session = LiveBusSession(connection, callbacks)
    session._handle_metrics_series(
        {"type": "metrics.series", "requestId": "metrics-1", "query": query}
    )
    # Metrics run on a dedicated worker; teardown drains it deterministically.
    session._teardown()

    assert connection.sent == [
        {
            "type": "metrics.seriesResult",
            "result": valid_metric_series_payload(metric="activity"),
            "requestId": "metrics-1",
        }
    ]
    assert calls == [query]


def test_index_links_and_serves_packaged_favicon():
    html = render_index_html()
    favicon = STATIC_ROOT / "favicon.ico"
    handler = _StaticHandler()

    send_static_asset(handler, "favicon.ico")

    assert '<link rel="icon" href="/static/favicon.ico" sizes="any">' in html
    assert html.index("/static/index.css") < html.index("/static/composer.css")
    assert html.index("/static/composer.css") < html.index("/static/messages.css")
    assert html.index("/static/messages.css") < html.index("/static/status-colors.css")
    assert html.index("/static/app.shell.js") < html.index("/static/app.composer.js")
    assert html.index("/static/app.composer.js") < html.index("/static/app.controls.js")
    assert html.index("/static/app.controls.js") < html.index(
        "/static/app.filter-model.js"
    )
    assert html.index("/static/app.filter-model.js") < html.index(
        "/static/app.panes.js"
    )
    assert favicon.is_file()
    assert handler.status == HTTPStatus.OK
    assert handler.headers["Content-Length"] == str(favicon.stat().st_size)
    assert "icon" in handler.headers["Content-Type"]
    assert handler.body.getvalue().startswith(b"\x00\x00\x01\x00")


def test_static_asset_sets_revalidation_headers():
    asset = STATIC_ROOT / "app.js"
    handler = _StaticHandler()

    send_static_asset(handler, "app.js")

    body = asset.read_bytes()
    assert len(body) > 0
    assert handler.status == HTTPStatus.OK
    assert handler.body.getvalue() == body
    assert set(handler.headers) == {
        "Content-Type",
        "Content-Length",
        "ETag",
        "Cache-Control",
    }
    assert handler.headers["Cache-Control"] == "no-cache"
    assert handler.headers["Content-Length"] == str(len(body))
    etag = handler.headers["ETag"]
    assert etag.startswith('"') and etag.endswith('"')


def test_static_asset_conditional_match_returns_not_modified():
    primer = _StaticHandler()
    send_static_asset(primer, "app.js")
    etag = primer.headers["ETag"]

    handler = _StaticHandler()
    send_static_asset(handler, "app.js", if_none_match=etag)

    assert handler.status == HTTPStatus.NOT_MODIFIED
    assert handler.body.getvalue() == b""
    assert set(handler.headers) == {"ETag", "Cache-Control"}
    assert handler.headers["ETag"] == etag
    assert handler.headers["Cache-Control"] == "no-cache"


def test_static_asset_conditional_mismatch_returns_full_body():
    asset = STATIC_ROOT / "app.js"
    handler = _StaticHandler()

    send_static_asset(handler, "app.js", if_none_match='"stale-etag"')

    body = asset.read_bytes()
    assert handler.status == HTTPStatus.OK
    assert handler.body.getvalue() == body
    assert handler.headers["ETag"] != '"stale-etag"'


def test_static_asset_rejects_shared_prefix_sibling_paths(tmp_path, monkeypatch):
    static_root = tmp_path / "static"
    static_root.mkdir()
    static_xyz = tmp_path / "staticXYZ"
    static_xyz.mkdir()
    (static_xyz / "secret").write_text("secret", encoding="utf-8")
    static_backup = tmp_path / "static-backup"
    static_backup.mkdir()
    (static_backup / "x").write_text("backup", encoding="utf-8")
    monkeypatch.setattr(serve_web, "STATIC_ROOT", static_root)

    for name in ("../staticXYZ/secret", "../static-backup/x"):
        handler = _StaticHandler()

        send_static_asset(handler, name)

        assert handler.status == HTTPStatus.NOT_FOUND
        assert handler.headers == {}
        assert handler.body.getvalue() == b""
