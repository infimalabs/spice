"""Serve lane-store ownership and load-order contracts."""

import re
import subprocess
from pathlib import Path

from spice.serve.payload import wire
from spice.serve.web import STATIC_ROOT, render_index_html


def test_lane_store_constructs_real_target_authority():
    fixture = Path(__file__).with_name("fixtures") / "lane_store_targets.js"

    result = subprocess.run(
        ["node", str(fixture), str(STATIC_ROOT / "app.lane-store.js")],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_lane_store_reconciles_team_snapshots_as_declarative_transitions():
    fixture = Path(__file__).with_name("fixtures") / "lane_store_team_snapshots.js"

    result = subprocess.run(
        ["node", str(fixture), str(STATIC_ROOT / "app.lane-store.js")],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_lane_store_owns_group_topology_as_declarative_transitions():
    fixture = Path(__file__).with_name("fixtures") / "lane_store_group_topology.js"

    result = subprocess.run(
        ["node", str(fixture), str(STATIC_ROOT / "app.lane-store.js")],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_lane_store_reduces_lane_chrome_facets_as_declarative_transitions():
    fixture = Path(__file__).with_name("fixtures") / "lane_store_chrome.js"

    result = subprocess.run(
        ["node", str(fixture), str(STATIC_ROOT / "app.lane-store.js")],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_lane_chrome_facet_authorities_mirror_the_wire_contract():
    """The browser reducer refuses a facet whose authority is not the one the
    wire contract assigns it, so the two tables have to say the same thing."""
    store_source = (STATIC_ROOT / "app.lane-store.js").read_text(encoding="utf-8")
    declaration = store_source.split("const LANE_CHROME_FACET_AUTHORITIES")[1]
    mirrored = dict(
        re.findall(r"(\w+): \"([\w-]+)\"", declaration.split("});")[0]),
    )

    assert mirrored == wire.LANE_CHROME_FACET_AUTHORITIES


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


def test_lane_consumers_use_the_exact_store_registry_surface():
    store_source = (STATIC_ROOT / "app.lane-store.js").read_text(encoding="utf-8")
    production = "\n".join(
        path.read_text(encoding="utf-8")
        for path in sorted(STATIC_ROOT.glob("app*.js"))
        if path.name != "app.lane-store.js"
    )
    api_names = (
        "registerLane",
        "removeLane",
        "laneForId",
        "hasLane",
        "lanesSnapshot",
    )

    assert "#lanes = new Map();" in store_source
    assert all(f"{name}(" in store_source for name in api_names)
    calls = {name: production.count(f"laneStore.{name}(") for name in api_names}
    assert calls == {
        "registerLane": 2,
        "removeLane": 1,
        "laneForId": 31,
        "hasLane": 8,
        # The relative-time tick once walked every lane a second time to sync
        # fused status; it now collects the fused hosts during the first walk
        # and drives those, so that snapshot consumer is gone by design.
        "lanesSnapshot": 22,
    }


def test_target_inventory_is_owned_by_the_store_and_consumed_through_its_api():
    """The store module privately owns the ordered collection and id index and
    builds the single production instance; consumers reach target state through
    its public methods."""
    store_source = (STATIC_ROOT / "app.lane-store.js").read_text(encoding="utf-8")
    production = "\n".join(
        path.read_text(encoding="utf-8")
        for path in sorted(STATIC_ROOT.glob("app*.js"))
        if path.name != "app.lane-store.js"
    )

    assert "#targets = [];" in store_source
    assert "#targetById = new Map();" in store_source
    assert "const laneStore = new ServeLaneStore();" in store_source

    instance_owners = sorted(
        path.name
        for path in STATIC_ROOT.glob("app*.js")
        if "new ServeLaneStore()" in path.read_text(encoding="utf-8")
    )
    assert instance_owners == ["app.lane-store.js"]

    assert (
        "laneStore.replaceTargets(workTrees.map(targetInventoryRecord));" in production
    )
    assert "applyLaneChromePayload(target);" in production
    assert "laneStore.applyLaneChrome(chrome)" in production
    assert "laneStore.targetsSnapshot()" in production
    assert "laneStore.targetForId(" in production
    assert "laneStore.updateTarget(targetId" in production


def test_server_lane_chrome_has_one_browser_write_boundary():
    sources = {
        path.name: path.read_text(encoding="utf-8")
        for path in sorted(STATIC_ROOT.glob("app*.js"))
    }
    production = "\n".join(
        source for name, source in sources.items() if name != "app.lane-store.js"
    )

    # Every wire ingress enters through one adapter, and only that adapter calls
    # the reducer. Rendering is driven by the reducer's changed-facet transition.
    assert production.count("laneStore.applyLaneChrome(chrome)") == 1
    assert "function applyLaneChromePayload(payload)" in sources["app.render.js"]
    assert "renderLaneChromeTransition(change.transition);" in sources["app.render.js"]
    assert (
        "const changed = new Set(transition.changedFacets || []);"
        in (sources["app.render.js"])
    )
    assert "applyLaneChromePayload(target);" in sources["app.lanes.js"]
    assert sources["app.live-bus.js"].count("applyLaneChromePayload(payload)") == 2
    assert "applyLaneChromePayload(config);" in sources["app.stream.js"]
    assert "applyLaneChromePayload(result);" in sources["app.stream.js"]

    forbidden_assignments = (
        "taskFilters",
        "effectiveTaskFilters",
        "taskFilterEntries",
        "taskFilterInventory",
        "privateTaskCount",
        "renewalIntent",
        "backendPendingInboxCount",
        "backendPendingInboxKeys",
        "backendPendingInboxRevision",
        "backendPendingInboxVersion",
        "serverLifetime",
        "teamIdentity",
    )
    for field in forbidden_assignments:
        assert not re.search(
            rf"\b(?:lane|host|member|target|updated)\.{field}\s*=",
            production,
        ), field

    assert "function syncTaskFilterInventoryState(" not in production
    assert "function applyTaskDrainRouteConfig(" not in production
    assert "lane.renewalIntent =" not in production
    assert "lane.taskFilters =" not in production
    assert "updated.teamIdentity =" not in production
