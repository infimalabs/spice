"""Static serve stream, filter, and message UI contracts."""

from __future__ import annotations

import json
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
        'pill.querySelector(".filter-pill-count").textContent ='
        "\n      taskFilterStemPillCountText(model);" in app_lanes
    )
    assert "function taskFilterStemPillTone(model)" in app_lanes
    assert 'if (model.readyTaskCount > 0) return "ready";' in app_lanes
    assert 'if (model.inFlightTaskCount > 0) return "active";' in app_lanes
    assert 'return "dormant";' in app_lanes
    assert 'classes.push("filter-pill--" + tone);' in app_lanes
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
    assert '"ready work drained by " + drainability.count' in app_lanes
    assert '"ready work not currently drained"' in app_lanes
    assert '"work in flight"' in app_lanes
    assert '"no task currently movable"' in app_lanes
    assert "if (fingerprint === renderedFilterPillsFingerprint) return;" in app_lanes
    assert "renderedFilterPillsFingerprint = fingerprint;" in app_lanes
    assert "font-family: ui-monospace, SFMono-Regular, Menlo, monospace;" in (
        filter_pill_rule
    )
    assert "gap: 4px;" in filter_pill_rule
    assert filter_count_rule == (
        ".filter-pill-count {\n"
        "  background: var(--accent);\n"
        "  border-radius: var(--pill-radius);\n"
        "  box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--button-accent-fg) 24%, transparent);\n"
        "  color: var(--button-accent-fg);\n"
        "  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;\n"
        "  font-size: 9px;\n"
        "  padding: 0 5px;\n"
    )
    assert (
        ".filter-pill--active .filter-pill-count { background: var(--team-teal-accent); }"
        in css
    )
    assert (
        ".filter-pill--dormant .filter-pill-count { background: var(--muted); }" in css
    )
    assert ".filter-pill--implicit {" in css
    assert ".filter-pill--system { color: var(--warn); }" in css


