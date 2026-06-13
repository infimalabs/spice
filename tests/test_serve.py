"""Serve app and live-bus contracts."""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass
from http import HTTPStatus
from io import BytesIO
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from spice.agent.renewal import (
    RENEWAL_HANDOFF_REQUEST_SUFFIX,
    renewal_rehydration_text,
)
from spice.cli.parser import build_parser
from spice.mail.inbox import (
    INBOX_CONTROL_DRAIN_QUEUE,
    collect_inbox_items,
    compose_inbox_text,
    inbox_payload_rows,
    inbox_request_body,
    parse_inbox_payload,
    pending_inbox_count,
    write_inbox_item,
)
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
    assert "--until PATH" in help_text
    assert "created," in help_text
    assert "deleted, touched, or changed" in help_text


def test_serve_parser_accepts_until_path(tmp_path):
    stop_path = tmp_path / "serve.stop"

    args = build_parser().parse_args(["serve", "--until", str(stop_path)])

    assert args.command == "serve"
    assert args.until == stop_path


def test_header_spice_menu_button_replaces_plus_and_fast_toggle():
    html = render_index_html()
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    app_js = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")
    button_start = css.index(".spice-menu-button {")
    button_end = css.index(".spice-menu-icon {", button_start)
    button_rules = css[button_start:button_end]

    assert 'id="fast-mode-toggle"' not in html
    assert 'class="add-lane"' not in html
    assert "<title>spice</title>" in html
    assert "Simultaneous Production, Integration, and Control Environment" not in html
    assert "<h1>spice</h1>" not in html
    assert ">+</button>" not in html
    assert 'id="open-lane" class="spice-menu-button"' in html
    assert 'aria-haspopup="menu" aria-expanded="false"' in html
    assert 'class="spice-menu-icon" aria-hidden="true">🌶️</span>' in html
    assert '<span class="spice-menu-label">spice</span>' in html
    assert 'querySelector("#fast-mode-toggle")' not in app_js
    assert 'openLaneButton.addEventListener("click", (event) => {' in app_js
    assert "button.primary:hover {\n  background: var(--accent-strong);" in css
    assert "button.primary:hover,\n.spice-menu-button:hover" not in css
    assert (
        "background: color-mix(in srgb, var(--control) 90%, var(--accent) 10%);"
        in button_rules
    )
    assert (
        "border-color: color-mix(in srgb, var(--border) 52%, transparent);"
        in button_rules
    )
    assert (
        "box-shadow: inset 0 0 0 1px "
        "color-mix(in srgb, var(--accent) 8%, transparent);" in button_rules
    )
    assert (
        "color: color-mix(in srgb, var(--accent-strong) 76%, var(--fg));"
        in button_rules
    )
    assert ".spice-menu-button:hover,\n.spice-menu-button:focus-visible {" in css
    assert (
        "background: color-mix(in srgb, var(--control) 82%, var(--accent) 18%);"
        in button_rules
    )
    assert (
        "border-color: color-mix(in srgb, var(--border) 64%, transparent);"
        in button_rules
    )
    assert (
        "box-shadow: inset 0 0 0 1px "
        "color-mix(in srgb, var(--accent) 12%, transparent);" in button_rules
    )
    assert ".spice-menu-button:active {" in css
    assert (
        "background: color-mix(in srgb, var(--control) 76%, var(--accent) 24%);"
        in button_rules
    )
    assert (
        "border-color: var(--border-soft);\n"
        "  box-shadow: inset 0 0 0 1px var(--border-soft);" in button_rules
    )
    assert '.spice-menu-button[aria-expanded="true"] {' in button_rules
    assert (
        '.spice-menu-button[aria-expanded="true"] {\n'
        "  background: color-mix(in srgb, var(--control) 76%, var(--accent) 24%);\n"
        "  border-color: var(--border-soft);\n"
        "  box-shadow: inset 0 0 0 1px var(--border-soft);" in button_rules
    )
    assert "var(--final-accent)" not in button_rules
    assert "color: currentColor;" in css
    assert (
        ".spice-menu-button--fast:hover,\n"
        ".spice-menu-button--fast:focus-visible {" in css
    )
    assert ".spice-menu-button--fast:active {" in css
    assert '.spice-menu-button--fast[aria-expanded="true"] {' in css
    assert (
        "background: color-mix(in srgb, var(--control) 64%, var(--say-accent) 36%);"
        in css
    )
    assert (
        '.spice-menu-button--fast[aria-expanded="true"] {\n'
        "  background: color-mix(in srgb, var(--control) 64%, var(--say-accent) 36%);\n"
        "  border-color: var(--border-soft);\n"
        "  box-shadow: inset 0 0 0 1px var(--border-soft);" in css
    )
    assert "height: 38px;" in css


