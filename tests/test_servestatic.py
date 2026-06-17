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


def test_mobile_header_pill_scroller_is_sole_grower():
    css = _serve_css_text()
    mobile_start = css.index("@media (max-width: 720px)")

    meta_start = css.index(".app-header .meta {", mobile_start)
    meta_rule = css[meta_start : css.index("}", meta_start)]
    strip_start = css.index(".filter-strip {", mobile_start)
    strip_rule = css[strip_start : css.index("}", strip_start)]

    # The status text must not grow, or it splits the header width with the
    # pill scroller (the bug: the scroller only filled ~half the width).
    assert "flex: 0 1 8rem;" in meta_rule
    assert "flex: 1 1 auto;" in strip_rule
    assert "min-width: 0;" in strip_rule


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
    assert "flex-direction: row-reverse;" in stack_rule
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


def test_static_draft_composers_use_14px_font():
    css = _serve_css_text()
    selector = ".composer-shard textarea {"
    start = css.index(selector)
    end = css.index("}", start)
    textarea_rule = css[start:end]

    assert "font-size: 14px;" in textarea_rule


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


def test_static_quote_close_control_keeps_composer_menu_styling_compact():
    css = _serve_css_text()
    app_shell = _shell_and_composer_text()
    button_start = css.index(
        ".composer-band-menu-button,\n.composer-band-close-button {"
    )
    button_end = css.index(".composer-band-menu-button:hover", button_start)
    button_rule = css[button_start:button_end]
    action_start = css.index(".composer-band-menu-action {")
    action_end = css.index(
        ".composer-band-menu-action .spice-menu-action-detail", action_start
    )
    action_rule = css[action_start:action_end]
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
    assert (
        ".composer-band-close-button:hover,\n.composer-band-close-button:focus-visible {"
        in css
    )
    assert '.composer-band-menu-button[aria-expanded="true"] {' in css
    assert (
        ".composer-band--menu-open textarea,\n.composer-band--menu-open .composer-attachments {"
        in css
    )
    assert "font-size: 12px;" in action_rule
    assert (
        ".composer-band-menu-action .spice-menu-action-detail {\n  display: none;"
        in css
    )


def test_static_lane_team_menu_exposes_close_split_and_restore_actions():
    css = _serve_css_text()
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
    assert 'label: "Split into individuals",' in app_groups
    assert 'label: "Restore previous team",' in app_groups
    assert "if (host.emptyTeam) return [closeTeamMenuAction(host)];" in app_groups
    assert 'detail: host.emptyTeam\n      ? "empty"' in app_groups
    close_team_index = app_groups.index('label: "Close team",')
    split_individuals_index = app_groups.index('label: "Split into individuals",')
    restore_previous_index = app_groups.index('label: "Restore previous team",')
    assert close_team_index < split_individuals_index
    assert split_individuals_index < restore_previous_index
    assert 'teamCommandPayload("splitTeamBack", {' in app_groups
    lane_start = css.index(".lane {")
    lane_end = css.index(".lane--shadowed", lane_start)
    lane_rule = css[lane_start:lane_end]
    view_stack_start = css.index(".lane-view-stack {")
    view_stack_end = css.index(".lane-view-stack--collapsed", view_stack_start)
    view_stack_rule = css[view_stack_start:view_stack_end]
    menu_start = css.index(".lane-team-menu {")
    menu_end = css.index(".lane-team-menu--empty-team-overlay {", menu_start)
    menu_rule = css[menu_start:menu_end]
    empty_team_overlay_start = css.index(".lane-team-menu--empty-team-overlay {")
    empty_team_overlay_end = css.index(
        ".lane-team-menu .lane-team-menu-action {", empty_team_overlay_start
    )
    empty_team_overlay_rule = css[empty_team_overlay_start:empty_team_overlay_end]
    action_start = css.index(".lane-team-menu .lane-team-menu-action {")
    action_end = css.index(
        ".lane-team-menu .lane-team-menu-action .spice-menu-action-label",
        action_start,
    )
    action_rule = css[action_start:action_end]
    text_start = css.index(
        ".lane-team-menu .lane-team-menu-action .spice-menu-action-label"
    )
    text_end = css.index(".lane-team-menu-action:disabled", text_start)
    text_rule = css[text_start:text_end]
    assert "position: relative;" in lane_rule
    assert "position: relative;" in view_stack_rule
    assert "align-content: stretch;" in menu_rule
    assert "position: absolute;" in menu_rule
    assert "inset: 0;" in menu_rule
    assert "height: var(--lane-team-menu-height, 120px);" in empty_team_overlay_rule
    assert "inset: var(--lane-team-menu-top, 0px) 0 auto;" in empty_team_overlay_rule
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
    assert "text-wrap: balance;" in text_rule
    assert "text-wrap: pretty;" in text_rule
    assert "white-space: normal;" in text_rule


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
    assert ".composer-shard--composer-drop-left::before" in css
    assert ".composer-shard--composer-drop-right::after" in css
    assert ".lane--composer-drop .composer-shards" in css
    assert "function composerReorderDropTarget(state, clientX, clientY)" in app_groups
    assert 'state.dropTarget = { kind: "move", lane: targetLane };' in app_groups
    assert 'teamCommandPayload("reorderTeamAgents", {' in app_groups
    assert "orderedTargetIds" in app_groups
    assert 'state.sourceShard?.classList.add("composer-shard--dragging");' in app_groups
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
    assert "function laneSubmittedMessagePendingFloor(lane)" in app_render
    assert "function reconcileSubmittedMessagePredictions(lane)" in app_render
    assert "const ackedKeys = new Set(ackKeysForMessages(lane.knownMessages));" in (
        app_render
    )
    assert "if (ackedKeys.has(key)) lane.optimisticSubmittedInboxKeys.delete(key);" in (
        app_render
    )
    assert "inboxKey: result.key," in app_stream


def test_static_lifetime_slider_uses_steer_drive_drain_without_renew_send_flag():
    app = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_controls = (STATIC_ROOT / "app.controls.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")

    assert 'const agentLifetimeLabels = ["Steer", "Drive", "Drain"];' in app
    assert 'Steer: "Manual filters only",' in app
    assert 'Drive: "Auto-subscribe to projects this team creates or claims",' in app
    assert 'Drain: "Boundary dissolved: see all assignable work",' in app
    assert "function agentLifetimeAutoManagesTasks(lifetime) {" in app
    assert 'return lifetime === "Drive";' in app
    assert "function agentLifetimeUsesStoredTaskFilters(lifetime) {" in app
    assert 'return lifetime === "Steer" || lifetime === "Drive";' in app
    assert "function agentLifetimeDissolvesTaskBoundary(lifetime) {" in app
    assert 'return lifetime === "Drain";' in app
    assert "function agentLifetimeHelpText(lifetime) {" in app
    assert "data-lifetime-label>Drive</span>" in app_shell
    assert "data-submit>Drive</button>" in app_shell
    assert "serverLifetime: target.lifetime || defaultAgentLifetime," in app_shell
    assert 'pendingLifetimeCommit: "",' in app_shell
    assert "pendingLifetimeConfigRevision: 0," in app_shell
    assert "pendingLifetimeRequestId: 0," in app_shell
    assert "lifetimeRequestId: 0," in app_shell
    assert "host.lifetimeRequestId = Math.max" in app_controls
    assert "host.pendingLifetimeCommit = lifetime;" in app_controls
    assert "host.pendingLifetimeRequestId = host.lifetimeRequestId;" in app_controls
    assert "host.serverLifetime = laneServerLifetime(host);" in app_controls
    assert "function serverLifetimeSupersedesPending(host, options = {})" in (
        app_controls
    )
    assert "if (options.supersedePending === false) return false;" in app_controls
    assert (
        "if (host.pendingLifetimeCommit && lifetime !== host.pendingLifetimeCommit)"
        in app_controls
    )
    assert 'host.pendingLifetimeCommit = "";' in app_controls
    assert "host.pendingLifetimeConfigRevision = 0;" in app_controls
    assert "host.pendingLifetimeRequestId = 0;" in app_controls
    assert "function laneLifetimeCommitMatches(host, lifetime, options = {})" in (
        app_controls
    )
    assert "function clearLaneLifetimeCommit(lane, lifetime, options = {})" in (
        app_controls
    )
    assert "function rollbackLaneLifetimeCommit(" in app_controls
    assert 'serverLifetime = "",' in app_controls
    assert "options = {}," in app_controls
    assert "applyServerLaneLifetime(lane, config.lifetime, {" in app_shell
    assert "applyServerLaneLifetime(lane, config.lifetime, {" in app_lanes
    assert "configRevision: config.revision," in app_shell
    assert "configRevision: config.revision," in app_lanes
    assert "configRevision: payload.configRevision," in app_render
    assert "pendingLaneLifetimeStateForMembers(members, lifetimeStateByTargetId)" in (
        app_groups
    )
    assert "laneLifetimeRuntimeState(lane)" in app_groups
    assert "restoreLaneLifetimeRuntimeState(" in app_groups
    assert "const requestedLifetime = payload.lifetime;" in app_stream
    assert "const lifetimeRequestId = Math.max" in app_stream
    assert "const pendingLifetimeRequestId =" in app_stream
    assert "lifetimeRequestId," in app_stream
    assert "settleLaneLifetimeCommit(" in app_stream
    assert "if (!requestedLifetimeRequestId) return;" in app_stream
    assert "if (options.lifetimeRequestId === undefined) return true;" in app_stream
    assert "taskDrainLifetimeResponseIsCurrent(lane, options)" in app_stream
    assert "if (pendingLifetimeRequestId)" in app_stream
    assert "requestId: pendingLifetimeRequestId," in app_stream
    assert "supersedePending: false," in app_stream
    assert "const lifetimeHelp = agentLifetimeHelpText(lifetime);" in app_controls
    assert "lane.lifetimeRangeEl.title = lifetimeHelp;" in app_controls
    assert '"Task subscription policy: " + lifetimeHelp' in app_controls
    assert "lane.lifetimeLabelEl.title = lifetimeHelp;" in app_controls
    assert (
        'lane.submitEl.title = "Send with " + lifetime + ": " + lifetimeHelp;'
        in app_controls
    )
    assert "renewAgent" not in app_controls
    assert '"Renew"' not in app


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


def test_static_filter_dropdown_skips_noop_rewrites_and_preserves_scroll():
    css = _serve_css_text()
    app_js = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_panes = (STATIC_ROOT / "app.panes.js").read_text(encoding="utf-8")

    filter_pill_start = css.index(".filter-pill {")
    filter_pill_rule = css[filter_pill_start : css.index("}", filter_pill_start)]
    filter_count_start = css.index(".filter-pill-count {")
    filter_count_rule = css[filter_count_start : css.index("}", filter_count_start)]
    chip_start = css.index(".lane-filter-chip {")
    chip_rule = css[chip_start : css.index("}", chip_start)]
    chip_count_start = css.index(".lane-filter-chip-count {")
    chip_count_rule = css[chip_count_start : css.index("}", chip_count_start)]

    assert 'let renderedFilterPillsFingerprint = "";' in app_js
    assert 'const taskFilterHeaderExtraStems = ["agent", "oops"];' in app_lanes
    assert "function filterPillModels()" in app_lanes
    assert "return taskFilterStemPills.map(taskFilterStemPillModel);" in app_lanes
    assert "function taskFilterStemPillModel(stem)" in app_lanes
    assert 'classes.push("filter-pill--system");' in app_lanes
    assert "function taskFilterStemScopeLabel(stemName)" in app_lanes
    assert 'return stemName === "oops" ? "oops" : stemName + ".*";' in app_lanes
    assert "function taskFilterStemIsSystem(stemName)" in app_lanes
    assert 'return stemName === "agent" || stemName === "oops";' in app_lanes
    assert "boundaryDissolved: Boolean(model.drainability.boundaryDissolved)" in (
        app_lanes
    )
    assert "function taskFilterStemDrainability(stem)" in app_lanes
    assert "!taskFilterStemIsSystem(stem.name)" in app_lanes
    assert "boundaryDissolved = true;" in app_lanes
    assert "agentLifetimeUsesStoredTaskFilters(lifetime)" in app_lanes
    assert 'classes.push("filter-pill--implicit");' in app_lanes
    assert '"drained by " + drainability.count' in app_lanes
    assert '"not currently drained"' in app_lanes
    assert "if (fingerprint === renderedFilterPillsFingerprint) return;" in app_lanes
    assert "renderedFilterPillsFingerprint = fingerprint;" in app_lanes
    assert 'renderedFilterPaneFingerprint: "",' in app_shell
    assert "agentLifetimeDissolvesTaskBoundary(lifetime) ||" in app_shell
    assert "function laneFilterPaneRenderModel(lane)" in app_panes
    assert "function laneFilterPolicyLabel(lifetime)" in app_panes
    assert 'return "all projects";' in app_panes
    assert 'return "auto";' in app_panes
    assert 'return "manual";' in app_panes
    assert "function laneAssignableTaskFilterQueueCount(lane)" in app_panes
    assert 'filterPolicy === "all projects"' in app_panes
    assert '"all assignable"' in app_panes
    assert 'filterPolicy + " " + queueCount + " queues"' in app_panes
    assert (
        "if (model.fingerprint === lane.renderedFilterPaneFingerprint) return;"
        in app_panes
    )
    assert "lane.renderedFilterPaneFingerprint = model.fingerprint;" in app_panes
    assert "function laneFilterPickerResultsScrollTop(picker)" in app_panes
    assert (
        "function restoreLaneFilterPickerResultsScroll(picker, scrollTop)" in app_panes
    )
    assert (
        "restoreLaneFilterPickerResultsScroll(picker, previousScrollTop);" in app_panes
    )
    assert (
        "if (input instanceof HTMLElement) input.focus({ preventScroll: true });"
        in app_panes
    )
    assert "function compareLaneFilterPickerActions(left, right)" in app_panes
    assert (
        "const actions = [...existing, ...stems].sort(compareLaneFilterPickerActions);"
        in app_panes
    )
    assert "gap: 4px;" in filter_pill_rule
    assert "background: var(--accent);" in filter_count_rule
    assert "border-radius: var(--pill-radius);" in filter_count_rule
    assert "color: var(--button-accent-fg);" in filter_count_rule
    assert "min-width:" not in filter_count_rule
    assert (
        ".filter-pill--undrainable .filter-pill-count { background: var(--muted); }"
        in css
    )
    assert ".filter-pill--implicit {" in css
    assert ".filter-pill--system { color: var(--warn); }" in css
    assert (
        "box-shadow: inset 0 -2px 0 color-mix(in srgb, var(--good) 42%, transparent);"
        in (css)
    )
    assert "flex: 0 1 10rem;" not in chip_rule
    assert "justify-content: space-between;" not in chip_rule
    assert "gap: 4px;" in chip_rule
    assert "padding: 3px 10px 3px 12px;" in chip_rule
    assert "background: var(--accent);" in chip_count_rule
    assert "border-radius: var(--pill-radius);" in chip_count_rule
    assert "color: var(--button-accent-fg);" in chip_count_rule
    assert "font-size: 9px;" in chip_count_rule
    assert "line-height: 13px;" in chip_count_rule
    assert "font-variant-numeric: tabular-nums;" not in chip_count_rule
    assert "min-width:" not in chip_count_rule
    assert "\n  height:" not in chip_count_rule
    assert "display: inline-grid;" not in chip_count_rule
    assert (
        ".lane-filter-chip--assign .lane-filter-chip-count,\n"
        ".lane-filter-chip--empty .lane-filter-chip-count {\n"
        "  background: var(--muted);\n"
        "}" in css
    )
    assert (
        ".lane-filter-chip--selected .lane-filter-chip-count {\n"
        "  background: var(--warn);\n"
        "}" in css
    )
    assert (
        ".lane-filter-chip--private .lane-filter-chip-count {\n"
        "  background: var(--final-accent);\n"
        "}" in css
    )


def test_static_message_footer_controls_stay_right_aligned_on_mobile():
    css = _serve_css_text()

    assert ".message-footer-right { justify-content: flex-end; }" in css
    assert (
        ".message-footer-left,\n  .message-footer-right {\n    flex: 1 1 100%;" in css
    )


def test_static_cmd_enter_submits_focused_composer_target_only():
    app_controls = (STATIC_ROOT / "app.controls.js").read_text(encoding="utf-8")
    app_shell = _shell_and_composer_text()

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


def test_static_keyboard_quote_submit_focuses_main_composer_after_reset():
    app_controls = (STATIC_ROOT / "app.controls.js").read_text(encoding="utf-8")
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    submit_start = app_controls.index("function submitLaneForm(")
    submit_body = app_controls[
        submit_start : app_controls.index(
            "\n}\n\nfunction keyboardSubmitFocusTarget", submit_start
        )
    ]
    focus_start = app_controls.index("function keyboardSubmitFocusTarget(")
    focus_body = app_controls[focus_start : app_controls.index("\n}", focus_start)]
    result_start = app_stream.index("function applyLaneSendResult(")
    result_body = app_stream[
        result_start : app_stream.index(
            "\n}\n\nfunction focusAfterComposerReset", result_start
        )
    ]
    focus_reset_start = app_stream.index("function focusAfterComposerReset(")
    focus_reset_body = app_stream[
        focus_reset_start : app_stream.index("\n}", focus_reset_start)
    ]

    assert "const focusAfterReset = keyboardSubmitFocusTarget(" in submit_body
    assert "{ focusAfterReset }" in submit_body
    assert 'if (event.type !== "keydown") return null;' in focus_body
    assert "if (!(target instanceof HTMLTextAreaElement)) return null;" in focus_body
    assert "if (!target.dataset.quoteDraftId) return null;" in focus_body
    assert (
        'throw new Error("keyboard quote submit requires main composer");' in focus_body
    )
    assert "return textarea;" in focus_body
    assert (
        "function enqueueSend(lane, payload, sourceLane = lane, options = {})"
        in app_stream
    )
    assert "sendLanePayload(lane, payload, sourceLane, options);" in app_stream
    assert "options = {}," in result_body
    assert (
        "resetLaneComposerDraft(sourceLane, lane.targetId);\n"
        "  focusAfterComposerReset(options.focusAfterReset);"
    ) in result_body
    assert (
        'throw new Error("composer focus target must remain in the document");'
        in focus_reset_body
    )
    assert "element.focus({ preventScroll: true });" in focus_reset_body


