"""Static serve UI contracts."""

from __future__ import annotations

import subprocess
from pathlib import Path

from spice.serve.web import STATIC_ROOT

SERVE_CSS_FILES = ("index.css", "composer.css", "messages.css", "status-colors.css")


def _serve_css_text() -> str:
    return "\n".join(
        (STATIC_ROOT / filename).read_text(encoding="utf-8")
        for filename in SERVE_CSS_FILES
    )


def _between(text: str, start: str, end: str) -> str:
    start_index = text.index(start)
    return text[start_index : text.index(end, start_index)]


def _assert_contains_all(text: str, snippets: tuple[str, ...]) -> None:
    for snippet in snippets:
        assert snippet in text


def _shell_and_composer_text() -> str:
    return "\n".join(
        (STATIC_ROOT / filename).read_text(encoding="utf-8")
        for filename in ("app.shell.js", "app.composer.js")
    )


def test_static_initial_bootstrap_waits_for_server_topology():
    app = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")

    assert (
        "async function init() {\n"
        "  await connectLiveBus();\n"
        "  await refreshServerTopology();\n"
        "  setInterval(updateLiveRelativeTimes, relativeTimeTickMs);\n"
        "}\n"
    ) in app


def test_static_send_route_applies_fresh_start_identity_before_refresh():
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    send_start = app_stream.index("function applyLaneSendResult(")
    send_body = app_stream[
        send_start : app_stream.index(
            "\n}\n\nfunction agentEnsureFailureStatus", send_start
        )
    ]
    route_start = app_stream.index("function applyTaskDrainRouteConfig(")
    route_body = app_stream[
        route_start : app_stream.index(
            "\n}\n\nfunction applyRouteConfigToTargetInventory",
            route_start,
        )
    ]
    inventory_start = app_stream.index("function applyRouteConfigToTargetInventory(")
    inventory_body = app_stream[inventory_start:]

    assert 'const previousThreadId = lane.targetThreadId || "";' in send_body
    assert "const changed = ensure.threadId !== previousThreadId;" in send_body
    assert "applyRouteConfigToTargetInventory(lane, config);" in route_body
    assert 'payloadHasField(config, "targetIdentity")' in route_body
    assert "applyLaneTargetIdentity(lane, config);" in route_body
    assert 'payloadHasField(config, "serveAgentIdentity")' in route_body
    assert "applyLaneServeAgentIdentity(lane, config);" in route_body
    assert "target.targetIdentity = config.targetIdentity;" in inventory_body
    assert "target.serveAgentIdentity = config.serveAgentIdentity;" in inventory_body
    assert "target.teamIdentity = config.teamIdentity;" in inventory_body


def test_static_lane_status_preview_requires_relative_time():
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    start = app_render.index("function setLaneStatus(lane, statusLine) {")
    body = app_render[
        start : app_render.index("\n}\n\nfunction setLaneStatusText", start)
    ]

    assert (
        "const previewHasTime = Boolean(preview && statusLine.lastAssistantAt);" in body
    )
    assert (
        'time: previewHasTime ? relativeTime(statusLine.lastAssistantAt) : "",' in body
    )
    assert 'preview: previewHasTime ? preview : "",' in body


def test_static_css_has_narrow_viewport_affordances():
    css = _serve_css_text()

    assert "@media (max-width: 720px)" in css
    assert "scroll-snap-type: x proximity" in css
    assert "--mobile-lane-gap: 8px" in css
    assert "--mobile-lane-gutter: 4px" in css
    assert "gap: var(--mobile-lane-gap)" in css
    assert "padding: 0 var(--mobile-lane-gutter) 8px" in css
    assert "scroll-padding-inline: var(--mobile-lane-gutter)" in css
    assert "touch-action: pan-x pan-y" in css
    assert "flex: 0 0 100%" in css
    assert "min-width: 100%" in css
    assert "border-radius: 7px" in css
    assert "height: 100dvh" in css


def test_audio_playback_enforces_single_owner():
    app_audio = (STATIC_ROOT / "app.audio.js").read_text(encoding="utf-8")

    # A new clip claims sole ownership: it bumps the generation token and
    # hard-stops any in-flight clip before creating the next one.
    play_start = app_audio.index("function playAudioBuffer(")
    play_rule = app_audio[play_start : app_audio.index("\n}", play_start)]
    assert "const generation = (playbackGeneration += 1);" in play_rule
    assert "stopActivePlayback();" in play_rule
    assert "activePlaybackAudio = audio;" in play_rule
    # A late-resolving play() that lost the race stops itself.
    assert "if (generation !== playbackGeneration) stopOrphanedPlayback(audio);" in (
        play_rule
    )
    # finish is idempotent so the pause/ended/error events cannot double-resolve.
    assert "if (settled) return;" in play_rule
    assert "function stopActivePlayback()" in app_audio


def test_header_pill_scroller_is_sole_grower_and_button_stays_right():
    css = _serve_css_text()

    strip_start = css.index(".filter-strip {")
    strip_rule = css[strip_start : css.index("}", strip_start)]
    button_start = css.index(".spice-menu-button {")
    button_rule = css[button_start : css.index("}", button_start)]

    # No separate status text slot can split header width with the pill scroller.
    assert ".app-header .meta" not in css
    assert "flex: 1 1 auto;" in strip_rule
    assert "min-width: 0;" in strip_rule
    assert "margin-left: auto;" in button_rule


