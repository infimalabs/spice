"""One executable schema for Python-to-browser serve payloads and JSDoc.

The declarations live in wireschema.py and the algebra they are written in
lives in wiretypes.py; what stays here is everything that reads them -- the
alias unions, the frame and emitter maps, payload validation, and the render
that keeps spice/serve/static/app.types.js in step with this contract.
"""

from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any, TypeVar

from spice.errors import SpiceError
from spice.serve.payload.wireschema import LANE_CHROME_FACET_SCHEMAS, WIRE_OBJECTS
from spice.serve.payload.wiretypes import (
    JSON_VALUE,
    WireObject,
    WireType,
    record,
    ref,
    union,
)

PayloadValue = TypeVar("PayloadValue")

WIRE_ALIASES = {
    "JsonValue": JSON_VALUE,
    "ShellSettings": record(ref("JsonValue")),
    "RestartRefusal": record(ref("JsonValue")),
    "LaneWirePayload": union(ref("LanePayload"), ref("LaneErrorPayload")),
    "RoutedResult": union(ref("TaskDrainResult"), ref("WorkTreeSendResult")),
    # What the browser reducer holds once it has read a facet out of a payload
    # but before it knows which one. Derived from the facet map so the union
    # cannot name a facet the payload does not carry, and so the reducer is
    # checked against every facet's common shape -- authority, order, value --
    # rather than against whichever one a reader happened to have in mind.
    "LaneChromeFacet": union(
        *(ref(name) for name in LANE_CHROME_FACET_SCHEMAS.values())
    ),
    # The two shapes a team command actually answers with: applied carries the
    # revision and snapshot it produced, refused carries the reason it did not.
    # Held as one object with every field optional, a reader could take the
    # revision off a refusal that never had one. The checkJs lane now runs with
    # strictNullChecks on, but that only makes such a read admit undefined
    # first: it cannot say the revision belongs to a command that was applied.
    # Split on the `ok` literal it is a type error until the reader narrows on
    # the outcome, which is what ties the field to the answer carrying it.
    "TeamCommandResponse": union(ref("TeamCommandApplied"), ref("TeamCommandRefused")),
    # The same tie for an ensure, split on whether an agent is now running
    # rather than on `ok` -- a skip answers ok true and starts nothing. Held as
    # one object, the thread id of a launch was readable off a refusal that
    # parked an inbox item instead, which is how a lane could bind itself to a
    # process no answer ever claimed to have started.
    "AgentEnsurePayload": union(
        ref("AgentEnsureLaunched"), ref("AgentEnsureUnstarted")
    ),
}

WIRE_OBJECTS_BY_NAME = {schema.name: schema for schema in WIRE_OBJECTS}

LIVE_BUS_FRAME_SCHEMAS = {
    "bus.error": "BusErrorFrame",
    "bus.pong": "BusPongFrame",
    "targets.payload": "TargetsFrame",
    "teams.payload": "TeamsFrame",
    "teams.commandResult": "TeamCommandFrame",
    "lanes.payload": "LanesFrame",
    "lane.payload": "LaneFrame",
    "lane.configured": "LaneConfiguredFrame",
    "lanes.dirty": "LanesDirtyFrame",
    "lane.unsubscribed": "LaneUnsubscribedFrame",
    "lane.sendResult": "LaneSendResultFrame",
    "lane.sendTiming": "LaneSendTimingFrame",
    "lane.taskDrainResult": "LaneTaskDrainFrame",
    "metrics.seriesResult": "MetricSeriesFrame",
    "metrics.summaryResult": "MetricsSummaryFrame",
    "lane.pending": "LanePendingFrame",
    "lane.submission": "LaneSubmissionFrame",
}

BROWSER_ONLY_FRAME_SCHEMAS = {
    "lane.append": "LaneAppendFrame",
}

