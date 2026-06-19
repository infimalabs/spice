"""Static serve UI header and menu contracts."""

from __future__ import annotations

from pathlib import Path
import subprocess

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
    assert html.index("/static/app.lanes.js") < html.index("/static/app.menu.js")
    assert html.index("/static/app.menu.js") < html.index("/static/app.shell.js")
    assert 'aria-label="Open teams"' in html
    assert 'id="open-lane" class="spice-menu-button"' in html
    assert 'aria-haspopup="menu" aria-expanded="false"' in html
    assert 'class="spice-menu-icon" aria-hidden="true">🌶️</span>' in html
    assert '<span class="spice-menu-label">spice</span>' in html
    assert 'const spiceServeBranding = {"name": "spice"};' in html
    assert "const serveBrandName = String(spiceServeBranding.name" in app_js
    assert "function serveBrandMenuTitle()" in app_js
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
    assert "font-weight: 400;" in button_rules
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


def test_index_branding_defaults_to_project_name_and_allows_explicit_override(
    tmp_path,
):
    pyproject = tmp_path / "pyproject.toml"
    pyproject.write_text('[project]\nname = "mission-control"\n', encoding="utf-8")

    project_html = render_index_html(tmp_path)

    assert "<title>mission-control</title>" in project_html
    assert 'title="Open mission-control menu"' in project_html
    assert 'aria-label="Open mission-control menu"' in project_html
    assert '<span class="spice-menu-label">mission-control</span>' in project_html
    assert 'const spiceServeBranding = {"name": "mission-control"};' in project_html

    pyproject.write_text(
        (
            '[project]\nname = "mission-control"\n\n'
            '[tool.spice.serve]\nbrand = "Ops Console"\n'
        ),
        encoding="utf-8",
    )

    override_html = render_index_html(tmp_path)

    assert "<title>Ops Console</title>" in override_html
    assert 'title="Open Ops Console menu"' in override_html
    assert 'aria-label="Open Ops Console menu"' in override_html
    assert '<span class="spice-menu-label">Ops Console</span>' in override_html
    assert 'const spiceServeBranding = {"name": "Ops Console"};' in override_html


def test_static_branding_config_feeds_fast_mode_and_audio_titles():
    app_types = (STATIC_ROOT / "app.types.js").read_text(encoding="utf-8")
    app_menu = (STATIC_ROOT / "app.menu.js").read_text(encoding="utf-8")
    app_audio = (STATIC_ROOT / "app.audio.js").read_text(encoding="utf-8")

    assert "@typedef {Object} ServeBranding" in app_types
    assert "var spiceServeBranding;" in app_types
    assert 'serveBrandMenuTitle() + " - fast mode on"' in app_menu
    assert ": serveBrandMenuTitle();" in app_menu
    assert "spiceServeBranding.name" in app_audio
    assert 'typeof spiceServeBranding === "object"' in app_audio
    assert "artist: defaultDocumentTitle" in app_audio


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


def test_static_lane_mode_rail_uses_text_labels_without_glyph_icons():
    css = _serve_css_text()
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    badge_start = css.index(".lane-mode-badge {")
    badge_rule = css[badge_start : css.index("}", badge_start)]

    assert "laneViewGlyphs" not in app_shell
    assert "lane-mode-glyph" not in app_shell
    assert "lane-mode-glyph" not in css
    assert (
        "'<span class=\"lane-mode-word\"></span>' +\n"
        "      '<span class=\"lane-mode-badge\" data-lane-view-badge hidden></span>'"
        in app_shell
    )
    assert ".lane-mode-word { display: none; }" not in css
    assert "font-family: ui-monospace, SFMono-Regular, Menlo, monospace;" in badge_rule
    assert (
        "box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--button-accent-fg) 24%, transparent);"
        in badge_rule
    )


