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
        "  installLiveBusLaneFocusTracking();\n"
        "  await connectLiveBus();\n"
        "  await refreshServerTopology();\n"
        "  setInterval(updateLiveRelativeTimes, relativeTimeTickMs);\n"
        "}\n"
    ) in app


def test_static_fresh_startup_keeps_import_shell_with_stale_restore_hints():
    app_lanes = STATIC_ROOT / "app.lanes.js"
    script = Path(__file__).with_name("fixtures") / "fresh_startup_import_shell.js"

    result = subprocess.run(
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.lane-store.js"),
            str(app_lanes),
        ],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_static_team_command_failure_forces_snapshot_refresh():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    body = _between(
        app_lanes,
        "async function requestTeamCommand(payload) {",
        "\n}\n\nfunction teamCommandPayload",
    )

    assert (
        "if (result.ok === false) {\n"
        "    await refreshTeamSnapshot({ force: true });\n"
        '    throw new Error(result.error || "team command failed");\n'
        "  }"
    ) in body


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
    assert "updated.targetIdentity = config.targetIdentity;" in inventory_body
    assert "updated.serveAgentIdentity = config.serveAgentIdentity;" in inventory_body
    assert "updated.teamIdentity = config.teamIdentity;" in inventory_body


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


def test_static_root_font_stack_includes_color_emoji_fallback():
    # Emoji (e.g. the spice pepper U+1F336 U+FE0F) must render in color across
    # the menu, message bodies, and composer, all of which inherit the :root
    # font. The base stack therefore ends in explicit color-emoji families.
    css = _serve_css_text()

    assert (
        "font-family: ui-sans-serif, system-ui, sans-serif, "
        '"Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji", emoji;'
    ) in css


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
    assert "if (generation !== playbackGeneration) stopStalePlayback(audio);" in (
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
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.lane-store.js"),
            str(app_render),
        ],
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


def test_static_messages_have_no_legacy_pack_vestige():
    # mosaic-demolition: the legacy grid packer (app.message-pack.js,
    # packMessageStream and its orbit in app.stream.js, every
    # --message-pack-* CSS custom property) is deleted outright, not shimmed
    # or left dormant. This asserts the negative -- no legacy trace survives
    # -- and that the still-real image-stack CSS and mosaic wiring it used to
    # sit alongside are untouched.
    css = _serve_css_text()
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    app_mosaic_stream = (STATIC_ROOT / "app.mosaic-stream.js").read_text(
        encoding="utf-8"
    )
    static_names = {path.name for path in STATIC_ROOT.glob("*.js")}

    assert "app.message-pack.js" not in static_names
    assert "message-pack" not in css.lower()
    assert "messagepack" not in css.lower()
    assert "message-pack" not in app_stream.lower()
    assert "messagepack" not in app_stream.lower()

    # UI-1k9zdKM9: the mosaic-native root-width mechanism (mosaicSyncRootWidth
    # et al, briefly restored in app.mosaic-stream.js under the same visual
    # concern) proved to be a genuine no-op empirically -- a lone visible
    # lane already reaches the swimlanes' full width via plain
    # .lane{flex:1 1 0} layout, and a sibling-lane-closes transition is
    # already corrected by the existing mosaicSyncResizeObserver, with or
    # without this mechanism running. Deleted outright; mosaic-geometry
    # reads clientWidth with no CSS-adornment step beforehand.
    assert "mosaicSyncRootWidth" not in app_stream
    assert "mosaicSyncRootWidth" not in app_mosaic_stream
    assert "mosaic-root-width" not in css.lower()

    # index.css has an unrelated `.messages { ... }` mobile-padding override
    # containing the substring ".messages {"; anchor the search from the
    # "---- messages ----" section banner, unique to messages.css, so the
    # base rule (not that override) is what gets sliced.
    messages_section = css.index("/* ---- messages ---- */")
    messages_rule = _between(css[messages_section:], ".messages {", "}")
    article_rule = _between(css[messages_section:], ".messages article {", "}")
    stack_rule = _between(css, ".message-body p.message-image-stack {", "}")
    stack_image_rule = _between(
        css, ".message-body p.message-image-stack .message-image img {", "}"
    )
    image_only_image_rule = _between(
        css, ".messages article.image-only .message-image img {", "}"
    )

    assert "--message-card-max-width: 30rem;" in messages_rule
    assert "--message-card-min-width: 20rem;" in messages_rule
    assert "--mosaic-image-height: 8.75rem;" in messages_rule
    assert "--mosaic-image-large-height: 15.75rem;" in messages_rule
    assert "display: grid;" not in messages_rule
    assert "grid-template-columns:" not in messages_rule
    # The mosaic host clips horizontal overflow rather than scrolling it: the
    # lattice always fits, so a horizontal scrollbar is never intended and its
    # appearance would shrink clientWidth and cascade a re-measure.
    assert "overflow-x: clip;" in messages_rule
    assert "overflow-x: auto;" not in messages_rule
    assert "position: relative;" in messages_rule
    assert "display: flex;" in article_rule
    assert "flex-direction: column;" in article_rule
    assert "grid-row-end:" not in article_rule
    assert "max-width: none;" in article_rule
    assert "direction: ltr;" in article_rule
    assert "display: flex;" in stack_rule
    assert "flex-direction: row;" in stack_rule
    assert "flex-wrap: nowrap;" in stack_rule
    assert "justify-content: flex-start;" in stack_rule
    assert "overflow-x: auto;" in stack_rule
    assert "max-width: 156px;" in stack_image_rule
    assert "height: var(--mosaic-image-large-height);" in image_only_image_rule
    assert "max-height: var(--mosaic-image-large-height);" in image_only_image_rule
    assert "max-width: 100%;" in image_only_image_rule
    assert "width: auto;" in image_only_image_rule
    assert "grid-row-end:" not in _between(css, ".history-sentinel {", "}")

    assert "mosaicRenderMessageStream(lane, visibleItems);" in app_stream
    assert "mosaicSyncResizeObserver(lane);" in app_mosaic_stream
    assert 'if (item.image_only) article.classList.add("image-only");' in app_render


