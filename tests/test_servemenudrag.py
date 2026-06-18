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

    close_start = app_lanes.index("function closeSpiceMenu() {")
    close_body = app_lanes[
        close_start : app_lanes.index("\n}\n\nfunction renderSpiceMenu", close_start)
    ]
    render_start = app_lanes.index("function renderSpiceMenu() {")
    render_body = app_lanes[
        render_start : app_lanes.index(
            "\n}\n\nfunction positionSpiceMenu", render_start
        )
    ]
    pointer_start = app_lanes.index(
        'button.addEventListener("pointerdown", (event) => {',
        app_lanes.index("function wireSpiceMenuTargetDrag"),
    )
    pointer_body = app_lanes[
        pointer_start : app_lanes.index(
            'button.addEventListener("pointermove", (event) => {', pointer_start
        )
    ]
    target_drag_start = css.index(".target-choice--dragging {")
    target_drag_rule = css[target_drag_start : css.index("}", target_drag_start)]

    assert "let spiceMenuTargetDragState = null;" in app_js
    assert "clearSpiceMenuTargetDrag();" in close_body
    assert "clearSpiceMenuTargetDrag();" in render_body
    assert "clearSpiceMenuTargetDrag();" in pointer_body
    assert "spiceMenuTargetDragState = {" in pointer_body
    assert "function clearSpiceMenuTargetDrag()" in app_lanes
    assert (
        'for (const ghost of document.querySelectorAll(".target-choice-drag-ghost"))'
        in app_lanes
    )
    assert (
        'for (const choice of document.querySelectorAll(".target-choice--dragging"))'
        in app_lanes
    )
    assert "function spiceMenuDesktopDropTargetFromPoint(clientX, clientY)" in app_lanes
    assert "return lanesEl.contains(element);" in app_lanes
    assert "openTargetTeam(target.id, { keepMenuOpen: true })" in app_lanes
    assert "border-style: dashed;" in target_drag_rule
    assert ".target-choice--dragging > *" in css
    assert ".swimlanes--menu-drop-ready" in css
