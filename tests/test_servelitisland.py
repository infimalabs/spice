"""Serve UI Lit island prototype contracts."""

from __future__ import annotations

import subprocess
from pathlib import Path

from spice.serve.web import STATIC_ROOT

PROJECT_ROOT = Path(__file__).resolve().parents[1]
DOC_PATH = PROJECT_ROOT / "docs" / "studies" / "serve-ui-lit-island-prototype.md"


def test_static_metrics_lit_island_is_opt_in_and_paints_while_loading():
    script = Path(__file__).with_name("fixtures") / "lit_metrics_island.js"

    subprocess.run(
        ["node", str(script), str(STATIC_ROOT / "app.panes.js")],
        check=True,
    )


def test_static_metrics_lit_island_records_comparison():
    app_panes = (STATIC_ROOT / "app.panes.js").read_text(encoding="utf-8")
    app_lit = (STATIC_ROOT / "app.metrics-lit.js").read_text(encoding="utf-8")
    css = (STATIC_ROOT / "index.css").read_text(encoding="utf-8")
    doc = DOC_PATH.read_text(encoding="utf-8")

    assert 'const laneMetricsLitIslandModulePath = "/static/app.metrics-lit.js";' in (
        app_panes
    )
    assert "import(laneMetricsLitIslandModulePath)" in app_panes
    assert "function laneMetricsRenderModel(lane)" in app_panes
    assert "function laneMetricsLitIslandEnabled()" in app_panes
    assert "window).__spiceLitMetricsModuleLoader" in app_panes
    assert "class SpiceLaneMetricsElement extends LitElement" in app_lit
    assert "createRenderRoot()" in app_lit
    assert "return this;" in app_lit
    assert "renderLaneMetricsLitIsland(host, model)" in app_lit
    assert 'preserveAspectRatio="none"' in app_lit
    assert "https://cdn.jsdelivr.net/gh/lit/dist@3/core/lit-core.min.js" in app_lit
    assert ".lane-metrics-lit-island { display: contents; }" in css
    assert "## Code Size" in doc
    assert "## Test Clarity" in doc
    assert "## CSS And Event Friction" in doc
    assert "## Static Serving Compatibility" in doc
    assert "## Decision" in doc
    assert "?litMetrics=1" in doc
