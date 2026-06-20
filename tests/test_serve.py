"""Serve app and live-bus contracts."""

from __future__ import annotations

import json
import hashlib
import socket
import subprocess
from dataclasses import dataclass
from http import HTTPStatus
from io import BytesIO
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from spice.agent.driver import CODEX_DRIVER
from spice.agent.renewal import (
    RENEWAL_HANDOFF_REQUEST_SUFFIX,
    renewal_rehydration_text,
)
from spice.cli.parser import build_parser
from spice.mail.acks import archive_ackd_inbox_items
from spice.mail.inbox import (
    INBOX_CONTROL_DRAIN_QUEUE,
    INBOX_CREDIT_FAILURE_DEADLETTER_THRESHOLD,
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    compose_inbox_text,
    inbox_dir,
    inbox_item_key,
    inbox_payload_rows,
    inbox_request_body,
    parse_inbox_payload,
    pending_inbox_count,
    write_inbox_item,
)
from spice.paths import shared_attachment_root
from spice.serve import agentapi, app, payloads
from spice.serve.app import (
    ServeState,
    team_command_response_payload,
    team_snapshot_response_payload,
    work_tree_send_response_payload,
    work_tree_task_drain_response_payload,
)
from spice.serve.livebus import LiveBusCallbacks, LiveBusSession
from spice.serve.teams import ServeTeamStore, TeamCommandService
from spice.serve.web import STATIC_ROOT, render_index_html, send_static_asset
from spice.serve.worktrees import WorktreeTarget
from spice.tasks import config as task_config

IMAGE_DATA_URL = "data:image/png;base64,aW1hZ2UtYnl0ZXM="
THREAD_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
THREAD_B = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"


@dataclass(frozen=True)
class _BusTarget:
    id: str


class _Connection:
    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []

    def send_json(self, payload: dict[str, Any]) -> None:
        self.sent.append(payload)


class _StaticHandler:
    def __init__(self) -> None:
        self.status: HTTPStatus | None = None
        self.headers: dict[str, str] = {}
        self.body = BytesIO()
        self.wfile = self.body

    def send_error(self, status: HTTPStatus) -> None:
        self.status = status

    def send_response(self, status: HTTPStatus) -> None:
        self.status = status

    def send_header(self, name: str, value: str) -> None:
        self.headers[name] = value

    def end_headers(self) -> None:
        pass


class _ImageHandler(_StaticHandler):
    def __init__(self, state: ServeState) -> None:
        super().__init__()
        self.server = SimpleNamespace(spice_state=state)

    @property
    def state(self) -> ServeState:
        return self.server.spice_state

    def send_error(self, status: HTTPStatus, *_args: object) -> None:
        self.status = status

    def _send_bytes(self, data: bytes, content_type: str) -> None:
        app._ServeHandler._send_bytes(self, data, content_type)


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
    assert expected_until_help in flat_help


def test_serve_parser_accepts_until_path(tmp_path):
    stop_path = tmp_path / "serve.stop"

    args = build_parser().parse_args(["serve", "--until", str(stop_path)])

    assert args.command == "serve"
    assert args.until == stop_path


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
    team_path = state.team_store.path
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
        team_path,
        team_path.with_name(f"{team_path.name}-wal"),
        team_path.with_name(f"{team_path.name}-shm"),
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
    live_attachment = payload["attachments"][0]
    assert live_attachment["name"] == "paste.png"
    assert live_attachment["url"].startswith(
        f"/api/work/trees/{target.id}/files/image?path="
    )
    refresh_payload = payloads.ack_context_payload_for_worktree(
        state, target, keys=[payload["key"]]
    )
    assert refresh_payload["acks"][0]["found"] is True
    assert refresh_payload["acks"][0]["attachments"][0] == live_attachment
    assert archive_ackd_inbox_items(repo, [payload["key"]]) == [payload["key"]]
    archived_refresh_payload = payloads.ack_context_payload_for_worktree(
        state, target, keys=[payload["key"]]
    )
    assert archived_refresh_payload["acks"][0]["found"] is True
    assert archived_refresh_payload["acks"][0]["attachments"][0] == live_attachment
    assert state.lane_send_count(target.id) == 1
    assert state.team_store.lane_metric_summary(THREAD_A, bucket_count=12).sends == 1
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
        payloads, "resolve_thread_id_for_target", lambda *_args: THREAD_A
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
    monkeypatch.setattr(payloads, "resolve_thread_id_for_target", lambda *_: THREAD_A)
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


def test_running_requested_renewal_sends_handoff_and_marks_pending(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    state.team_store.create_team(members=[THREAD_A])
    state.team_store.set_agent_renewal_request(THREAD_A, requested=True)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)

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
            (THREAD_A,),
        ).fetchone()
    assert status == HTTPStatus.OK
    assert payload["agentEnsure"] == {}
    assert payload["requestText"] == "wrap this up"
    assert payload["requestHtml"] == "<p>wrap this up</p>"
    assert payload["attachments"][0]["name"] == "paste.png"
    assert RENEWAL_HANDOFF_REQUEST_SUFFIX in inbox_request_body(item.text)
    assert payload["renewalIntent"]["requested"] is False
    assert payload["renewalIntent"]["state"] == "pending"
    assert renewal["state"] == "pending"
    assert renewal["ancestor_thread_id"] == THREAD_A
    assert renewal["successor_agent_id"] == ""


