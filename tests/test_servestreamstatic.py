"""Static serve stream, filter, and message UI contracts."""

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


def test_static_filter_header_pills_render_models_and_styles():
    css = _serve_css_text()
    app_js = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")

    filter_pill_start = css.index(".filter-pill {")
    filter_pill_rule = css[filter_pill_start : css.index("}", filter_pill_start)]
    filter_count_start = css.index(".filter-pill-count {")
    filter_count_rule = css[filter_count_start : css.index("}", filter_count_start)]

    assert 'let renderedFilterPillsFingerprint = "";' in app_js
    assert 'const taskFilterHeaderExtraStems = ["agent", "oops"];' in app_lanes
    assert "function filterPillModels()" in app_lanes
    assert "return taskFilterStemPills.map(taskFilterStemPillModel);" in app_lanes
    assert "function taskFilterStemPillModel(stem)" in app_lanes
    assert (
        "pill.innerHTML =\n"
        "      '<span class=\"filter-pill-label\"></span>' +\n"
        "      '<span class=\"filter-pill-count\"></span>';" in app_lanes
    )
    assert (
        'pill.querySelector(".filter-pill-count").textContent = '
        "String(model.openTaskCount);" in app_lanes
    )
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
    assert "gap: 4px;" in filter_pill_rule
    assert filter_count_rule == (
        ".filter-pill-count {\n"
        "  background: var(--accent);\n"
        "  border-radius: var(--pill-radius);\n"
        "  color: var(--button-accent-fg);\n"
        "  font-size: 9px;\n"
        "  padding: 0 5px;\n"
    )
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


def test_static_filter_dropdown_skips_noop_rewrites_and_preserves_scroll():
    css = _serve_css_text()
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_panes = (STATIC_ROOT / "app.panes.js").read_text(encoding="utf-8")

    chip_start = css.index(".lane-filter-chip {")
    chip_rule = css[chip_start : css.index("}", chip_start)]
    chip_count_start = css.index(".lane-filter-chip-count {")
    chip_count_rule = css[chip_count_start : css.index("}", chip_count_start)]

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
    assert "tasks · stem" in app_panes
    assert (
        "chip.innerHTML =\n"
        "    '<span class=\"lane-filter-chip-label\"></span>' +\n"
        "    '<span class=\"lane-filter-chip-count\"></span>';" in app_panes
    )
    assert (
        'chip.querySelector(".lane-filter-chip-count").textContent = String(count);'
        in app_panes
    )
    assert "countEl.textContent = String(count);" in app_panes
    assert "button.append(countEl);" in app_panes
    assert "flex: 0 1 10rem;" not in chip_rule
    assert "justify-content: space-between;" not in chip_rule
    assert "gap: 4px;" in chip_rule
    assert "padding: 3px 10px 3px 12px;" in chip_rule
    assert chip_count_rule == (
        ".lane-filter-chip-count {\n"
        "  background: var(--accent);\n"
        "  border-radius: var(--pill-radius);\n"
        "  color: var(--button-accent-fg);\n"
        "  flex: 0 0 auto;\n"
        "  font-size: 9px;\n"
        "  line-height: 13px;\n"
        "  padding: 0 5px;\n"
    )
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


def test_static_filter_pane_uses_pure_filter_model_helpers():
    app_filter_model = (STATIC_ROOT / "app.filter-model.js").read_text(encoding="utf-8")
    app_panes = (STATIC_ROOT / "app.panes.js").read_text(encoding="utf-8")

    assert "function taskFilterEffectiveAssignedNames(" in app_filter_model
    assert "function availableTaskFilterNames(" in app_filter_model
    assert "function availableTaskFilterOpenTaskCount(" in app_filter_model
    assert "function taskFilterOpenCount(" in app_filter_model
    assert "return taskFilterEffectiveAssignedNames(inventory, assignedFilters);" in (
        app_panes
    )
    assert "return availableTaskFilterNames(laneFilterInventory(lane)" in app_panes
    assert "return taskFilterOpenCount(laneFilterInventory(lane), filter);" in (
        app_panes
    )


