"""Serve lane identity/status reconciliation fixtures."""

from __future__ import annotations

import subprocess
from pathlib import Path

from spice.serve.web import STATIC_ROOT


def test_lane_identity_refresh_clears_stale_thread_and_agent_fields():
    script = Path(__file__).with_name("fixtures") / "lane_identity_reconcile.js"

    subprocess.run(
        ["node", str(script), str(STATIC_ROOT / "app.render.js")],
        check=True,
    )
