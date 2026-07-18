"""The generated serve wire schema is the one cross-language authority."""

from __future__ import annotations

import ast
import inspect
from pathlib import Path

from spice.serve import agentapi, httpapi, livebus, observer, submissions, workroutes
from spice.serve.payload import message, metric, wire
from spice.serve.worktree import inventory
from tests.test_wirefixtures import LIVE_BUS_FRAME_FIXTURES

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
    tree = ast.parse(inspect.getsource(livebus))
    actual = sorted(
        {
            value.value
            for node in ast.walk(tree)
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