def test_static_composer_band_menu_matches_large_team_action_sizing():
    css = _serve_css_text()
    composer_grid_start = css.index(".composer-band-menu.spice-menu-actions {")
    composer_grid_rule = css[composer_grid_start : css.index("}", composer_grid_start)]
    composer_action_start = css.index(".composer-band-menu-action {")
    composer_action_rule = css[
        composer_action_start : css.index("}", composer_action_start)
    ]
    composer_text_start = css.index(
        ".composer-band-menu-action .spice-menu-action-label,\n"
        ".composer-band-menu-action .spice-menu-action-detail {"
    )
    composer_text_rule = css[composer_text_start : css.index("}", composer_text_start)]
    composer_label_start = css.index(
        ".composer-band-menu-action .spice-menu-action-label {"
    )
    composer_label_rule = css[
        composer_label_start : css.index("}", composer_label_start)
    ]
    composer_detail_start = css.index(
        ".composer-band-menu-action .spice-menu-action-detail {",
        composer_label_start,
    )
    composer_detail_rule = css[
        composer_detail_start : css.index("}", composer_detail_start)
    ]
    team_grid_start = css.index(".lane-team-menu {")
    team_grid_rule = css[team_grid_start : css.index("}", team_grid_start)]
    team_action_start = css.index(".lane-team-menu .lane-team-menu-action {")
    team_action_rule = css[team_action_start : css.index("}", team_action_start)]
    team_label_start = css.index(
        ".lane-team-menu .lane-team-menu-action .spice-menu-action-label {",
        team_action_start,
    )
    team_label_rule = css[team_label_start : css.index("}", team_label_start)]

    for expected in (
        "gap: 6px;",
        "grid-auto-rows: minmax(72px, 1fr);",
        "grid-template-columns: repeat(auto-fit, minmax(min(148px, 100%), 1fr));",
    ):
        assert expected in composer_grid_rule
        assert expected in team_grid_rule

    for expected in (
        "flex-direction: column;",
        "gap: 6px;",
        "justify-content: center;",
        "min-height: 0;",
        "padding: 8px 10px;",
        "text-align: center;",
    ):
        assert expected in composer_action_rule
        assert expected in team_action_rule

    assert "overflow-wrap: anywhere;" in composer_text_rule
    assert "white-space: normal;" in composer_text_rule
    assert "font-size: clamp(12px, 7cqi, 16px);" in composer_label_rule
    assert "font-weight: 400;" in composer_label_rule
    assert "font-size: clamp(12px, 7cqi, 16px);" in team_label_rule
    assert "font-weight: 400;" in team_label_rule
    assert "font-size: clamp(10px, 5.25cqi, 13px);" in composer_detail_rule
    assert "margin-left: 0;" in composer_detail_rule
    assert "text-align: center;" in composer_detail_rule
    assert "width: 100%;" in composer_detail_rule
    assert (
        ".composer-band--menu-open .composer-band-menu.spice-menu-actions {" not in css
    )
    assert ".composer-band--menu-open .composer-band-menu-action {" not in css


