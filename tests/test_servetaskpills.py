"""Serve task-pill presentation contracts."""

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


def test_global_filter_pills_show_waiting_tasks_with_distinct_system_style():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    css = _serve_css_text()
    waiting_rule_start = css.index(".filter-pill--waiting {")
    waiting_rule = css[waiting_rule_start : css.index("}", waiting_rule_start)]

    assert 'const taskFilterHeaderExtraStems = ["agent", "waiting", "oops"];' in (
        app_lanes
    )
    assert 'if (stem.name === "waiting") classes.push("filter-pill--waiting");' in (
        app_lanes
    )
    assert 'label === "waiting"' in app_lanes
    assert "spice task wake <handle>" in app_lanes
    assert 'label === "oops"' in app_lanes
    assert "spice task oops" in app_lanes
    assert "border-style: dotted;" in waiting_rule
    assert "color: var(--say-accent);" in waiting_rule


def test_global_filter_pills_use_fill_not_extra_border_for_drain_scope():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    css = _serve_css_text()
    implicit_rule = _between(css, ".filter-pill--implicit {", "}")
    drainable_rule = _between(css, ".filter-pill--drainable {", "}")

    assert "model.drainability.boundaryDissolved" in app_lanes
    assert 'classes.push("filter-pill--implicit");' in app_lanes
    assert (
        implicit_rule == ".filter-pill--implicit {\n"
        "  background: color-mix(in srgb, var(--good) 8%, transparent);\n"
    )
    assert "border-color: var(--good);" in drainable_rule


def test_global_filter_pills_reject_stale_inventory_resurrection():
    script = Path(__file__).with_name("fixtures") / "task_filter_inventory_reconcile.js"

    subprocess.run(
        ["node", str(script), str(STATIC_ROOT / "app.lanes.js")],
        check=True,
        capture_output=True,
        text=True,
    )
