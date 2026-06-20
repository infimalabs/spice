"""Serve task-pill presentation contracts."""

from spice.serve.web import STATIC_ROOT


SERVE_CSS_FILES = ("index.css", "composer.css", "messages.css", "status-colors.css")


def _serve_css_text() -> str:
    return "\n".join(
        (STATIC_ROOT / filename).read_text(encoding="utf-8")
        for filename in SERVE_CSS_FILES
    )


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
