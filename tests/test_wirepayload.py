"""The generated serve wire schema is the one cross-language authority."""

from __future__ import annotations

import ast
import inspect
import subprocess
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
    typecheck,
    workroutes,
)
from spice.serve.payload import chrome, message, metric, wire
from spice.serve.worktree import inventory
from tests.test_wirefixtures import LIVE_BUS_FRAME_FIXTURES, valid_wire_payload

PROJECT_ROOT = Path(__file__).resolve().parents[1]

# Every envelope that carries lane chrome, named beside the deleted flat fields
# whose values now live only in the canonical facet record. This is the
# executable tombstone for the superseded superset shape: adding one of these
# names back to any carrier fails this census before a browser can gain a second
# authority.
LANE_CHROME_ENVELOPE_SUPERSESSIONS = {
    "LaneChromeSourcePayload": (
        "taskFilters",
        "taskFilterEntries",
        "effectiveTaskFilters",
        "laneFilterVersion",
        "taskFilterInventory",
        "privateTaskCount",
        "teamIdentity",
        "lifetime",
        "renewalIntent",
    ),
    "LanePayload": (
        "pendingInboxCount",
        "pendingInboxLabel",
        "pendingInboxKeys",
        "pendingInboxRevision",
        "pendingInboxVersion",
        "taskFilters",
        "taskFilterEntries",
        "effectiveTaskFilters",
        "laneFilterVersion",
        "teamIdentity",
        "lifetime",
        "renewalIntent",
        "taskFilterInventory",
        "targetWorktreeName",
        "targetBranch",
    ),
    "PendingLanePayload": (
        "pendingInboxCount",
        "pendingInboxLabel",
        "pendingInboxKeys",
        "pendingInboxRevision",
        "pendingInboxVersion",
    ),
    "WorkTreePayload": (
        "pendingCount",
        "privateTaskCount",
        "taskFilters",
        "taskFilterEntries",
        "effectiveTaskFilters",
        "laneFilterVersion",
        "teamIdentity",
        "lifetime",
        "renewalIntent",
        "taskFilterInventory",
        "pendingInboxCount",
        "pendingInboxLabel",
        "pendingInboxKeys",
        "pendingInboxRevision",
        "pendingInboxVersion",
        "lastAssistantAt",
        "pendingLabel",
        "targetWorktreeName",
        "targetBranch",
    ),
    "WorkTreeRoute": (
        "teamIdentity",
        "taskFilters",
        "effectiveTaskFilters",
        "taskFilterEntries",
        "laneFilterVersion",
        "lifetime",
        "laneName",
    ),
    "WorkTreeSendResult": (
        "pendingInboxCount",
        "pendingInboxLabel",
        "pendingInboxKeys",
        "pendingInboxRevision",
        "pendingInboxVersion",
        "renewalIntent",
    ),
}


