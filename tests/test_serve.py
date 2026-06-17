"""Serve app and live-bus contracts."""

from __future__ import annotations

import json
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
SERVE_CSS_FILES = ("index.css", "composer.css", "messages.css", "status-colors.css")


def _serve_css_text() -> str:
    return "\n".join(
        (STATIC_ROOT / filename).read_text(encoding="utf-8")
        for filename in SERVE_CSS_FILES
    )


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


def test_lane_watch_paths_include_task_backend_files(tmp_path):
    backend = tmp_path / "task-backend"
    data = backend / "data"
    data.mkdir(parents=True)
    taskrc = backend / "taskrc"
    taskrc.write_text("data.location=data\n", encoding="utf-8")
    pending = data / "pending.data"
    pending.write_text("task one\n", encoding="utf-8")
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    task_config.set_backend(str(backend))
    try:
        paths = app.lane_watch_paths_for_target(state, target, THREAD_A, transcript)
    finally:
        task_config.set_backend(None)

    assert backend in paths
    assert data in paths
    assert taskrc in paths
    assert pending in paths


def test_lane_signature_changes_when_task_backend_changes(tmp_path):
    backend = tmp_path / "task-backend"
    data = backend / "data"
    data.mkdir(parents=True)
    (backend / "taskrc").write_text("data.location=data\n", encoding="utf-8")
    pending = data / "pending.data"
    pending.write_text("task one\n", encoding="utf-8")
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text("", encoding="utf-8")
    task_config.set_backend(str(backend))
    try:
        before = app.lane_signature_for_target(state, target, THREAD_A, transcript)
        pending.write_text("task one\ntask two\n", encoding="utf-8")
        after = app.lane_signature_for_target(state, target, THREAD_A, transcript)
    finally:
        task_config.set_backend(None)

    assert after != before


def test_header_spice_menu_button_replaces_plus_and_fast_toggle():
    html = render_index_html()
    css = _serve_css_text()
    app_js = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")
    header_start = css.index(".app-header {")
    header_end = css.index(".app-header .meta", header_start)
    header_rules = css[header_start:header_end]
    button_start = css.index(".spice-menu-button {")
    button_end = css.index(".spice-menu-icon {", button_start)
    button_rules = css[button_start:button_end]
    icon_start = css.index(".spice-menu-icon {")
    icon_end = css.index(".spice-menu-label {", icon_start)
    icon_rules = css[icon_start:icon_end]
    label_start = css.index(".spice-menu-label {")
    label_end = css.index(".icon-button svg", label_start)
    label_rules = css[label_start:label_end]
    mobile_header_start = css.index(
        "  .app-header {", css.index("@media (max-width: 720px)")
    )
    mobile_header_end = css.index("  .app-header .meta", mobile_header_start)
    mobile_header_rules = css[mobile_header_start:mobile_header_end]
    mobile_filter_start = css.index("  .filter-strip {", mobile_header_start)
    mobile_filter_end = css.index("  .swimlanes", mobile_filter_start)
    mobile_filter_rules = css[mobile_filter_start:mobile_filter_end]

    assert 'id="fast-mode-toggle"' not in html
    assert 'class="add-lane"' not in html
    assert "<title>spice</title>" in html
    assert "Simultaneous Production, Integration, and Control Environment" not in html
    assert "<h1>spice</h1>" not in html
    assert ">+</button>" not in html
    assert 'aria-label="Open teams"' in html
    assert 'id="open-lane" class="spice-menu-button"' in html
    assert 'aria-haspopup="menu" aria-expanded="false"' in html
    assert 'class="spice-menu-icon" aria-hidden="true">🌶️</span>' in html
    assert '<span class="spice-menu-label">spice</span>' in html
    assert 'querySelector("#fast-mode-toggle")' not in app_js
    assert 'openLaneButton.addEventListener("click", (event) => {' in app_js
    assert "button.primary:hover {\n  background: var(--accent-strong);" in css
    assert "button.primary:hover,\n.spice-menu-button:hover" not in css
    assert "min-height: 50px;" in header_rules
    assert "padding: 7px 10px;" in header_rules
    assert (
        "--control-border-soft: color-mix(in srgb, var(--border) 36%, transparent);"
        in css
    )
    assert "--control-surface-soft:" in css
    assert "--control-inset-soft: none;" in css
    assert (
        "background: color-mix(in srgb, var(--control-surface-soft) 88%, var(--accent) 12%);"
        in button_rules
    )
    assert "border-color: var(--control-border-soft);" in button_rules
    assert "box-shadow: var(--control-inset-soft);" in button_rules
    assert (
        "color: color-mix(in srgb, var(--accent-strong) 76%, var(--fg));"
        in button_rules
    )
    assert "gap: 4px;" in button_rules
    assert "height: 30px;" in button_rules
    assert "padding: 0 8px 0 6px;" in button_rules
    assert "font-size: 15px;" in icon_rules
    assert "color: currentColor;" in label_rules
    assert "font-size: 17px;" in label_rules
    assert ".spice-menu-button:hover,\n.spice-menu-button:focus-visible {" in css
    assert (
        "background: color-mix(in srgb, var(--control-surface-soft) 78%, var(--accent) 22%);"
        in button_rules
    )
    assert "border-color: var(--control-border-soft-hover);" in button_rules
    assert ".spice-menu-button:active {" in css
    assert (
        "background: color-mix(in srgb, var(--control) 76%, var(--accent) 24%);"
        in button_rules
    )
    assert (
        "border-color: var(--border-soft);\n"
        "  box-shadow: var(--control-inset-soft);" in button_rules
    )
    assert '.spice-menu-button[aria-expanded="true"] {' in button_rules
    assert (
        '.spice-menu-button[aria-expanded="true"] {\n'
        "  background: color-mix(in srgb, var(--control) 76%, var(--accent) 24%);\n"
        "  border-color: var(--border-soft);\n"
        "  box-shadow: var(--control-inset-soft);" in button_rules
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
        "  box-shadow: var(--control-inset-soft);" in css
    )
    assert "height: 30px;" in button_rules
    assert "flex-wrap: nowrap;" in mobile_header_rules
    assert "min-height: 46px;" in mobile_header_rules
    assert "padding: 8px;" in mobile_header_rules
    assert "flex: 1 1 auto;" in mobile_filter_rules
    assert "min-width: 0;" in mobile_filter_rules


