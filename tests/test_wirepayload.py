"""The generated serve wire schema is the one cross-language authority."""

from __future__ import annotations

import ast
import inspect
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.serve import (
    agentapi,
    httpapi,
    livebus,
    livebusmutation,
    observer,
    submissions,
    workroutes,
)
from spice.serve.payload import message, metric, wire
from spice.serve.worktree import inventory
from tests.test_wirefixtures import LIVE_BUS_FRAME_FIXTURES, valid_wire_payload

PROJECT_ROOT = Path(__file__).resolve().parents[1]


def test_browser_payload_emitters_match_the_exact_schema_registry():
    modules = (
        agentapi,
        httpapi,
        observer,
        message,
        metric,
        submissions,
        workroutes,
        inventory,
    )
    actual = sorted(
        {
            call.args[0].value
            for module in modules
            for call in ast.walk(ast.parse(inspect.getsource(module)))
            if isinstance(call, ast.Call)
            and isinstance(call.func, ast.Name)
            and call.func.id == "validate_emitter_payload"
            and call.args
            and isinstance(call.args[0], ast.Constant)
            and isinstance(call.args[0].value, str)
        }
    )

    assert actual == sorted(wire.BROWSER_PAYLOAD_EMITTER_SCHEMAS)


def test_live_bus_outbound_discriminants_match_the_exact_frame_registry():
    modules = (livebus, livebusmutation)
    actual = sorted(
        {
            value.value
            for module in modules
            for node in ast.walk(ast.parse(inspect.getsource(module)))
            if isinstance(node, ast.Dict)
            for key, value in zip(node.keys, node.values, strict=True)
            if isinstance(key, ast.Constant)
            and key.value == "type"
            and isinstance(value, ast.Constant)
            and isinstance(value.value, str)
        }
    )

    assert actual == sorted(wire.LIVE_BUS_FRAME_SCHEMAS)


def test_live_bus_frame_registry_recursively_accepts_exact_named_representatives():
    actual = {
        frame_type: wire.validate_live_bus_frame(frame)
        for frame_type, frame in LIVE_BUS_FRAME_FIXTURES.items()
    }

    assert actual == LIVE_BUS_FRAME_FIXTURES
    assert sorted(actual) == sorted(wire.LIVE_BUS_FRAME_SCHEMAS)


def test_generated_app_types_are_the_exact_schema_render():
    actual = (PROJECT_ROOT / wire.APP_TYPES_GIT_PATH).read_text(encoding="utf-8")

    assert actual == wire.render_app_types_js()


def test_named_opaque_json_fields_have_the_exact_intentional_allowlist():
    assert wire.OPAQUE_JSON_ALLOWLIST == {
        "AgentEnsurePayload.restartRefusal": "driver-specific launch refusal facts",
        "TeamConfigPayload.shellSettings": "user-defined team shell preferences",
    }


def test_browser_only_frame_registry_names_the_append_variant():
    assert wire.BROWSER_ONLY_FRAME_SCHEMAS == {"lane.append": "LaneAppendFrame"}


def test_lane_chrome_contract_names_the_exact_independent_authorities():
    assert wire.LANE_CHROME_FACET_AUTHORITIES == {
        "identity": "target-registry",
        "teamConfig": "team-store",
        "pendingInbox": "inbox",
        "taskBoard": "task-board",
        "lifecycle": "lifecycle-reconciler",
        "renewal": "team-store",
        "activity": "transcript",
    }
    assert wire.LANE_CHROME_FACET_SCHEMAS == {
        "identity": "LaneChromeIdentityFacet",
        "teamConfig": "LaneChromeTeamConfigFacet",
        "pendingInbox": "LaneChromePendingInboxFacet",
        "taskBoard": "LaneChromeTaskBoardFacet",
        "lifecycle": "LaneChromeLifecycleFacet",
        "renewal": "LaneChromeRenewalFacet",
        "activity": "LaneChromeActivityFacet",
    }


def test_lane_chrome_value_fields_have_one_explicit_facet_home():
    expected = {
        "LaneChromeIdentity": {
            "displayName",
            "branch",
            "targetIdentity",
            "serveAgentIdentity",
        },
        "LaneChromeTeamConfig": {"teamIdentity"},
        "LaneChromePendingInbox": {"count", "label", "keys"},
        "LaneChromeTaskBoard": {
            "taskFilters",
            "taskFilterEntries",
            "effectiveTaskFilters",
            "taskFilterInventory",
            "laneInfo",
            "privateTaskCount",
        },
        "LaneChromeLifecycle": {
            "processStatus",
            "visualStatus",
            "bindingStatus",
            "rolloutStatus",
        },
        "LaneChromeRenewal": {"lifetime", "renewalIntent"},
        "LaneChromeActivity": {
            "lastAssistantAt",
            "latestActivityKind",
            "latestMessagePreview",
            "latestActivityPreview",
            "preview",
            "claimedTask",
        },
    }

    assert {
        schema_name: {
            field.name for field in wire.WIRE_OBJECTS_BY_NAME[schema_name].fields
        }
        for schema_name in expected
    } == expected


def test_lane_chrome_patch_accepts_independently_ordered_values_and_clears():
    facets = {
        name: valid_wire_payload(schema_name)
        for name, schema_name in wire.LANE_CHROME_FACET_SCHEMAS.items()
    }
    payload = valid_wire_payload(
        "LaneChromePayload",
        targetId="target-fixture",
        **facets,
    )

    assert wire.validate_wire_payload("LaneChromePayload", payload) == payload
    assert "revision" not in payload

    for name, schema_name in wire.LANE_CHROME_FACET_SCHEMAS.items():
        clear = valid_wire_payload(schema_name, value=None)
        patch = {"targetId": "target-fixture", name: clear}
        assert wire.validate_wire_payload("LaneChromePayload", patch) == patch


def test_lane_chrome_patch_rejects_cross_authority_and_global_ordering():
    identity = valid_wire_payload(
        "LaneChromeIdentityFacet",
        authority=wire.LANE_CHROME_FACET_AUTHORITIES["teamConfig"],
    )
    with pytest.raises(SpiceError, match="must equal 'target-registry'"):
        wire.validate_wire_payload(
            "LaneChromePayload",
            {"targetId": "target-fixture", "identity": identity},
        )

    with pytest.raises(SpiceError, match="undeclared fields: revision"):
        wire.validate_wire_payload(
            "LaneChromePayload",
            {"targetId": "target-fixture", "revision": 3},
        )


def test_lane_chrome_contract_rejects_authority_expansion_fields():
    assert wire.LANE_CHROME_EXCLUDED_FIELDS == {
        "messages",
        "ackContexts",
        "removedMessageKeys",
        "error",
        "teams",
        "members",
        "memberAgents",
        "composerState",
        "submission",
        "presentationState",
        "dom",
    }
    for field in wire.LANE_CHROME_EXCLUDED_FIELDS:
        with pytest.raises(SpiceError, match=f"undeclared fields: {field}"):
            wire.validate_wire_payload(
                "LaneChromePayload",
                {"targetId": "target-fixture", field: None},
            )


def test_lane_chrome_contract_is_not_yet_an_emitter_migration():
    assert "LaneChromePayload" not in wire.BROWSER_PAYLOAD_EMITTER_SCHEMAS.values()

    rendered = wire.render_app_types_js()
    assert " * @typedef {Object} LaneChromePayload" in rendered
    assert " * @property {string} targetId" in rendered
    assert " * @property {LaneChromeIdentityFacet=} identity" in rendered
    assert " * @property {LaneChromeActivityFacet=} activity" in rendered