def test_static_spice_menu_replaces_picker_lane():
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    app_js = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")

    assert "let spiceMenuEl = null;" in app_js
    assert "let fastModeEnabled = false;" in app_js
    assert "function openSpiceMenu()" in app_lanes
    assert "function laneStateTargetIds()" in app_lanes
    assert "function sameStringSets(left, right)" in app_lanes
    assert (
        "if (!sameStringSets(openBefore, laneStateTargetIds())) renderSpiceMenu();"
        in app_lanes
    )
    assert "if (laneStates.size) closeSpiceMenu();" not in app_lanes
    assert "function setFastModeEnabled(enabled)" in app_lanes
    assert "function createEmptyTeamFromMenu()" in app_lanes
    assert (
        'teamCommandPayload("createTeam", {\n      config: defaultTeamConfig(),'
        in app_lanes
    )
    assert 'button.setAttribute("role", "menuitem");' in app_lanes
    assert 'className = "lane picker"' not in app_lanes
    assert "openPickerLane" not in app_lanes
    assert "renderPickerChoices" not in app_shell
    assert ".spice-context-menu" in css
    assert '.spice-menu-action[aria-checked="true"]' in css
    assert ".picker" not in css


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
    assert payload["attachments"][0]["name"] == "paste.png"
    assert payload["attachments"][0]["url"].startswith(
        f"/api/work/trees/{target.id}/files/image?path="
    )
    assert state.lane_send_count(target.id) == 1
    assert state.team_store.lane_metric_summary(THREAD_A, bucket_count=12).sends == 1
    assert pending_inbox_count(repo) == 1
    assert inbox_request_body(items[0].text) == "inspect this image"
    assert items[0].attachments[0].path.read_bytes() == b"image-bytes"


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
    monkeypatch.setattr(app, "transcript_path_for_thread", lambda _thread: rollout)
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
    monkeypatch.setattr(app, "transcript_path_for_thread", lambda _thread_id: rollout)
    handler = _ImageHandler(state)

    app._ServeHandler._send_message_image(
        handler,
        target,
        {"offset": ["0"], "item": ["0"]},
    )

    assert handler.status == HTTPStatus.OK
    assert handler.headers["Content-Type"] == "image/png"
    assert handler.body.getvalue() == b"image-bytes"


def test_worktree_image_resolves_archived_attachment_from_live_reference(tmp_path):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _live, archived = _inbox_attachment_paths(repo)
    archived.parent.mkdir(parents=True)
    archived.write_bytes(b"archived-image")
    handler = _ImageHandler(state)

    app._ServeHandler._send_worktree_image(
        handler,
        target,
        {"path": [".spice/inbox/20260102T000000000001Z.attachments/01-image.png"]},
    )

    assert handler.status == HTTPStatus.OK
    assert handler.headers["Content-Type"] == "image/png"
    assert handler.body.getvalue() == b"archived-image"


