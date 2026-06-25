"""Serve work-route, live-bus, and static-route contracts."""

from __future__ import annotations

from http import HTTPStatus
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from spice.agent.renewal import (
    RENEWAL_HANDOFF_REQUEST_SUFFIX,
    renewal_rehydration_text,
)
from spice.mail.inbox import (
    INBOX_CONTROL_DRAIN_QUEUE,
    INBOX_CREDIT_FAILURE_DEADLETTER_THRESHOLD,
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    compose_inbox_text,
    inbox_payload_rows,
    inbox_request_body,
    parse_inbox_payload,
    pending_inbox_count,
    write_inbox_item,
)
from spice.serve import agentapi, web as serve_web
from spice.serve.payload import identity, lane, message
from spice.serve.app import (
    team_command_response_payload,
    team_snapshot_response_payload,
)
from spice.serve.livebus import LiveBusCallbacks, LiveBusSession
from spice.serve.web import STATIC_ROOT, render_index_html, send_static_asset
from spice.serve.workroutes import (
    work_tree_send_response_payload,
    work_tree_send_accepted_response_payload,
    work_tree_task_drain_response_payload,
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


def test_work_tree_send_accepted_response_does_not_ensure_synchronously(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    ensure_calls: list[dict[str, object]] = []

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        raise AssertionError("accepted send must not ensure synchronously")

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

    payload, status = work_tree_send_accepted_response_payload(
        state,
        target,
        {
            "text": "> > quoted context\n> > with newline\n\nwake this lane",
            "fastMode": True,
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
    assert payload["agentEnsure"] == {}
    assert payload["pendingInboxCount"] == 1
    assert payload["pendingInboxLabel"] == "1"
    assert payload["pendingInboxKeys"] == [payload["key"]]
    assert payload["pendingInboxRevision"]
    assert payload["pendingInboxVersion"] > 0
    assert inbox_request_body(items[0].text) == (
        "> > quoted context\n> > with newline\n\nwake this lane"
    )
    assert ensure_calls == []


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

    payload, status = work_tree_send_response_payload(
        state,
        target,
        {"text": "continue from handoff", "fastMode": True},
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
            "configPatch": {"selectedView": "metrics"},
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
    assert fresh_snapshot["changed"] is False
    assert fresh_snapshot["revision"] == advanced["revision"]
    current = team_snapshot_response_payload(state, since_revision=None)
    assert current["snapshot"]["teams"][0]["config"]["lifetime"] == "Drive"
    assert current["snapshot"]["teams"][0]["config"]["selectedView"] == "compose"
    unchanged = team_snapshot_response_payload(
        state, since_revision=advanced["revision"]
    )
    assert unchanged["changed"] is False


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
            "agentIds": [ACTOR_A, target_actor],
        },
    )

    members = [
        member["agentId"] for member in reorder["snapshot"]["teams"][0]["members"]
    ]
    assert create_status == HTTPStatus.OK
    assert reorder_status == HTTPStatus.OK
    assert members == [ACTOR_A, target_actor]


def test_messages_refresh_wakes_stopped_agent_for_cli_written_inbox(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    write_inbox_item(
        repo,
        "20260101T000000000001Z.txt",
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
    assert ensure_calls == [{"target": target, "fast_mode": False, "force_new": False}]
    assert state.pending_agent_ensure_attempts[target.id] > 0


def test_pending_inbox_deadletters_after_credit_failure(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    write_inbox_item(
        repo,
        "20260101T000000000001Z.txt",
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

    assert ensure_calls == INBOX_CREDIT_FAILURE_DEADLETTER_THRESHOLD
    assert payload["agentEnsure"]["deadletteredInboxKey"] == "20260101T000000000001Z"
    assert (
        payload["agentEnsure"]["deadletterRequeueCommand"]
        == "spice agent requeue-deadletter 20260101T000000000001Z"
    )
    assert (
        payload["agentEnsure"]["creditFailureThreshold"]
        == INBOX_CREDIT_FAILURE_DEADLETTER_THRESHOLD
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
        "20260101T000000000001Z.txt"
    ]


def test_pending_inbox_deadletters_after_generic_ensure_failure(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    write_inbox_item(
        repo,
        "20260101T000000000002Z.txt",
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
    assert payload["agentEnsure"]["deadletteredInboxKey"] == "20260101T000000000002Z"
    assert (
        payload["agentEnsure"]["deadletterRequeueCommand"]
        == "spice agent requeue-deadletter 20260101T000000000002Z"
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
        "20260101T000000000002Z.txt"
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
    return LiveBusCallbacks(
        resolve_target=lambda selector: target if selector == target.id else None,
        work_trees_payload=lambda: {"workTrees": []},
        messages_payload=messages_payload,
        send_payload=lambda _target, payload: (
            calls.append(("send", payload)) or {"ok": True, "key": "inbox-key"},
            HTTPStatus.OK,
        ),
        task_drain_payload=lambda _target, payload: (
            calls.append(("taskDrain", payload)) or {"ok": True, "route": {}},
            HTTPStatus.OK,
        ),
        team_snapshot_payload=lambda since_revision: {
            "ok": True,
            "revision": since_revision or 0,
        },
        team_command_payload=lambda payload: (
            calls.append(("teamCommand", payload)) or {"ok": True, "revision": 2},
            HTTPStatus.OK,
        ),
        metric_series_payload=lambda _query: {"ok": True, "points": []},
        thread_id=lambda _target: "thread",
        transcript_resolution=lambda _thread_id: _transcript_resolution(
            "thread", Path("rollout.jsonl")
        ),
        lane_watch_paths=lambda *_args: (),
        lane_signature=lambda *_args: (),
    )


def test_livebus_routes_send_task_drain_team_command_and_history_requests():
    target = _BusTarget(id="lane")
    connection = _Connection()
    calls: list[tuple[str, dict[str, Any]]] = []

    def messages_payload(_target, **kwargs):
        calls.append(("messages", kwargs))
        return {"messages": [{"key": "m1"}], "statusLine": {}}

    callbacks = _route_test_livebus_callbacks(target, calls, messages_payload)
    session = LiveBusSession(connection, callbacks)

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
    assert connection.sent == [
        {
            "type": "lane.sendResult",
            "result": {"ok": True, "key": "inbox-key"},
            "requestId": "send-1",
        },
        {
            "type": "lane.taskDrainResult",
            "result": {"ok": True, "route": {}},
            "requestId": "drain-1",
        },
        {
            "type": "teams.commandResult",
            "result": {"ok": True, "revision": 2},
            "requestId": "team-1",
        },
        {
            "type": "lane.payload",
            "payload": {"messages": [{"key": "m1"}], "statusLine": {}},
            "requestId": "history-1",
        },
    ]
    assert calls == [
        ("send", {"text": "hello"}),
        ("taskDrain", {"replaceTaskFilters": True}),
        ("teamCommand", {"command": "createTeam"}),
        (
            "messages",
            {"limit": 9, "before": "oldest", "expected_thread_id": "thread"},
        ),
    ]


def test_livebus_routes_metric_series_requests():
    query = {"metric": "activity", "agentId": "agent-a", "start": 0, "end": 60}
    connection = _Connection()
    calls: list[dict[str, Any]] = []
    callbacks = LiveBusCallbacks(
        resolve_target=lambda _selector: None,
        work_trees_payload=lambda: {"workTrees": []},
        messages_payload=lambda _target, **_kwargs: {},
        send_payload=lambda _target, _payload: ({}, HTTPStatus.OK),
        task_drain_payload=lambda _target, _payload: ({}, HTTPStatus.OK),
        team_snapshot_payload=lambda _since_revision: {},
        team_command_payload=lambda _payload: ({}, HTTPStatus.OK),
        metric_series_payload=lambda payload: (
            calls.append(payload) or {"ok": True, "points": []}
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
            "result": {"ok": True, "points": []},
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