def test_static_inline_task_directives_use_quote_like_accented_blocks():
    css = _serve_css_text()
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")

    quote_rule = _between(css, ".message-body .task-directive-quote {", "}")
    property_rule = _between(css, ".task-directive-property {", "}")
    detail_rule = _between(css, ".task-directive-property dd {", "}")
    palette = _between(app_render, "const messageOccupantAccentPalette = [", "];")

    # The accent slot count that bounds attribution indices (app.stream.js)
    # must equal the render palette length, or an in-range index could still
    # exceed the palette and make messageOccupantAccent throw mid-render.
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    palette_length = len([line for line in palette.splitlines() if "var(--" in line])
    assert palette_length == 6
    assert f"const MESSAGE_ACCENT_SLOT_COUNT = {palette_length};" in app_stream
    assert "% MESSAGE_ACCENT_SLOT_COUNT" in app_stream

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
    line_rule = _between(css, ".lane-metric-series-line {", "}")

    assert "display: flex;" in metrics_panel_rule
    assert "flex-direction: column;" in metrics_panel_rule
    assert "width: 100%;" in metrics_panel_rule
    assert "flex: 1 1 auto;" in metrics_grid_rule
    assert "grid-template-rows: minmax(96px, 1fr) repeat(4, max-content);" in (
        metrics_grid_rule
    )
    assert "min-height: 0;" in metrics_grid_rule
    assert "width: 100%;" in metrics_grid_rule
    assert "display: flex;" in chart_rule
    assert "height: 100%;" in svg_rule
    assert "min-height: 0;" in svg_rule
    assert "stroke-width: 1.1;" in line_rule
    assert "vector-effect: non-scaling-stroke;" in line_rule
    assert 'svg.setAttribute("preserveAspectRatio", "none");' in app_panes
    assert 'dot.setAttribute("r", "0.65");' in app_panes
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
    assert "laneStore.applyLaneGroups(groupRuns, {" in app_groups
    assert "function currentLaneGroupRunsWithReplacements(replacements)" in app_groups
    assert (
        'if (member !== host) member.element.classList.add("lane--shadowed");'
        in app_groups
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

    assert "const label = laneComposeTargetLabel(member);" in app_shell
    assert "function laneComposeTargetLabel(member)" in app_shell
    assert 'return agentBranchLabel(agent, branch, ", ");' in app_shell
    assert "const claimedTask = laneClaimedTask(member);" in app_shell
    assert "const claimedTaskLabel = laneClaimedTaskLabel(claimedTask);" in app_shell
    assert (
        'return [label, status, claimedTaskLabel].filter(Boolean).join("\\n");'
        in app_shell
    )
    assert "function laneClaimedTaskLabel(task)" in app_shell
    assert 'task.handle + ", " + task.phase' in app_shell
    assert "function laneComposeTaskTooltip(member)" in app_shell
    assert (
        'return [laneClaimedTaskLabel(task), task.title].filter(Boolean).join("\\n");'
        in app_shell
    )
    assert "textarea.title = laneComposeTaskTooltip(member);" in app_shell
    assert 'textarea.closest(".composer-band--primary")' in app_shell
    assert "function laneComposePlaceholderStatus(member)" in app_shell
    assert "const pending = lanePendingDisplayCount(member);" in app_shell
    assert 'parts.push(pending + " pending");' in app_shell
    assert 'if (pending > 0) parts.push(pending + " pending");' not in app_shell
    assert "const statusLine = member.lastRenderedStatusLine || {};" in app_shell
    assert (
        "const status = statusLine.agentVisualStatus || "
        'statusLine.agentProcessStatus || "";' in app_shell
    )
    assert "if (status) parts.push(status);" in app_shell
    assert 'return "Steer " + laneMemberTargetLabel(lane);' not in app_shell
    assert 'textarea.placeholder = "Reply with quoted context";' not in app_shell
    assert "const member = laneStore.laneForId(targetId) || lane;" in app_shell
    assert "syncComposerQuoteBand(band, lane, targetId, member, draft);" in app_shell
    assert "createComposerQuoteTextarea(lane, targetId, draft);" in app_shell
    assert (
        app_shell.count("textarea.placeholder = laneComposePlaceholder(member);") >= 3
    )


def test_static_composer_terminal_status_placeholder_uses_idle_visual_status():
    app_render = STATIC_ROOT / "app.render.js"
    app_submissions = STATIC_ROOT / "app.submissions.js"
    app_composer = STATIC_ROOT / "app.composer.js"
    script = Path(__file__).with_name("fixtures") / "composer_terminal_status.js"

    result = subprocess.run(
        [
            "node",
            str(script),
            str(app_render),
            str(app_submissions),
            str(app_composer),
        ],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_static_target_choice_labels_show_agent_name_on_branch():
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")

    assert (
        'function agentBranchLabel(agentName, branchName, separator = " on ")'
        in app_render
    )
    assert "return agent + separator + branch;" in app_render
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

    result = subprocess.run(
        ["node", str(script), str(app_stream), str(app_render)],
        check=True,
    )
    assert result.returncode == 0


def test_static_lane_differential_frames_update_pending_and_messages():
    app_render = STATIC_ROOT / "app.render.js"
    app_live_bus = STATIC_ROOT / "app.live-bus.js"
    app_stream = STATIC_ROOT / "app.stream.js"
    app_submissions = STATIC_ROOT / "app.submissions.js"
    script = Path(__file__).with_name("fixtures") / "lane_diff_frames.js"

    result = subprocess.run(
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.lane-store.js"),
            str(app_render),
            str(app_live_bus),
            str(app_stream),
            str(app_submissions),
        ],
        check=True,
    )
    assert result.returncode == 0


def test_static_submission_lifecycle_is_monotonic_and_member_scoped():
    script = Path(__file__).with_name("fixtures") / "submission_lifecycle.js"
    result = subprocess.run(
        ["node", str(script), str(STATIC_ROOT / "app.submissions.js")],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_static_submission_lifecycle_uses_current_keyed_event_shape():
    submissions = (STATIC_ROOT / "app.submissions.js").read_text(encoding="utf-8")
    live_bus = (STATIC_ROOT / "app.live-bus.js").read_text(encoding="utf-8")
    stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    composer = (STATIC_ROOT / "app.composer.js").read_text(encoding="utf-8")

    assert "function applyLaneSubmissionLifecycle(lane, rawSubmission)" in submissions
    assert "function reconcileLaneSubmissionMessages(lane, messages)" in submissions
    assert '["lane.submission", handleLaneSubmissionPush]' in live_bus
    assert "applyLaneSubmissionLifecycle(lane, result.submission);" in stream
    before_menu_start = composer.index("function composerPrimaryHeaderBeforeMenu(")
    before_menu_end = (
        composer.index(
            "\n}\n\nfunction composerPrimaryLatestMessageLink", before_menu_start
        )
        + 2
    )
    before_menu = composer[before_menu_start:before_menu_end]
    assert before_menu == (
        "function composerPrimaryHeaderBeforeMenu(latest, member) {\n"
        "  return [\n"
        "    latest\n"
        "      ? composerPrimaryLatestMessageLink(latest, member)\n"
        "      : composerPrimaryLatestMessageNote(member),\n"
        "  ];\n"
        "}"
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
    assert sync_body.count("const member = laneStore.laneForId(targetId) || lane;") == 2
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
    assert (
        'return item.kind === "assistant" || item.kind === "final" || '
        'item.kind === "reply";' in app_shell
    )
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
    assert "return index % MESSAGE_ACCENT_SLOT_COUNT;" in app_stream
    assert "syncComposerBandAccent(primary, lane, member);" in app_shell
    assert "syncComposerBandAccent(band, lane, member);" in app_shell