def test_global_transient_status_renders_in_lane_status_line():
    app_render = STATIC_ROOT / "app.render.js"
    script = Path(__file__).with_name("fixtures") / "global_status_line.js"

    result = subprocess.run(
        ["node", str(script), str(app_render)],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_static_css_centers_two_pip_lane_light_stack():
    css = _serve_css_text()
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
    css = _serve_css_text()
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    messages_start = css.rindex(".messages {")
    messages_end = css.index(".messages article {", messages_start)
    messages_rule = css[messages_start:messages_end]
    article_start = css.index(".messages article {")
    article_end = css.index(".messages article.image-only", article_start)
    article_rule = css[article_start:article_end]
    stack_start = css.index(".message-body p.message-image-stack {")
    stack_end = css.index(
        ".message-body p.message-image-stack .message-image", stack_start
    )
    stack_rule = css[stack_start:stack_end]
    stack_image_start = css.index(
        ".message-body p.message-image-stack .message-image img {"
    )
    stack_image_end = css.index("}", stack_image_start)
    stack_image_rule = css[stack_image_start:stack_image_end]

    assert "grid-template-columns: repeat(" in css
    assert "direction: rtl;" in messages_rule
    assert "minmax(156px, 1fr)" in messages_rule
    assert "overflow-x: auto;" in messages_rule
    assert "direction: ltr;" in article_rule
    assert ".messages article.image-only" in css
    assert "grid-column: span 1" in css
    assert "display: flex;" in stack_rule
    assert "flex-direction: row;" in stack_rule
    assert "flex-wrap: nowrap;" in stack_rule
    assert "justify-content: flex-start;" in stack_rule
    assert "overflow-x: auto;" in stack_rule
    assert "max-height: 136px;" in stack_image_rule
    assert "max-width: 156px;" in stack_image_rule
    assert "object-fit: contain;" in stack_image_rule
    assert ".messages article.image-only .message-image img" in css
    assert "max-height: 136px" in css
    assert ".history-sentinel {\n  grid-column: 1 / -1;" in css
    assert 'if (item.image_only) article.classList.add("image-only");' in app_render


def test_static_inline_task_directives_use_quote_like_accented_blocks():
    css = _serve_css_text()
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")

    quote_rule = _between(css, ".message-body .task-directive-quote {", "}")
    property_rule = _between(css, ".task-directive-property {", "}")
    detail_rule = _between(css, ".task-directive-property dd {", "}")
    palette = _between(app_render, "const messageOccupantAccentPalette = [", "];")

    assert '"var(--team-plum-accent)",' in palette.splitlines()[6]
    assert "--quote-accent: var(--team-plum-accent);" in quote_rule
    assert "background: color-mix(in srgb, var(--quote-accent) 7%, transparent);" in (
        quote_rule
    )
    assert "display: grid;" in quote_rule
    assert ".task-directive-kicker {" in css
    assert ".task-directive-properties {" in css
    assert "grid-template-columns: minmax(64px, max-content) minmax(0, 1fr);" in (
        property_rule
    )
    assert "font: 12px/1.35 ui-monospace, SFMono-Regular, Menlo, monospace;" in (
        detail_rule
    )
    assert "overflow-wrap: anywhere;" in detail_rule


def test_static_draft_composers_use_14px_font():
    css = _serve_css_text()
    selector = ".composer-shard textarea {"
    start = css.index(selector)
    end = css.index("}", start)
    textarea_rule = css[start:end]

    assert "font-size: 14px;" in textarea_rule


def test_static_metrics_pane_preserves_controls_and_top_chart():
    css = _serve_css_text()
    app_panes = (STATIC_ROOT / "app.panes.js").read_text(encoding="utf-8")
    metrics_panel_rule = _between(
        css, '.lane-view-panel[data-lane-view-panel="metrics"] {', "}"
    )
    metrics_grid_rule = _between(css, ".lane-metrics-grid {", "}")
    chart_rule = _between(css, ".lane-metric-series-chart {", "}")
    svg_rule = _between(css, ".lane-metric-series-svg {", "}")

    assert "display: flex;" in metrics_panel_rule
    assert "flex-direction: column;" in metrics_panel_rule
    assert "flex: 1 1 auto;" in metrics_grid_rule
    assert "grid-template-rows: minmax(96px, 1fr) repeat(4, max-content);" in (
        metrics_grid_rule
    )
    assert "min-height: 0;" in metrics_grid_rule
    assert "display: flex;" in chart_rule
    assert "height: 100%;" in svg_rule
    assert "min-height: 0;" in svg_rule
    _assert_contains_all(
        app_panes,
        (
            "function laneMetricGridSlot(grid, slot)",
            "syncLaneMetricElementChildren(grid, nodes);",
            "__spiceLaneMetricSlot",
            "__spiceLaneMetricSeriesSelect",
            "syncLaneMetricSeriesSelectOptions(select, selectedValue, options);",
            "select.value = selected;",
        ),
    )


def test_static_composer_shards_reverse_visually_without_retargeting():
    css = _serve_css_text()
    app_shell = _shell_and_composer_text()
    composer_start = css.index(".lane-composer {")
    composer_end = css.index("/* Shards", composer_start)
    composer_rule = css[composer_start:composer_end]
    shards_start = css.index(".composer-shards {")
    shards_end = css.index(".composer-shard {", shards_start)
    shards_rule = css[shards_start:shards_end]
    sync_start = app_shell.index("function syncComposerShards(lane, members)")
    sync_end = app_shell.index("function composerShardElementForTarget", sync_start)
    sync_body = app_shell[sync_start:sync_end]

    assert "grid-template-columns: minmax(0, 1fr) auto;" in composer_rule
    assert "flex-direction: row-reverse;" in shards_rule
    assert "const shards = wanted.map((member) => {" in sync_body
    assert "syncComposerShard(lane, shard, member);" in sync_body
    assert "syncComposerShardOrder(lane.shardsEl, shards);" in sync_body
    assert ".reverse()" not in sync_body


def test_static_composer_attachment_thumbnails_fill_header():
    css = _serve_css_text()
    app_shell = _shell_and_composer_text()

    attachments_start = css.index(".composer-attachments {")
    attachments_end = css.index(".composer-attachments[hidden]", attachments_start)
    attachments_rule = css[attachments_start:attachments_end]
    header_start = css.index(".composer-band-header {")
    header_end = css.index(".composer-band-header--attachments", header_start)
    header_rule = css[header_start:header_end]
    attachment_header_start = css.index(".composer-band-header--attachments {")
    attachment_header_end = css.index("}", attachment_header_start)
    attachment_header_rule = css[attachment_header_start:attachment_header_end]
    list_start = css.index(".composer-attachment-list {")
    list_end = css.index(".composer-attachment-chip {", list_start)
    list_rule = css[list_start:list_end]
    title_start = css.index(".composer-band-title {")
    title_end = css.index("}", title_start)
    title_rule = css[title_start:title_end]
    title_shadow_start = css.index(
        ".composer-band-body--attachments .composer-band-title {"
    )
    title_shadow_end = css.index("}", title_shadow_start)
    title_shadow_rule = css[title_shadow_start:title_shadow_end]
    chip_start = css.index(".composer-attachment-chip {")
    chip_end = css.index(".composer-attachment-chip img", chip_start)
    chip_rule = css[chip_start:chip_end]
    chip_image_start = css.index(".composer-attachment-chip img {")
    chip_image_end = css.index("}", chip_image_start)
    chip_image_rule = css[chip_image_start:chip_image_end]
    name_start = css.index(".composer-attachment-name {")
    name_end = css.index("}", name_start)
    name_rule = css[name_start:name_end]

    assert 'body.className = "composer-band-body";' in app_shell
    assert 'const body = parent.querySelector(".composer-band-body");' in app_shell
    assert "composer-band-header--attachments" in app_shell
    assert "padding: 0 5px 0 8px;" in header_rule
    assert "gap: 6px;" in attachment_header_rule
    assert "padding-left: 8px;" in attachment_header_rule
    assert (
        'wrap.style.setProperty("--composer-attachment-count", String(attachments.length));'
        in app_shell
    )
    assert "flex: 1 1 auto;" in title_rule
    assert "overflow: hidden;" in title_rule
    assert "text-overflow: ellipsis;" in title_rule
    assert (
        "-webkit-mask-image: linear-gradient(90deg, #000 calc(100% - 18px), transparent);"
        in title_shadow_rule
    )
    assert (
        "mask-image: linear-gradient(90deg, #000 calc(100% - 18px), transparent);"
        in title_shadow_rule
    )
    assert "flex: 0 0 auto;" in attachments_rule
    assert "overflow-x: auto;" in attachments_rule
    assert "height: 100%;" in attachments_rule
    assert "justify-content: flex-end;" in attachments_rule
    assert "margin-left: auto;" in attachments_rule
    assert "min-width: 26px;" in attachments_rule
    assert (
        "max-width: min(100%, calc(var(--composer-attachment-count, 1) * 28px - 2px));"
        in attachments_rule
    )
    assert "flex-direction: row-reverse;" in list_rule
    assert "gap: 2px;" in list_rule
    assert "height: 26px;" in chip_rule
    assert "flex: 0 0 26px;" in chip_rule
    assert "min-width: 26px;" in chip_rule
    assert "width: 26px;" in chip_rule
    assert "min-width: 100%;" in chip_image_rule
    assert "display: none;" in name_rule


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
    assert "return [leave, create, renew];" in app_shell
    assert app_shell.index('composerBandMenuAction(\n    "Leave all teams",') < (
        app_shell.index('composerBandMenuAction(\n    "Create new team",')
    )
    assert app_shell.index('composerBandMenuAction(\n    "Create new team",') < (
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


def test_static_composer_header_drag_suppresses_browser_selection():
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")

    pointer_start = app_groups.index(
        'handle.addEventListener("pointerdown", (event) => {'
    )
    pointer_end = app_groups.index(
        'handle.addEventListener("pointermove", (event) => {', pointer_start
    )
    pointer_block = app_groups[pointer_start:pointer_end]

    assert (
        "event.preventDefault();\n"
        "    const state = beginComposerMoveDrag(host, targetId, event, handle);"
    ) in pointer_block
    assert (
        "state.pointerCleanup = wireComposerMovePointerDocumentEvents(handle);"
        in pointer_block
    )
    assert "handle.setPointerCapture(event.pointerId);" in pointer_block


def test_static_composer_drag_has_ghost_drop_zones_and_reorder_command():
    css = _serve_css_text()
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")
    app_shell = _shell_and_composer_text()

    assert ".composer-shard--drag-ghost" in css
    assert ".composer-shard--dragging > *" in css
    assert ".composer-band--dragging > *" in css
    assert ".composer-shards--reordering .composer-shard" in css
    assert "transition: transform" in css
    assert ".composer-shard--reorder-shift" in css
    assert ".lane--composer-drop .composer-shards" in css
    assert ".lane--dragging > *" in css
    assert ".lane-drag-ghost" in css
    assert "function composerReorderDropTarget(state, clientX, clientY)" in app_groups
    assert "function currentLaneGroupHostByMemberTargetId()" in app_groups
    assert "function stableLaneGroupHost(members, previousHostByMemberTargetId)" in (
        app_groups
    )
    assert "const shadows = members.filter((member) => member !== host);" in (
        app_groups
    )
    assert 'state.dropTarget = { kind: "move", lane: targetLane };' in app_groups
    assert 'teamCommandPayload("reorderTeamAgents", {' in app_groups
    assert "orderedTargetIds" in app_groups
    assert 'state.sourceShard?.classList.add("composer-shard--dragging");' in app_groups
    assert "function ensureLaneDragGhost(state)" in app_groups
    assert "function updateLaneDragGhost(state, clientX, clientY)" in app_groups
    assert "state.dragGhost?.remove();" in app_groups
    assert "wireComposerMoveDrag(lane, header, member.targetId);" in app_shell


def test_static_relative_times_are_monospace_and_padded():
    css = _serve_css_text()
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")

    assert 'return String(value).padStart(2, "\xa0") + unit;' in app_render
    assert ".compaction-meta time,\n.lane-status-time,\n.message-footer time {" in css
    assert "font-family: ui-monospace, SFMono-Regular, Menlo, monospace;" in css
    assert "white-space: pre;" in css
    assert ".composer-quote-time" in css
    assert "font-variant-numeric: tabular-nums;" in css


def test_static_composer_placeholders_use_uniform_agent_status_copy():
    app_shell = _shell_and_composer_text()

    assert "const label = laneMemberTargetLabel(member);" in app_shell
    assert 'return [label, status].filter(Boolean).join("\\n");' in app_shell
    assert "function laneComposePlaceholderStatus(member)" in app_shell
    assert "const pending = lanePendingDisplayCount(member);" in app_shell
    assert 'parts.push(pending + " pending");' in app_shell
    assert 'if (pending > 0) parts.push(pending + " pending");' not in app_shell
    assert (
        'const status = (member.lastRenderedStatusLine || {}).agentProcessStatus || "";'
        in app_shell
    )
    assert "if (status) parts.push(status);" in app_shell
    assert 'return "Steer " + laneMemberTargetLabel(lane);' not in app_shell
    assert 'textarea.placeholder = "Reply with quoted context";' not in app_shell
    assert "const member = laneStates.get(targetId) || lane;" in app_shell
    assert "syncComposerQuoteBand(band, lane, targetId, member, draft);" in app_shell
    assert "createComposerQuoteTextarea(lane, targetId, draft);" in app_shell
    assert (
        app_shell.count("textarea.placeholder = laneComposePlaceholder(member);") >= 3
    )


def test_static_target_choice_labels_show_agent_name_on_branch():
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")

    assert "function agentBranchLabel(agentName, branchName)" in app_render
    assert 'return agent + " on " + branch;' in app_render
    assert "return agentBranchLabel(agent, branch);" in app_groups
    assert "return targetIdentityDisplayLabel(target.targetIdentity);" in app_lanes


def test_static_submitted_message_predictions_reconcile_against_server_echoes():
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")

    assert "optimisticSubmittedInboxKeys: new Set()," in app_shell
    assert "optimisticPendingInboxFloor: 0," in app_shell
    assert 'const inboxKey = String(options.inboxKey || "");' in app_render
    assert "const submittedPendingFloor = hasBackendCount" in app_render
    assert "if (accepted && inboxKey && submittedPendingFloor > 0)" in app_render
    assert "lane.optimisticSubmittedInboxKeys.add(inboxKey);" in app_render
    assert "laneSubmittedMessagePendingFloor(lane)" in app_render
    assert "clearDrainedSubmittedMessagePredictions(lane)" in app_render
    assert "Number(lane.pendingSubmissionCount)" in app_render
    assert "function laneSubmittedMessagePendingFloor(lane)" in app_render
    assert "function reconcileSubmittedMessagePredictions(lane)" in app_render
    assert "const ackedKeys = new Set(ackKeysForMessages(lane.knownMessages));" in (
        app_render
    )
    assert "if (ackedKeys.has(key)) lane.optimisticSubmittedInboxKeys.delete(key);" in (
        app_render
    )
    assert "inboxKey: result.key," in app_stream


def test_static_pending_count_clears_stale_submitted_predictions_after_drain():
    app_stream = STATIC_ROOT / "app.stream.js"
    app_render = STATIC_ROOT / "app.render.js"
    script = Path(__file__).with_name("fixtures") / "pending_count_reconcile.js"

    subprocess.run(
        ["node", str(script), str(app_stream), str(app_render)],
        check=True,
    )


def test_static_lifetime_slider_uses_steer_drive_drain_without_renew_send_flag():
    app = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_controls = (STATIC_ROOT / "app.controls.js").read_text(encoding="utf-8")

    _assert_contains_all(
        app,
        (
            'const agentLifetimeLabels = ["Steer", "Drive", "Drain"];',
            'Steer: "Manual filters only",',
            'Drive: "Auto-subscribe to projects this team creates or claims",',
            'Drain: "Boundary dissolved: see all assignable work",',
            "function agentLifetimeAutoManagesTasks(lifetime) {",
            'return lifetime === "Drive";',
            "function agentLifetimeUsesStoredTaskFilters(lifetime) {",
            'return lifetime === "Steer" || lifetime === "Drive";',
            "function agentLifetimeDissolvesTaskBoundary(lifetime) {",
            'return lifetime === "Drain";',
            "function agentLifetimeHelpText(lifetime) {",
        ),
    )
    _assert_contains_all(
        app_shell,
        (
            "data-lifetime-label>Drive</span>",
            "data-submit>Drive</button>",
            "const lifetime = target.lifetime || defaultAgentLifetime;",
            "serverLifetime: lifetime,",
            'pendingLifetimeCommit: "",',
            "pendingLifetimeConfigRevision: 0,",
            "pendingLifetimeRequestId: 0,",
            "lifetimeRequestId: 0,",
            "applyServerLaneLifetime(lane, config.lifetime, {",
            "configRevision: config.revision,",
        ),
    )
    assert "renewAgent" not in app_controls
    assert '"Renew"' not in app


def test_static_lifetime_slider_tracks_pending_state_in_controls():
    app_controls = (STATIC_ROOT / "app.controls.js").read_text(encoding="utf-8")

    _assert_contains_all(
        app_controls,
        (
            "host.lifetimeRequestId = Math.max",
            "host.pendingLifetimeCommit = lifetime;",
            "host.pendingLifetimeRequestId = host.lifetimeRequestId;",
            "host.serverLifetime = laneServerLifetime(host);",
            "function updateLaneLifetimeForLane(lane) {",
            "function updateEmptyTeamLifetimeForLane(host) {",
            "if (host.emptyTeam && host.teamId) {",
            "configPatch: { lifetime: requestedLifetime },",
            "function serverLifetimeSupersedesPending(host, options = {})",
            "if (options.supersedePending !== true) return false;",
            "function serverLifetimeSettlesPending(host, lifetime, options = {})",
            "if (host.pendingLifetimeCommit && lifetime !== host.pendingLifetimeCommit)",
            "serverLifetimeSettlesPending(host, lifetime, options)",
            'host.pendingLifetimeCommit = "";',
            "host.pendingLifetimeConfigRevision = 0;",
            "host.pendingLifetimeRequestId = 0;",
            "function laneLifetimeCommitMatches(host, lifetime, options = {})",
            "function clearLaneLifetimeCommit(lane, lifetime, options = {})",
            "function rollbackLaneLifetimeCommit(",
            'serverLifetime = "",',
            "options = {},",
            "const lifetimeHelp = agentLifetimeHelpText(lifetime);",
            "lane.lifetimeRangeEl.title = lifetimeHelp;",
            '"Task subscription policy: " + lifetimeHelp',
            "lane.lifetimeLabelEl.title = lifetimeHelp;",
            'lane.submitEl.title = "Send with " + lifetime + ": " + lifetimeHelp;',
        ),
    )


def test_static_lifetime_slider_syncs_server_state_sources():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")

    _assert_contains_all(
        app_stream,
        (
            "const requestedLifetime = payload.lifetime;",
            "const lifetimeRequestId = Math.max",
            "const pendingLifetimeRequestId =",
            "lifetimeRequestId,",
            "settleLaneLifetimeCommit(",
            "if (!requestedLifetimeRequestId) return;",
            "if (options.lifetimeRequestId === undefined) return true;",
            "taskDrainLifetimeResponseIsCurrent(lane, options)",
            "requestId: options.lifetimeRequestId,",
            "if (pendingLifetimeRequestId)",
            "requestId: pendingLifetimeRequestId,",
            "supersedePending: false,",
        ),
    )
    _assert_contains_all(
        app_lanes,
        (
            "applyServerLaneLifetime(lane, config.lifetime, {",
            "configRevision: config.revision,",
        ),
    )
    _assert_contains_all(
        app_render,
        (
            'payloadHasField(payload, "teamIdentity")',
            "teamIdentityConfigRevision(payload.teamIdentity)",
        ),
    )
    _assert_contains_all(
        app_groups,
        (
            "pendingLaneLifetimeStateForMembers(members, lifetimeStateByTargetId)",
            "laneLifetimeRuntimeState(lane)",
            "restoreLaneLifetimeRuntimeState(",
        ),
    )


def test_lifetime_slider_pending_commit_ignores_stale_server_lifetimes():
    app_controls = STATIC_ROOT / "app.controls.js"
    script = Path(__file__).with_name("fixtures") / "lifetime_slider_pending.js"

    result = subprocess.run(
        ["node", str(script), str(app_controls)],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_lane_pane_slider_moves_panels_with_rail_direction():
    app_shell = STATIC_ROOT / "app.shell.js"
    script = Path(__file__).with_name("fixtures") / "lane_pane_direction.js"

    result = subprocess.run(
        ["node", str(script), str(app_shell)],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_static_sync_composer_placeholders_refreshes_existing_quote_textareas():
    app_shell = _shell_and_composer_text()
    sync_start = app_shell.index("function syncComposerPlaceholders(lane) {")
    sync_body = app_shell[
        sync_start : app_shell.index(
            "\n}\n\nfunction laneComposerDraftText", sync_start
        )
    ]

    assert "for (const [targetId, textarea] of lane.shardTextareas)" in sync_body
    assert "[data-composer-quote-stack-target-id]" in sync_body
    assert (
        'const targetId = stack.dataset.composerQuoteStackTargetId || "";' in sync_body
    )
    assert 'stack.querySelectorAll("textarea[data-quote-draft-id]")' in sync_body
    assert sync_body.count("const member = laneStates.get(targetId) || lane;") == 2
    assert (
        sync_body.count("textarea.placeholder = laneComposePlaceholder(member);") == 2
    )


def test_static_primary_composer_links_latest_message_like_quote_composers():
    css = _serve_css_text()
    status_css = (STATIC_ROOT / "status-colors.css").read_text(encoding="utf-8")
    app_shell = _shell_and_composer_text()
    primary_header_start = css.index(".composer-band-header--primary {")
    primary_header_rule = css[
        primary_header_start : css.index("}", primary_header_start)
    ]
    quote_header_start = css.index(".composer-band-header--quote {")
    quote_header_rule = css[quote_header_start : css.index("}", quote_header_start)]

    assert "const latest = latestComposerMessage(member);" in app_shell
    assert "title: composerPrimaryHeaderTitle(latest)," in app_shell
    assert "function composerPrimaryHeaderTitle(latest)" in app_shell
    assert (
        'return latest ? composerQuotePreview(latest) : "No assistant messages yet";'
        in app_shell
    )
    assert "beforeMenu: composerPrimaryHeaderBeforeMenu(latest, member)," in app_shell
    assert "function composerPrimaryHeaderBeforeMenu(latest, member)" in app_shell
    assert "composerPrimaryLatestMessageLink(latest, member)" in app_shell
    assert "composerPrimaryLatestMessageNote(member)" in app_shell
    assert "function composerPrimaryLatestMessageLink(latest, member)" in app_shell
    assert 'const time = document.createElement("a");' in app_shell
    assert 'time.href = "#" + messageDomId(latest.key);' in app_shell
    assert 'time.title = "Jump to latest message";' in app_shell
    assert 'time.className = "composer-quote-time composer-latest-time";' in app_shell
    assert 'time.dataset.relativeFallback = "message";' in app_shell
    assert "function composerPrimaryLatestMessageNote(member)" in app_shell
    assert 'note.textContent = "no messages";' in app_shell
    assert 'note.title = "No latest message";' in app_shell
    assert "function syncComposerHeaderStatus(element, member)" in app_shell
    assert "const statusLine = member.lastRenderedStatusLine || {};" in app_shell
    assert (
        'statusLine.agentVisualStatus || statusLine.agentProcessStatus || "unknown"'
        in app_shell
    )
    assert "syncComposerHeaderStatus(time, member);" in app_shell
    assert "syncComposerHeaderStatus(note, member);" in app_shell
    assert "composerQuoteBandHeader(lane, targetId, member, draft)" in app_shell
    assert ".agent-status-pip,\n.composer-quote-time[data-agent-status] {" in status_css
    assert "--agent-status-color: var(--muted);" in status_css
    assert (
        '.agent-status-pip[data-agent-status="running"],\n'
        '.composer-quote-time[data-agent-status="running"] {' in status_css
    )
    assert (
        '.agent-status-pip[data-agent-status="idle"],\n'
        '.composer-quote-time[data-agent-status="idle"] {' in status_css
    )
    assert (
        ".composer-quote-time[data-agent-status] {\n  color: var(--agent-status-color);"
        in status_css
    )
    assert "grid-template-columns: auto minmax(0, 1fr) auto;" in primary_header_rule
    assert "grid-template-columns: auto minmax(0, 1fr) auto;" in quote_header_rule
    assert ".composer-latest-time--empty {" in css
    assert "text-decoration: none;" in css
    assert "function latestComposerMessage(member)" in app_shell
    assert "return member.knownMessages.find(isComposerLatestMessage);" in app_shell
    assert "function isComposerLatestMessage(item)" in app_shell
    assert 'return item.kind === "assistant" || item.kind === "final";' in app_shell
    assert (
        'return String(item.preview || item.display_text || item.text || "assistant message")'
        in app_shell
    )
    assert (
        "return member.knownMessages.find((item) => !isPresenceMessage(item));"
        not in (app_shell)
    )
    assert 'href: messageKey ? "#" + messageDomId(messageKey) : "",' in app_shell
    assert 'anchor.title = "Jump to quoted message";' in app_shell


def test_static_composer_headers_use_agent_accent_border():
    css = _serve_css_text()
    app_shell = _shell_and_composer_text()
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    band_start = css.index(".composer-band {")
    band_rule = css[band_start : css.index("}", band_start)]
    header_start = css.index(".composer-band-header {")
    header_rule = css[header_start : css.index("}", header_start)]
    title_start = css.index(".composer-band-title {")
    title_rule = css[title_start : css.index("}", title_start)]
    textarea_start = css.index(".composer-shard textarea {")
    textarea_rule = css[textarea_start : css.index("}", textarea_start)]

    assert "--composer-header-accent: var(--border-soft);" in band_rule
    assert "border-bottom: 2px solid" in header_rule
    assert (
        "color-mix(in srgb, var(--composer-header-accent) 64%, var(--border-soft))"
        in header_rule
    )
    assert "var(--composer-header-accent, var(--muted)) 70%" in title_rule
    assert "font-weight: 400;" in title_rule
    assert "border-top" not in textarea_rule
    assert "function syncComposerBandAccent(band, lane, member)" in app_shell
    assert (
        'band.style.setProperty("--composer-header-accent", '
        "composerMemberAccent(lane, member));" in app_shell
    )
    assert "function composerMemberAccent(lane, member)" in app_shell
    assert (
        "return messageOccupantAccent(composerMemberAccentIndex(lane, member));"
        not in app_shell
    )
    assert (
        "return messageOccupantAccent(laneMemberAccentIndex(lane, member));"
        in app_shell
    )
    assert "function laneMemberAccentIndex(lane, member)" in app_stream
    assert (
        "const index = laneGroupMemberTargetIds(host).indexOf(member.targetId);"
        in app_stream
    )
    assert (
        'throw new Error("team slot accent requires a lane group member");'
        in app_stream
    )
    assert "return index;" in app_stream
    assert "syncComposerBandAccent(primary, lane, member);" in app_shell
    assert "syncComposerBandAccent(band, lane, member);" in app_shell


def test_static_composer_driver_icons_use_local_driver_assets():
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_composer = (STATIC_ROOT / "app.composer.js").read_text(encoding="utf-8")
    openai_icon = STATIC_ROOT / "icons" / "openai.svg"
    claude_icon = STATIC_ROOT / "icons" / "claude.svg"
    assert openai_icon.is_file()
    assert claude_icon.is_file()
    assert 'fill="currentColor"' in openai_icon.read_text(encoding="utf-8")
    assert 'fill="currentColor"' in claude_icon.read_text(encoding="utf-8")

    _assert_contains_all(
        app_render,
        (
            "function targetIdentityDriverName(identity)",
            "function targetIdentityDriverModel(identity)",
            "function targetIdentityDriverEffort(identity)",
            "function applyLaneServeAgentIdentity(lane, payload)",
            "function serveAgentDesiredDriverName(identity)",
            "function serveAgentActualDriverName(identity)",
            "function identityDisplayPair(actual, desired)",
            "lane.driverName = identityDisplayPair(actualDriver, desiredDriver);",
            "lane.driverIconName = actualDriver || transcriptOwner || desiredDriver;",
        ),
    )
    _assert_contains_all(
        app_shell,
        (
            "const serveAgentIdentity = target.serveAgentIdentity || {};",
            "serveAgentActualDriverName(serveAgentIdentity)",
            "serveAgentDesiredDriverName(serveAgentIdentity)",
            "driverIconName:",
        ),
    )
    _assert_contains_all(
        app_composer,
        (
            "syncComposerDriverIcon(primary, member);",
            'claude: "/static/icons/claude.svg",',
            'codex: "/static/icons/openai.svg",',
            'openai: "/static/icons/openai.svg",',
            "icon.dataset.composerDriverIcon = driver;",
            "const tooltip = composerDriverTooltip(member, driver);",
            "icon.title = tooltip;",
            'icon.setAttribute("aria-label", tooltip);',
            'icon.setAttribute("role", "img");',
            '"Codex driver"',
            '"driver: " + driverName',
            '"model: " + model',
            '"effort: " + effort',
            '"thread: " + (threadId || "unbound")',
            '"session: " + session',
        ),
    )
    assert '"source: worktree launch config"' not in app_composer
    assert (
        'icon.style.setProperty("--composer-driver-icon-url", '
        "'url(\"' + src + '\")');" in app_composer
    )


def test_composer_driver_icon_rerender_keeps_matching_dom_node():
    app_composer = STATIC_ROOT / "app.composer.js"
    script = Path(__file__).with_name("fixtures") / "composer_driver_icon_reconcile.js"

    result = subprocess.run(
        ["node", str(script), str(app_composer)],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_accepted_composer_send_clears_duplicate_draft_text():
    app_composer = STATIC_ROOT / "app.composer.js"
    script = Path(__file__).with_name("fixtures") / "composer_accepted_draft_clear.js"

    result = subprocess.run(
        ["node", str(script), str(app_composer)],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_static_composer_driver_icons_style_local_driver_assets():
    css = _serve_css_text()
    icon_start = css.index(".composer-driver-icon {")
    icon_rule = css[icon_start : css.index("}", icon_start)]
    icon_before_start = css.index(".composer-driver-icon::before {")
    icon_before_rule = css[icon_before_start : css.index("}", icon_before_start)]
    claude_rule_start = css.index(".composer-driver-icon--claude {")
    claude_rule = css[claude_rule_start : css.index("}", claude_rule_start)]
    openai_rule_start = css.index(
        ".composer-driver-icon--codex,\n.composer-driver-icon--openai {"
    )
    openai_rule = css[openai_rule_start : css.index("}", openai_rule_start)]
    menu_open_rule_start = css.index(
        ".composer-band--menu-open .composer-driver-icon {"
    )
    menu_open_rule = css[menu_open_rule_start : css.index("}", menu_open_rule_start)]
    textarea_start = css.index(".composer-band--primary textarea {")
    textarea_rule = css[textarea_start : css.index("}", textarea_start)]

    _assert_contains_all(
        icon_rule,
        (
            "bottom: 8px;",
            "cursor: help;",
            "height: 18px;",
            "pointer-events: auto;",
            "position: absolute;",
            "right: 8px;",
            "width: 18px;",
        ),
    )
    _assert_contains_all(
        icon_before_rule,
        (
            'content: "";',
            "inset: 2px;",
            "-webkit-mask: var(--composer-driver-icon-url) center / contain no-repeat;",
            "mask: var(--composer-driver-icon-url) center / contain no-repeat;",
            "position: absolute;",
        ),
    )
    assert (
        "--composer-driver-icon-color: color-mix(in srgb, #d97706 88%, var(--fg));"
        in (claude_rule)
    )
    assert "opacity: 0.72;" in claude_rule
    assert (
        "--composer-driver-icon-color: color-mix(in srgb, var(--fg) 86%, var(--control));"
        in openai_rule
    )
    assert "opacity: 0.74;" in openai_rule
    assert "display: none;" in menu_open_rule
    assert "padding-bottom: 28px;" in textarea_rule
    assert "padding-right: 32px;" in textarea_rule


def test_static_message_accents_follow_team_slots_for_single_member_teams():
    css = _serve_css_text()
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")

    assert ".messages article[data-accent-slot]" in css
    assert ".messages article[data-occupant]" not in css
    assert "Boolean(laneGroupHost(lane).teamId)" in app_stream
    assert "laneMessageAttributionAgentCount(lane) > 1" in app_stream
    assert "function laneMessageAccentIndex(lane, item)" in app_stream
    assert "function laneMessageProducerTargetId(lane, item)" in app_stream
    assert "if (item.producerTargetId) return item.producerTargetId;" in app_stream
    assert (
        "candidate.targetThreadId === threadId ||\n"
        "      candidate.activeThreadId === threadId" in app_stream
    )
    assert (
        "const index = laneGroupMemberTargetIds(host).indexOf(targetId);" in app_stream
    )
    assert "return laneOccupantOrdinal(host, item.threadId);" in app_stream
    assert "accentSlot: laneMessageAccentIndex(lane, item)," in app_stream
    assert "attributed: laneShouldAttributeMessages(lane)," in app_stream
    assert "const accentSlot = laneMessageAccentIndex(lane, item);" in app_render
    assert "if (item.threadId && laneShouldAttributeMessages(lane))" not in app_render
    assert "if (laneShouldAttributeMessages(lane))" in app_render
    assert "article.dataset.accentSlot = String(accentSlot);" in app_render
    assert "messageOccupantAccent(accentSlot)" in app_render


def test_static_message_accent_palette_names_all_six_team_slots():
    css = _serve_css_text()
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")

    assert "--team-teal-accent: #007c89;" in css
    assert "--team-plum-accent: #8a4fbf;" in css
    assert "--team-teal-accent: #5fd6d0;" in css
    assert "--team-plum-accent: #d1a3ff;" in css
    assert '"var(--team-teal-accent)",' in app_render
    assert '"var(--team-plum-accent)",' in app_render
    assert "if (index < messageOccupantAccentPalette.length)" in app_render
    assert "return messageOccupantAccentPalette[index];" in app_render
    assert (
        'throw new Error("team slot accent requires one of six team slots");'
        in app_render
    )
    assert "generatedMessageAccentHueStep" not in app_render
    assert "oklch(72% 0.14 " not in app_render
    assert (
        "messageOccupantAccentPalette[index % messageOccupantAccentPalette.length]"
        not in app_render
    )


def test_static_agent_names_use_accent_colors_without_bold_weight():
    css = _serve_css_text()
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")

    message_name_start = css.rindex(".message-agent-name {")
    message_name_rule = css[message_name_start : css.index("}", message_name_start)]
    compaction_label_start = css.index(".compaction-label {")
    compaction_label_rule = css[
        compaction_label_start : css.index("}", compaction_label_start)
    ]
    target_name_start = css.index(".target-choice-name {")
    target_name_rule = css[target_name_start : css.index("}", target_name_start)]

    assert "var(--message-occupant-accent, var(--muted)) 70%" in message_name_rule
    assert "font-weight: 400;" in message_name_rule
    assert "var(--compaction-accent, var(--fg)) 70%" in compaction_label_rule
    assert "font-weight: 400;" in compaction_label_rule
    assert "var(--target-choice-name-accent, var(--fg)) 70%" in target_name_rule
    assert "font-weight: 400;" in target_name_rule
    assert (
        '<span class="target-choice-copy"><span class="target-choice-name"></span>'
        '<span class="target-choice-meta"></span></span>' in app_lanes
    )
    assert (
        '<span class="target-choice-copy"><strong></strong><span></span></span>'
        not in app_lanes
    )
    assert "function syncTargetChoiceNameAccent(button, target)" in app_lanes
    assert (
        'button.style.setProperty("--target-choice-name-accent", accent);' in app_lanes
    )