def test_worktree_image_resolves_live_attachment_from_archive_reference(tmp_path):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    live, _archived = _inbox_attachment_paths(repo)
    live.parent.mkdir(parents=True)
    live.write_bytes(b"live-image")
    handler = _ImageHandler(state)

    app._ServeHandler._send_worktree_image(
        handler,
        target,
        {
            "path": [
                ".spice/inbox/archive/20260102T000000000001Z.attachments/01-image.png"
            ]
        },
    )

    assert handler.status == HTTPStatus.OK
    assert handler.headers["Content-Type"] == "image/png"
    assert handler.body.getvalue() == b"live-image"


def test_worktree_image_prefers_archived_attachment_when_both_exist(tmp_path):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    live, archived = _inbox_attachment_paths(repo)
    live.parent.mkdir(parents=True)
    archived.parent.mkdir(parents=True)
    live.write_bytes(b"live-image")
    archived.write_bytes(b"archived-image")
    handler = _ImageHandler(state)

    app._ServeHandler._send_worktree_image(
        handler,
        target,
        {"path": [".spice/inbox/20260102T000000000001Z.attachments/01-image.png"]},
    )

    assert handler.status == HTTPStatus.OK
    assert handler.headers["Content-Type"] == "image/png"
    assert handler.body.getvalue() == b"archived-image"


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


def test_running_renew_sends_handoff_request_and_records_pending_renewal(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    state.team_store.create_team(members=[THREAD_A])
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)

    payload, status = work_tree_send_response_payload(
        state, target, {"text": "wrap this up", "renewAgent": True}
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
    assert RENEWAL_HANDOFF_REQUEST_SUFFIX in inbox_request_body(item.text)
    assert renewal["state"] == "pending"
    assert renewal["ancestor_thread_id"] == THREAD_A
    assert renewal["successor_agent_id"] == ""


def test_stopped_renew_starts_successor_and_moves_team_membership(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    created = state.team_store.create_team(members=[THREAD_A])
    ensure_calls: list[dict[str, object]] = []
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)

    def fake_ensure(ensured_target, **kwargs):
        ensure_calls.append({"target": ensured_target, **kwargs})
        return {"ok": True, "threadId": THREAD_B}, HTTPStatus.OK

    monkeypatch.setattr(agentapi, "agent_ensure_response_payload", fake_ensure)

    payload, status = work_tree_send_response_payload(
        state,
        target,
        {"text": "continue from handoff", "renewAgent": True, "fastMode": True},
    )

    body = inbox_request_body(collect_inbox_items(repo)[0].text)
    assert status == HTTPStatus.OK
    assert payload["agentEnsure"]["threadId"] == THREAD_B
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
            "taskFilters": ["serve.ui", "", "task.review"],
            "lifetime": "Drive",
        },
    )

    team_id = state.team_store.current_team_for_agent(THREAD_A)
    assert status == HTTPStatus.OK
    assert payload["route"]["actor"] == THREAD_A
    assert payload["route"]["teamId"] == team_id
    assert payload["route"]["taskFilters"] == ["serve.ui", "task.review"]
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
        transcript_path=lambda _thread_id: Path("rollout.jsonl"),
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
    assert favicon.is_file()
    assert handler.status == HTTPStatus.OK
    assert handler.headers["Content-Length"] == str(favicon.stat().st_size)
    assert "icon" in handler.headers["Content-Type"]
    assert handler.body.getvalue().startswith(b"\x00\x00\x01\x00")


def test_static_css_has_narrow_viewport_affordances():
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")

    assert "@media (max-width: 720px)" in css
    assert "scroll-snap-type: x mandatory" in css
    assert "flex: 0 0 100%" in css
    assert "height: 100dvh" in css


def test_static_css_centers_two_pip_lane_light_stack():
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    stack_start = css.index(".lane-pip-stack {")
    stack_end = css.index(".agent-status-pip {", stack_start)
    stack_rules = css[stack_start:stack_end]
    lights_start = css.index(".lane-lights {")
    lights_end = css.index(".lane-lights .lane-light {", lights_start)
    lights_rules = css[lights_start:lights_end]

    assert "justify-content: center;" in stack_rules
    assert "min-width: 18px;" in stack_rules
    assert "place-content: center;" in lights_rules


def test_static_messages_use_compact_image_grid():
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")

    assert "grid-template-columns: repeat(" in css
    assert "minmax(min(calc(50% - 4px), 156px), 1fr)" in css
    assert ".messages article.image-only" in css
    assert "grid-column: span 1" in css
    assert ".messages article.image-only .message-image img" in css
    assert "max-height: 136px" in css
    assert ".history-sentinel {\n  grid-column: 1 / -1;" in css
    assert 'if (item.image_only) article.classList.add("image-only");' in app_render


def test_static_draft_composers_use_14px_font():
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    selector = ".composer-shard textarea {"
    start = css.index(selector)
    end = css.index("}", start)
    textarea_rule = css[start:end]

    assert "font-size: 14px;" in textarea_rule


def test_static_composer_attachment_thumbnails_fill_header():
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")

    attachments_start = css.index(".composer-attachments {")
    attachments_end = css.index(".composer-attachments[hidden]", attachments_start)
    attachments_rule = css[attachments_start:attachments_end]
    list_start = css.index(".composer-attachment-list {")
    list_end = css.index(".composer-attachment-chip {", list_start)
    list_rule = css[list_start:list_end]
    chip_start = css.index(".composer-attachment-chip {")
    chip_end = css.index(".composer-attachment-chip img", chip_start)
    chip_rule = css[chip_start:chip_end]
    name_start = css.index(".composer-attachment-name {")
    name_end = css.index("}", name_start)
    name_rule = css[name_start:name_end]

    assert 'body.className = "composer-band-body";' in app_shell
    assert 'const body = parent.querySelector(".composer-band-body");' in app_shell
    assert "composer-band-header--attachments" in app_shell
    assert ".composer-band-body--attachments .composer-band-title" in css
    assert "overflow-x: auto;" in attachments_rule
    assert "height: 100%;" in attachments_rule
    assert "gap: 2px;" in list_rule
    assert "height: 26px;" in chip_rule
    assert "width: 26px;" in chip_rule
    assert "display: none;" in name_rule


def test_static_composer_menu_replaces_header_remove_control():
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")

    assert 'trigger.className = "composer-band-menu-button";' in app_shell
    assert 'trigger.setAttribute("aria-haspopup", "menu");' in app_shell
    assert 'trigger.textContent = "☰";' in app_shell
    assert 'menu.className = "composer-band-menu";' in app_shell
    assert (
        'button.className = "composer-band-menu-action spice-menu-action";' in app_shell
    )
    assert "if (action.detail) button.title = action.detail;" in app_shell
    assert "function syncComposerBandMenuState(band)" in app_shell
    assert 'label: "Leave all teams",' in app_shell
    assert 'label: "Create new team",' in app_shell
    assert '"Remove " + label + " from all teams"' in app_shell
    assert '"Move only " + label + " to a new team"' in app_shell
    assert '.composer-band-menu-button[aria-expanded="true"] {' in css
    assert (
        ".composer-band--menu-open textarea,\n.composer-band--menu-open .composer-attachments {"
        in css
    )
    assert (
        ".composer-band-menu-action .spice-menu-action-detail {\n  display: none;"
        in css
    )
    assert 'teamCommandPayload("splitTeam", {' in app_groups
    assert "agentIds: [laneTeamAgentId(member)]," in app_groups


def test_static_relative_times_are_monospace_and_padded():
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")

    assert 'return String(value).padStart(2, "\xa0") + unit;' in app_render
    assert ".compaction-meta time,\n.lane-status-time,\n.message-footer time {" in css
    assert "font-family: ui-monospace, SFMono-Regular, Menlo, monospace;" in css
    assert "white-space: pre;" in css
    assert ".composer-quote-time" in css
    assert "font-variant-numeric: tabular-nums;" in css


