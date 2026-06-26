"""Serve app and live-bus contracts."""

from __future__ import annotations

import http.client
import json
import hashlib
import socket
import threading
from http import HTTPStatus
from pathlib import Path

import pytest

from spice.cli.parser import build_parser
from spice.mail.acks import archive_ackd_inbox_items
from spice.mail.inbox import (
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    compose_inbox_text,
    inbox_dir,
    inbox_item_key,
    inbox_request_body,
    pending_inbox_count,
    write_inbox_item,
)
from spice.paths import shared_attachment_root
from spice.serve import agentapi, app
from spice.serve.payload import identity, message
from spice.serve.workroutes import work_tree_send_response_payload
from spice.tasks import config as task_config
from tests.test_servehelpers import (
    ACTOR_A,
    IMAGE_DATA_URL,
    TEAM_HISTORICAL_TEST_BUCKET_COUNT,
    THREAD_A,
    _ImageHandler,
    _patch_agent_status,
    _repo,
    _serve_state,
    _target,
    _transcript_resolution,
)


def test_serve_parser_exposes_until_path_help(capsys):
    with pytest.raises(SystemExit) as exc:
        build_parser().parse_args(["serve", "--help"])

    assert exc.value.code == 0
    help_text = capsys.readouterr().out
    flat_help = " ".join(help_text.split())
    expected_until_help = (
        "Watch PATH and stop the server after it is touched or changed."
    )
    assert "--until PATH" in help_text
    assert "--allow-insecure-bind" in help_text
    assert "--auth-token TOKEN" in help_text
    assert "Origin-equals-Host" in flat_help
    assert "rebinding-resistant authority match" in flat_help
    assert "supplied token" in flat_help
    assert "is the operative defense" in flat_help
    assert expected_until_help in flat_help


def test_serve_parser_accepts_until_path(tmp_path):
    stop_path = tmp_path / "serve.stop"

    args = build_parser().parse_args(["serve", "--until", str(stop_path)])

    assert args.command == "serve"
    assert args.until == stop_path


def test_serve_parser_accepts_bind_guard_options():
    args = build_parser().parse_args(
        [
            "serve",
            "--host",
            "0.0.0.0",
            "--allow-insecure-bind",
            "--auth-token",
            "secret",
        ]
    )

    assert args.command == "serve"
    assert args.host == "0.0.0.0"
    assert args.allow_insecure_bind is True
    assert args.auth_token == "secret"


def test_serve_auth_token_protects_http_requests(tmp_path):
    state = app.ServeState(anchor_root=tmp_path, auth_token="secret")
    server = app._ServeHttpServer(("127.0.0.1", 0), app._ServeHandler, state)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    host, port = server.server_address[:2]

    try:
        conn = http.client.HTTPConnection(host, port, timeout=2)
        conn.request("GET", "/")
        response = conn.getresponse()
        body = response.read().decode("utf-8")
        conn.close()
        assert response.status == HTTPStatus.UNAUTHORIZED
        assert "spice serve auth token required" in body

        conn = http.client.HTTPConnection(host, port, timeout=2)
        conn.request("GET", "/?token=secret")
        response = conn.getresponse()
        html = response.read().decode("utf-8")
        cookie = response.getheader("Set-Cookie") or ""
        conn.close()
        assert response.status == HTTPStatus.OK
        assert "<!doctype html>" in html
        assert app.SERVE_AUTH_COOKIE_NAME in cookie

        conn = http.client.HTTPConnection(host, port, timeout=2)
        conn.request("GET", "/", headers={"Cookie": cookie.split(";", 1)[0]})
        response = conn.getresponse()
        response.read()
        conn.close()
        assert response.status == HTTPStatus.OK
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


