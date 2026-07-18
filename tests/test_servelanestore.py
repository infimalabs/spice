"""Serve lane-store ownership and load-order contracts."""

import re
import subprocess
from pathlib import Path

from spice.serve.web import STATIC_ROOT, render_index_html


def test_lane_store_constructs_real_target_authority():
    fixture = Path(__file__).with_name("fixtures") / "lane_store_targets.js"

    result = subprocess.run(
        ["node", str(fixture), str(STATIC_ROOT / "app.lane-store.js")],
        check=True,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0


def test_lane_store_loads_before_every_production_consumer():
    html = render_index_html()
    store_index = html.index("/static/app.lane-store.js")

    for filename in (
        "app.js",
        "app.lanes.js",
        "app.menu.js",
        "app.shell.js",
        "app.stream.js",
    ):
        assert store_index < html.index(f"/static/{filename}")


def test_target_consumers_have_no_bare_collection_or_index_vestige():
    production = "\n".join(
        path.read_text(encoding="utf-8")
        for path in sorted(STATIC_ROOT.glob("app*.js"))
        if path.name != "app.lane-store.js"
    )

    assert "targetById" not in production
    assert re.search(r"\b(?:let|const|var)\s+targets\b", production) is None
    assert re.search(r"\btargets\s*=", production) is None
    for direct_read in (
        r"(?<![.\w])targets\.length",
        r"(?<![.\w])targets\.map\(",
        r"(?<![.\w])targets\.filter\(",
        r"(?<![.\w])targets\.slice\(",
        r"for \(const target of targets\)",
    ):
        assert re.search(direct_read, production) is None

    assert "laneStore.replaceTargets(payload.workTrees || []);" in production
    assert "laneStore.updateTarget(targetId" in production
    assert "laneStore.targetsSnapshot()" in production
    assert "laneStore.targetForId(" in production
