"""Static serve UI header and menu contracts."""

from __future__ import annotations

from spice.serve.web import STATIC_ROOT, render_index_html

SERVE_CSS_FILES = ("index.css", "composer.css", "messages.css", "status-colors.css")


def _serve_css_text() -> str:
    return "\n".join(
        (STATIC_ROOT / filename).read_text(encoding="utf-8")
        for filename in SERVE_CSS_FILES
    )


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


def test_static_composer_band_menu_uses_compact_local_action_sizing():
    css = _serve_css_text()
    base_grid_start = css.index(".composer-band-menu.spice-menu-actions {")
    compact_grid_start = css.index(
        ".composer-band--menu-open .composer-band-menu.spice-menu-actions {"
    )
    compact_grid_rule = css[compact_grid_start : css.index("}", compact_grid_start)]
    compact_menu_start = css.index(".composer-band--menu-open .composer-band-menu {")
    compact_menu_rule = css[compact_menu_start : css.index("}", compact_menu_start)]
    compact_action_start = css.index(
        ".composer-band--menu-open .composer-band-menu-action {"
    )
    compact_action_rule = css[
        compact_action_start : css.index("}", compact_action_start)
    ]
    compact_text_start = css.index(
        ".composer-band--menu-open .composer-band-menu-action .spice-menu-action-label,\n"
        ".composer-band--menu-open .composer-band-menu-action .spice-menu-action-detail {"
    )
    compact_text_rule = css[compact_text_start : css.index("}", compact_text_start)]
    compact_label_start = css.index(
        ".composer-band--menu-open .composer-band-menu-action .spice-menu-action-label {"
    )
    compact_label_rule = css[compact_label_start : css.index("}", compact_label_start)]
    compact_detail_start = css.index(
        ".composer-band--menu-open .composer-band-menu-action .spice-menu-action-detail {",
        compact_label_start,
    )
    compact_detail_rule = css[
        compact_detail_start : css.index("}", compact_detail_start)
    ]

    assert base_grid_start < compact_grid_start
    assert "padding: 6px;" in compact_menu_rule
    assert "gap: 4px;" in compact_grid_rule
    assert "grid-auto-rows: minmax(34px, auto);" in compact_grid_rule
    assert "flex-direction: row;" in compact_action_rule
    assert "justify-content: space-between;" in compact_action_rule
    assert "min-height: 34px;" in compact_action_rule
    assert "padding: 5px 7px;" in compact_action_rule
    assert "text-align: left;" in compact_action_rule
    assert "overflow: hidden;" in compact_text_rule
    assert "overflow-wrap: normal;" in compact_text_rule
    assert "text-overflow: ellipsis;" in compact_text_rule
    assert "white-space: nowrap;" in compact_text_rule
    assert "font-size: 12px;" in compact_label_rule
    assert "font-weight: 600;" in compact_label_rule
    assert "flex: 0 1 46%;" in compact_detail_rule
    assert "font-size: 10px;" in compact_detail_rule


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
    assert "function compareSpiceMenuTargetChoices(left, right)" in app_lanes
    assert "function spiceMenuTeamSortKey(group)" in app_lanes
    assert "function wireSpiceMenuTargetDrag(button, target)" in app_lanes
    assert "function wireSpiceMenuTeamDropTarget(container, group)" in app_lanes
    assert "function moveTargetToMenuTeam(teamId, targetId)" in app_lanes
    assert "const buttonRect = openLaneButton.getBoundingClientRect();" in app_lanes
    assert "const top = Math.max(margin, buttonRect.bottom + margin);" in app_lanes
    assert (
        "const width = spiceMenuWidthForButton(buttonRect, viewportWidth, margin);"
        in app_lanes
    )
    assert (
        "const left = spiceMenuLeftForButton(buttonRect, width, viewportWidth, margin);"
        in app_lanes
    )
    assert "spiceMenuMinimumLaneWidthPx()" in app_lanes
    assert (
        "function spiceMenuWidthForButton(buttonRect, viewportWidth, margin)"
        in app_lanes
    )
    assert "Math.max(spiceMenuMinimumLaneWidthPx(), buttonRect.width)" in app_lanes
    assert (
        "function spiceMenuLeftForButton(buttonRect, width, viewportWidth, margin)"
        in app_lanes
    )
    assert "const rightAlignedLeft = buttonRect.right - width;" in app_lanes
    assert "Math.min(rightAlignedLeft, viewportWidth - width - margin)" in app_lanes
    team_groups_start = app_lanes.index("function spiceMenuTeamGroups(choices)")
    team_groups_end = app_lanes.index(
        "function renderSpiceMenuTeamGroup(group)", team_groups_start
    )
    team_groups_block = app_lanes[team_groups_start:team_groups_end]
    assert ".sort(compareSpiceMenuTargetChoices);" in team_groups_block
    assert "group.targets.sort(compareSpiceMenuTargetChoices);" in team_groups_block
    assert "unassigned.sort(compareSpiceMenuTargetChoices);" in team_groups_block
    assert "compareTargetChoices" not in team_groups_block
    compare_groups_start = app_lanes.index(
        "function compareSpiceMenuTeamGroups(left, right)"
    )
    compare_groups_end = app_lanes.index(
        "function renderSpiceMenuTeamGroup(group)", compare_groups_start
    )
    compare_groups_block = app_lanes[compare_groups_start:compare_groups_end]
    assert "spiceMenuTeamSortKey(left)" in compare_groups_block
    assert "spiceMenuTeamSortKey(right)" in compare_groups_block
    assert "compareTargetChoices" not in compare_groups_block
    assert "function spiceMenuUsesViewportWidth(viewportWidth)" in app_lanes
    assert "return viewportWidth < spiceMenuMinimumLaneWidthPx() + 20;" in app_lanes
    assert (
        "if (spiceMenuUsesViewportWidth(viewportWidth)) return viewportWidth;"
        in app_lanes
    )
    assert "if (spiceMenuUsesViewportWidth(viewportWidth)) return 0;" in app_lanes
    assert 'spiceMenuEl.style.height = "";' in app_lanes
    assert 'spiceMenuEl.style.maxHeight = height + "px";' in app_lanes
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


def test_static_empty_teams_reconcile_and_close_from_team_snapshot():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")

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
    assert "lane.pipEl.hidden = true;" in empty_team_sync
    assert "lane.laneLightsEl.hidden = true;" in empty_team_sync
    assert "lane.laneLightsEl.replaceChildren();" in empty_team_sync
    assert "lane.emptyTeamCanClose = nextCanClose;" in empty_team_sync
    assert (
        'lane.element.classList.toggle("lane--empty-team-closable", nextCanClose);'
        in empty_team_sync
    )


def test_static_empty_team_controls_lock_collapsed_until_populated():
    css = _serve_css_text()
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")

    empty_team_sync_start = app_shell.index(
        "function syncEmptyTeamLane(lane, team = {}, options = {}) {"
    )
    empty_team_sync_end = app_shell.index("function emptyTeamImportPanel(lane) {")
    empty_team_sync = app_shell[empty_team_sync_start:empty_team_sync_end]
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
    assert "setLanePaneCollapse(lane, lanePaneMaxHeight(lane));" in app_shell
    assert "if (host.emptyTeam) {\n    lockEmptyTeamPane(host);" in app_shell
    assert "if (lane.emptyTeam) return false;" in app_shell
    assert (
        "const requestedCollapsePx = lane.emptyTeam ? maxHeight : collapsePx;"
        in app_shell
    )
    assert (
        'lane.viewStackEl.classList.toggle("lane-view-stack--collapsed", visibleHeight < 1);'
        in app_shell
    )
    assert (
        'lane.modeRailEl.classList.toggle("lane-mode-rail--disabled", disabled);'
        in app_shell
    )
    assert "button.disabled = disabled;" in app_shell
    assert "button.tabIndex = disabled ? -1 : active ? 0 : -1;" in app_shell
    assert "lane.teamMenuButtonEl.disabled = false;" in app_groups
    assert 'lane.teamMenuButtonEl.removeAttribute("aria-hidden");' in app_groups
    assert 'lane.teamMenuButtonEl.removeAttribute("tabindex");' in app_groups
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


def test_static_empty_team_importer_renders_message_stream_choices():
    css = _serve_css_text()
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")

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
    assert ".empty-team-importer" in css
    assert "grid-column: 1 / -1;" in css
    assert ".empty-team-import-list" in css
    importer_start = css.index(".empty-team-importer {")
    importer_rules = css[importer_start : css.index("}", importer_start)]
    importer_copy_start = css.index(".empty-team-importer .target-choice-copy {")
    importer_copy_rules = css[importer_copy_start : css.index("}", importer_copy_start)]
    assert "direction: ltr;" in importer_rules
    assert "flex: 1 1 auto;" in importer_copy_rules
    assert ".empty-team-importer .target-choice-signal" not in css