def test_browser_payload_emitters_match_the_exact_schema_registry():
    modules = (
        agentapi,
        chrome,
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


def _checkjs_probe(tmp_path: Path, source: str) -> int:
    """Run the shipped checkJs lane over app.types.js plus one probe file."""
    tmp_path.mkdir(parents=True, exist_ok=True)
    (tmp_path / "app.types.js").write_text(wire.render_app_types_js(), encoding="utf-8")
    probe = tmp_path / "probe.js"
    probe.write_text(source, encoding="utf-8")
    argv = typecheck.serve_web_typecheck_argv(("app.types.js", "probe.js"))
    return subprocess.run(argv, cwd=tmp_path, capture_output=True).returncode


# The generated types describe both directions only if the lane's flags say so.
# PlanItem.status is a required string, so writing undefined into it is the
# smallest statement of "this frame is missing a field the server promised".
UNDEFINED_WIRE_FIELD_PROBE = """/** @type {PlanItem} */
const item = { step: "one", status: undefined };
"""
PRESENT_WIRE_FIELD_PROBE = """/** @type {PlanItem} */
const item = { step: "one", status: "ok" };
"""


def test_checkjs_lane_rejects_undefined_written_into_a_wire_required_field(tmp_path):
    assert _checkjs_probe(tmp_path / "ok", PRESENT_WIRE_FIELD_PROBE) == 0
    assert _checkjs_probe(tmp_path / "undef", UNDEFINED_WIRE_FIELD_PROBE) != 0


def _reaches_opaque_json(value_type: wire.WireType, seen: frozenset[str]) -> bool:
    """Whether a declared type bottoms out in the opaque json primitive.

    Aliases are followed the way validation follows them, so opacity reached one
    hop away -- a record of JsonValue wearing a name -- counts the same as the
    primitive written inline. Object references are not followed: an opaque field
    inside a referenced object is named by that object's own key, not by every
    field pointing at it. ``seen`` makes a self-naming alias terminate here.
    """
    if value_type.kind == "json":
        return True
    if value_type.kind == "reference":
        if value_type.name in seen or value_type.name not in wire.WIRE_ALIASES:
            return False
        return _reaches_opaque_json(
            wire.WIRE_ALIASES[value_type.name], seen | {value_type.name}
        )
    return any(_reaches_opaque_json(item, seen) for item in value_type.items)


def _opaque_json_fields() -> set[str]:
    """Every field the schema itself leaves opaque, as `Schema.field` keys."""
    return {
        f"{schema.name}.{field.name}"
        for schema in wire.WIRE_OBJECTS
        for field in schema.fields
        if _reaches_opaque_json(field.value_type, frozenset())
    }


def test_named_opaque_json_fields_have_the_exact_intentional_allowlist():
    # The dict pins what was intended, prose included. The set pins what the
    # schema actually does, so a field that becomes opaque without being named
    # fails here rather than reaching the browser as an undescribed hole.
    assert wire.OPAQUE_JSON_ALLOWLIST == {
        "AgentEnsurePayload.restartRefusal": "driver-specific launch refusal facts",
        "AgentStatusPayload.restartRefusal": "driver-specific launch refusal facts",
        "TeamConfigPayload.shellSettings": "user-defined team shell preferences",
    }
    assert _opaque_json_fields() == set(wire.OPAQUE_JSON_ALLOWLIST)


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
    assert {
        field.name for field in wire.WIRE_OBJECTS_BY_NAME["LaneChromePayload"].fields
    } == {"targetId", *wire.LANE_CHROME_FACET_AUTHORITIES}


def test_lane_chrome_value_fields_have_one_explicit_facet_home():
    expected = {
        "LaneChromeIdentity": {
            "displayName",
            "target",
            "driver",
            "thread",
            "launch",
            "actorId",
            "agentName",
        },
        "LaneChromeTeamConfig": {"teamIdentity"},
        "LaneChromePendingInbox": {"count", "label", "keys"},
        "LaneChromeTaskBoard": {
            "taskFilters",
            "taskFilterEntries",
            "effectiveTaskFilters",
            "taskFilterInventory",
            "privateTaskCount",
            "reviewPressure",
            "claimedTask",
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
        },
    }

    actual = {
        schema_name: {
            field.name for field in wire.WIRE_OBJECTS_BY_NAME[schema_name].fields
        }
        for schema_name in expected
    }
    field_homes = [
        (field, schema_name)
        for schema_name, fields in actual.items()
        for field in fields
    ]

    assert actual == expected
    assert len(field_homes) == len({field for field, _schema_name in field_homes})


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


def test_lane_chrome_identity_cannot_absorb_renewal_facts():
    identity = valid_wire_payload("LaneChromeIdentity")
    identity_references = {
        field.value_type.name
        for field in wire.WIRE_OBJECTS_BY_NAME["LaneChromeIdentity"].fields
        if field.value_type.kind == "reference"
    }
    assert identity_references == {
        "ServeTargetIdentity",
        "ServeAgentDriverIdentity",
        "ThreadIdentity",
        "ServeAgentLaunchIdentity",
    }
    assert {
        "ServeAgentIdentity",
        "ServeRenewalIdentity",
        "RenewalIntentPayload",
    }.isdisjoint(identity_references)

    for field in ("renewal", "renewalIntent"):
        with pytest.raises(SpiceError, match=f"undeclared fields: {field}"):
            wire.validate_wire_payload(
                "LaneChromeIdentity",
                {**identity, field: {}},
            )


def test_lane_chrome_task_board_rejects_lane_info_and_team_topology():
    task_board = valid_wire_payload("LaneChromeTaskBoard")
    for field in ("laneInfo", "members", "memberAgents", "teams"):
        with pytest.raises(SpiceError, match=f"undeclared fields: {field}"):
            wire.validate_wire_payload(
                "LaneChromeTaskBoard",
                {**task_board, field: []},
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
        "laneInfo",
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


def test_chrome_carrying_envelopes_keep_only_the_facts_they_still_own():
    carriers = {
        name
        for name, schema in wire.WIRE_OBJECTS_BY_NAME.items()
        for field in schema.fields
        if field.name == "chrome"
    }
    assert carriers == set(LANE_CHROME_ENVELOPE_SUPERSESSIONS)

    for name, superseded in LANE_CHROME_ENVELOPE_SUPERSESSIONS.items():
        payload = valid_wire_payload(name, chrome={"targetId": "target-fixture"})

        assert wire.validate_wire_payload(name, payload) == payload
        for field in superseded:
            with pytest.raises(SpiceError, match=f"undeclared fields: {field}"):
                wire.validate_wire_payload(name, {**payload, field: "lane-a"})


def test_lane_chrome_is_the_assembler_emitted_contract():
    emitter = wire.BROWSER_PAYLOAD_EMITTER_SCHEMAS[
        "payload.chrome.assemble_lane_chrome"
    ]
    assert emitter == "LaneChromePayload"

    rendered = wire.render_app_types_js()
    assert " * @typedef {Object} LaneChromePayload" in rendered
    assert " * @property {string} targetId" in rendered
    assert " * @property {LaneChromeIdentityFacet=} identity" in rendered
    assert " * @property {LaneChromeActivityFacet=} activity" in rendered


def _arms_reported(message: str, arms: tuple[str, ...]) -> list[str]:
    """The arms a union rejection actually measured the value against."""
    return [arm for arm in arms if f"as {arm}," in message]


TEAM_COMMAND_ARMS = ("TeamCommandApplied", "TeamCommandRefused")
LANE_CHROME_ARMS = tuple(wire.LANE_CHROME_FACET_SCHEMAS.values())
ROUTED_RESULT_ARMS = ("TaskDrainResult", "WorkTreeSendResult")


def test_union_rejection_names_the_one_arm_a_discriminant_narrows_to():
    snapshot = valid_wire_payload("TeamSnapshot")
    del snapshot["globalSettings"]
    applied = {"ok": True, "revision": 3, "differential": False, "snapshot": snapshot}

    with pytest.raises(SpiceError) as rejection:
        wire.validate_wire_payload("TeamCommandResponse", applied)

    message = str(rejection.value)
    assert _arms_reported(message, TEAM_COMMAND_ARMS) == ["TeamCommandApplied"]
    assert (
        "TeamCommandResponse.snapshot is missing required fields: globalSettings"
        in message
    )


def test_union_rejection_keeps_every_arm_that_shares_the_literal_value():
    facet = valid_wire_payload("LaneChromeTeamConfigFacet")
    del facet["value"]

    with pytest.raises(SpiceError) as rejection:
        wire.validate_wire_payload("LaneChromeFacet", facet)

    message = str(rejection.value)
    assert _arms_reported(message, LANE_CHROME_ARMS) == [
        "LaneChromeTeamConfigFacet",
        "LaneChromeRenewalFacet",
    ]
    assert "LaneChromeFacet is missing required fields: value" in message


def test_union_rejection_reports_every_arm_when_none_pins_a_literal():
    with pytest.raises(SpiceError) as rejection:
        wire.validate_wire_payload("RoutedResult", {"bogus": 1})

    message = str(rejection.value)
    assert _arms_reported(message, ROUTED_RESULT_ARMS) == list(ROUTED_RESULT_ARMS)
    assert "RoutedResult has undeclared fields: bogus" in message


def test_union_rejection_reports_every_arm_when_the_literal_matches_none():
    with pytest.raises(SpiceError) as rejection:
        wire.validate_wire_payload("LaneChromeFacet", {"authority": "nowhere"})

    message = str(rejection.value)
    assert _arms_reported(message, LANE_CHROME_ARMS) == list(LANE_CHROME_ARMS)


def test_union_rejection_draws_the_literal_an_otherwise_complete_value_missed():
    applied = valid_wire_payload("TeamCommandApplied")

    with pytest.raises(SpiceError) as rejection:
        wire.validate_wire_payload("TeamCommandResponse", {**applied, "ok": "maybe"})

    message = str(rejection.value)
    assert "as TeamCommandApplied, TeamCommandResponse.ok must equal True" in message


def test_union_acceptance_is_unchanged_by_the_rejection_detail():
    applied = valid_wire_payload("TeamCommandApplied")
    refused = valid_wire_payload("TeamCommandRefused")

    assert wire.validate_wire_payload("TeamCommandResponse", applied) == applied
    assert wire.validate_wire_payload("TeamCommandResponse", refused) == refused