def test_static_soft_control_border_reaches_lane_controls():
    css = _serve_css_text()

    icon_rules = css[css.index(".icon-button {") : css.index(".icon-button:hover")]
    lane_rules = css[css.index(".lane {") : css.index(".lane--shadowed")]
    rail_rules = css[
        css.index(".lane-mode-rail {\n  min-width") : css.index(".lane-mode-button {")
    ]
    team_button_rules = css[
        css.index('.lane-team-menu-button[aria-expanded="true"] {') : css.index(
            ".lane-team-menu-icon"
        )
    ]
    slider_rules = css[css.index(".stack-slider {") : css.index(".submit-action {")]
    submit_rules = css[
        css.index(".primary.submit-action {") : css.index(
            "button.primary.submit-action:hover"
        )
    ]
    menu_action_rules = css[
        css.index(".spice-menu-action {") : css.index(".spice-menu-action:hover")
    ]
    target_choice_rules = css[
        css.index(".target-choice {") : css.index(".target-choice-signal")
    ]
    composer_button_rules = css[
        css.index(
            ".composer-band-menu-button,\n.composer-band-close-button {"
        ) : css.index(".composer-band-menu-button:hover")
    ]

    assert (
        "background: color-mix(in srgb, var(--control-surface-soft) 88%, var(--accent) 12%);"
        in css
    )
    assert (
        "background: color-mix(in srgb, var(--control-surface-soft) 78%, var(--accent) 22%);"
        in css
    )
    assert (
        "background: color-mix(in srgb, var(--control-surface-soft) 82%, var(--accent) 18%);"
        in team_button_rules
    )
    assert (
        "border-color: color-mix(in srgb, var(--accent) 54%, var(--border));"
        in team_button_rules
    )
    assert "box-shadow: inset 0 0 0 1px color-mix" in team_button_rules
    assert (
        "border-color: color-mix(in srgb, var(--control-state-accent, var(--accent)) 72%, var(--border));"
        in submit_rules
    )
    assert "box-shadow: inset 0 0 0 1px color-mix" in submit_rules

    for rules in (
        icon_rules,
        lane_rules,
        rail_rules,
        slider_rules,
        menu_action_rules,
        target_choice_rules,
        composer_button_rules,
    ):
        assert "border: 1px solid var(--control-border-soft);" in rules
        assert "box-shadow: var(--control-inset-soft);" in rules

    for rules in (
        icon_rules,
        rail_rules,
        slider_rules,
        menu_action_rules,
        target_choice_rules,
        composer_button_rules,
    ):
        assert "background: var(--control-surface-soft);" in rules


def test_static_spice_menu_replaces_picker_lane():
    css = _serve_css_text()
    app_js = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")

    assert "let spiceMenuEl = null;" in app_js
    assert 'let spiceMenuDragTargetId = "";' in app_js
    assert "let fastModeEnabled = false;" in app_js
    assert "function openSpiceMenu()" in app_lanes
    assert "function laneStateTargetIds()" in app_lanes
    assert "function sameStringSets(left, right)" in app_lanes
    assert (
        "if (!sameStringSets(openBefore, laneStateTargetIds())) renderSpiceMenu();"
        in app_lanes
    )
    assert (
        'lane.element.scrollIntoView({ block: "nearest", inline: "nearest" });'
        in app_lanes
    )
    assert "if (laneStates.size) closeSpiceMenu();" not in app_lanes
    assert "function setFastModeEnabled(enabled)" in app_lanes
    assert "function createEmptyTeamFromMenu()" in app_lanes
    assert "function spiceMenuTeamGroups(choices)" in app_lanes
    assert "function renderSpiceMenuTeamGroup(group)" in app_lanes
    assert "function spiceMenuTeamDetail(group)" in app_lanes
    assert "function wireSpiceMenuTargetDrag(button, target)" in app_lanes
    assert "function wireSpiceMenuTeamDropTarget(container, group)" in app_lanes
    assert "function moveTargetToMenuTeam(teamId, targetId)" in app_lanes
    assert "const laneGrid = lanesEl.getBoundingClientRect();" in app_lanes
    assert "const anchorLane = spiceMenuAnchorLane();" in app_lanes
    assert (
        "const anchorRect = anchorLane ? anchorLane.getBoundingClientRect() : null;"
        in app_lanes
    )
    assert (
        "const laneLeft = anchorRect ? anchorRect.left : laneGrid.left + paddingLeft;"
        in app_lanes
    )
    assert "spiceMenuMinimumLaneWidthPx()" in app_lanes
    assert "function spiceMenuAnchorLane()" in app_lanes
    assert (
        "return visible.find(spiceMenuPrefersLaneElement) || visible[0] || null;"
        in app_lanes
    )
    assert "function spiceMenuPrefersLaneElement(element)" in app_lanes
    assert "const laneElement = /** @type {HTMLElement} */ (element);" in app_lanes
    assert 'laneStates.get(laneElement.dataset.targetId || "");' in app_lanes
    assert "return Boolean(lane && lane.emptyTeam);" in app_lanes
    assert (
        "function spiceMenuWidth(anchorLane, laneLeft, laneWidth, margin)" in app_lanes
    )
    assert "function spiceMenuLeft(anchorLane, laneLeft, width, margin)" in app_lanes
    assert "function spiceMenuUsesViewportWidth()" in app_lanes
    assert 'window.matchMedia("(max-width: 720px)").matches' in app_lanes
    assert (
        "if (!anchorLane && spiceMenuUsesViewportWidth()) return window.innerWidth;"
        in app_lanes
    )
    assert "if (anchorLane) return laneLeft;" in app_lanes
    assert "if (spiceMenuUsesViewportWidth()) return 0;" in app_lanes
    assert "openLaneButton.getBoundingClientRect();" in app_lanes
    assert (
        "Math.min(buttonRect.right - width, window.innerWidth - width - margin)"
        in app_lanes
    )
    assert 'spiceMenuEl.style.height = anchorLane ? height + "px" : "";' in app_lanes
    assert "function cssPixelValue(value)" in app_lanes
    assert 'className = "lane picker"' not in app_lanes
    assert "openPickerLane" not in app_lanes
    assert "renderPickerChoices" not in app_shell
    assert ".spice-context-menu" in css
    assert ".picker" not in css


