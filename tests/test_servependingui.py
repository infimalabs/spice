"""Focused serve UI pending-count regression tests."""

from __future__ import annotations

import subprocess
from pathlib import Path

STATIC_ROOT = Path(__file__).resolve().parents[1] / "spice" / "serve" / "static"


def test_target_refresh_updates_lane_chrome_without_replacing_live_transcript():
    app_render = STATIC_ROOT / "app.render.js"
    app_lanes = STATIC_ROOT / "app.lanes.js"
    script = (
        Path(__file__).with_name("fixtures") / "target_refresh_pending_reconcile.js"
    )

    result = subprocess.run(
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.lane-store.js"),
            str(app_render),
            str(app_lanes),
        ],
        check=True,
    )
    assert result.returncode == 0


def test_team_snapshot_renewal_reuses_existing_lane_without_empty_placeholder():
    app_render = STATIC_ROOT / "app.render.js"
    app_lanes = STATIC_ROOT / "app.lanes.js"
    script = Path(__file__).with_name("fixtures") / "team_snapshot_renewal_reconcile.js"

    result = subprocess.run(
        [
            "node",
            str(script),
            str(STATIC_ROOT / "app.lane-store.js"),
            str(app_render),
            str(app_lanes),
        ],
        check=True,
    )
    assert result.returncode == 0
