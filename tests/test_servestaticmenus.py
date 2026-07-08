"""Static serve UI contracts: composer and lane/team menu actions."""

from __future__ import annotations

from spice.serve.web import STATIC_ROOT
from tests.test_servestatic import (
    _between,
    _serve_css_text,
    _shell_and_composer_text,
)


def test_static_composer_menu_trigger_and_dismissal_are_wired():
    app_shell = _shell_and_composer_text()

    assert "trailingControl: composerBandMenuTrigger(" in app_shell
    assert (
        "function composerBandMenuTrigger(menuTitle, menuLabel, menuActions)"
        in app_shell
    )
    assert 'trigger.className = "composer-band-menu-button";' in app_shell
    assert 'trigger.setAttribute("aria-haspopup", "menu");' in app_shell
    assert "trigger.replaceChildren(composerBandMenuIcon());" in app_shell
    assert "function composerBandMenuIcon()" in app_shell
    assert 'icon.className = "composer-band-menu-icon";' in app_shell
    assert 'menu.className = "composer-band-menu spice-menu-actions";' in app_shell
    assert (
        'button.className = "composer-band-menu-action spice-menu-action";' in app_shell
    )
    assert "if (action.detail) button.title = action.detail;" in app_shell
    assert (
        'button.setAttribute("role", hasPressed ? "menuitemcheckbox" : "menuitem");'
        in app_shell
    )
    assert 'button.setAttribute("aria-checked", String(action.pressed));' in app_shell
    assert "let composerBandMenuDismissHandler = null;" in app_shell
    assert "closeComposerBandMenusExcept(band);" in app_shell
    assert (
        'document.addEventListener("pointerdown", composerBandMenuDismissHandler, true);'
        in app_shell
    )
    assert "function dismissComposerBandMenusOnPointerDown(event)" in app_shell
    assert (
        "if (menu?.contains(target) || trigger?.contains(target)) continue;"
        in app_shell
    )
    assert "function syncComposerBandMenuState(band)" in app_shell


def test_static_composer_menu_actions_include_team_moves_and_renewal():
    app_shell = _shell_and_composer_text()
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")

    assert 'composerBandMenuAction(\n    "Leave all teams",' in app_shell
    assert 'composerBandMenuAction(\n    "Create new team",' in app_shell
    assert 'composerBandMenuAction(\n    "Renew this agent",' in app_shell
    assert "return [create, leave, renew];" in app_shell
    assert app_shell.index('composerBandMenuAction(\n    "Create new team",') < (
        app_shell.index('composerBandMenuAction(\n    "Leave all teams",')
    )
    assert app_shell.index('composerBandMenuAction(\n    "Leave all teams",') < (
        app_shell.index('composerBandMenuAction(\n    "Renew this agent",')
    )
    assert '"Remove " + label + " from all teams"' in app_shell
    assert '"Move only " + label + " to a new team"' in app_shell
    assert "renew.keepOpen = true;" in app_shell
    assert (
        "renew.onClick = (requested) =>\n"
        "    toggleComposerAgentRenewalIntent(lane, member, requested);"
    ) in app_shell
    assert "if (!action.keepOpen) closeComposerBandMenu(band);" in app_shell
    assert (
        "if (hasPressed) syncComposerBandMenuActionPressed(button, nextPressed);"
        in app_shell
    )
    assert "function syncComposerBandMenuActionPressed(button, pressed)" in app_shell
    assert "requested = !composerRenewalIntentRequested(member)," in app_shell
    assert 'teamCommandPayload("setAgentRenewalIntent", {' in app_shell
    assert "agentId: laneTeamAgentId(member)," in app_shell
    assert "requested," in app_shell
    assert 'return "handoff pending";' in app_shell
    assert 'teamCommandPayload("splitTeam", {' in app_groups
    assert "agentIds: [laneTeamAgentId(member)]," in app_groups


def test_static_team_routing_uses_explicit_actor_ids():
    app_js = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_menu = (STATIC_ROOT / "app.menu.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")

    assert 'return id ? "target:" + id : "";' in app_js
    assert 'return actor ? "thread:" + actor : "";' in app_js
    assert "function targetTeamAgentId(target)" in app_js
    assert "function laneTeamAgentId(lane)" in app_lanes
    assert "return targetActor || targetTeamActorId(lane.targetId);" in app_lanes
    assert "const threadId = teamActorThreadId(actorId);" in app_lanes
    assert "members: [targetTeamAgentId(target)]," in app_lanes
    assert "members: [targetTeamAgentId(target)]," in app_menu
    assert "agentId: targetTeamAgentId(target)," in app_menu
    assert "agentId: targetTeamAgentId(target)," in app_shell
    assert "agentAliases: targetTeamAgentAliases(target)," in app_menu
    assert "return targetTeamAgentAliases(target);" in app_shell