def test_static_spice_menu_team_groups_and_actions():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    css = _serve_css_text()

    assert (
        'teamCommandPayload("createTeam", {\n      config: defaultTeamConfig(),'
        in app_lanes
    )
    assert 'heading.textContent = "open team";' in app_lanes
    assert '? "loading teams"\n      : "team list unavailable";' in app_lanes
    assert 'list.textContent = "no agents available";' in app_lanes
    assert (
        'label.textContent = group.unassigned\n    ? "agents without team"' in app_lanes
    )
    assert '    ? "drop here to remove from team"' in app_lanes
    assert "function spiceMenuEmptyUnassignedDropHint()" in app_lanes
    assert 'hint.className = "spice-menu-team-empty-drop";' in app_lanes
    assert 'hint.textContent = "Drop agent here";' in app_lanes
    assert "wireSpiceMenuTeamDropTarget(container, group);" in app_lanes
    assert '"open any member; " + count + " agents open together"' in app_lanes
    assert "const alreadyOpen = laneStates.has(target.id);" in app_lanes
    assert 'if (alreadyOpen) actionLabel = "Show team";' in app_lanes
    assert (
        'else if (group && !group.unassigned) actionLabel = "Open team";' in app_lanes
    )
    assert 'button.classList.toggle("target-choice--open", alreadyOpen);' in app_lanes
    assert 'if (laneStates.has(target.id)) parts.push("open");' in app_lanes
    assert 'setGlobalTransientStatus("open team failed");' in app_lanes
    assert (
        'function targetChoiceButton(target, actionLabel, onClick, role = "menuitem")'
        in app_lanes
    )
    assert 'if (role) button.setAttribute("role", role);' in app_lanes
    assert ".spice-menu-team {" in css
    assert ".spice-menu-team--unassigned {" in css
    assert ".spice-menu-team--drop-ready {" in css
    assert ".spice-menu-team-header {" in css
    assert ".spice-menu-team-targets {" in css
    assert ".target-choice--open {" in css
    assert '.spice-menu-action[aria-checked="true"]' in css


def test_static_spice_menu_target_metadata_and_status_update_live():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    css = _serve_css_text()

    assert "button.dataset.targetChoiceId = target.id;" in app_lanes
    assert "button.dataset.targetChoiceActionLabel = actionLabel;" in app_lanes
    assert "function updateLiveTargetChoiceMetadata()" in app_lanes
    assert 'document.querySelectorAll("[data-target-choice-id]")' in app_lanes
    assert "updateTargetChoiceButtonPresentation(" in app_lanes
    assert "function targetChoiceStatusLine(target)" in app_lanes
    assert "lane.lastRenderedStatusLine" in app_lanes
    assert "const pending = targetChoicePendingCount(target);" in app_lanes
    assert "liveAgentVisualStatus(statusLine)" in app_lanes
    assert "agentStatusLabel(status)" in app_lanes
    assert "updateLiveTargetChoiceMetadata();" in app_render
    assert ".target-choice--running-stale .target-choice-signal" in css


def test_static_spice_menu_drag_manages_team_membership():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    css = _serve_css_text()

    assert "wireSpiceMenuTargetDrag(button, target);" in app_lanes
    assert 'button.style.touchAction = "none";' in app_lanes
    assert 'button.addEventListener("click", (event) => {' in app_lanes
    assert "suppressNextClick = true;" in app_lanes
    assert "button.setPointerCapture(event.pointerId);" in app_lanes
    assert "dragState.dragGhost = createSpiceMenuTargetDragGhost(button);" in app_lanes
    assert "updateSpiceMenuTargetDragGhost(dragState, event);" in app_lanes
    assert "function createSpiceMenuTargetDragGhost(button)" in app_lanes
    assert 'ghost.classList.add("target-choice-drag-ghost");' in app_lanes
    assert "function updateSpiceMenuTargetDragGhost(state, event)" in app_lanes
    assert "state.dragGhost?.remove();" in app_lanes
    assert 'button.addEventListener("pointerdown", (event) => {' in app_lanes
    assert 'button.addEventListener("pointermove", (event) => {' in app_lanes
    assert 'button.addEventListener("pointerup", (event) => {' in app_lanes
    assert 'button.addEventListener("pointercancel", (event) => {' in app_lanes
    assert "Math.abs(dx) < 6 && Math.abs(dy) < 6" in app_lanes
    assert (
        "const el = document.elementFromPoint(event.clientX, event.clientY);"
        in app_lanes
    )
    assert "container.dataset.spiceMenuTeamId = group.teamId;" in app_lanes
    assert (
        'container.dataset.spiceMenuUnassigned = group.unassigned ? "true" : "false";'
        in app_lanes
    )
    assert "function spiceMenuDropTeamId(container)" in app_lanes
    assert 'container.classList.add("spice-menu-team--drop-ready");' in app_lanes
    assert "moveTargetToMenuTeam(teamId, target.id).catch(() => {" in app_lanes
    assert 'teamCommandPayload("moveAgentToTeam", {' in app_lanes
    assert 'teamCommandPayload("removeAgentFromTeam", {' in app_lanes
    assert "agentId: targetTeamAgentId(target)," in app_lanes
    assert "agentAliases: targetTeamAgentAliases(target)," in app_lanes
    assert "await refreshServerTopology();" in app_lanes
    assert (
        'setGlobalTransientStatus(teamId ? "team updated" : "agent removed from team");'
        in app_lanes
    )
    assert (
        'setGlobalTransientStatus(\n          teamId ? "move to team failed" : "remove from team failed",'
        in app_lanes
    )
    assert ".target-choice--draggable" in css
    assert ".target-choice--dragging {" in css
    assert ".target-choice-drag-ghost {" in css
    assert ".target-choice-drag-affordance {" in css
    assert ".spice-menu-team-empty-drop {" in css
    assert ".spice-menu-team--drop-ready .spice-menu-team-empty-drop {" in css