def test_wildcard_bind_websocket_upgrade_requires_auth_token(tmp_path):
    state = app.ServeState(anchor_root=tmp_path, auth_token="secret")
    server = app._ServeHttpServer(("0.0.0.0", 0), app._ServeHandler, state)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    _host, port = server.server_address[:2]
    authority = f"evil.example:{port}"

    try:
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
        conn.request(
            "GET",
            "/api/live/bus",
            headers={
                "Host": authority,
                "Origin": f"http://{authority}",
                "Connection": "Upgrade",
                "Upgrade": "websocket",
                "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
                "Sec-WebSocket-Version": "13",
            },
        )
        response = conn.getresponse()
        body = response.read().decode("utf-8")
        conn.close()

        assert response.status == HTTPStatus.UNAUTHORIZED
        assert "spice serve auth token required" in body
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


def test_serve_handler_closes_socket_reader_after_request_line_timeout():
    server_socket, client_socket = socket.socketpair()
    server_socket.settimeout(0.001)
    reader = server_socket.makefile("rb")
    handler = object.__new__(app._ServeHandler)

    try:
        handler.rfile = reader
        handler.close_connection = False

        app._ServeHandler.handle_one_request(handler)
        assert handler.close_connection is True

        handler.close_connection = False
        app._ServeHandler.handle_one_request(handler)
        assert handler.close_connection is True
    finally:
        reader.close()
        server_socket.close()
        client_socket.close()


def test_lane_watch_paths_use_exact_live_bus_sources(tmp_path):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    backend = tmp_path / "task-backend"

    task_config.set_backend(str(backend))
    try:
        event_path = task_config.task_event_path()
        paths = app.lane_watch_paths_for_target(
            state,
            target,
            THREAD_A,
            _transcript_resolution(THREAD_A, transcript),
        )
    finally:
        task_config.set_backend(None)

    assert inbox_dir(repo).is_dir()
    assert paths == (
        inbox_dir(repo),
        event_path,
        transcript,
    )