def test_live_lane_payload_refreshes_global_task_filter_inventory():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")

    assert "function applyTaskFilterInventory(inventory)" in app_lanes
    assert "function taskFilterInventoryIsFresh(inventory)" in app_lanes
    assert "function syncTaskFilterInventoryState(inventory)" in app_lanes
    assert "function renderTaskFilterInventoryPanes()" in app_lanes
    assert "applyTaskFilterInventory(payload.taskFilterInventory || {});" in app_lanes
    assert (
        "if (payload.taskFilterInventory)\n"
        "    applyTaskFilterInventory(payload.taskFilterInventory);" in app_render
    )
    assert "lane.taskFilterInventory = payload.taskFilterInventory;" not in app_render


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
    assert chip_rule == (
        ".lane-filter-chip {\n"
        "  align-items: center;\n"
        "  background: color-mix(in srgb, var(--accent) 8%, var(--control));\n"
        "  border: 1px solid color-mix(in srgb, var(--accent) 30%, var(--border));\n"
        "  border-radius: var(--pill-radius);\n"
        "  color: var(--fg);\n"
        "  cursor: pointer;\n"
        "  display: inline-flex;\n"
        "  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;\n"
        "  font-size: 12px;\n"
        "  gap: 4px;\n"
        "  line-height: 1.1;\n"
        "  max-width: 100%;\n"
        "  min-height: 30px;\n"
        "  overflow: hidden;\n"
        "  padding: 3px 10px 3px 12px;\n"
        "  text-overflow: ellipsis;\n"
        "  white-space: nowrap;\n"
    )
    assert chip_count_rule == (
        ".lane-filter-chip-count {\n"
        "  background: var(--accent);\n"
        "  border-radius: var(--pill-radius);\n"
        "  box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--button-accent-fg) 24%, transparent);\n"
        "  color: var(--button-accent-fg);\n"
        "  flex: 0 0 auto;\n"
        "  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;\n"
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
    assert "return availableTaskFilterNames(laneFilterInventory(lane)" in app_panes
    assert "return taskFilterOpenCount(laneFilterInventory(lane), filter);" in (
        app_panes
    )


def test_static_filter_pane_renders_server_effective_filters_not_durable_rows():
    # The filter pane must reflect the lifetime-lensed set the allocator
    # actually routes on (Drain -> every assignable stem), which the server
    # ships as effectiveTaskFilters. Rendering the raw durable taskFilters here
    # is what desynced the chips/counts from in-flight work, so lock the pane's
    # source to effectiveTaskFilters and lock the plumbing that feeds it.
    app_panes = (STATIC_ROOT / "app.panes.js").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")

    # The chip/count source is the server lens, never the durable rows.
    assert "function laneAssignedTaskFilters(lane) {" in app_panes
    assert "for (const filter of member.effectiveTaskFilters || []) {" in app_panes

    # The lens rides in on every route/config path that seeds a lane or target.
    assert "payload.effectiveTaskFilters || lane.effectiveTaskFilters" in app_render
    assert (
        "lane.effectiveTaskFilters = uniqueStringList(config.effectiveTaskFilters);"
        in app_stream
    )
    assert (
        "updated.effectiveTaskFilters = uniqueStringList(config.effectiveTaskFilters);"
        in app_stream
    )
    assert (
        "lane.effectiveTaskFilters = uniqueStringList(config.effectiveTaskFilters);"
        in app_lanes
    )
    assert "laneStore.replaceTargets(payload.workTrees || []);" in app_lanes
    assert "renderLaneChrome(lane, laneStore.targetForId(lane.targetId));" in app_lanes
    assert (
        "effectiveTaskFilters: uniqueStringList(target.effectiveTaskFilters || []),"
        in app_shell
    )
    assert (
        "lane.effectiveTaskFilters = uniqueStringList(config.effectiveTaskFilters);"
        in app_shell
    )


def test_static_filter_model_helpers_are_pure_and_covered():
    script = Path(__file__).with_name("fixtures") / "filter_model.js"

    result = subprocess.run(
        ["node", str(script), str(STATIC_ROOT / "app.filter-model.js")],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


def test_static_mosaic_geometry_helpers_are_pure_and_covered():
    script = Path(__file__).with_name("fixtures") / "mosaic_geometry.js"

    result = subprocess.run(
        ["node", str(script), str(STATIC_ROOT / "app.mosaic-geometry.js")],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


def test_static_mosaic_geometry_is_wired_before_app_stream():
    app_js = (STATIC_ROOT.parent / "web.py").read_text(encoding="utf-8")
    geometry_index = app_js.index('src="/static/app.mosaic-geometry.js"')
    app_stream_index = app_js.index('src="/static/app.stream.js"')
    assert geometry_index < app_stream_index


def test_static_submission_lifecycle_is_wired_after_stream_before_lane_consumers():
    app_js = (STATIC_ROOT.parent / "web.py").read_text(encoding="utf-8")
    stream_index = app_js.index('src="/static/app.stream.js"')
    submissions_index = app_js.index('src="/static/app.submissions.js"')
    lanes_index = app_js.index('src="/static/app.lanes.js"')

    assert stream_index < submissions_index < lanes_index


def test_static_mosaic_engine_helpers_are_pure_and_covered():
    script = Path(__file__).with_name("fixtures") / "mosaic_engine.js"

    result = subprocess.run(
        ["node", str(script), str(STATIC_ROOT / "app.mosaic-engine.js")],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


def test_static_mosaic_engine_is_wired_before_app_stream():
    app_js = (STATIC_ROOT.parent / "web.py").read_text(encoding="utf-8")
    engine_index = app_js.index('src="/static/app.mosaic-engine.js"')
    app_stream_index = app_js.index('src="/static/app.stream.js"')
    assert engine_index < app_stream_index


def test_static_mosaic_sizing_helpers_are_pure_and_covered():
    script = Path(__file__).with_name("fixtures") / "mosaic_sizing.js"

    result = subprocess.run(
        ["node", str(script), str(STATIC_ROOT / "app.mosaic-sizing.js")],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


def test_static_mosaic_sizing_is_wired_before_app_stream():
    app_js = (STATIC_ROOT.parent / "web.py").read_text(encoding="utf-8")
    sizing_index = app_js.index('src="/static/app.mosaic-sizing.js"')
    app_stream_index = app_js.index('src="/static/app.stream.js"')
    assert sizing_index < app_stream_index


def test_static_mosaic_reservations_helpers_are_pure_and_covered():
    script = Path(__file__).with_name("fixtures") / "mosaic_reservations.js"

    result = subprocess.run(
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.mosaic-geometry.js"),
            str(STATIC_ROOT / "app.mosaic-sizing.js"),
            str(STATIC_ROOT / "app.mosaic-reservations.js"),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


def test_static_mosaic_reservations_is_wired_after_sizing_before_app_stream():
    app_js = (STATIC_ROOT.parent / "web.py").read_text(encoding="utf-8")
    sizing_index = app_js.index('src="/static/app.mosaic-sizing.js"')
    reservations_index = app_js.index('src="/static/app.mosaic-reservations.js"')
    app_stream_index = app_js.index('src="/static/app.stream.js"')
    assert sizing_index < reservations_index < app_stream_index


def test_static_mosaic_span_helpers_are_pure_and_covered():
    script = Path(__file__).with_name("fixtures") / "mosaic_span.js"

    result = subprocess.run(
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.mosaic-engine.js"),
            str(STATIC_ROOT / "app.mosaic-sizing.js"),
            str(STATIC_ROOT / "app.mosaic-span.js"),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


def test_static_mosaic_span_is_wired_after_sizing_before_render():
    app_js = (STATIC_ROOT.parent / "web.py").read_text(encoding="utf-8")
    sizing_index = app_js.index('src="/static/app.mosaic-sizing.js"')
    span_index = app_js.index('src="/static/app.mosaic-span.js"')
    render_index = app_js.index('src="/static/app.mosaic-render.js"')
    app_stream_index = app_js.index('src="/static/app.stream.js"')
    assert sizing_index < span_index < render_index < app_stream_index


def test_static_mosaic_render_is_wired_after_engine_before_app_stream():
    app_js = (STATIC_ROOT.parent / "web.py").read_text(encoding="utf-8")
    engine_index = app_js.index('src="/static/app.mosaic-engine.js"')
    render_index = app_js.index('src="/static/app.mosaic-render.js"')
    app_stream_index = app_js.index('src="/static/app.stream.js"')
    assert engine_index < render_index < app_stream_index


def test_static_mosaic_scroll_helpers_are_pure_and_covered():
    script = Path(__file__).with_name("fixtures") / "mosaic_scroll.js"

    result = subprocess.run(
        ["node", str(script), str(STATIC_ROOT / "app.mosaic-scroll.js")],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


def test_static_mosaic_scroll_is_wired_after_render_before_app_stream():
    app_js = (STATIC_ROOT.parent / "web.py").read_text(encoding="utf-8")
    render_index = app_js.index('src="/static/app.mosaic-render.js"')
    scroll_index = app_js.index('src="/static/app.mosaic-scroll.js"')
    app_stream_index = app_js.index('src="/static/app.stream.js"')
    assert render_index < scroll_index < app_stream_index


def test_static_mosaic_stream_is_wired_after_scroll_before_app_stream():
    app_js = (STATIC_ROOT.parent / "web.py").read_text(encoding="utf-8")
    scroll_index = app_js.index('src="/static/app.mosaic-scroll.js"')
    stream_index = app_js.index('src="/static/app.mosaic-stream.js"')
    app_stream_index = app_js.index('src="/static/app.stream.js"')
    assert scroll_index < stream_index < app_stream_index


def test_static_mosaic_wet_frozen_helpers_are_pure_and_covered():
    script = Path(__file__).with_name("fixtures") / "mosaic_wet_frozen.js"

    result = subprocess.run(
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.mosaic-engine.js"),
            str(STATIC_ROOT / "app.mosaic-wet-frozen.js"),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


def test_static_mosaic_wet_frozen_is_wired_after_engine_before_app_stream():
    app_js = (STATIC_ROOT.parent / "web.py").read_text(encoding="utf-8")
    engine_index = app_js.index('src="/static/app.mosaic-engine.js"')
    wet_frozen_index = app_js.index('src="/static/app.mosaic-wet-frozen.js"')
    app_stream_index = app_js.index('src="/static/app.stream.js"')
    assert engine_index < wet_frozen_index < app_stream_index


def test_static_mosaic_full_replay_helpers_are_pure_and_covered():
    script = Path(__file__).with_name("fixtures") / "mosaic_full_replay.js"

    result = subprocess.run(
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.mosaic-engine.js"),
            str(STATIC_ROOT / "app.mosaic-wet-frozen.js"),
            str(STATIC_ROOT / "app.mosaic-full-replay.js"),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


def test_static_mosaic_full_replay_is_wired_after_wet_frozen_before_sizing():
    app_js = (STATIC_ROOT.parent / "web.py").read_text(encoding="utf-8")
    wet_frozen_index = app_js.index('src="/static/app.mosaic-wet-frozen.js"')
    full_replay_index = app_js.index('src="/static/app.mosaic-full-replay.js"')
    sizing_index = app_js.index('src="/static/app.mosaic-sizing.js"')
    assert wet_frozen_index < full_replay_index < sizing_index


def test_static_mosaic_event_log_helpers_are_pure_and_covered():
    script = Path(__file__).with_name("fixtures") / "mosaic_event_log.js"

    result = subprocess.run(
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.mosaic-engine.js"),
            str(STATIC_ROOT / "app.mosaic-wet-frozen.js"),
            str(STATIC_ROOT / "app.mosaic-full-replay.js"),
            str(STATIC_ROOT / "app.mosaic-event-log.js"),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


def test_static_mosaic_event_log_is_wired_after_full_replay_before_sizing():
    app_js = (STATIC_ROOT.parent / "web.py").read_text(encoding="utf-8")
    full_replay_index = app_js.index('src="/static/app.mosaic-full-replay.js"')
    event_log_index = app_js.index('src="/static/app.mosaic-event-log.js"')
    sizing_index = app_js.index('src="/static/app.mosaic-sizing.js"')
    assert full_replay_index < event_log_index < sizing_index


def test_static_mosaic_seam_rule_holds_across_widths_including_fractional_colw():
    script = Path(__file__).with_name("fixtures") / "mosaic_seam.js"

    result = subprocess.run(
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.mosaic-geometry.js"),
            str(STATIC_ROOT / "app.mosaic-render.js"),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


def test_static_mosaic_seam_rule_colw_multiplication_confined_to_geometry():
    # Seam rule: only the edges[] table construction in mosaic-geometry.js
    # may reference colW in code. Any other mosaic file computing with colW
    # would be deriving a card's x/width independently of the shared integer
    # edges table -- exactly the per-card rounding the seam rule forbids.
    # Comments may still discuss colW (e.g. explaining what NOT to do), so
    # strip `//` line comments before scanning.
    def code_only(text):
        return "\n".join(line.split("//", 1)[0] for line in text.splitlines())

    mosaic_files = sorted(STATIC_ROOT.glob("app.mosaic-*.js"))
    offenders = [
        path.name
        for path in mosaic_files
        if path.name != "app.mosaic-geometry.js"
        and "colW" in code_only(path.read_text(encoding="utf-8"))
    ]
    assert offenders == []


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
    assert ".message-body .task-directive-stack {" in css
    assert ".message-body .task-directive-stack .task-directive-quote {" in css
    assert "grid-template-columns: repeat(" in css
    assert "var(--message-card-max-width)" in css
    assert "max-width: min(100%, var(--message-card-max-width));" in css
    assert "min-width: 0;" in css
    assert ".message-body .task-directive-quote--hidden {" in css
    assert ".message-body .task-directive-quote--oops {" in css
    assert ".message-body .task-directive-quote--private {" in css
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


def test_static_keyboard_submit_refocuses_target_composer_after_unlock():
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
    send_payload_start = app_stream.index("async function sendLanePayload(")
    send_payload_body = app_stream[
        send_payload_start : app_stream.index(
            "\n}\n\nfunction applyLaneSendResult", send_payload_start
        )
    ]

    assert "const focusAfterReset = keyboardSubmitFocusTarget(" in submit_body
    assert "{ focusAfterReset }" in submit_body
    assert 'if (event.type !== "keydown") return null;' in focus_body
    assert "if (!(target instanceof HTMLTextAreaElement)) return null;" in focus_body
    assert "if (!target.dataset.quoteDraftId) return target;" in focus_body
    assert (
        'throw new Error("keyboard quote submit requires main composer");' in focus_body
    )
    assert "return textarea;" in focus_body
    assert (
        "function enqueueSend(lane, payload, sourceLane = lane, options = {})"
        in app_stream
    )
    assert "const latencyProbe = startLaneSubmitLatencyProbe(lane, payload);" in (
        app_stream
    )
    assert 'markLaneSubmitLatency(latencyProbe, "optimisticRenderedAt");' in app_stream
    assert (
        "sendLanePayload(lane, payload, sourceLane, { ...options, latencyProbe });"
        in app_stream
    )
    assert 'markLaneSubmitLatency(latencyProbe, "requestAwaitStartAt");' in (
        send_payload_body
    )
    assert 'markLaneSubmitLatency(latencyProbe, "responseResolvedAt");' in (
        send_payload_body
    )
    assert 'finishLaneSubmitLatencyProbe(latencyProbe, "closed");' in (
        send_payload_body
    )
    assert "applyLaneSendResult(lane, payload, result, sourceLane, options);" in (
        send_payload_body
    )
    assert 'markLaneSubmitLatency(latencyProbe, "resultAppliedAt");' in (
        send_payload_body
    )
    assert 'result.ok ? "accepted" : "rejected"' in send_payload_body
    assert 'markLaneSubmitLatency(latencyProbe, "errorAt");' in send_payload_body
    assert 'finishLaneSubmitLatencyProbe(latencyProbe, "error");' in (send_payload_body)
    assert "options = {}," in result_body
    assert result_body.index("finishLanePendingSubmission(lane") < result_body.index(
        "focusAfterComposerReset(options.focusAfterReset);"
    )
    assert "clearAcceptedComposerDrafts(sourceLane, lane.targetId);" in result_body
    assert (
        'throw new Error("composer focus target must remain in the document");'
        in focus_reset_body
    )
    assert "element.focus({ preventScroll: true });" in focus_reset_body


def test_static_send_latency_probe_records_submit_timing_buckets():
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    app_live_bus = (STATIC_ROOT / "app.live-bus.js").read_text(encoding="utf-8")
    app_static = app_live_bus + "\n" + app_stream

    assert ").__spiceSubmitLatencySamples =" in app_live_bus
    assert "laneSubmitLatencySamples;" in app_live_bus
    assert ").__spiceLiveBusDiagnostics = liveBusDiagnostics;" in app_live_bus
    assert "function recordLaneSendTiming(message)" in app_live_bus
    assert 'message.type !== "lane.sendTiming"' in app_live_bus
    assert "sample.serverTiming = message.serverTiming || {};" in app_live_bus
    assert "pendingLaneSendServerTimings.has(probe.requestId)" in app_stream
    assert '"lanes.dirty", handleBackgroundLanesDirtyPush' in app_live_bus
    assert "focused: liveBusLaneIsFocused(lane)" in app_live_bus
    assert "function installLiveBusLaneFocusTracking()" in app_live_bus
    assert "function startLaneSubmitLatencyProbe(lane, payload)" in app_static
    assert "function finishLaneSubmitLatencyProbe(probe, status)" in app_static
    assert "function laneSubmitLatencyDurations(marks)" in app_static
    assert "optimisticRenderMs:" in app_static
    assert "liveBusOpenMs:" in app_static
    assert "sendResultWaitMs:" in app_static
    assert "responseHandlingMs:" in app_static
    assert "totalMs:" in app_static
    assert "latencyProbe.serverTiming = result.serverTiming || {};" in app_static
    assert 'markLaneSubmitLatency(timing, "liveBusConnectStartAt");' in app_live_bus
    assert 'markLaneSubmitLatency(pending.timing, "liveBusResponseReceivedAt");' in (
        app_live_bus
    )


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
    assert "card.append(historySentinelForLane(member));" in app_stream
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
        "  applyPayloadAckContexts(lane, payload);\n"
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
        "function removePayloadMessages(lane, payload) {\n"
        "  const keys = new Set(payload.removedMessageKeys || []);\n"
        "  if (!keys.size) return;\n"
        "  lane.knownMessages = lane.knownMessages.filter((item) => !keys.has(item.key));\n"
        "  lane.knownMessageKeys = new Set(lane.knownMessages.map((item) => item.key));\n"
        "}\n"
        "\n"
        "function mergeOlderPayloadMessages(lane, payload) {\n"
        "  applyPayloadAckContexts(lane, payload);\n"
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


def test_static_stream_uses_server_supplied_ack_contexts():
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    app_live_bus = (STATIC_ROOT / "app.live-bus.js").read_text(encoding="utf-8")

    assert "function applyPayloadAckContexts(lane, payload)" in app_stream
    assert "payload?.ackContexts || []" in app_stream
    assert "function hydrateAckContextsForMessages" not in app_stream
    assert 'targetApi(lane.targetId, "/acks")' not in app_stream
    assert "hydrateAckContextsForMessages" not in app_live_bus


_KNOWN_MESSAGE_ORDER_SCRIPT = """
const fs = require("fs");
const vm = require("vm");
const ctx = { console, Date, Set, Map, Number, Math, JSON, Boolean, String, Array, Object };
ctx.window = ctx;
ctx.document = { querySelectorAll: () => [] };
ctx.payloadHasField = () => false;
ctx.targetIdentityThreadId = () => "";
vm.createContext(ctx);
vm.runInContext(fs.readFileSync(process.argv[1], "utf8"), ctx);
const lane = {
  knownMessages: [],
  knownMessageKeys: new Set(),
  retainedMessageLimit: 200,
  activeThreadId: "t",
  occupants: new Map(),
  recentSentAckKeys: new Set(),
  ackContextByKey: new Map(),
  missingAckContextKeys: new Set(),
};
const tmsg = (t, i) => ({ key: t + "#" + i, timestamp: t, index: i, kind: "assistant" });
// Live appends populate the cache transcript-only, newest first.
ctx.mergePayloadMessages(lane, { messages: [
  tmsg("2026-06-22T15:36:00.000Z", 50),
  tmsg("2026-06-22T15:30:00.000Z", 40),
  tmsg("2026-06-22T15:20:00.000Z", 30),
] });
// A full-window payload (the only place task cards appear) folds an older task
// card back in; it must land by timestamp, not jump to the newest slot.
ctx.mergePayloadMessages(lane, { messages: [
  tmsg("2026-06-22T15:36:00.000Z", 50),
  { key: "2026-06-22T15:29:00.000Z#task-card:RTK", timestamp: "2026-06-22T15:29:00.000Z", index: 0, kind: "task-card" },
  tmsg("2026-06-22T15:30:00.000Z", 40),
] });
process.stdout.write(JSON.stringify(lane.knownMessages.map((m) => m.timestamp)));
"""


def test_stream_merge_keeps_known_messages_newest_first_with_task_cards():
    result = subprocess.run(
        ["node", "-e", _KNOWN_MESSAGE_ORDER_SCRIPT, str(STATIC_ROOT / "app.stream.js")],
        capture_output=True,
        text=True,
        check=True,
    )
    assert json.loads(result.stdout) == [
        "2026-06-22T15:36:00.000Z",
        "2026-06-22T15:30:00.000Z",
        "2026-06-22T15:29:00.000Z",
        "2026-06-22T15:20:00.000Z",
    ]


def test_static_stream_renders_message_badge_dom():
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    badge_start = app_render.index("function renderBadges")
    badge_end = app_render.index("// The packer", badge_start)

    assert app_render[badge_start:badge_end] == (
        "function renderBadges(ackCount, kind, maximAckCount, taskCardCount, nackCount) {\n"
        "  const visibleAckCount = Math.max(0, ackCount - maximAckCount);\n"
        "  const visibleNackCount = Math.max(0, Number(nackCount) || 0);\n"
        "  const visibleTaskCount = Math.max(0, Number(taskCardCount) || 0);\n"
        "  if (\n"
        "    !maximAckCount &&\n"
        "    !visibleAckCount &&\n"
        "    !visibleNackCount &&\n"
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
        '  if (visibleAckCount) add("ACK", "", visibleAckCount);\n'
        '  if (visibleNackCount) add("NACK", "nack-badge", visibleNackCount);\n'
        '  if (visibleTaskCount) add("TASK", "task-badge", visibleTaskCount);\n'
        '  if (kind === "final") add("FINAL", "final-badge");\n'
        '  if (maximAckCount) add("MAXIM", "maxim-badge");\n'
        "  return badges;\n"
        "}\n"
        "\n"
    )
    assert app_render.index('add("ACK", "", visibleAckCount)') < app_render.index(
        'add("NACK", "nack-badge", visibleNackCount)'
    )
    assert app_render.index(
        'add("NACK", "nack-badge", visibleNackCount)'
    ) < app_render.index('add("TASK", "task-badge", visibleTaskCount)')
    assert app_render.index(
        'add("TASK", "task-badge", visibleTaskCount)'
    ) < app_render.index('add("FINAL", "final-badge")')
    assert app_render.index('add("FINAL", "final-badge")') < app_render.index(
        'add("MAXIM", "maxim-badge")'
    )
    assert "    item.nack_count || 0,\n  );" in app_render


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
    badge_count_start = messages_css.index(".badge-count {")
    badge_count_rule = messages_css[
        badge_count_start : messages_css.index("}", badge_count_start)
    ]
    article_start = messages_css.index(".messages article {")
    article_rule = messages_css[article_start : messages_css.index("}", article_start)]
    acked_article_start = messages_css.index(".messages article.acked {")
    acked_article_rule = messages_css[
        acked_article_start : messages_css.index("}", acked_article_start)
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
    refused_article_start = messages_css.index(".messages article.refused {")
    refused_article_rule = messages_css[
        refused_article_start : messages_css.index("}", refused_article_start)
    ]
    final_refused_start = css.index(".messages article.final.refused {")
    final_refused_end = css.index("}", final_refused_start)

    assert "--message-occupant-accent" not in badges_css_rule
    assert "--message-badge-surface: var(--panel);" in article_rule
    assert "--message-badge-surface: var(--ack-tint);" in acked_article_rule
    assert "--message-badge-accent: var(--accent-strong);" in badge_css_rule
    assert "background: var(--message-badge-accent);" in badge_css_rule
    assert (
        "border: 1px solid color-mix(in srgb, var(--message-badge-accent) 42%, var(--message-badge-surface));"
        in badge_css_rule
    )
    assert "color: var(--button-accent-fg);" in badge_css_rule
    assert "align-items: center;" in badge_css_rule
    assert "display: inline-flex;" in badge_css_rule
    assert "font-family: ui-monospace, SFMono-Regular, Menlo, monospace;" in (
        badge_css_rule
    )
    assert "gap: 4px;" in badge_css_rule
    assert "padding: 2px 6px;" in badge_css_rule
    assert ".badge-label {" not in messages_css
    assert badge_count_rule == (
        ".badge-count {\n"
        "  background: var(--message-badge-surface);\n"
        "  border: 0;\n"
        "  border-radius: var(--pill-radius);\n"
        "  box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--message-badge-accent) 24%, transparent);\n"
        "  color: var(--message-badge-accent);\n"
        "  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;\n"
        "  font-size: 9px;\n"
        "  padding: 0 5px;\n"
    )
    assert "--message-badge-accent: var(--final-accent);" in final_badge_rule
    assert "--message-badge-accent: var(--team-plum-accent);" in task_badge_rule
    assert maxim_badge_rule == (
        ".badge.maxim-badge {\n  --message-badge-accent: var(--maxim-accent);\n"
    )
    assert "background: var(--accent);" in filter_count_rule
    assert "background: var(--accent);" in chip_count_rule
    assert css[final_css_start:final_css_end] == (
        ".messages article.final {\n"
        "  --message-badge-surface: var(--final-tint);\n"
        "  background: var(--final-tint);\n"
        "  border-color: var(--final-accent);\n"
        "  box-shadow: inset 0 3px 0 var(--final-accent);\n"
        "}\n"
    )
    # A refusal tints the whole card with --warn: warn-tint surface, a warn rail,
    # and the same treatment folded onto a final refusal.
    assert "--warn-tint: color-mix(in srgb, var(--warn) 8%, var(--panel));" in index_css
    assert "--message-badge-surface: var(--warn-tint);" in refused_article_rule
    assert "box-shadow: inset 3px 0 0 var(--warn);" in refused_article_rule
    assert css[final_refused_start:final_refused_end] == (
        ".messages article.final.refused {\n"
        "  --message-final-refused-surface: color-mix(in srgb, var(--warn-tint) 50%, var(--final-tint));\n"
        "  --message-badge-surface: var(--message-final-refused-surface);\n"
        "  background: var(--message-final-refused-surface);\n"
        "  box-shadow:\n"
        "    inset 3px 0 0 var(--warn),\n"
        "    inset 0 3px 0 var(--final-accent);\n"
    )
    assert ".badge.nack-badge { --message-badge-accent: var(--warn); }" in messages_css


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
    app_live_bus = (STATIC_ROOT / "app.live-bus.js").read_text(encoding="utf-8")
    apply_start = app_live_bus.index("async function applyLaneBusPayload")
    apply_body = app_live_bus[
        apply_start : app_live_bus.index(
            "\n}\n\nfunction applyLanePendingBusPayload", apply_start
        )
    ]

    assert 'if (wasSpeechPrimed && source === "watch")' not in apply_body
    assert "if (wasSpeechPrimed) {" in apply_body
    assert (
        "const fresh = (payload.messages || []).filter(\n"
        "      (item) => item.key && !knownBefore.has(item.key),\n"
        "    );" in apply_body
    )
    assert "queueSpeechForMessages(lane, fresh);" in apply_body


def test_static_stream_queues_initial_payload_before_silent_prime():
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    app_live_bus = (STATIC_ROOT / "app.live-bus.js").read_text(encoding="utf-8")
    apply_start = app_live_bus.index("async function applyLaneBusPayload")
    apply_body = app_live_bus[
        apply_start : app_live_bus.index(
            "\n}\n\nfunction applyLanePendingBusPayload", apply_start
        )
    ]

    assert (
        "const initialSpeechMessages = wasSpeechPrimed ? [] : payload.messages || [];"
    ) in apply_body
    assert (
        "if (!lane.speechPrimed) {\n"
        "    queueSpeechForMessages(lane, initialSpeechMessages);\n"
        "    primeSpeechBoundary(lane);\n"
        "  }"
    ) in apply_body
    # The grace-window pre-filter is retired; the single materialization gate
    # lives in queueSpeechForMessages (app.audio.js).
    assert "messageIsFreshForInitialSpeech" not in app_stream
    assert "initialSpeechStartupGraceMs" not in app_stream
    app_audio = (STATIC_ROOT / "app.audio.js").read_text(encoding="utf-8")
    assert "function messageIsBeforeLaneMaterialization" in app_audio
    assert "timestamp < lane.speechPrimeStartedAt" in app_audio


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
    assert (
        'if (change.kind !== "lanes" || change.transition !== "removed") return;\n'
        "  syncNarrationMediaSession();" in app_lanes
    )


def test_static_speech_session_title_leads_with_agent_identity():
    script = Path(__file__).with_name("fixtures") / "speech_session_title.js"

    result = subprocess.run(
        ["node", str(script), str(STATIC_ROOT / "app.audio.js")],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


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
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.lane-store.js"),
            str(app_groups),
        ],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr
