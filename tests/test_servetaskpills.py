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


def test_global_filter_pills_build_from_catalog_stems_and_private_channel():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")

    # The header inventory is data-driven: public project stems, then the private
    # agent channel, then every hidden stem the catalog knows about. Hidden stems
    # are marked from catalog.hiddenStems so their pill can collapse; the agent
    # channel is named as the single private (not hidden) exception.
    assert 'const taskFilterPrivateChannelStem = "agent";' in app_lanes
    assert "const hiddenStems = new Set(catalog.hiddenStems || []);" in app_lanes
    assert "...(catalog.approvedStems || [])," in app_lanes
    assert "...(catalog.hiddenStems || [])," in app_lanes
    assert "pills.push({ ...stem, hidden: hiddenStems.has(stemName) });" in app_lanes
    assert 'label === "oops"' in app_lanes
    assert "spice task oops" in app_lanes


def test_global_filter_pills_use_fill_not_extra_border_for_drain_scope():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    css = _serve_css_text()
    pill_rule = _between(css, ".filter-pill {", "}")
    implicit_rule = _between(css, ".filter-pill--implicit {", "}")
    saturated_rule = _between(css, ".filter-pill--saturated {", "}")
    active_rule = _between(css, ".filter-pill--active {", "}")
    assigned_rule = _between(css, ".filter-pill--assigned {", "}")
    dormant_rule = _between(css, ".filter-pill--dormant {", "}")
    idle_rule = _between(css, ".filter-pill--idle {", "}")

    assert "--good: #2f9e44;" in css
    assert "model.drainability.boundaryDissolved" in app_lanes
    assert 'classes.push("filter-pill--implicit");' in app_lanes
    assert (
        implicit_rule == ".filter-pill--implicit {\n"
        "  background: color-mix(in srgb, var(--good) 8%, transparent);\n"
    )
    # The saturated end of the ramp is the full ready-green.
    assert "border-color: var(--good);" in saturated_rule
    # Each mid-ramp tone mixes the ready green toward a floor carrying only the
    # theme's --muted lightness -- hue `none`, saturation zero -- so
    # active/assigned/dormant hold exactly 70/45/20% of the ready saturation in
    # both appearances. Mixing toward --muted itself made that share swing with
    # the theme, because HSL saturation is lightness-normalized and --muted is
    # far lighter than --good only in the dark override.
    assert "--filter-pill-floor: hsl(from var(--muted) none 0% l);" in pill_rule
    assert (
        "--filter-pill-tone: color-mix(in hsl, var(--good) 70%,"
        " var(--filter-pill-floor));" in active_rule
    )
    assert (
        "background: color-mix(in srgb, var(--filter-pill-tone) 8%, transparent);"
        in active_rule
    )
    assert "border-color: var(--filter-pill-tone);" in active_rule
    assert "color: var(--filter-pill-tone);" in active_rule
    assert (
        "--filter-pill-tone: color-mix(in hsl, var(--good) 45%,"
        " var(--filter-pill-floor));" in assigned_rule
    )
    assert (
        "--filter-pill-tone: color-mix(in hsl, var(--good) 20%,"
        " var(--filter-pill-floor));" in dormant_rule
    )
    assert "border-color: var(--filter-pill-tone);" in dormant_rule
    assert "color: var(--filter-pill-tone);" in dormant_rule
    assert "color: var(--muted);" in idle_rule


def test_global_filter_pills_reject_stale_inventory_resurrection():
    script = Path(__file__).with_name("fixtures") / "task_filter_inventory_reconcile.js"

    result = subprocess.run(
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.lane-store.js"),
            str(STATIC_ROOT / "app.render.js"),
            str(STATIC_ROOT / "app.lanes.js"),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0


def test_global_filter_pill_ready_active_unavailable_state_model():
    script = Path(__file__).with_name("fixtures") / "task_filter_pill_states.js"

    result = subprocess.run(
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.lane-store.js"),
            str(STATIC_ROOT / "app.lanes.js"),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0