def test_static_empty_teams_render_importer_in_message_stream():
    css = _serve_css_text()
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")

    assert 'const emptyTeamTargetPrefix = "empty-team:";' in app_lanes
    assert "function ensureEmptyTeamLane(team, options = {})" in app_shell
    assert "const targetId = emptyTeamTargetId(team.teamId);" in app_lanes
    assert "const canCloseEmptyTeam = teams.length > 1;" in app_lanes
    assert "ensureEmptyTeamLane(team, { canClose: canCloseEmptyTeam });" in app_lanes
    assert "if (!targetById.has(lane.targetId) && !lane.emptyTeam)" in app_lanes
    assert "if (lane.emptyTeam) syncEmptyTeamLane(lane);" in app_lanes
    close_lane_start = app_lanes.index("function closeLane(lane) {")
    close_lane_end = app_lanes.index("function closeLaneCore(lane)", close_lane_start)
    assert (
        "if (host.emptyTeam && !host.emptyTeamCanClose) return;"
        in app_lanes[close_lane_start:close_lane_end]
    )
    assert "if (!host.teamId) return;" in app_lanes[close_lane_start:close_lane_end]
    assert "function addEmptyTeamLane(team, options = {})" in app_shell
    assert (
        'element.className = emptyTeam ? "lane lane--empty-team" : "lane";' in app_shell
    )
    empty_team_sync_start = app_shell.index(
        "function syncEmptyTeamLane(lane, team = {}, options = {}) {"
    )
    empty_team_sync_end = app_shell.index("function emptyTeamImportPanel(lane) {")
    empty_team_sync = app_shell[empty_team_sync_start:empty_team_sync_end]
    assert "function emptyTeamImportPanel(lane)" in app_shell
    assert "function emptyTeamImportChoice(lane, target)" in app_shell
    assert "lane.shardsEl.replaceChildren();" in empty_team_sync
    assert "renderMessagesIfChanged(lane);" in empty_team_sync
    assert empty_team_sync.index(
        "lane.shardsEl.replaceChildren();"
    ) < empty_team_sync.index("renderMessagesIfChanged(lane);")
    assert "lane.pipEl.hidden = true;" in empty_team_sync
    assert "lane.laneLightsEl.hidden = true;" in empty_team_sync
    assert "lane.laneLightsEl.replaceChildren();" in empty_team_sync
    assert "lane.emptyTeamCanClose = nextCanClose;" in empty_team_sync
    assert (
        'lane.element.classList.toggle("lane--empty-team-closable", nextCanClose);'
        in empty_team_sync
    )
    assert "if (lane.emptyTeamCanClose) {" in empty_team_sync
    assert "lane.teamMenuButtonEl.disabled = false;" in empty_team_sync
    assert 'lane.teamMenuButtonEl.removeAttribute("aria-hidden");' in empty_team_sync
    assert "lane.teamMenuButtonEl.disabled = true;" in empty_team_sync
    assert "lane.teamMenuButtonEl.tabIndex = -1;" in empty_team_sync
    assert (
        'lane.teamMenuButtonEl.setAttribute("aria-hidden", "true");' in empty_team_sync
    )
    assert 'lane.teamMenuButtonEl.title = "";' in empty_team_sync
    assert "lane.selectedView = defaultLaneViewMode;" in empty_team_sync
    assert "lockEmptyTeamPane(lane);" in empty_team_sync
    assert "function lockEmptyTeamPane(lane)" in app_shell
    assert "if (host.emptyTeam) {\n    lockEmptyTeamPane(host);" in app_shell
    assert "if (lane.emptyTeam) return false;" in app_shell
    assert "const requestedCollapsePx = lane.emptyTeam ? 0 : collapsePx;" in app_shell
    assert (
        'lane.modeRailEl.classList.toggle("lane-mode-rail--disabled", disabled);'
        in app_shell
    )
    assert "button.disabled = disabled;" in app_shell
    assert "button.tabIndex = disabled ? -1 : active ? 0 : -1;" in app_shell
    assert "lane.teamMenuButtonEl.disabled = false;" in app_groups
    assert 'lane.teamMenuButtonEl.removeAttribute("aria-hidden");' in app_groups
    assert 'lane.teamMenuButtonEl.removeAttribute("tabindex");' in app_groups
    assert 'const button = targetChoiceButton(\n    target,\n    "Import",' in app_shell
    assert '    "",\n  );' in app_shell
    assert "button.dataset.emptyTeamImportTargetId = target.id;" in app_shell
    assert 'teamCommandPayload("moveAgentToTeam", {' in app_shell
    assert "agentAliases: emptyTeamImportAliases(target)," in app_shell
    assert "if (lane.emptyTeam) return;" in app_stream
    assert (
        "if (isLaneOpen(lane) && !lane.emptyTeam) subscribeLaneToLiveBus(lane);"
        in app_stream
    )
    assert "function renderEmptyTeamMessages(lane)" in app_stream
    assert "function emptyTeamMessageFingerprint(lane)" in app_stream
    assert "targets: targets.map(emptyTeamTargetFingerprint)," in app_stream
    assert "function emptyTeamTargetFingerprint(target)" in app_stream
    assert 'target.displayName || "",' in app_stream
    assert 'target.threadId || "",' in app_stream
    assert 'target.lastAssistantAt || "",' in app_stream
    assert 'statusLine.lastAssistantAt || "",' in app_stream
    assert "target.pendingCount || 0," in app_stream
    assert "target.pendingInboxCount || 0," in app_stream
    assert 'target.agentProcessStatus || "",' in app_stream
    assert 'target.bindingStatus || "",' in app_stream
    assert (
        "lane.messagesEl.replaceChildren(\n"
        "    emptyTeamImportPanel(lane),\n"
        "    lane.historySentinelEl,\n"
        "  );"
    ) in app_stream
    assert "if (lane.emptyTeam) {\n    syncEmptyTeamLane(lane);" in app_groups
    assert (
        ".lane--empty-team .lane-pip-stack,\n"
        ".lane--empty-team:not(.lane--empty-team-closable) "
        "[data-lane-team-menu] {" in css
    )
    assert "pointer-events: none;" in css
    assert "visibility: hidden;" in css
    assert ".lane--empty-team .lane-mode-rail--disabled" in css
    assert "opacity: 0.72;" in css
    assert ".lane--empty-team .composer-controls" in css
    assert "display: none;" in css
    assert ".empty-team-importer" in css
    assert "grid-column: 1 / -1;" in css
    assert ".empty-team-import-list" in css


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
        app, "transcript_path_for_thread", lambda _thread, _repo_root: rollout
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
        app, "transcript_path_for_thread", lambda _thread_id, _repo_root: rollout
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
        state, target, {"text": "wrap this up"}
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
    assert payload["agentEnsure"]["pendingInboxCount"] == 0
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
    assert payload["pendingInboxCount"] == 0
    assert payload["statusLine"]["pendingInboxCount"] == 0
    assert payload["statusLine"]["pendingInboxLabel"] == "0"
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
    assert html.index("/static/index.css") < html.index("/static/composer.css")
    assert html.index("/static/composer.css") < html.index("/static/messages.css")
    assert html.index("/static/messages.css") < html.index("/static/status-colors.css")
    assert html.index("/static/app.shell.js") < html.index("/static/app.composer.js")
    assert html.index("/static/app.composer.js") < html.index("/static/app.controls.js")
    assert favicon.is_file()
    assert handler.status == HTTPStatus.OK
    assert handler.headers["Content-Length"] == str(favicon.stat().st_size)
    assert "icon" in handler.headers["Content-Type"]
    assert handler.body.getvalue().startswith(b"\x00\x00\x01\x00")


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