def test_static_filter_model_helpers_are_pure_and_covered():
    script = Path(__file__).with_name("fixtures") / "filter_model.js"

    subprocess.run(
        ["node", str(script), str(STATIC_ROOT / "app.filter-model.js")],
        check=True,
        capture_output=True,
        text=True,
    )


def test_static_message_footer_controls_stay_right_aligned_on_mobile():
    css = _serve_css_text()

    assert ".message-footer-right { justify-content: flex-end; }" in css
    assert (
        ".message-footer-left,\n  .message-footer-right {\n    flex: 1 1 100%;" in css
    )


def test_static_message_footer_actions_use_soft_border_only_agent_accent():
    css = (STATIC_ROOT / "messages.css").read_text(encoding="utf-8")
    footer_start = css.index(".message-footer {\n  --message-action-accent")
    footer_rule = css[footer_start : css.index("}", footer_start)]
    action_start = css.index(".message-footer .icon-button {")
    action_rule = css[action_start : css.index("}", action_start)]
    hover_start = css.index(".message-footer .icon-button:hover,")
    hover_rule = css[hover_start : css.index("}", hover_start)]
    active_start = css.index(".message-footer .speech-button--playing,")
    active_rule = css[active_start : css.index("}", active_start)]

    assert (
        "--message-action-accent: var(--message-occupant-accent, var(--muted));"
        in footer_rule
    )
    assert (
        "border-color: color-mix(in srgb, var(--message-action-accent) 24%, var(--control-border-soft));"
        in action_rule
    )
    assert "color: var(--muted);" in action_rule
    assert (
        "border-color: color-mix(in srgb, var(--message-action-accent) 42%, var(--control-border-soft-hover));"
        in hover_rule
    )
    assert "color: var(--fg);" in hover_rule
    assert (
        "border-color: color-mix(in srgb, var(--good) 48%, var(--control-border-soft-hover));"
        in active_rule
    )
    assert "color: var(--fg);" in active_rule


def test_static_task_directive_card_styles_are_present():
    css = (STATIC_ROOT / "messages.css").read_text(encoding="utf-8")

    assert ".message-body .task-directive-quote {" in css
    assert ".task-directive-kicker {" in css
    assert ".task-directive-properties {" in css
    assert ".task-directive-property {" in css


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
    message_selector = (
        ".message-body {\n"
        "  --quote-accent: var(--message-occupant-accent, var(--accent));"
    )
    message_start = css.index(message_selector)
    message_rule = css[message_start : css.index("}", message_start)]
    ack_selector = ".ack-quote {\n  --quote-accent: var(--accent);"
    ack_start = css.index(ack_selector)
    ack_rule = css[ack_start : css.index("}", ack_start)]
    ack_attachments_start = css.index(".ack-attachments {")
    ack_attachments_end = css.index(".ack-attachment {", ack_attachments_start)
    ack_attachments_rule = css[ack_attachments_start:ack_attachments_end]
    ack_attachment_start = css.index(".ack-attachment {")
    ack_attachment_end = css.index(".ack-attachment img", ack_attachment_start)
    ack_attachment_rule = css[ack_attachment_start:ack_attachment_end]

    assert ".message-body,\n.ack-quote {" in css
    assert (
        "--quote-accent: var(--message-occupant-accent, var(--accent));" in message_rule
    )
    assert "--quote-accent: var(--accent);" in ack_rule
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
    assert "flex-direction: row;" in ack_attachments_rule
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


def test_static_team_stream_history_sentinels_track_each_member_lane():
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")

    assert "lane.historySentinelEl.dataset.historyTargetId = targetId;" in app_shell
    assert "const member = historyLaneForSentinel(lane, entry.target);" in app_stream
    assert "maybeHydrateOlderMessages(member);" in app_stream
    assert "if (member === lane || !member.historyObserver) continue;" in app_stream
    assert "member.historyObserver.disconnect();" in app_stream
    assert (
        "for (const sentinel of lane.messagesEl.querySelectorAll(\n"
        '    "[data-history-sentinel]",\n'
        "  ))"
    ) in app_stream
    assert "function historySentinelMembersByMessageKey" in app_stream
    assert "oldestMessageKeyByTargetId.set(targetId, item.key);" in app_stream
    assert (
        "return laneIsFusedHost(lane) ? laneGroupMemberLanes(lane) : [lane];"
        in app_stream
    )
    assert "nodes.push(historySentinelForLane(member));" in app_stream
    assert "lane.historySentinelEl.dataset.historyTargetId = lane.targetId;" in (
        app_stream
    )


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


def test_static_stream_uses_message_payload_merge_shape():
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    merge_start = app_stream.index("function mergePayloadMessages")
    merge_end = app_stream.index("function upsertKnownMessage", merge_start)

    assert app_stream[merge_start:merge_end] == (
        "function mergePayloadMessages(lane, payload) {\n"
        '  const threadId = payloadHasField(payload, "targetIdentity")\n'
        "    ? targetIdentityThreadId(payload.targetIdentity)\n"
        '    : lane.activeThreadId || "";\n'
        "  for (const item of [...(payload.messages || [])].reverse()) {\n"
        "    stampMessageProducer(item, lane, threadId);\n"
        '    upsertKnownMessage(lane, item, "newest");\n'
        "  }\n"
        "  trimKnownMessages(lane);\n"
        "}\n"
        "\n"
        "function mergeOlderPayloadMessages(lane, payload) {\n"
        '  const threadId = payloadHasField(payload, "targetIdentity")\n'
        "    ? targetIdentityThreadId(payload.targetIdentity)\n"
        '    : lane.activeThreadId || "";\n'
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


def test_static_stream_renders_message_badge_dom():
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    badge_start = app_render.index("function renderBadges")
    badge_end = app_render.index("function renderCompactionDivider", badge_start)

    assert app_render[badge_start:badge_end] == (
        "function renderBadges(ackCount, kind, maximAckCount, taskCardCount) {\n"
        "  const visibleAckCount = Math.max(0, ackCount - maximAckCount);\n"
        "  const visibleTaskCount = Math.max(0, Number(taskCardCount) || 0);\n"
        "  if (\n"
        "    !maximAckCount &&\n"
        "    !visibleAckCount &&\n"
        "    !visibleTaskCount &&\n"
        '    kind !== "final"\n'
        "  )\n"
        "    return null;\n"
        '  const badges = document.createElement("div");\n'
        '  badges.className = "badges";\n'
        "  const add = (label, className, count) => {\n"
        '    const badge = document.createElement("span");\n'
        '    badge.className = className ? "badge " + className : "badge";\n'
        '    const text = document.createElement("span");\n'
        '    text.className = "badge-label";\n'
        "    text.textContent = label;\n"
        "    badge.append(text);\n"
        '    if (count !== undefined && count !== null && count !== "") {\n'
        '      const countEl = document.createElement("span");\n'
        '      countEl.className = "badge-count";\n'
        "      countEl.textContent = String(count);\n"
        "      badge.append(countEl);\n"
        "    }\n"
        "    badges.append(badge);\n"
        "  };\n"
        '  if (maximAckCount) add("MAXIM", "maxim-badge");\n'
        '  if (kind === "final") add("FINAL", "final-badge");\n'
        '  if (visibleTaskCount) add("TASK", "task-badge", visibleTaskCount);\n'
        '  if (visibleAckCount) add("ACK", "", visibleAckCount);\n'
        "  return badges;\n"
        "}\n"
        "\n"
    )
    assert "    item.task_card_count || 0,\n  );" in app_render


def test_static_message_badge_css_uses_compact_semantic_counts():
    css = _serve_css_text()
    index_css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    messages_css = (STATIC_ROOT / "messages.css").read_text(encoding="utf-8")
    badges_css_start = messages_css.index(".badges {")
    badges_css_rule = messages_css[
        badges_css_start : messages_css.index("}", badges_css_start)
    ]
    badge_css_start = messages_css.index(".badge {")
    badge_css_rule = messages_css[
        badge_css_start : messages_css.index("}", badge_css_start)
    ]
    badge_label_start = messages_css.index(".badge-label {")
    badge_label_rule = messages_css[
        badge_label_start : messages_css.index("}", badge_label_start)
    ]
    badge_count_start = messages_css.index(".badge-count {")
    badge_count_rule = messages_css[
        badge_count_start : messages_css.index("}", badge_count_start)
    ]
    final_badge_start = messages_css.index(".badge.final-badge {")
    final_badge_rule = messages_css[
        final_badge_start : messages_css.index("}", final_badge_start)
    ]
    task_badge_start = messages_css.index(".badge.task-badge {")
    task_badge_rule = messages_css[
        task_badge_start : messages_css.index("}", task_badge_start)
    ]
    maxim_badge_start = messages_css.index(".badge.maxim-badge {")
    maxim_badge_rule = messages_css[
        maxim_badge_start : messages_css.index("}", maxim_badge_start)
    ]
    filter_count_start = index_css.index(".filter-pill-count {")
    filter_count_rule = index_css[
        filter_count_start : index_css.index("}", filter_count_start)
    ]
    chip_count_start = index_css.index(".lane-filter-chip-count {")
    chip_count_rule = index_css[
        chip_count_start : index_css.index("}", chip_count_start)
    ]
    final_css_start = css.index(".messages article.final {")
    final_css_end = css.index(".messages article.final.acked {", final_css_start)

    assert "--message-occupant-accent" not in badges_css_rule
    assert "--message-badge-accent: var(--accent-strong);" in badge_css_rule
    assert (
        "border: 1px solid color-mix(in srgb, var(--message-badge-accent) 58%, var(--border));"
        in badge_css_rule
    )
    assert (
        "color: color-mix(in srgb, var(--message-badge-accent) 82%, var(--fg));"
        in badge_css_rule
    )
    assert "align-items: center;" in badge_css_rule
    assert "display: inline-flex;" in badge_css_rule
    assert "gap: 5px;" in badge_css_rule
    assert "padding: 2px 5px 2px 8px;" in badge_css_rule
    assert "line-height: 1.15;" in badge_label_rule
    assert badge_count_rule == (
        ".badge-count {\n"
        "  background: var(--message-badge-accent);\n"
        "  border-radius: var(--pill-radius);\n"
        "  color: var(--button-accent-fg);\n"
        "  font-size: 9px;\n"
        "  line-height: 13px;\n"
        "  padding: 0 5px;\n"
    )
    assert "--message-badge-accent: var(--final-accent);" in final_badge_rule
    assert "--message-badge-accent: var(--team-plum-accent);" in task_badge_rule
    assert "--message-badge-accent: var(--maxim-accent);" in maxim_badge_rule
    assert "font-weight: 700;" in maxim_badge_rule
    assert "background: var(--accent);" in filter_count_rule
    assert "background: var(--accent);" in chip_count_rule
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


def test_static_stream_queues_fresh_initial_payload_before_silent_prime():
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    apply_start = app_stream.index("async function applyLaneBusPayload")
    apply_body = app_stream[
        apply_start : app_stream.index(
            "\n}\n\nfunction initialPayloadSpeechMessages", apply_start
        )
    ]

    assert (
        "const initialSpeechMessages = wasSpeechPrimed\n"
        "    ? []\n"
        "    : initialPayloadSpeechMessages(lane, payload.messages || []);"
    ) in apply_body
    assert (
        "if (!lane.speechPrimed) {\n"
        "    queueSpeechForMessages(lane, initialSpeechMessages);\n"
        "    primeSpeechBoundary(lane);\n"
        "  }"
    ) in apply_body
    assert "function messageIsFreshForInitialSpeech" in app_stream
    assert "timestamp >= boundary" in app_stream


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