def test_static_spice_menu_replaces_picker_lane():
    css = _serve_css_text()
    app_js = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_menu = (STATIC_ROOT / "app.menu.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_static = app_lanes + app_menu

    assert "let spiceMenuEl = null;" in app_js
    assert 'let spiceMenuDragTargetId = "";' in app_js
    assert "let spiceMenuRenderPending = false;" in app_js
    assert 'const fastModeStorageKey = "spice.serve.fastMode";' in app_js
    assert "let fastModeEnabled = storedFastModeEnabled();" in app_js
    assert "function storedFastModeEnabled()" in app_js
    assert 'storage.getItem(fastModeStorageKey) === "true"' in app_js
    assert "function persistFastModeEnabled(enabled)" in app_js
    assert 'storage.setItem(fastModeStorageKey, enabled ? "true" : "false");' in app_js
    assert (
        'if (typeof syncFastModeButtonState === "function") syncFastModeButtonState();'
        in app_js
    )
    assert "function openSpiceMenu()" in app_menu
    assert "function renderSpiceMenuIfAvailable()" in app_lanes
    assert 'if (typeof renderSpiceMenu === "function") renderSpiceMenu();' in app_lanes
    assert "function laneStateTargetIds()" in app_lanes
    assert "function sameStringSets(left, right)" in app_lanes
    assert (
        "if (!sameStringSets(openBefore, laneStateTargetIds()))\n"
        "    renderSpiceMenuIfAvailable();" in app_lanes
    )
    assert "renderSpiceMenuIfAvailable();" in app_shell
    assert (
        'lane.element.scrollIntoView({ block: "nearest", inline: "nearest" });'
        in app_lanes
    )
    assert "if (laneStates.size) closeSpiceMenu();" not in app_static
    assert "function setFastModeEnabled(enabled)" in app_menu
    assert "persistFastModeEnabled(fastModeEnabled);" in app_menu
    assert "function syncFastModeButtonState()" in app_menu
    assert (
        'if (typeof openLaneButton === "undefined" || !openLaneButton) return;'
        in app_menu
    )
    assert "function createEmptyTeamFromMenu()" not in app_menu
    assert "const spiceMenuNewTeamDropId" in app_menu
    assert "function spiceMenuTeamGroups(choices)" in app_menu
    assert "function spiceMenuNewTeamDropGroup()" in app_menu
    assert "function renderSpiceMenuTeamGroup(group)" in app_menu
    assert "function spiceMenuTeamDetail(group)" in app_menu
    assert "function compareSpiceMenuTargetChoices(left, right)" in app_menu
    assert "function spiceMenuTeamSortKey(group)" in app_menu
    assert "function wireSpiceMenuTargetDrag(button, target)" in app_menu
    assert "function wireSpiceMenuTeamDropTarget(container, group)" in app_menu
    assert 'label: "New team",' not in app_menu
    assert '"new team created"' in app_menu
    assert "New empty team" not in app_menu
    assert "empty team created" not in app_menu
    assert (
        "function moveTargetToMenuTeam(teamId, targetId, sourceTarget = null)"
        in app_menu
    )
    assert "const buttonRect = openLaneButton.getBoundingClientRect();" in app_menu
    assert "const top = Math.max(margin, buttonRect.bottom + margin);" in app_menu
    assert (
        "const width = spiceMenuWidthForButton(buttonRect, viewportWidth, margin);"
        in app_menu
    )
    assert (
        "const left = spiceMenuLeftForButton(buttonRect, width, viewportWidth, margin);"
        in app_menu
    )
    assert "spiceMenuMinimumLaneWidthPx()" in app_menu
    assert (
        "function spiceMenuWidthForButton(buttonRect, viewportWidth, margin)"
        in app_menu
    )
    assert "Math.max(spiceMenuMinimumLaneWidthPx(), buttonRect.width)" in app_menu
    assert (
        "function spiceMenuLeftForButton(buttonRect, width, viewportWidth, margin)"
        in app_menu
    )
    assert "const rightAlignedLeft = buttonRect.right - width;" in app_menu
    assert "Math.min(rightAlignedLeft, viewportWidth - width - margin)" in app_menu
    team_groups_start = app_menu.index("function spiceMenuTeamGroups(choices)")
    team_groups_end = app_menu.index(
        "function renderSpiceMenuTeamGroup(group)", team_groups_start
    )
    team_groups_block = app_menu[team_groups_start:team_groups_end]
    assert ".sort(compareSpiceMenuTargetChoices);" in team_groups_block
    assert "group.targets.sort(compareSpiceMenuTargetChoices);" in team_groups_block
    assert "unassigned.sort(compareSpiceMenuTargetChoices);" in team_groups_block
    assert "return compareTargetChoices(left, right);" in team_groups_block
    compare_groups_start = app_menu.index(
        "function compareSpiceMenuTeamGroups(left, right)"
    )
    compare_groups_end = app_menu.index(
        "function renderSpiceMenuTeamGroup(group)", compare_groups_start
    )
    compare_groups_block = app_menu[compare_groups_start:compare_groups_end]
    assert "spiceMenuTeamSortKey(left)" in compare_groups_block
    assert "spiceMenuTeamSortKey(right)" in compare_groups_block
    assert "return compareTargetChoices(left, right);" in compare_groups_block
    assert "function spiceMenuUsesViewportWidth(viewportWidth)" in app_menu
    assert "return viewportWidth < spiceMenuMinimumLaneWidthPx() + 20;" in app_menu
    assert (
        "if (spiceMenuUsesViewportWidth(viewportWidth)) return viewportWidth;"
        in app_menu
    )
    assert "if (spiceMenuUsesViewportWidth(viewportWidth)) return 0;" in app_menu
    assert 'spiceMenuEl.style.height = "";' in app_menu
    assert 'spiceMenuEl.style.maxHeight = height + "px";' in app_menu
    assert 'className = "lane picker"' not in app_static
    assert "openPickerLane" not in app_static
    assert "renderPickerChoices" not in app_shell
    assert ".spice-context-menu" in css
    assert ".picker" not in css


def test_static_spice_menu_team_groups_and_actions():
    app_menu = (STATIC_ROOT / "app.menu.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    css = _serve_css_text()

    assert 'teamCommandPayload("createTeam", {' in app_menu
    assert "members: [targetTeamAgentId(target)]," in app_menu
    assert 'heading.textContent = "open team";' in app_menu
    assert '? "loading teams"\n      : "team list unavailable";' in app_menu
    assert 'list.textContent = "no agents available";' in app_menu
    assert (
        'label.textContent = group.unassigned\n    ? "agents without team"' in app_menu
    )
    assert 'group.newTeam\n      ? "new team"' in app_menu
    assert '    ? "drop here to remove from team"' in app_menu
    assert '"drop agent to create"' in app_menu
    assert "spiceMenuNewTeamDropGroup()" in app_menu
    assert "function spiceMenuNewTeamDropHint()" in app_menu
    assert "function spiceMenuEmptyUnassignedDropHint()" in app_menu
    assert 'hint.className = "spice-menu-team-new-drop";' in app_menu
    assert 'hint.className = "spice-menu-team-empty-drop";' in app_menu
    assert 'hint.textContent = "Drop agent here";' in app_menu
    assert "wireSpiceMenuTeamDropTarget(container, group);" in app_menu
    assert '"open any member; " + count + " agents open together"' in app_menu
    assert "const alreadyOpen = laneStates.has(target.id);" in app_menu
    assert 'if (alreadyOpen) actionLabel = "Show team";' in app_menu
    assert 'else if (group && !group.unassigned) actionLabel = "Open team";' in app_menu
    assert 'button.classList.toggle("target-choice--open", alreadyOpen);' in app_menu
    assert 'if (laneStates.has(target.id)) parts.push("open");' in app_lanes
    assert 'setGlobalTransientStatus("open team failed");' in app_menu
    assert (
        'function targetChoiceButton(target, actionLabel, onClick, role = "menuitem")'
        in app_lanes
    )
    assert 'if (role) button.setAttribute("role", role);' in app_lanes
    assert ".spice-menu-team {" in css
    assert ".spice-menu-team--unassigned {" in css
    assert ".spice-menu-team--new-team-drop {" in css
    assert ".spice-menu-team--drop-ready {" in css
    assert ".spice-menu-team-header {" in css
    assert ".spice-menu-team-targets {" in css
    assert ".spice-menu-team-new-drop {" in css
    assert ".target-choice--open {" in css
    assert '.spice-menu-action[aria-checked="true"]' in css
    generic_action_start = css.index(".spice-menu-action {")
    action_label_start = css.index(".spice-menu-action-label {", generic_action_start)
    action_label_rule = css[action_label_start : css.index("}", action_label_start)]
    action_detail_start = css.index(".spice-menu-action-detail {", generic_action_start)
    action_detail_rule = css[action_detail_start : css.index("}", action_detail_start)]
    assert "white-space: nowrap;" in action_label_rule
    assert "white-space: nowrap;" in action_detail_rule
    assert "margin-left: auto;" in action_detail_rule
    assert "text-align: right;" in action_detail_rule


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
    assert "function compareTargetChoices(left, right)" in app_lanes
    assert (
        "const byStatus =\n"
        "    targetChoiceStatusOrder(left) - targetChoiceStatusOrder(right);"
        in app_lanes
    )
    assert "if (byStatus) return byStatus;" in app_lanes
    assert "const byRecency = compareTargetChoiceRecency(left, right);" in app_lanes
    assert "if (byRecency) return byRecency;" in app_lanes
    assert "function compareTargetChoiceRecency(left, right)" in app_lanes
    assert (
        "if (targetChoiceIsRunning(left) || targetChoiceIsRunning(right)) return 0;"
        in app_lanes
    )
    assert "return leftAt > rightAt ? -1 : 1;" in app_lanes
    assert "function targetChoiceIsRunning(target)" in app_lanes
    assert 'return status === "running" || status === "running-stale";' in app_lanes
    assert (
        "const byName = targetChoiceName(left).localeCompare(targetChoiceName(right));"
        in app_lanes
    )
    assert "if (byName) return byName;" in app_lanes
    assert (
        'return String(left.id || "").localeCompare(String(right.id || ""));'
        in app_lanes
    )
    assert "function targetChoiceStatusOrder(target)" in app_lanes
    assert "const index = targetChoiceStatusValues.indexOf(status);" in app_lanes
    assert "updateLiveTargetChoiceMetadata();" in app_render
    assert ".target-choice--running-stale .target-choice-signal" in css


def test_static_spice_menu_drag_manages_team_membership():
    app_menu = (STATIC_ROOT / "app.menu.js").read_text(encoding="utf-8")
    css = _serve_css_text()

    assert "wireSpiceMenuTargetDrag(button, target);" in app_menu
    assert 'button.style.touchAction = "none";' in app_menu
    assert 'button.addEventListener("click", (event) => {' in app_menu
    assert "suppressNextClick = true;" in app_menu
    assert "button.setPointerCapture(event.pointerId);" in app_menu
    assert "const state = {" in app_menu
    assert "spiceMenuTargetDragState = state;" in app_menu
    assert "state.dragGhost = createSpiceMenuTargetDragGhost(state.button);" in app_menu
    assert "updateSpiceMenuTargetDragGhost(state, event);" in app_menu
    assert "function spiceMenuTargetDragMatches(state, event, targetId)" in app_menu
    assert "function wireSpiceMenuTargetPointerDocumentEvents(target)" in app_menu
    assert "function updateSpiceMenuTargetDragFromEvent(event, target" in app_menu
    assert "function finishSpiceMenuTargetDragFromEvent(event, target)" in app_menu
    assert "function suppressNextSpiceMenuDragClick()" in app_menu
    assert "function moveTargetToMenuTeamOptimisticUi(teamId, targetId)" in app_menu
    assert "function clearSpiceMenuTargetDrag()" in app_menu
    assert "function createSpiceMenuTargetDragGhost(button)" in app_menu
    assert 'ghost.classList.add("target-choice-drag-ghost");' in app_menu
    assert "function updateSpiceMenuTargetDragGhost(state, event)" in app_menu
    assert "state.dragGhost?.remove();" in app_menu
    assert 'button.addEventListener("pointerdown", (event) => {' in app_menu
    assert 'button.addEventListener("pointermove", (event) => {' in app_menu
    assert 'button.addEventListener("pointerup", (event) => {' in app_menu
    assert 'button.addEventListener("pointercancel", (event) => {' in app_menu
    assert "Math.abs(dx) < 6 && Math.abs(dy) < 6" in app_menu
    assert "const el = document.elementFromPoint(clientX, clientY);" in app_menu
    assert "container.dataset.spiceMenuTeamId = group.teamId;" in app_menu
    assert (
        'container.dataset.spiceMenuUnassigned = group.unassigned ? "true" : "false";'
        in app_menu
    )
    assert 'container.dataset.spiceMenuNewTeam = group.newTeam ? "true" : "false";' in (
        app_menu
    )
    assert "function spiceMenuDropTeamId(container)" in app_menu
    assert (
        'if (container.dataset.spiceMenuNewTeam === "true")\n'
        "    return spiceMenuNewTeamDropId;" in app_menu
    )
    assert 'container.classList.add("spice-menu-team--drop-ready");' in app_menu
    assert "moveTargetToMenuTeamOptimisticUi(menuDropTeamId, target.id);" in app_menu
    assert (
        "moveTargetToMenuTeam(menuDropTeamId, target.id, sourceTarget).catch(() => {"
        in app_menu
    )
    assert "function optimisticNewMenuTeamIdentity(targetId)" in app_menu
    assert 'teamCommandPayload("moveAgentToTeam", {' in app_menu
    assert 'teamCommandPayload("createTeam", {' in app_menu
    assert 'teamCommandPayload("removeAgentFromTeam", {' in app_menu
    assert "members: [targetTeamAgentId(target)]," in app_menu
    assert "agentId: targetTeamAgentId(target)," in app_menu
    assert "agentAliases: targetTeamAgentAliases(target)," in app_menu
    assert "await refreshServerTopology();" in app_menu
    assert '"new team created"' in app_menu
    assert '"create team failed"' in app_menu
    assert (
        'setGlobalTransientStatus(teamId ? "team updated" : "agent removed from team");'
        not in app_menu
    )
    assert (
        'menuDropTeamId ? "move to team failed" : "remove from team failed",'
        not in app_menu
    )
    assert ".target-choice--draggable" in css
    assert ".target-choice--dragging {" in css
    assert ".target-choice-drag-ghost {" in css
    assert ".target-choice-drag-affordance {" in css
    assert ".spice-menu-team-empty-drop {" in css
    assert ".spice-menu-team-new-drop {" in css
    assert ".spice-menu-team--drop-ready .spice-menu-team-empty-drop {" in css
    assert ".spice-menu-team--drop-ready .spice-menu-team-new-drop {" in css


def test_spice_menu_new_team_drop_keeps_created_team_near_drop_zone():
    script = Path(__file__).with_name("fixtures") / "spice_menu_new_team_order.js"
    subprocess.run(
        ["node", str(script), str(STATIC_ROOT / "app.menu.js")],
        check=True,
    )


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
    assert "function teamImportPanel(lane, options = {})" in app_shell
    assert "function emptyTeamImportChoice(lane, target)" in app_shell
    assert "function teamImportChoice(lane, target, options = {})" in app_shell
    assert "lane.shardsEl.replaceChildren();" in empty_team_sync
    assert "renderMessagesIfChanged(lane);" in empty_team_sync
    assert empty_team_sync.index(
        "lane.shardsEl.replaceChildren();"
    ) < empty_team_sync.index("renderMessagesIfChanged(lane);")
    assert "return teamImportPanel(lane);" in app_shell
    assert "const overlay = Boolean(options.overlay);" in app_shell
    assert 'panel.classList.add("team-import-overlay");' in app_shell
    assert "function teamImportTargets(lane)" in app_shell
    assert "const memberTargetIds = new Set(laneGroupMemberTargetIds(host));" in (
        app_shell
    )
    assert 'const button = targetChoiceButton(\n    target,\n    "Import",' in app_shell
    assert '    "",\n  );' in app_shell
    assert "button.dataset.emptyTeamImportTargetId = target.id;" in app_shell
    assert "button.dataset.teamImportTargetId = target.id;" in app_shell
    assert 'teamCommandPayload("moveAgentToTeam", {' in app_shell
    assert "agentAliases: teamImportAliases(target)," in app_shell
    assert "function toggleTeamImportOverlay(lane)" in app_shell
    assert "function syncTeamImportOverlay(lane)" in app_shell
    assert "function positionTeamImportOverlay(host, overlay)" in app_shell
    assert "positionTeamImportOverlay(host, overlay);" in app_shell
    assert "host.element.append(overlay);" in app_shell
    assert (
        '    "--team-import-overlay-top",\n    host.messagesEl.offsetTop + "px",'
        in (app_shell)
    )
    assert 'host.teamMenuButtonEl.setAttribute("aria-expanded", "true");' in (app_shell)
    assert "syncTeamImportOverlay(lane);" in app_stream
    assert "if (lane.emptyTeam) return;" in app_stream
    assert (
        "if (isLaneOpen(lane) && !lane.emptyTeam) subscribeLaneToLiveBus(lane);"
        in app_stream
    )
    assert "function renderEmptyTeamMessages(lane)" in app_stream
    assert "function emptyTeamMessageFingerprint(lane)" in app_stream
    assert "targets: targets.map(emptyTeamTargetFingerprint)," in app_stream
    assert "function emptyTeamTargetFingerprint(target)" in app_stream
    assert "targetIdentityBranch(target.targetIdentity)," in app_stream
    assert "targetIdentityAgentName(target.targetIdentity)," in app_stream
    assert "targetIdentityThreadId(target.targetIdentity)," in app_stream
    assert 'target.lastAssistantAt || "",' in app_stream
    assert 'statusLine.lastAssistantAt || "",' in app_stream
    assert "target.pendingCount || 0," in app_stream
    assert "target.pendingInboxCount || 0," in app_stream
    assert 'target.agentProcessStatus || "",' in app_stream
    assert "targetIdentityThreadState(target.targetIdentity)," in app_stream
    assert (
        "lane.messagesEl.replaceChildren(\n"
        "    emptyTeamImportPanel(lane),\n"
        "    lane.historySentinelEl,\n"
        "  );"
    ) in app_stream
    assert ".empty-team-importer" in css
    assert ".team-import-overlay" in css
    assert "grid-column: 1 / -1;" in css
    assert ".empty-team-import-list" in css
    importer_start = css.index(".empty-team-importer {")
    importer_rules = css[importer_start : css.index("}", importer_start)]
    importer_copy_start = css.index(".empty-team-importer .target-choice-copy {")
    importer_copy_rules = css[importer_copy_start : css.index("}", importer_copy_start)]
    assert "direction: ltr;" in importer_rules
    assert "flex: 1 1 auto;" in importer_copy_rules
    assert ".empty-team-importer .target-choice-signal" not in css