def test_static_quote_close_control_keeps_composer_menu_actions_polished():
    css = _serve_css_text()
    app_shell = _shell_and_composer_text()
    button_start = css.index(
        ".composer-band-menu-button,\n.composer-band-close-button {"
    )
    button_end = css.index(".composer-band-menu-button:hover", button_start)
    button_rule = css[button_start:button_end]
    action_start = css.index(".composer-band-menu-action {")
    action_end = css.index(
        ".composer-band-menu-action .spice-menu-action-label", action_start
    )
    action_rule = css[action_start:action_end]
    shared_detail_start = css.index(
        ".composer-band-menu-action .spice-menu-action-label,\n"
        ".composer-band-menu-action .spice-menu-action-detail {",
        action_start,
    )
    shared_detail_rule = css[shared_detail_start : css.index("}", shared_detail_start)]
    detail_start = css.index(
        ".composer-band-menu-action .spice-menu-action-detail {\n  font-size",
        action_start,
    )
    detail_rule = css[detail_start : css.index("}", detail_start)]
    shared_grid_start = css.index(".spice-menu-actions {")
    shared_grid_end = css.index(".spice-menu-target-list {", shared_grid_start)
    shared_grid_rule = css[shared_grid_start:shared_grid_end]
    menu_grid_start = css.index(".composer-band-menu.spice-menu-actions {")
    menu_grid_end = css.index(".composer-band-menu-action {", menu_grid_start)
    menu_grid_rule = css[menu_grid_start:menu_grid_end]

    assert "trailingControl: composerBandCloseButton(" in app_shell
    assert (
        "function composerBandCloseButton(closeTitle, closeLabel, onClose)" in app_shell
    )
    assert 'close.className = "composer-band-close-button";' in app_shell
    assert 'close.textContent = "×";' in app_shell
    assert '"Remove quote",\n      "Remove quoted composer",' in app_shell
    assert "() => removeComposerQuoteDraft(lane, targetId, draft.id)" in app_shell
    assert 'menuTitle: "Quoted composer actions",' not in app_shell
    assert 'label: "Remove quote",' not in app_shell
    assert "border-radius: 50%;" in button_rule
    assert "height: 22px;" in button_rule
    assert "width: 22px;" in button_rule
    assert 'icon.style.height = "8px";' in app_shell
    assert 'icon.style.width = "11px";' in app_shell
    assert "display: grid;" in shared_grid_rule
    assert "grid-template-columns: repeat(auto-fit" in shared_grid_rule
    assert "display: grid;" in menu_grid_rule
    assert "grid-template-columns: repeat(auto-fit" in menu_grid_rule
    assert "grid-auto-rows: minmax(72px, 1fr);" in menu_grid_rule
    assert (
        "grid-template-columns: repeat(auto-fit, minmax(min(148px, 100%), 1fr));"
        in menu_grid_rule
    )
    assert (
        ".composer-band-close-button:hover,\n.composer-band-close-button:focus-visible {"
        in css
    )
    assert '.composer-band-menu-button[aria-expanded="true"] {' in css
    assert (
        ".composer-band--menu-open textarea,\n.composer-band--menu-open .composer-attachments {"
        in css
    )
    assert "align-items: center;" in action_rule
    assert "container-type: inline-size;" in action_rule
    assert "text-align: center;" in action_rule
    assert "display: block;" in shared_detail_rule
    assert "text-align: center;" in detail_rule
    assert "width: 100%;" in detail_rule
    assert "text-wrap: pretty;" in detail_rule
    assert (
        ".composer-band-menu-action .spice-menu-action-detail {\n  display: none;"
        not in css
    )