def test_static_css_adds_visible_nested_quote_depth():
    css = _serve_css_text()
    ack_selector = ".ack-quote {\n  background"
    ack_start = css.index(ack_selector)
    ack_rule = css[ack_start : css.index("}", ack_start)]
    ack_attachments_start = css.index(".ack-attachments {")
    ack_attachments_end = css.index(".ack-attachment {", ack_attachments_start)
    ack_attachments_rule = css[ack_attachments_start:ack_attachments_end]
    ack_attachment_start = css.index(".ack-attachment {")
    ack_attachment_end = css.index(".ack-attachment img", ack_attachment_start)
    ack_attachment_rule = css[ack_attachment_start:ack_attachment_end]

    assert ".message-body,\n.ack-quote {" in css
    assert "--quote-accent: var(--message-occupant-accent, var(--accent));" in css
    assert "--quote-nested-step: 8px;" in css
    assert "--quote-nest-indent: calc(" in css
    assert "--quote-deep-nest-indent: calc(" in css
    assert "--quote-nested-pad-inline: 6px;" in css
    assert "--quote-pad-block: 6px;" in css
    assert "--quote-nested-bottom-gap: 6px;" in css
    assert ".message-body blockquote blockquote,\n.ack-quote blockquote {" in css
    assert (
        ".message-body blockquote blockquote blockquote,\n"
        ".ack-quote blockquote blockquote {" in css
    )
    assert (
        "margin: 6px 0 var(--quote-nested-bottom-gap) var(--quote-nest-indent);" in css
    )
    assert "margin-left: var(--quote-deep-nest-indent);" in css
    assert (
        "border-left: var(--quote-rail-width) solid "
        "color-mix(in srgb, var(--quote-accent) 72%, var(--fg));" in css
    )
    assert "padding: var(--quote-pad-block) var(--quote-nested-pad-inline);" in css
    assert "--quote-rail-width: 3px;" in css
    assert "border-left: var(--quote-rail-width) solid var(--quote-accent);" in ack_rule
    assert "padding: var(--quote-pad-block) var(--quote-pad-inline);" in ack_rule
    assert "flex-direction: row-reverse;" in ack_attachments_rule
    assert "flex-wrap: nowrap;" in ack_attachments_rule
    assert "justify-content: flex-start;" in ack_attachments_rule
    assert "overflow-x: auto;" in ack_attachments_rule
    assert "flex: 0 0 92px;" in ack_attachment_rule
    assert "width: 92px;" in ack_attachment_rule


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
    assert "toggleMessageSpeech(lane, item, speechLane)" in app_render
    assert (
        "function enqueueSpeech(lane, messageKey, texts, targetLane = lane)"
        in app_audio
    )
    assert "await playSpeech(entry.targetLane, text);" in app_audio


def test_static_stream_uses_message_payload_and_standard_badges():
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    css = _serve_css_text()
    merge_start = app_stream.index("function mergePayloadMessages")
    merge_end = app_stream.index("function upsertKnownMessage", merge_start)
    badge_start = app_render.index("function renderBadges")
    badge_end = app_render.index("function renderCompactionDivider", badge_start)
    final_css_start = css.index(".messages article.final {")
    final_css_end = css.index(".messages article.final.acked {", final_css_start)

    assert app_stream[merge_start:merge_end] == (
        "function mergePayloadMessages(lane, payload) {\n"
        '  const threadId = payload.targetThreadId || lane.activeThreadId || "";\n'
        "  for (const item of [...(payload.messages || [])].reverse()) {\n"
        "    stampMessageProducer(item, lane, threadId);\n"
        '    upsertKnownMessage(lane, item, "newest");\n'
        "  }\n"
        "  trimKnownMessages(lane);\n"
        "}\n"
        "\n"
        "function mergeOlderPayloadMessages(lane, payload) {\n"
        '  const threadId = payload.targetThreadId || lane.activeThreadId || "";\n'
        "  let added = 0;\n"
        "  for (const item of payload.messages || []) {\n"
        "    stampMessageProducer(item, lane, threadId);\n"
        '    if (upsertKnownMessage(lane, item, "oldest")) added += 1;\n'
        "  }\n"
        "  if (added > 0) lane.retainedMessageLimit += added;\n"
        "  trimKnownMessages(lane);\n"
        "  return added;\n"
        "}\n"
        "\n"
    )
    assert app_render[badge_start:badge_end] == (
        "function renderBadges(ackCount, kind, maximAckCount) {\n"
        "  const visibleAckCount = Math.max(0, ackCount - maximAckCount);\n"
        "  if (\n"
        "    !maximAckCount &&\n"
        "    !visibleAckCount &&\n"
        '    kind !== "final"\n'
        "  )\n"
        "    return null;\n"
        '  const badges = document.createElement("div");\n'
        '  badges.className = "badges";\n'
        "  const add = (label, className) => {\n"
        '    const badge = document.createElement("span");\n'
        '    badge.className = className ? "badge " + className : "badge";\n'
        "    badge.textContent = label;\n"
        "    badges.append(badge);\n"
        "  };\n"
        '  if (maximAckCount) add("MAXIM", "maxim-badge");\n'
        '  if (kind === "final") add("FINAL", "final-badge");\n'
        "  if (visibleAckCount)\n"
        '    add(visibleAckCount + "\\u00a0ACK" + (visibleAckCount === 1 ? "" : "s"));\n'
        "  return badges;\n"
        "}\n"
        "\n"
    )
    assert css[final_css_start:final_css_end] == (
        ".messages article.final {\n"
        "  background: var(--final-tint);\n"
        "  border-color: var(--final-accent);\n"
        "  box-shadow: inset 0 3px 0 var(--final-accent);\n"
        "}\n"
    )


def test_static_stream_reports_deadlettered_agent_ensure_failure():
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")

    assert "function agentEnsureFailureStatus(ensure)" in app_stream
    assert (
        "setLaneTransientStatus(sourceLane, agentEnsureFailureStatus(ensure));"
        in app_stream
    )
    assert 'parts.push("parked inbox " + ensure.deadletteredInboxKey);' in app_stream
    assert 'parts.push("requeue: " + ensure.deadletterRequeueCommand);' in app_stream


def test_static_stream_queues_fresh_speech_for_all_post_prime_sources():
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    apply_start = app_stream.index("async function applyLaneBusPayload")
    apply_body = app_stream[
        apply_start : app_stream.index("\n}\n\nfunction syncLaneThreadId", apply_start)
    ]

    assert 'if (source === "watch" && (payload.messages || []).length)' in apply_body
    assert 'if (wasSpeechPrimed && source === "watch")' not in apply_body
    assert "if (wasSpeechPrimed) {" in apply_body
    assert (
        "const fresh = (payload.messages || []).filter(\n"
        "      (item) => item.key && !knownBefore.has(item.key),\n"
        "    );" in apply_body
    )
    assert "queueSpeechForMessages(lane, fresh);" in apply_body


def test_static_manual_speech_playback_aborts_active_entry():
    app_audio = (STATIC_ROOT / "app.audio.js").read_text(encoding="utf-8")

    # Manual play is a hard reset: stopAllSpeech() clears the entire queue
    # (all lanes) and halts current playback, then — unless this toggled the
    # active message off — only this one message is enqueued.
    assert "function toggleMessageSpeech(lane, item, targetLane = lane) {" in app_audio
    assert "const messageKey = item.key;" in app_audio
    assert "const texts = messageSpeechUtterances(item);" in app_audio
    assert "stopAllSpeech();" in app_audio
    assert "if (wasPlaying) return;" in app_audio
    assert "enqueueSpeech(lane, messageKey, texts, targetLane);" in app_audio
    assert (
        "function stopAllSpeech() {\n"
        "  speechQueue.length = 0;\n"
        "  speechEpoch += 1;\n"
        "  stopCurrentSpeech();\n"
        "}"
    ) in app_audio


def test_static_narration_mode_holds_media_session_state():
    app_audio = (STATIC_ROOT / "app.audio.js").read_text(encoding="utf-8")
    app_controls = (STATIC_ROOT / "app.controls.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")

    assert "function syncNarrationMediaSession()" in app_audio
    assert 'session.setActionHandler("pause", () => stopAllSpeech());' in app_audio
    assert 'session.setActionHandler("stop", () => stopAllSpeech());' in app_audio
    assert (
        'return currentSpeech || narrationMediaSessionActive() ? "playing" : "none";'
        in app_audio
    )
    assert 'laneEffectiveSpeechMode(lane) === "narrate"' in app_audio
    assert (
        "if (external && !narrationMediaSessionActive()) stopAllSpeech();" in app_audio
    )
    assert "syncNarrationMediaSession();" in app_controls
    close_start = app_lanes.index("function closeLaneCore(lane) {")
    close_body = app_lanes[close_start : app_lanes.index("\n}", close_start)]
    assert "syncNarrationMediaSession();" in close_body


def test_static_speech_sync_updates_now_playing_message_accent():
    css = _serve_css_text()
    app_audio = (STATIC_ROOT / "app.audio.js").read_text(encoding="utf-8")
    start = css.index(".messages article.now-playing")
    end = css.index(".messages article[data-accent-slot]", start)
    now_playing = css[start:end]

    assert "function syncNowPlayingMessages()" in app_audio
    assert 'document.querySelectorAll("article[data-message-key]")' in app_audio
    assert 'messageArticle.classList.toggle(\n      "now-playing",' in app_audio
    assert "syncNowPlayingMessages();" in app_audio
    assert "--control-max-accent: var(--say-accent);" in css
    assert "--control-state-accent: var(--control-max-accent);" in now_playing
    assert "var(--message-occupant-accent" not in now_playing


def test_static_compaction_divider_spans_grid_and_uses_agent_accent():
    css = _serve_css_text()
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")

    assert "grid-column: 1 / -1;" in css
    assert "grid-template-columns: minmax(16px, 1fr) auto minmax(16px, 1fr)" in css
    assert "background: var(--compaction-accent, var(--border));" in css
    assert "const accentSlot = laneMessageAccentIndex(lane, item);" in app_render
    assert "divider.dataset.accentSlot = String(accentSlot);" in app_render
    assert "messageOccupantAccent(accentSlot)" in app_render
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