BROWSER_PAYLOAD_EMITTER_SCHEMAS = {
    "agentapi.agent_ensure_failure_payload": "AgentEnsurePayload",
    "agentapi.agent_ensure_payload": "AgentEnsurePayload",
    "agentapi.agent_status_payload": "AgentStatusPayload",
    "httpapi.task_burndown_metrics_response_payload": "TaskBurndownMetricsResponse",
    "httpapi.task_distribution_metrics_response_payload": (
        "TaskDistributionMetricsResponse"
    ),
    "httpapi.team_command_response_payload": "TeamCommandResponse",
    "httpapi.team_historical_metrics_response_payload": "TeamHistoricalMetricsResponse",
    "httpapi.team_snapshot_response_payload": "TeamSnapshotResponse",
    "observer.observer_agent_status_payload": "AgentStatusPayload",
    "observer.observer_messages_payload": "LanePayload",
    "observer.targets_payload": "TargetsPayload",
    "observer.team_snapshot_payload": "TeamSnapshotResponse",
    "payload.chrome.assemble_lane_chrome": "LaneChromePayload",
    "payload.message._messages_worktree_payload": "LanePayload",
    "payload.metric.metric_series_payload": "MetricSeriesPayload",
    "submissions.SubmissionLifecycle.event_payload": "SubmissionLifecycle",
    "web.branding_payload": "ServeBranding",
    "web.initial_global_settings_payload": "ServeInitialGlobalSettings",
    "workroutes.work_tree_send_accepted_response_payload": "WorkTreeSendResult",
    "workroutes.work_tree_send_response_payload": "WorkTreeSendResult",
    "workroutes.work_tree_task_drain_response_payload": "TaskDrainResult",
    "worktree.inventory.work_trees_payload": "TargetsPayload",
}

# Every field whose interior the contract declines to describe, named so the
# generated types carry why. Both payloads that report a refusal name it here:
# they carry the same driver-supplied facts under the same type, and listing one
# alone left the other rendering the identical property undocumented.
OPAQUE_JSON_ALLOWLIST = {
    "AgentEnsureUnstarted.restartRefusal": "driver-specific launch refusal facts",
    "AgentStatusPayload.restartRefusal": "driver-specific launch refusal facts",
    "TeamConfigPayload.shellSettings": "user-defined team shell preferences",
}

APP_TYPES_GIT_PATH = Path("spice/serve/static/app.types.js")


def validate_emitter_payload(emitter: str, payload: PayloadValue) -> PayloadValue:
    try:
        schema_name = BROWSER_PAYLOAD_EMITTER_SCHEMAS[emitter]
    except KeyError as exc:
        raise SpiceError(
            f"browser payload emitter has no wire schema: {emitter}"
        ) from exc
    return validate_wire_payload(schema_name, payload)


def validate_live_bus_frame(payload: PayloadValue) -> PayloadValue:
    if not isinstance(payload, dict):
        raise SpiceError("live-bus frame must be an object")
    kind = str(payload.get("type") or "")
    try:
        schema = WIRE_OBJECTS_BY_NAME[LIVE_BUS_FRAME_SCHEMAS[kind]]
    except KeyError as exc:
        raise SpiceError(f"live-bus frame has no wire schema: {kind or '-'}") from exc
    _validate_object(schema, payload, path=schema.name, descend_references=True)
    return payload


def validate_wire_payload(schema_name: str, payload: PayloadValue) -> PayloadValue:
    _validate(ref(schema_name), payload, path=schema_name, descend_references=True)
    return payload


def _validate(
    value_type: WireType,
    value: Any,
    *,
    path: str,
    descend_references: bool,
) -> None:
    if value_type.kind in {"reference", "array", "record", "union"}:
        _validate_composite(
            value_type,
            value,
            path=path,
            descend_references=descend_references,
        )
        return
    _validate_scalar(value_type, value, path=path)


def _union_arm_object(candidate: WireType) -> WireObject | None:
    """The object a union arm names, or None when the arm does not name one.

    Resolves aliases by recursion, the way validation itself does, so a schema
    that ever names itself through one raises RecursionError here too instead of
    spinning in place.
    """
    if candidate.kind != "reference":
        return None
    if candidate.name in WIRE_ALIASES:
        return _union_arm_object(WIRE_ALIASES[candidate.name])
    return WIRE_OBJECTS_BY_NAME.get(candidate.name)


def _union_arm_literals(candidate: WireType) -> dict[str, Any] | None:
    """The fields a union arm pins to a literal, or None when it names no object."""
    schema = _union_arm_object(candidate)
    if schema is None:
        return None
    return {
        field.name: field.value_type.literal
        for field in schema.fields
        if field.value_type.kind == "literal"
    }


def _narrow_union_arms(value_type: WireType, value: Any) -> list[WireType]:
    """The arms a value could still be, by the literals every arm pins.

    Arms routinely pin a shared field to a literal: `ok` on a team command
    response, `authority` on a lane chrome facet. Where the value carries such a
    field, the arms whose literal disagrees are not what it meant, and their
    complaints bury the one that matters. Arms sharing a literal value stay
    together rather than being resolved further, so `authority` still cuts seven
    chrome facets down to the two the team store produces.

    Two shapes deliberately narrow to everything. Arms that pin nothing in
    common leave the intersection empty, and every arm passes vacuously. A value
    whose field matches no arm's literal has named a shape the union does not
    have, so every arm reports rather than none. What each one then says depends
    on how much else is wrong: an otherwise-complete value draws the literal that
    arm wanted, because an object reports the fields it is missing before it
    checks the value of any field it has.
    """
    literals = [_union_arm_literals(candidate) for candidate in value_type.items]
    pinned = [entry for entry in literals if entry is not None]
    if not isinstance(value, dict) or len(pinned) != len(literals):
        return list(value_type.items)
    shared = set.intersection(*(set(entry) for entry in pinned)) & set(value)
    narrowed = [
        candidate
        for candidate, entry in zip(value_type.items, pinned)
        if all(entry[field] == value[field] for field in shared)
    ]
    return narrowed or list(value_type.items)


def _union_rejection(
    value_type: WireType,
    value: Any,
    *,
    path: str,
    descend_references: bool,
) -> str:
    """Why each arm a value could plausibly have been refused it.

    Reached only once every arm has already refused, so re-validating the
    plausible ones costs nothing on the accepting path and buys back the field
    paths the union would otherwise discard.
    """
    reasons = []
    for candidate in _narrow_union_arms(value_type, value):
        try:
            _validate(
                candidate, value, path=path, descend_references=descend_references
            )
        except SpiceError as exc:
            reasons.append(f"as {_jsdoc_type(candidate)}, {exc}")
    return f"{path} does not match {_jsdoc_type(value_type)}: " + "; ".join(reasons)


def _validate_composite(
    value_type: WireType,
    value: Any,
    *,
    path: str,
    descend_references: bool,
) -> None:
    if value_type.kind == "reference":
        if not descend_references:
            if not isinstance(value, dict):
                raise SpiceError(f"{path} must be an object")
            return
        if value_type.name in WIRE_ALIASES:
            _validate(
                WIRE_ALIASES[value_type.name],
                value,
                path=path,
                descend_references=True,
            )
            return
        _validate_object(
            WIRE_OBJECTS_BY_NAME[value_type.name],
            value,
            path=path,
            descend_references=True,
        )
        return
    if value_type.kind == "array":
        if not isinstance(value, list):
            raise SpiceError(f"{path} must be an array")
        for index, item in enumerate(value):
            _validate(
                value_type.items[0],
                item,
                path=f"{path}[{index}]",
                descend_references=descend_references,
            )
        return
    if value_type.kind == "record":
        if not isinstance(value, dict) or any(
            not isinstance(key, str) for key in value
        ):
            raise SpiceError(f"{path} must be a string-keyed object")
        for key, item in value.items():
            _validate(
                value_type.items[0],
                item,
                path=f"{path}.{key}",
                descend_references=descend_references,
            )
        return
    if value_type.kind == "union":
        for candidate in value_type.items:
            try:
                _validate(
                    candidate,
                    value,
                    path=path,
                    descend_references=descend_references,
                )
                return
            except SpiceError:
                continue
        raise SpiceError(
            _union_rejection(
                value_type,
                value,
                path=path,
                descend_references=descend_references,
            )
        )
    raise AssertionError(f"unknown composite wire type: {value_type.kind}")


def _validate_scalar(value_type: WireType, value: Any, *, path: str) -> None:
    if value_type.kind == "literal":
        if value != value_type.literal:
            raise SpiceError(f"{path} must equal {value_type.literal!r}")
        return
    if value_type.kind == "string" and isinstance(value, str):
        return
    if value_type.kind == "boolean" and isinstance(value, bool):
        return
    if (
        value_type.kind == "integer"
        and isinstance(value, int)
        and not isinstance(value, bool)
    ):
        return
    if (
        value_type.kind == "number"
        and isinstance(value, (int, float))
        and not isinstance(value, bool)
        and math.isfinite(value)
    ):
        return
    if value_type.kind == "json":
        _validate_json(value, path=path)
        return
    raise SpiceError(f"{path} must be {_jsdoc_type(value_type)}")