def test_static_lane_team_menu_exposes_close_split_and_restore_actions():
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")

    assert 'data-lane-team-menu title="Team actions"' in app_shell
    assert "function toggleLaneTeamMenu(lane, event = null)" in app_groups
    assert "host.viewStackEl.append(menu);" in app_groups
    assert 'menu.classList.add("lane-team-menu--empty-team-overlay");' in app_groups
    assert "host.element.append(menu);" in app_groups
    assert "function positionEmptyTeamMenuOverlay(host, menu)" in app_groups
    assert "syncLanePaneMetrics(host);" in app_groups
    assert (
        "menu.style.setProperty(\n"
        '    "--lane-team-menu-top",\n'
        '    host.viewStackEl.offsetTop + "px",\n'
        "  );" in app_groups
    )
    assert (
        "menu.style.setProperty(\n"
        '    "--lane-team-menu-height",\n'
        '    lanePaneMaxHeight(host) + "px",\n'
        "  );" in app_groups
    )
    assert 'label: "Close team",' in app_groups
    assert 'label: "Import agent",' in app_groups
    assert 'detail: host.teamImportOverlayOpen ? "close panel" : "choose agent",' in (
        app_groups
    )
    assert "cover messages" not in app_groups
    assert 'label: "Split into individuals",' in app_groups
    assert 'label: "Restore previous team",' in app_groups
    assert "if (host.emptyTeam) return [closeTeamMenuAction(host)];" in app_groups
    assert 'detail: host.emptyTeam\n      ? "empty"' in app_groups
    assert "onClick: () => toggleTeamImportOverlay(host)," in app_groups
    assert "if (host.teamImportOverlayOpen) {\n    closeTeamImportOverlay(host);" in (
        app_groups
    )
    assert "if (host.teamImportOverlayEl?.contains(target)) continue;" in app_groups
    assert "closeTeamImportOverlay(host);" in app_groups
    close_team_index = app_groups.index('label: "Close team",')
    import_agent_index = app_groups.index('label: "Import agent",')
    split_individuals_index = app_groups.index('label: "Split into individuals",')
    restore_previous_index = app_groups.index('label: "Restore previous team",')
    assert close_team_index < import_agent_index
    assert import_agent_index < split_individuals_index
    assert split_individuals_index < restore_previous_index
    assert 'teamCommandPayload("splitTeamBack", {' in app_groups


def test_static_lane_team_menu_keeps_large_tiles_and_centered_detail():
    css = _serve_css_text()
    messages_css = (STATIC_ROOT / "messages.css").read_text(encoding="utf-8")

    lane_rule = _between(css, ".lane {", ".lane--shadowed")
    view_stack_rule = _between(css, ".lane-view-stack {", ".lane-view-stack--collapsed")
    messages_rule = _between(messages_css, ".messages {", ".messages article")
    menu_rule = _between(
        css, ".lane-team-menu {", ".lane-team-menu--empty-team-overlay {"
    )
    menu_override_rule = _between(
        css,
        ".lane-team-menu.spice-menu-actions {",
        "}",
    )
    empty_team_overlay_rule = _between(
        css,
        ".lane-team-menu--empty-team-overlay {",
        ".lane-team-menu .lane-team-menu-action {",
    )
    action_rule = _between(
        css,
        ".lane-team-menu .lane-team-menu-action {",
        ".lane-team-menu .lane-team-menu-action .spice-menu-action-label",
    )
    text_rule = _between(
        css,
        ".lane-team-menu .lane-team-menu-action .spice-menu-action-label",
        ".lane-team-menu-action:disabled",
    )
    team_import_overlay_rule = _between(css, ".team-import-overlay {", "}")

    assert "position: relative;" in lane_rule
    assert "position: relative;" in view_stack_rule
    assert "position: relative;" in messages_rule
    assert "align-content: stretch;" in menu_rule
    assert "align-content: stretch;" in menu_override_rule
    assert "position: absolute;" in menu_rule
    assert "inset: 0;" in menu_rule
    assert "height: var(--lane-team-menu-height, 120px);" in empty_team_overlay_rule
    assert "inset: var(--lane-team-menu-top, 0px) 0 auto;" in empty_team_overlay_rule
    assert "position: absolute;" in team_import_overlay_rule
    assert "align-self: stretch;" in team_import_overlay_rule
    assert "justify-self: stretch;" in team_import_overlay_rule
    assert "top: var(--team-import-overlay-top, 0px);" in team_import_overlay_rule
    assert "bottom: 0;" in team_import_overlay_rule
    assert "left: 0;" in team_import_overlay_rule
    assert "right: 0;" in team_import_overlay_rule
    assert "z-index: 7;" in team_import_overlay_rule
    assert "grid-auto-rows: minmax(72px, 1fr);" in menu_rule
    assert "z-index: 6;" in menu_rule
    assert "align-items: center;" in action_rule
    assert "container-type: inline-size;" in action_rule
    assert "flex-direction: column;" in action_rule
    assert "gap: 6px;" in action_rule
    assert "justify-content: center;" in action_rule
    assert "min-height: 0;" in action_rule
    assert "overflow: hidden;" in action_rule
    assert "padding: 8px 10px;" in action_rule
    assert "text-align: center;" in action_rule
    assert "display: block;" in text_rule
    assert "max-width: 100%;" in text_rule
    assert "overflow-wrap: anywhere;" in text_rule
    assert "font-size: clamp(12px, 7cqi, 16px);" in text_rule
    assert "font-size: clamp(10px, 5.25cqi, 13px);" in text_rule
    assert "margin-left: 0;" in text_rule
    assert "text-align: center;" in text_rule
    assert "text-wrap: balance;" in text_rule
    assert "text-wrap: pretty;" in text_rule
    assert "white-space: normal;" in text_rule
    assert "width: 100%;" in text_rule