def test_stopped_requested_renewal_starts_successor_and_moves_team_membership(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[THREAD_A])
    state.team_store.set_agent_renewal_request(THREAD_A, requested=True)
    ensure_calls: list[dict[str, object]] = []
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)

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
    assert renewal_rehydration_text(THREAD_A) in body
    assert ensure_calls == [
        {
            "target": target,
            "fast_mode": True,
            "force_new": True,
        }
    ]
    assert state.team_store.current_team_for_agent(THREAD_A) is None
    assert state.team_store.current_team_for_agent(THREAD_B) == created.team_id


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

    team_id = state.team_store.current_team_for_agent(THREAD_A)
    assert status == HTTPStatus.OK
    assert payload["route"]["actor"] == THREAD_A
    assert payload["route"]["teamIdentity"]["teamId"] == team_id
    assert payload["route"]["taskFilters"] == ["serve", "task.review"]
    assert payload["route"]["lifetime"] == "Drive"
    assert payload["route"]["memberAgents"] == [THREAD_A]


def test_team_command_payloads_report_revisions_and_stale_valid_command_applies(
    tmp_path,
):
    state = _serve_state(tmp_path, _target(_repo(tmp_path)))
    created, create_status = team_command_response_payload(
        state,
        {
            "command": "createTeam",
            "members": [THREAD_A],
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
    assert stale_status == HTTPStatus.OK
    assert stale["revision"] > advanced["revision"]
    assert stale["snapshot"]["teams"][0]["config"]["lifetime"] == "Drive"
    assert stale["snapshot"]["teams"][0]["config"]["selectedView"] == "metrics"
    assert fresh_snapshot["changed"] is True
    assert fresh_snapshot["revision"] == stale["revision"]
    unchanged = team_snapshot_response_payload(state, since_revision=stale["revision"])
    assert unchanged["changed"] is False


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

    payload = payloads.messages_payload_for_worktree(state, target, limit=5)

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

    payload = payloads.messages_payload_for_worktree(state, target, limit=5)

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

    payload = payloads.messages_payload_for_worktree(state, target, limit=5)

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
    monkeypatch.setattr(payloads, "agent_status", lambda *_args, **_kwargs: status)

    line = payloads.status_line_payload(state, target, items=[], error=None)

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
    monkeypatch.setattr(payloads, "agent_status", lambda *_args, **_kwargs: status)

    line = payloads.status_line_payload(state, target, items=[], error=None)

    assert line["bindingStatus"] == "bound"
    assert line["bindingError"] == ""
    assert line["rolloutStatus"] == "ok"


def test_livebus_routes_send_task_drain_team_command_and_history_requests():
    target = _BusTarget(id="lane")
    connection = _Connection()
    calls: list[tuple[str, dict[str, Any]]] = []

    def messages_payload(_target, **kwargs):
        calls.append(("messages", kwargs))
        return {"messages": [{"key": "m1"}], "statusLine": {}}

    callbacks = LiveBusCallbacks(
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
        thread_id=lambda _target: "thread",
        transcript_resolution=lambda _thread_id: _transcript_resolution(
            "thread", Path("rollout.jsonl")
        ),
        lane_watch_paths=lambda *_args: (),
        lane_signature=lambda *_args: (),
    )
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


def _repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir(exist_ok=True)
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    return repo


def _target(repo: Path) -> WorktreeTarget:
    return WorktreeTarget(id="target-1", repo_root=repo, name=repo.name, branch="main")


def _transcript_resolution(thread_id: str, path: Path) -> app.TranscriptResolution:
    return app.TranscriptResolution(
        thread_id=thread_id,
        path=path,
        owner_driver=CODEX_DRIVER,
    )


def _serve_state(tmp_path: Path, target: WorktreeTarget) -> ServeState:
    state = ServeState(anchor_root=tmp_path)
    state.cached_targets = [target]
    state.team_store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    state.team_commands = TeamCommandService(state.team_store)
    return state


def _patch_agent_status(monkeypatch, *, thread_id: str, running: bool) -> None:
    status = SimpleNamespace(
        running=running,
        thread_id=thread_id,
        process_status="running" if running else "idle",
        pid=123 if running else 0,
        process_group_id=123 if running else 0,
        model="gpt-test",
        reasoning_effort="low",
        service_tier="fast",
        started_at="",
        log_path=None,
        prompt_skill_path=None,
    )
    monkeypatch.setattr(app, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(agentapi, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(payloads, "agent_status", lambda *_args, **_kwargs: status)
