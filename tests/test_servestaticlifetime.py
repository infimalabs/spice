"""Static serve UI contracts: lifetime slider and lane-pane slider."""

from __future__ import annotations

import subprocess
from pathlib import Path

from spice.serve.web import STATIC_ROOT
from tests.test_servestatic import _assert_contains_all


def test_static_lifetime_slider_uses_steer_drive_drain_without_renew_send_flag():
    app = (STATIC_ROOT / "app.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_controls = (STATIC_ROOT / "app.controls.js").read_text(encoding="utf-8")

    _assert_contains_all(
        app,
        (
            'const agentLifetimeLabels = ["Steer", "Drive", "Drain"];',
            'Steer: "Manual filters only",',
            'Drive: "Auto-subscribe to projects this team creates or claims",',
            'Drain: "Boundary dissolved: see all assignable work",',
            "function agentLifetimeAutoManagesTasks(lifetime) {",
            'return lifetime === "Drive";',
            "function agentLifetimeUsesStoredTaskFilters(lifetime) {",
            'return lifetime === "Steer" || lifetime === "Drive";',
            "function agentLifetimeDissolvesTaskBoundary(lifetime) {",
            'return lifetime === "Drain";',
            "function agentLifetimeHelpText(lifetime) {",
        ),
    )
    _assert_contains_all(
        app_shell,
        (
            "data-lifetime-label>Drive</span>",
            "data-submit>Drive</button>",
            "const lifetime = target.lifetime || defaultAgentLifetime;",
            "serverLifetime: lifetime,",
            'pendingLifetimeCommit: "",',
            "pendingLifetimeConfigRevision: 0,",
            "pendingLifetimeRequestId: 0,",
            "lifetimeRequestId: 0,",
            "applyServerLaneLifetime(lane, config.lifetime, {",
            "configRevision: config.revision,",
        ),
    )
    assert "renewAgent" not in app_controls
    assert '"Renew"' not in app


def test_static_lifetime_slider_tracks_pending_state_in_controls():
    app_controls = (STATIC_ROOT / "app.controls.js").read_text(encoding="utf-8")

    _assert_contains_all(
        app_controls,
        (
            "host.lifetimeRequestId = Math.max",
            "host.pendingLifetimeCommit = lifetime;",
            "host.pendingLifetimeRequestId = host.lifetimeRequestId;",
            "host.serverLifetime = laneServerLifetime(host);",
            "function updateLaneTeamConfigForLane(lane, configPatch) {",
            'return Promise.reject(new Error("team config update requires team id"));',
            'teamCommandPayload("updateTeamConfig", {',
            "configPatch,",
            "function updateLaneLifetimeForLane(lane) {",
            "updateLaneTeamConfigForLane(host, { lifetime: requestedLifetime })",
            "function serverLifetimeSupersedesPending(host, options = {})",
            "if (options.supersedePending !== true) return false;",
            "function serverLifetimeSettlesPending(host, lifetime, options = {})",
            "if (host.pendingLifetimeCommit && lifetime !== host.pendingLifetimeCommit)",
            "serverLifetimeSettlesPending(host, lifetime, options)",
            'host.pendingLifetimeCommit = "";',
            "host.pendingLifetimeConfigRevision = 0;",
            "host.pendingLifetimeRequestId = 0;",
            "function laneLifetimeCommitMatches(host, lifetime, options = {})",
            "function rollbackLaneLifetimeCommit(",
            'serverLifetime = "",',
            "options = {},",
            "const lifetimeHelp = agentLifetimeHelpText(lifetime);",
            "lane.lifetimeRangeEl.title = lifetimeHelp;",
            '"Task subscription policy: " + lifetimeHelp',
            "lane.lifetimeLabelEl.title = lifetimeHelp;",
            'lane.submitEl.title = "Send with " + lifetime + ": " + lifetimeHelp;',
        ),
    )
    assert app_controls.count("pendingLifetimeCommit") >= 4
    assert "function updateEmptyTeamLifetimeForLane" not in app_controls


def test_static_lifetime_slider_syncs_server_state_sources():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_groups = (STATIC_ROOT / "app.groups.js").read_text(encoding="utf-8")
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")
    app_controls = (STATIC_ROOT / "app.controls.js").read_text(encoding="utf-8")
    app_panes = (STATIC_ROOT / "app.panes.js").read_text(encoding="utf-8")

    _assert_contains_all(
        app_controls,
        (
            "function updateLaneTeamConfigForLane(lane, configPatch) {",
            'teamCommandPayload("updateTeamConfig", {',
            "teamId: host.teamId,",
            "configPatch,",
            "updateLaneTeamConfigForLane(host, { lifetime: requestedLifetime })",
            'rollbackLaneLifetimeCommit(host, requestedLifetime, "", { requestId });',
            'setLaneTransientStatus(host, "lifetime update failed");',
        ),
    )
    _assert_contains_all(
        app_panes,
        (
            "updateLaneTeamConfigForLane(host, {",
            "taskFilters: uniqueStringList(updateFilters(laneManualTaskFilters(host)))",
            'setLaneTransientStatus(host, "task filters update failed");',
            "function laneManualTaskFilters(lane) {",
            "manualTaskFilterProjects(member.taskFilterEntries)",
        ),
    )
    _assert_contains_all(
        app_lanes,
        (
            "applyServerLaneLifetime(lane, config.lifetime, {",
            "configRevision: config.revision,",
        ),
    )
    _assert_contains_all(
        app_render,
        (
            'payloadHasField(payload, "teamIdentity")',
            "teamIdentityConfigRevision(payload.teamIdentity)",
        ),
    )
    _assert_contains_all(
        app_groups,
        (
            "pendingLaneLifetimeStateForMembers(members, lifetimeStateByTargetId)",
            "laneLifetimeRuntimeState(lane)",
            "restoreLaneLifetimeRuntimeState(",
        ),
    )
    assert "pendingLifetimeRequestId" in app_controls
    assert "pendingLifetimeRequestId" not in app_stream
    assert "function updateTaskDrainForLane" not in app_stream
    assert "settleLaneLifetimeCommit(" not in app_stream
    assert "replaceTaskFilters" not in app_panes
    # Membership flows must never round-trip a filter list; pins are the only
    # filter payload the UI writes and they go through mutateLaneTaskFilters.
    assert "taskFilters:" not in app_groups
    assert "laneAssignedTaskFilters" not in app_groups


def test_lifetime_slider_pending_commit_ignores_stale_server_lifetimes():
    app_controls = STATIC_ROOT / "app.controls.js"
    script = Path(__file__).with_name("fixtures") / "lifetime_slider_pending.js"

    result = subprocess.run(
        ["node", str(script), str(app_controls)],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_lane_pane_slider_moves_panels_with_rail_direction():
    app_shell = STATIC_ROOT / "app.shell.js"
    script = Path(__file__).with_name("fixtures") / "lane_pane_direction.js"

    result = subprocess.run(
        ["node", str(script), str(app_shell)],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr
