"""Focused serve UI pending-count regression tests."""

from __future__ import annotations

import subprocess
from pathlib import Path

STATIC_ROOT = Path(__file__).resolve().parents[1] / "spice" / "serve" / "static"


def test_target_refresh_clears_stale_open_lane_pending_count():
    app_lanes = STATIC_ROOT / "app.lanes.js"
    script = (
        Path(__file__).with_name("fixtures") / "target_refresh_pending_reconcile.js"
    )

    subprocess.run(
        ["node", str(script), str(app_lanes)],
        check=True,
    )


def test_team_snapshot_renewal_reuses_existing_lane_without_empty_placeholder():
    app_render = STATIC_ROOT / "app.render.js"
    app_lanes = STATIC_ROOT / "app.lanes.js"
    script = Path(__file__).with_name("fixtures") / "team_snapshot_renewal_reconcile.js"

    subprocess.run(
        ["node", str(script), str(app_render), str(app_lanes)],
        check=True,
    )