def test_static_composer_pending_placeholder_omits_parentheses():
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")

    assert 'laneMemberTargetLabel(lane) +\n    "\\n"' in app_shell
    assert (
        'lanePendingDisplayCount(lane) +\n    " pending, " +\n    status' in app_shell
    )
    assert (
        'return "(" + lanePendingDisplayCount(lane) + " pending, " + status + ")";'
        not in app_shell
    )


def test_static_cmd_enter_submits_focused_composer_target_only():
    app_controls = (STATIC_ROOT / "app.controls.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")

    assert (
        "lane.formEl.addEventListener("
        '"submit", (event) => submitLaneForm(lane, event));' in app_shell
    )
    assert 'function submitLaneForm(lane, event, targetId = "")' in app_controls
    assert (
        "const targetEntries = targetId\n"
        "    ? [[targetId, host.shardTextareas.get(targetId)]]\n"
        "    : host.shardTextareas;" in app_controls
    )
    assert "submitLaneForm(lane, event, targetId);" in app_shell
    assert "lane.formEl.requestSubmit();" not in app_shell


def test_static_css_adds_visible_nested_quote_depth():
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    ack_selector = ".ack-quote {\n  background"
    ack_start = css.index(ack_selector)
    ack_rule = css[ack_start : css.index("}", ack_start)]

    assert ".message-body,\n.ack-quote {" in css
    assert "--quote-nested-step: 8px;" in css
    assert "--quote-nest-indent: calc(" in css
    assert "--quote-deep-nest-indent: calc(" in css
    assert "--quote-nested-pad-inline: 6px;" in css
    assert "--quote-pad-block: 6px;" in css
    assert ".message-body blockquote blockquote,\n.ack-quote blockquote {" in css
    assert (
        ".message-body blockquote blockquote blockquote,\n"
        ".ack-quote blockquote blockquote {" in css
    )
    assert "margin: 6px 0 0 var(--quote-nest-indent);" in css
    assert "margin-left: var(--quote-deep-nest-indent);" in css
    assert (
        "border-left: var(--quote-rail-width) solid "
        "color-mix(in srgb, var(--accent) 72%, var(--fg));" in css
    )
    assert "padding: var(--quote-pad-block) var(--quote-nested-pad-inline);" in css
    assert "--quote-rail-width: 3px;" in css
    assert "border-left: var(--quote-rail-width) solid var(--accent);" in ack_rule
    assert "padding: var(--quote-pad-block) var(--quote-pad-inline);" in ack_rule


def test_static_message_anchor_restore_does_not_drive_pane_collapse():
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")

    assert (
        "suppressLanePaneScrollIntentForFrame(lane);\n  lane.messagesEl.replaceChildren"
        in app_stream
    )
    assert (
        "setLaneScrollTopWithoutPaneIntent(lane, lane.messagesEl.scrollTop + delta)"
        in app_stream
    )
    assert "lane.messagesEl.scrollTop += delta" not in app_stream


def test_static_image_only_messages_omit_copy_and_play_actions():
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")

    assert "if (!item.image_only) appendSpeechAction(right, lane, item);" in app_render
    assert "if (!item.image_only) appendCopyAction(right, lane, item);" in app_render
    assert "appendQuoteAction(right, lane, item);" in app_render


def test_static_speech_buttons_use_centered_svg_icons():
    app_audio = (STATIC_ROOT / "app.audio.js").read_text(encoding="utf-8")

    assert "const speechPlayIconSvg" in app_audio
    assert "const speechStopIconSvg" in app_audio
    assert '<rect x="7" y="7" width="10" height="10"' in app_audio
    assert (
        "button.innerHTML = playing ? speechStopIconSvg : speechPlayIconSvg;"
        in app_audio
    )
    assert 'button.textContent = playing ? "◼" : "⏵";' not in app_audio


def test_static_message_speech_routes_to_producer_lane():
    app_audio = (STATIC_ROOT / "app.audio.js").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")

    assert "function speechLaneForMessage(lane, item)" in app_render
    assert "const targetId = item.producerTargetId || lane.targetId;" in app_render
    assert "const speechLane = speechLaneForMessage(lane, item);" in app_render
    assert "toggleMessageSpeech(lane, item.key, speech, speechLane)" in app_render
    assert (
        "function enqueueSpeech(lane, messageKey, texts, targetLane = lane)"
        in app_audio
    )
    assert "await playSpeech(entry.targetLane, text);" in app_audio


def test_static_manual_speech_playback_aborts_active_entry():
    app_audio = (STATIC_ROOT / "app.audio.js").read_text(encoding="utf-8")

    assert (
        "function toggleMessageSpeech(lane, messageKey, texts, targetLane = lane) {\n"
        "  speechQueue.length = 0;"
    ) in app_audio
    assert "const activeSpeech = currentSpeech;" in app_audio
    assert "if (activeSpeech) abortLaneSpeech(activeSpeech.lane);" in app_audio
    assert "enqueueSpeech(lane, messageKey, texts, targetLane);" in app_audio


def test_static_speech_sync_updates_now_playing_message_accent():
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    app_audio = (STATIC_ROOT / "app.audio.js").read_text(encoding="utf-8")
    start = css.index(".messages article.now-playing")
    end = css.index(".messages article[data-occupant]", start)
    now_playing = css[start:end]

    assert "function syncNowPlayingMessages()" in app_audio
    assert 'document.querySelectorAll("article[data-message-key]")' in app_audio
    assert 'messageArticle.classList.toggle(\n      "now-playing",' in app_audio
    assert "syncNowPlayingMessages();" in app_audio
    assert "--control-max-accent: var(--say-accent);" in css
    assert "--control-state-accent: var(--control-max-accent);" in now_playing
    assert "var(--message-occupant-accent" not in now_playing


def test_static_compaction_divider_spans_grid_and_uses_agent_accent():
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")

    assert "grid-column: 1 / -1;" in css
    assert "grid-template-columns: minmax(16px, 1fr) auto minmax(16px, 1fr)" in css
    assert "background: var(--compaction-accent, var(--border));" in css
    assert 'compactionAgentLabel(lane, item) + " compacted context"' in app_render
    assert "--compaction-accent" in app_render


def test_static_fused_lane_status_line_uses_latest_member_compact_preview():
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")

    assert "syncFusedLaneStatusLine(lane);" in app_render
    assert "function syncFusedLaneStatusLine(lane)" in app_groups
    assert "fusedLaneLatestStatusLine(laneGroupMemberLanes(lane))" in app_groups
    assert "function fusedLaneMemberStatusLine(member)" in app_groups
    assert "statusLine.latestActivityPreview" in app_groups
    assert "statusLine.agentVisualStatus || statusLine.agentProcessStatus" in app_groups
    assert "const label = laneMemberTargetLabel(member)" not in app_groups
    assert "summaries.join" not in app_groups


def test_fused_lane_status_restores_host_status_on_split():
    app_groups = STATIC_ROOT / "app.groups.js"
    script = Path(__file__).with_name("fixtures") / "fused_status_split.js"

    result = subprocess.run(
        ["node", str(script), str(app_groups)],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def _repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir(exist_ok=True)
    return repo


def _target(repo: Path) -> WorktreeTarget:
    return WorktreeTarget(id="target-1", repo_root=repo, name=repo.name, branch="main")


def _serve_state(tmp_path: Path, target: WorktreeTarget) -> ServeState:
    state = ServeState(anchor_root=tmp_path)
    state.cached_targets = [target]
    state.team_store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    state.team_commands = TeamCommandService(state.team_store)
    return state


def _inbox_attachment_paths(repo: Path) -> tuple[Path, Path]:
    relative = Path("20260102T000000000001Z.attachments") / "01-image.png"
    live = repo / ".spice" / "inbox" / relative
    archived = repo / ".spice" / "inbox" / "archive" / relative
    return live, archived


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
