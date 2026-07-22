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
    implicit_rule = _between(css, ".filter-pill--implicit {", "}")
    saturated_rule = _between(css, ".filter-pill--saturated {", "}")
    active_rule = _between(css, ".filter-pill--active {", "}")
    dormant_rule = _between(css, ".filter-pill--dormant {", "}")
    idle_rule = _between(css, ".filter-pill--idle {", "}")

    assert "model.drainability.boundaryDissolved" in app_lanes
    assert 'classes.push("filter-pill--implicit");' in app_lanes
    assert (
        implicit_rule == ".filter-pill--implicit {\n"
        "  background: color-mix(in srgb, var(--good) 8%, transparent);\n"
    )
    # The saturated end of the ramp is the full ready-green.
    assert "border-color: var(--good);" in saturated_rule
    # The mid-ramp tones desaturate the ready-green toward the idle gray at a
    # constant hue: each derives from --good via relative color (hue and
    # lightness held, only saturation stepped down) rather than the off-hue teal
    # accent, so the pill washes out instead of drifting green->cyan. Active work
    # holds ~75% saturation; dormant drops to ~25%; uncovered-but-ready idle is
    # the neutral endpoint.
    assert (
        "--filter-pill-tone: hsl(from var(--good) h calc(s * 0.75) l);" in active_rule
    )
    assert (
        "background: color-mix(in srgb, var(--filter-pill-tone) 8%, transparent);"
        in active_rule
    )
    assert "border-color: var(--filter-pill-tone);" in active_rule
    assert "color: var(--filter-pill-tone);" in active_rule
    assert (
        "--filter-pill-tone: hsl(from var(--good) h calc(s * 0.25) l);" in dormant_rule
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
