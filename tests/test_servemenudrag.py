"""Static serve UI menu drag contracts."""

from __future__ import annotations

from spice.serve.web import STATIC_ROOT

SERVE_CSS_FILES = ("index.css", "composer.css", "messages.css", "status-colors.css")


def _serve_css_text() -> str:
    return "\n".join(
        (STATIC_ROOT / filename).read_text(encoding="utf-8")
        for filename in SERVE_CSS_FILES
    )


def test_static_spice_menu_target_drag_cleans_ghosts_and_keeps_desktop_drops_open():
    css = _serve_css_text()
    app_js = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_menu = (STATIC_ROOT / "app.menu.js").read_text(encoding="utf-8")

    close_start = app_menu.index("function closeSpiceMenu() {")
    close_body = app_menu[
        close_start : app_menu.index("\n}\n\nfunction renderSpiceMenu", close_start)
    ]
    render_start = app_menu.index("function renderSpiceMenu() {")
    render_body = app_menu[
        render_start : app_menu.index("\n}\n\nfunction positionSpiceMenu", render_start)
    ]
    pointer_start = app_menu.index(
        'button.addEventListener("pointerdown", (event) => {',
        app_menu.index("function wireSpiceMenuTargetDrag"),
    )
    pointer_body = app_menu[
        pointer_start : app_menu.index(
            'button.addEventListener("pointermove", (event) => {', pointer_start
        )
    ]
    target_drag_start = css.index(".target-choice--dragging {")
    target_drag_rule = css[target_drag_start : css.index("}", target_drag_start)]

    assert "let spiceMenuTargetDragState = null;" in app_js
    assert "let spiceMenuRenderPending = false;" in app_js
    assert "clearSpiceMenuTargetDrag();" in close_body
    assert "spiceMenuRenderPending = false;" in close_body
    assert "if (spiceMenuTargetDragState) {" in render_body
    assert "spiceMenuRenderPending = true;" in render_body
    assert "clearSpiceMenuTargetDrag();" in render_body
    assert "function flushPendingSpiceMenuRender()" in app_menu
    assert "flushPendingSpiceMenuRender();" in app_menu
    assert "clearSpiceMenuTargetDrag();" in pointer_body
    assert "const state = {" in pointer_body
    assert "spiceMenuTargetDragState = state;" in pointer_body
    assert "event.preventDefault();" in pointer_body
    assert (
        "state.pointerCleanup = wireSpiceMenuTargetPointerDocumentEvents(target);"
        in app_menu
    )
    assert 'document.addEventListener("pointermove", onMove);' in app_menu
    assert 'document.addEventListener("pointerup", onUp);' in app_menu
    assert 'document.addEventListener("pointercancel", onCancel);' in app_menu
    assert 'document.removeEventListener("pointermove", onMove);' in app_menu
    assert (
        "function updateSpiceMenuTargetDropTarget(state, targetId, clientX, clientY)"
        in app_menu
    )
    assert (
        "updateSpiceMenuTargetDropTarget(\n"
        "      state,\n"
        "      target.id,\n"
        "      event.clientX,\n"
        "      event.clientY," in app_menu
    )
    assert "function suppressNextSpiceMenuDragClick()" in app_menu
    assert 'document.addEventListener("click", onClick, true);' in app_menu
    assert "if (suppressClick) suppressNextSpiceMenuDragClick();" in app_menu
    assert "moveTargetToMenuTeamOptimisticUi(menuDropTeamId, target.id);" in app_menu
    assert "sourceTarget = targetById.get(target.id) || target;" in app_menu
    assert "function clearSpiceMenuTargetDrag()" in app_menu
    assert (
        'for (const ghost of document.querySelectorAll(".target-choice-drag-ghost"))'
        in app_menu
    )
    assert (
        'for (const choice of document.querySelectorAll(".target-choice--dragging"))'
        in app_menu
    )
    assert "function spiceMenuDesktopDropTargetFromPoint(clientX, clientY)" in app_menu
    assert "return lanesEl.contains(element);" in app_menu
    assert "openTargetTeam(target.id, { keepMenuOpen: true })" in app_menu
    assert "if (keepMenuOpen) await refreshTargets();" in app_lanes
    assert "border-style: dashed;" in target_drag_rule
    assert ".target-choice--dragging > *" in css
    assert ".swimlanes--menu-drop-ready" in css


def test_static_lane_team_drag_uses_menu_style_pointer_tracking():
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")
    app_menu = (STATIC_ROOT / "app.menu.js").read_text(encoding="utf-8")

    pointer_start = app_groups.index(
        'handle.addEventListener("pointerdown", (event) => {',
        app_groups.index("function wireLaneDrag"),
    )
    pointer_body = app_groups[
        pointer_start : app_groups.index(
            'handle.addEventListener("pointermove", (event) => {', pointer_start
        )
    ]

    assert "function wireSpiceMenuTargetPointerDocumentEvents(target)" in app_menu
    assert "function wireLaneDragPointerDocumentEvents()" in app_groups
    assert 'document.addEventListener("pointermove", onMove);' in app_groups
    assert 'document.addEventListener("pointerup", onUp);' in app_groups
    assert 'document.addEventListener("pointercancel", onCancel);' in app_groups
    assert 'document.removeEventListener("pointermove", onMove);' in app_groups
    assert 'document.removeEventListener("pointerup", onUp);' in app_groups
    assert 'document.removeEventListener("pointercancel", onCancel);' in app_groups
    assert "event.preventDefault();" in pointer_body
    assert "clearLaneDrag(laneDragState);" in pointer_body
    assert "startY: event.clientY," in pointer_body
    assert "pointerCleanup: null," in pointer_body
    assert "pointerCaptureFailed: false," in pointer_body
    assert "laneDragState.pointerCleanup = wireLaneDragPointerDocumentEvents();" in (
        pointer_body
    )
    assert "handle.setPointerCapture(event.pointerId);" in pointer_body
    assert "function updateLaneDragFromEvent(event)" in app_groups
    assert (
        "Math.abs(dx) < laneDragThresholdPx && Math.abs(dy) < laneDragThresholdPx"
        in app_groups
    )
    assert "function visibleLaneElementFromPoint(clientX, clientY)" in app_groups
    assert "document.elementFromPoint(clientX, clientY);" in app_groups
    assert "function suppressNextLaneDragClick()" in app_groups
    assert "if (dragging) suppressNextLaneDragClick();" in app_groups
    assert "state.handle.releasePointerCapture(state.pointerId);" in app_groups
