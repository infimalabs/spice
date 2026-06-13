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

    assert "grid-template-columns: repeat(" in css
    assert "minmax(min(calc(50% - 4px), 156px), 1fr)" in css
    assert ".messages article.image-only" in css
    assert "grid-column: span 1" in css
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


def test_static_composer_menu_stays_primary_while_quotes_keep_close_control():
    css = _serve_css_text()
    app_shell = _shell_and_composer_text()
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")
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
    assert 'menu.className = "composer-band-menu";' in app_shell
    assert (
        'button.className = "composer-band-menu-action spice-menu-action";' in app_shell
    )
    assert "if (action.detail) button.title = action.detail;" in app_shell
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
    assert 'composerBandMenuAction(\n    "Leave all teams",' in app_shell
    assert 'composerBandMenuAction(\n    "Create new team",' in app_shell
    assert '"Remove " + label + " from all teams"' in app_shell
    assert '"Move only " + label + " to a new team"' in app_shell
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
    assert 'teamCommandPayload("splitTeam", {' in app_groups
    assert "agentIds: [laneTeamAgentId(member)]," in app_groups


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
    textarea_start = css.index(".composer-shard textarea {")
    textarea_rule = css[textarea_start : css.index("}", textarea_start)]

    assert "--composer-header-accent: var(--border-soft);" in band_rule
    assert "border-bottom: 2px solid" in header_rule
    assert (
        "color-mix(in srgb, var(--composer-header-accent) 64%, var(--border-soft))"
        in header_rule
    )
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


def test_static_css_adds_visible_nested_quote_depth():
    css = _serve_css_text()
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


def test_static_stream_uses_message_payload_and_standard_badges():
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    css = _serve_css_text()
    merge_start = app_stream.index("function mergePayloadMessages")
    merge_end = app_stream.index("function upsertKnownMessage", merge_start)
    badge_start = app_render.index("function renderBadges")
    badge_end = app_render.index("function renderCompactionDivider", badge_start)
    final_css_start = css.index(".messages article.final {")
    final_css_end = css.index(".messages article.said {", final_css_start)

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
        "function renderBadges(ackCount, sayCount, kind, maximAckCount) {\n"
        "  const visibleAckCount = Math.max(0, ackCount - maximAckCount);\n"
        "  if (\n"
        "    !maximAckCount &&\n"
        "    !visibleAckCount &&\n"
        "    !sayCount &&\n"
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
        '  if (sayCount) add(sayCount + " SAY" + (sayCount === 1 ? "" : "s"), "say-badge");\n'
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