def test_work_tree_send_writes_inbox_and_returns_attachment_payload(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)

    payload, status = work_tree_send_response_payload(
        state,
        target,
        {
            "text": "inspect this image",
            "noSay": True,
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
    assert payload["requestText"] == "inspect this image"
    assert payload["noSay"] is True
    assert payload["pendingInboxCount"] == 1
    assert payload["pendingInboxKeys"] == [inbox_item_key(items[0].name)]
    assert payload["pendingInboxRevision"]
    assert payload["pendingInboxVersion"] > 0
    live_attachment = payload["attachments"][0]
    assert live_attachment["name"] == "paste.png"
    assert live_attachment["url"].startswith(
        f"/api/work/trees/{target.id}/files/image?path="
    )
    refresh_payload = message.ack_context_payload_for_worktree(
        state, target, keys=[payload["key"]]
    )
    assert refresh_payload["acks"][0]["found"] is True
    assert refresh_payload["acks"][0]["attachments"][0] == live_attachment
    assert archive_ackd_inbox_items(repo, [payload["key"]]) == [payload["key"]]
    archived_refresh_payload = message.ack_context_payload_for_worktree(
        state, target, keys=[payload["key"]]
    )
    assert archived_refresh_payload["acks"][0]["found"] is True
    assert archived_refresh_payload["acks"][0]["attachments"][0] == live_attachment
    assert state.team_store.lane_metric_summary(ACTOR_A, bucket_count=12).sends == 1
    assert pending_inbox_count(repo) == 0
    assert inbox_request_body(items[0].text) == "inspect this image"
    assert items[0].attachments[0].path.read_bytes() == b"image-bytes"
    assert shared_attachment_root(repo) in items[0].attachments[0].path.parents
    attachment_path = Path(live_attachment["path"])
    assert attachment_path.is_absolute()
    assert shared_attachment_root(repo) in attachment_path.parents

    handler = _ImageHandler(state)
    app._ServeHandler._send_worktree_image(
        handler,
        target,
        {"path": [live_attachment["path"]]},
    )

    assert handler.status == HTTPStatus.OK
    assert handler.headers["Content-Type"] == "image/png"
    assert handler.body.getvalue() == b"image-bytes"


def test_work_tree_send_reuses_pending_key_for_exact_duplicate_text(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)

    first_payload, first_status = work_tree_send_response_payload(
        state,
        target,
        {"text": "same steering"},
    )
    second_payload, second_status = work_tree_send_response_payload(
        state,
        target,
        {"text": "same steering"},
    )
    items = collect_inbox_items(repo)

    assert first_status == HTTPStatus.OK
    assert second_status == HTTPStatus.OK
    assert first_payload["key"] == second_payload["key"]
    assert second_payload["pendingInboxKeys"] == [first_payload["key"]]
    assert [inbox_request_body(item.text) for item in items] == ["same steering"]
    assert pending_inbox_count(repo) == 1


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
        "20260102T000000000001Z.txt",
        compose_inbox_text(body="automated maxim", priority="maxim", stop=False),
    )
    write_inbox_item(
        repo,
        "20260102T000000000002Z.txt",
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
        "20260102T000000000001Z.txt",
        compose_inbox_text(body="automated maxim", priority="maxim", stop=False),
    )
    write_inbox_item(
        repo,
        "20260102T000000000002Z.txt",
        compose_inbox_text(
            body="automated review feedback", priority="review", stop=False
        ),
    )
    write_inbox_item(
        repo,
        "20260102T000000000003Z.txt",
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

    assert payload["deadletteredInboxKey"] == "20260102T000000000003Z"
    assert [item.name for item in collect_inbox_items(repo)] == [
        "20260102T000000000001Z.txt",
        "20260102T000000000002Z.txt",
    ]
    assert [item.name for item in collect_deadlettered_inbox_items(repo)] == [
        "20260102T000000000003Z.txt"
    ]


def test_serve_metrics_text_reports_gauges_and_request_counters(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    rollout = tmp_path / "rollout.jsonl"
    rollout.write_text("{}\n", encoding="utf-8")
    write_inbox_item(
        repo,
        "20260102T000000000001Z.txt",
        compose_inbox_text(body="pending", priority=None, stop=False),
    )
    monkeypatch.setattr(
        identity, "resolve_thread_id_for_target", lambda *_args: THREAD_A
    )
    monkeypatch.setattr(
        app,
        "resolve_thread_transcript",
        lambda _thread, _repo_root: _transcript_resolution(THREAD_A, rollout),
    )
    state.record_http_request("GET", "/")
    state.record_http_request("GET", f"/api/work/trees/{target.id}/acks")
    state.record_http_request("GET", f"/api/work/trees/{target.id}/not-a-route")
    state.record_http_request("POST", "/api/teams/command")
    state.record_http_request("GET", "/unmatched")
    state.record_http_request("GET", "/metrics")

    text = app.serve_metrics_text(state)

    assert "# TYPE spice_serve_bound gauge" in text
    assert "spice_serve_bound 1\n" in text
    assert "spice_serve_pending_inbox_items 1\n" in text
    assert "spice_serve_rollout_present 1\n" in text
    assert (
        'spice_serve_http_requests_total{method="GET",path="/api/work/trees/{id}/acks"} 1'
        in text
    )
    assert (
        'spice_serve_http_requests_total{method="GET",path="/api/work/trees/{id}/other"} 1'
        in text
    )
    assert (
        'spice_serve_http_requests_total{method="POST",path="/api/teams/command"} 1'
        in text
    )
    assert 'spice_serve_http_requests_total{method="GET",path="other"} 1' in text
    assert 'spice_serve_http_requests_total{method="GET",path="/metrics"} 1' in text
    assert "not-a-route" not in text


def test_serve_metrics_path_templates_bound_cardinality():
    assert app.serve_metrics_path_template("/") == "/"
    assert app.serve_metrics_path_template("/metrics") == "/metrics"
    assert (
        app.serve_metrics_path_template("/api/teams/team-a/metrics?start=0")
        == "/api/teams/{id}/metrics"
    )
    assert (
        app.serve_metrics_path_template("/api/metrics/tasks/burndown?teamId=team-a")
        == "/api/metrics/tasks/burndown"
    )
    assert (
        app.serve_metrics_path_template("/api/metrics/tasks/distribution?teamId=team-a")
        == "/api/metrics/tasks/distribution"
    )
    assert (
        app.serve_metrics_path_template("/api/work/trees/main/agent/status")
        == "/api/work/trees/{id}/agent/status"
    )
    assert (
        app.serve_metrics_path_template("/api/work/trees/main/messages?limit=1")
        == "/api/work/trees/{id}/messages"
    )
    assert (
        app.serve_metrics_path_template("/api/work/trees/main/not-a-route")
        == "/api/work/trees/{id}/other"
    )
    assert app.serve_metrics_path_template("/static/index.css") == "/static/{asset}"
    assert app.serve_metrics_path_template("/elsewhere") == "other"


def test_team_historical_metrics_endpoint_projects_membership_intervals(
    tmp_path, monkeypatch
):
    clock = {"now": 0.0}
    monkeypatch.setattr("spice.serve.team.store.time.time", lambda: clock["now"])
    repo = _repo(tmp_path)
    state = _serve_state(tmp_path, _target(repo))

    def set_time(timestamp: float) -> None:
        clock["now"] = timestamp

    set_time(0)
    team = state.team_store.create_team(members=["agent-a", "agent-b"])
    state.team_store.record_agent_metric_delta("agent-a", message_timestamps=[60])
    state.team_store.record_agent_metric_delta("agent-b", message_timestamps=[60])

    set_time(120)
    split = state.team_store.split_team(
        team.team_id, agent_ids=["agent-a"], new_team_id="team-split"
    )
    state.team_store.record_agent_metric_delta("agent-a", message_timestamps=[180])
    state.team_store.record_agent_metric_delta("agent-b", message_timestamps=[180])

    set_time(240)
    state.team_store.merge_teams(split.team_id, team.team_id)
    state.team_store.record_agent_metric_delta("agent-a", message_timestamps=[300])
    state.team_store.record_agent_metric_delta("agent-b", message_timestamps=[300])

    set_time(360)
    state.team_store.split_team_back(team.team_id)
    state.team_store.record_agent_metric_delta("agent-a", message_timestamps=[420])
    state.team_store.record_agent_metric_delta("agent-b", message_timestamps=[420])

    set_time(480)
    state.team_store.remove_agent(team.team_id, "agent-b")
    state.team_store.record_agent_metric_delta("agent-b", message_timestamps=[540])

    set_time(600)
    state.team_store.close_team(split.team_id)
    state.team_store.record_agent_metric_delta("agent-a", message_timestamps=[660])

    query = {"start": ["0"], "end": ["720"], "bucketSeconds": ["60"]}
    team_payload = app.team_historical_metrics_response_payload(
        state, team.team_id, query
    )
    split_payload = app.team_historical_metrics_response_payload(
        state, split.team_id, query
    )
    narrow_payload = app.team_historical_metrics_response_payload(
        state,
        team.team_id,
        {"start": ["120"], "end": ["360"], "bucketSeconds": ["60"]},
    )

    assert team_payload["ok"] is True
    assert team_payload["lens"] == "team-historical"
    assert team_payload["teamId"] == team.team_id
    assert team_payload["agentIds"] == ["agent-a", "agent-b"]
    assert team_payload["messages"] == 6
    assert team_payload["cumulativeMessages"] == 6
    assert team_payload["range"] == {"start": 0, "end": 720}
    assert team_payload["bucketCount"] == TEAM_HISTORICAL_TEST_BUCKET_COUNT
    assert sum(team_payload["sparkline"]) == 6
    assert sum(point["messages"] for point in team_payload["series"]) == 6

    assert split_payload["teamId"] == split.team_id
    assert split_payload["agentIds"] == ["agent-a"]
    assert split_payload["messages"] == 2
    assert sum(split_payload["sparkline"]) == 2

    handler = _ImageHandler(state)
    app._ServeHandler._get_team_metrics(
        handler,
        team.team_id,
        "start=0&end=720&bucketSeconds=60",
    )
    endpoint_payload = json.loads(handler.body.getvalue())

    assert handler.status == HTTPStatus.OK
    assert handler.headers["Content-Type"].startswith("application/json")
    assert endpoint_payload["lens"] == "team-historical"
    assert endpoint_payload["teamId"] == team.team_id
    assert endpoint_payload["messages"] == 6

    assert narrow_payload["messages"] == 3
    assert narrow_payload["cumulativeMessages"] == 5
    assert narrow_payload["range"] == {"start": 120, "end": 360}
    assert sum(point["messages"] for point in narrow_payload["series"]) == 3


@pytest.mark.parametrize(
    ("query", "error_text"),
    [
        ("end=inf", "finite"),
        ("now=nan", "finite"),
        ("start=inf&end=120", "finite"),
        (
            "start=0&end="
            f"{app.TEAM_HISTORICAL_MAX_BUCKET_COUNT * app.METRIC_BUCKET_SECONDS}"
            f"&bucketSeconds={app.METRIC_BUCKET_SECONDS}",
            "range exceeds",
        ),
        (
            f"bucketCount={app.TEAM_HISTORICAL_MAX_BUCKET_COUNT + 1}",
            "bucketCount exceeds",
        ),
    ],
)
def test_team_historical_metrics_endpoint_rejects_unbounded_queries(
    tmp_path, query, error_text
):
    repo = _repo(tmp_path)
    state = _serve_state(tmp_path, _target(repo))
    handler = _ImageHandler(state)

    app._ServeHandler._get_team_metrics(handler, "team-a", query)
    payload = json.loads(handler.body.getvalue())

    assert handler.status == HTTPStatus.BAD_REQUEST
    assert payload["ok"] is False
    assert error_text in payload["error"]


def test_task_burndown_metrics_endpoint_projects_team_and_agent_series(
    tmp_path,
):
    repo = _repo(tmp_path)
    state = _serve_state(tmp_path, _target(repo))
    store = state.team_store
    store.record_task_lifecycle_event(
        "complete", task_id="task-a", agent_id="agent-a", team_id="team-a", ts=60
    )
    store.record_task_lifecycle_event(
        "drain", task_id="task-a", agent_id="agent-a", team_id="team-a", ts=61
    )
    store.record_task_lifecycle_event(
        "complete", task_id="task-b", agent_id="agent-b", team_id="team-a", ts=120
    )
    store.record_task_lifecycle_event(
        "drain", task_id="task-c", agent_id="agent-a", team_id="team-b", ts=180
    )

    team_payload = app.task_burndown_metrics_response_payload(
        state,
        {
            "teamId": ["team-a"],
            "start": ["0"],
            "end": ["180"],
            "bucketSeconds": ["60"],
        },
    )
    agent_payload = app.task_burndown_metrics_response_payload(
        state,
        {
            "agentId": ["agent-a"],
            "start": ["0"],
            "end": ["180"],
            "bucketSeconds": ["60"],
        },
    )
    combined_payload = app.task_burndown_metrics_response_payload(
        state,
        {
            "agentId": ["agent-a"],
            "teamId": ["team-a"],
            "start": ["0"],
            "end": ["180"],
            "bucketSeconds": ["60"],
        },
    )

    assert team_payload["ok"] is True
    assert team_payload["lens"] == "task-burndown"
    assert team_payload["teamIds"] == ["team-a"]
    assert team_payload["agentIds"] == []
    assert team_payload["completed"] == 2
    assert team_payload["drained"] == 1
    assert team_payload["range"] == {"start": 0, "end": 180}
    assert team_payload["series"] == [
        {"bucketStart": 60, "completed": 1, "drained": 1},
        {"bucketStart": 120, "completed": 1, "drained": 0},
    ]

    assert agent_payload["agentIds"] == ["agent-a"]
    assert agent_payload["teamIds"] == []
    assert agent_payload["completed"] == 1
    assert agent_payload["drained"] == 2
    assert combined_payload["completed"] == 1
    assert combined_payload["drained"] == 1

    handler = _ImageHandler(state)
    app._ServeHandler._get_task_burndown_metrics(
        handler,
        "teamId=team-a&start=0&end=180&bucketSeconds=60",
    )
    endpoint_payload = json.loads(handler.body.getvalue())

    assert handler.status == HTTPStatus.OK
    assert endpoint_payload["lens"] == "task-burndown"
    assert endpoint_payload["completed"] == 2
    assert endpoint_payload["drained"] == 1


def test_task_burndown_metrics_endpoint_rejects_oversized_ranges(tmp_path):
    repo = _repo(tmp_path)
    state = _serve_state(tmp_path, _target(repo))
    handler = _ImageHandler(state)

    app._ServeHandler._get_task_burndown_metrics(
        handler,
        "teamId=team-a&start=0&end=120000&bucketSeconds=60",
    )
    payload = json.loads(handler.body.getvalue())

    assert handler.status == HTTPStatus.BAD_REQUEST
    assert payload["ok"] is False
    assert "exceeds" in payload["error"]


def _task_distribution_metrics_state(tmp_path):
    repo = _repo(tmp_path)
    state = _serve_state(tmp_path, _target(repo))
    store = state.team_store
    for kind, task_id, agent_id, team_id, ts in (
        ("claim", "task-a", "agent-a", "team-a", 60),
        ("phaseAdvance", "task-a", "agent-a", "team-a", 61),
        ("claim", "task-b", "agent-b", "team-a", 62),
        ("review", "task-b", "agent-b", "team-a", 120),
        ("claim", "task-c", "agent-a", "team-b", 180),
        ("complete", "task-a", "agent-a", "team-a", 180),
    ):
        store.record_task_lifecycle_event(
            kind, task_id=task_id, agent_id=agent_id, team_id=team_id, ts=ts
        )
    return state


def _task_distribution_work_rows(payload):
    return [
        {
            key: point[key]
            for key in ("bucketStart", "agentId", "claimed", "active", "work")
        }
        for point in payload["series"]
    ]


def test_task_distribution_metrics_endpoint_projects_per_agent_share(tmp_path):
    state = _task_distribution_metrics_state(tmp_path)
    team_payload = app.task_distribution_metrics_response_payload(
        state,
        {
            "teamId": ["team-a"],
            "start": ["0"],
            "end": ["180"],
            "bucketSeconds": ["60"],
        },
    )

    assert team_payload["ok"] is True
    assert team_payload["lens"] == "task-distribution"
    assert team_payload["teamIds"] == ["team-a"]
    assert team_payload["agentIds"] == []
    assert team_payload["claimed"] == 1
    assert team_payload["active"] == 4
    assert team_payload["work"] == 5
    assert team_payload["range"] == {"start": 0, "end": 180}
    assert team_payload["bucketCount"] == 4
    assert _task_distribution_work_rows(team_payload) == [
        {
            "bucketStart": 60,
            "agentId": "agent-a",
            "claimed": 0,
            "active": 1,
            "work": 1,
        },
        {
            "bucketStart": 60,
            "agentId": "agent-b",
            "claimed": 1,
            "active": 0,
            "work": 1,
        },
        {
            "bucketStart": 120,
            "agentId": "agent-a",
            "claimed": 0,
            "active": 1,
            "work": 1,
        },
        {
            "bucketStart": 120,
            "agentId": "agent-b",
            "claimed": 0,
            "active": 1,
            "work": 1,
        },
        {
            "bucketStart": 180,
            "agentId": "agent-b",
            "claimed": 0,
            "active": 1,
            "work": 1,
        },
    ]
    assert team_payload["series"][0]["share"] == pytest.approx(1 / 2)
    assert team_payload["series"][1]["share"] == pytest.approx(1 / 2)
    assert team_payload["series"][2]["share"] == pytest.approx(1 / 2)
    assert team_payload["series"][3]["share"] == pytest.approx(1 / 2)
    assert team_payload["series"][4]["share"] == pytest.approx(1.0)


def test_task_distribution_metrics_endpoint_filters_combined_agent_team(tmp_path):
    state = _task_distribution_metrics_state(tmp_path)
    combined_payload = app.task_distribution_metrics_response_payload(
        state,
        {
            "agentId": ["agent-a"],
            "teamId": ["team-a"],
            "start": ["0"],
            "end": ["180"],
            "bucketSeconds": ["60"],
        },
    )

    assert combined_payload["claimed"] == 0
    assert combined_payload["active"] == 2
    assert combined_payload["work"] == 2
    assert combined_payload["series"] == [
        {
            "bucketStart": 60,
            "agentId": "agent-a",
            "claimed": 0,
            "active": 1,
            "work": 1,
            "share": 1.0,
        },
        {
            "bucketStart": 120,
            "agentId": "agent-a",
            "claimed": 0,
            "active": 1,
            "work": 1,
            "share": 1.0,
        },
    ]


def test_task_distribution_metrics_route_projects_per_agent_share(tmp_path):
    state = _task_distribution_metrics_state(tmp_path)
    handler = _ImageHandler(state)
    app._ServeHandler._get_task_distribution_metrics(
        handler,
        "teamId=team-a&start=0&end=180&bucketSeconds=60",
    )
    endpoint_payload = json.loads(handler.body.getvalue())

    assert handler.status == HTTPStatus.OK
    assert endpoint_payload["lens"] == "task-distribution"
    assert endpoint_payload["claimed"] == 1
    assert endpoint_payload["active"] == 4


def test_message_image_route_accepts_zero_item_index(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    rollout = tmp_path / "rollout.jsonl"
    rollout.write_text(
        json.dumps(
            {
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "output": [
                        {
                            "type": "input_image",
                            "image_url": {"url": IMAGE_DATA_URL},
                        }
                    ],
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(identity, "resolve_thread_id_for_target", lambda *_: THREAD_A)
    monkeypatch.setattr(
        app,
        "resolve_thread_transcript",
        lambda _thread_id, _repo_root: _transcript_resolution(THREAD_A, rollout),
    )
    handler = _ImageHandler(state)

    app._ServeHandler._send_message_image(
        handler,
        target,
        {"offset": ["0"], "item": ["0"]},
    )

    assert handler.status == HTTPStatus.OK
    assert handler.headers["Content-Type"] == "image/png"
    assert handler.body.getvalue() == b"image-bytes"


def test_worktree_image_resolves_shared_attachment_reference(tmp_path):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    data = b"shared-image"
    digest = hashlib.sha256(data).hexdigest()
    shared = shared_attachment_root(repo) / digest / "01-image.png"
    shared.parent.mkdir(parents=True)
    shared.write_bytes(data)
    handler = _ImageHandler(state)

    app._ServeHandler._send_worktree_image(
        handler,
        target,
        {"path": [shared.as_posix()]},
    )

    assert handler.status == HTTPStatus.OK
    assert handler.headers["Content-Type"] == "image/png"
    assert handler.body.getvalue() == data


def test_worktree_image_resolves_absolute_shared_attachment_reference(tmp_path):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    data = b"absolute-shared-image"
    digest = hashlib.sha256(data).hexdigest()
    shared = shared_attachment_root(repo) / digest / "01-image.png"
    shared.parent.mkdir(parents=True)
    shared.write_bytes(data)
    handler = _ImageHandler(state)

    app._ServeHandler._send_worktree_image(
        handler,
        target,
        {"path": [shared.as_posix()]},
    )

    assert handler.status == HTTPStatus.OK
    assert handler.headers["Content-Type"] == "image/png"
    assert handler.body.getvalue() == data


def test_worktree_image_rejects_missing_shared_attachment_reference(tmp_path):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    handler = _ImageHandler(state)

    app._ServeHandler._send_worktree_image(
        handler,
        target,
        {
            "path": [
                (shared_attachment_root(repo) / "missing" / "01-image.png").as_posix()
            ]
        },
    )

    assert handler.status == HTTPStatus.NOT_FOUND