def _validate_object(
    schema: WireObject,
    value: Any,
    *,
    path: str,
    descend_references: bool,
) -> None:
    if not isinstance(value, dict):
        raise SpiceError(f"{path} must be an object")
    fields = {field.name: field for field in schema.fields}
    unknown = sorted(set(value) - set(fields))
    if unknown:
        raise SpiceError(f"{path} has undeclared fields: {', '.join(unknown)}")
    missing = [
        field.name
        for field in schema.fields
        if not field.optional and field.name not in value
    ]
    if missing:
        raise SpiceError(f"{path} is missing required fields: {', '.join(missing)}")
    for key, item in value.items():
        _validate(
            fields[key].value_type,
            item,
            path=f"{path}.{key}",
            descend_references=descend_references,
        )


def _validate_json(value: Any, *, path: str) -> None:
    if value is None or isinstance(value, (str, bool)):
        return
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        if math.isfinite(value):
            return
        raise SpiceError(f"{path} must be finite JSON data")
    if isinstance(value, list):
        for index, item in enumerate(value):
            _validate_json(item, path=f"{path}[{index}]")
        return
    if isinstance(value, dict) and all(isinstance(key, str) for key in value):
        for key, item in value.items():
            _validate_json(item, path=f"{path}.{key}")
        return
    raise SpiceError(f"{path} must be JSON data")


def render_app_types_js() -> str:
    lines = [
        '"use strict";',
        "",
        "// Generated by `spice dev serve-web-types --write` from",
        "// spice.serve.payload.wire. Do not edit this file by hand.",
        "",
    ]
    for name, value_type in WIRE_ALIASES.items():
        lines.extend(
            ["/**", f" * @typedef {{{_jsdoc_type(value_type)}}} {name}", " */", ""]
        )
    for schema in WIRE_OBJECTS:
        lines.extend(["/**", f" * @typedef {{Object}} {schema.name}"])
        for field in schema.fields:
            suffix = "=" if field.optional else ""
            description = OPAQUE_JSON_ALLOWLIST.get(f"{schema.name}.{field.name}")
            detail = f" - {description}" if description else ""
            lines.append(
                f" * @property {{{_jsdoc_type(field.value_type)}{suffix}}} "
                f"{field.name}{detail}"
            )
        lines.extend([" */", ""])
    lines.extend(
        [
            "/** @type {ServeBranding} */",
            "var spiceServeBranding;",
            "",
            "/** @type {ServeInitialGlobalSettings} */",
            "var spiceServeInitialGlobalSettings;",
            "",
        ]
    )
    return "\n".join(lines)


def _jsdoc_type(value_type: WireType) -> str:
    if value_type.kind in {"string", "boolean", "number", "undefined"}:
        return value_type.kind
    if value_type.kind == "integer":
        return "number"
    if value_type.kind == "json":
        return "*"
    if value_type.kind == "reference":
        return value_type.name
    if value_type.kind == "array":
        return f"Array.<{_jsdoc_type(value_type.items[0])}>"
    if value_type.kind == "record":
        return f"Object.<string, {_jsdoc_type(value_type.items[0])}>"
    if value_type.kind == "union":
        return "(" + "|".join(_jsdoc_type(item) for item in value_type.items) + ")"
    if value_type.kind == "literal":
        return "null" if value_type.literal is None else json.dumps(value_type.literal)
    raise AssertionError(f"unknown wire type: {value_type.kind}")


def write_app_types_js(repo_root: Path) -> Path:
    path = repo_root / APP_TYPES_GIT_PATH
    path.write_text(render_app_types_js(), encoding="utf-8")
    return path


def check_app_types_js(repo_root: Path) -> None:
    path = repo_root / APP_TYPES_GIT_PATH
    expected = render_app_types_js()
    try:
        actual = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise SpiceError(f"could not read generated serve wire types: {exc}") from exc
    if actual != expected:
        raise SpiceError(
            "run `spice dev serve-web-types --write`; serve wire typedefs are stale"
        )
