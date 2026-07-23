"""Reusable valid representatives for every server-to-browser wire frame."""

from __future__ import annotations

from typing import Any

from spice.serve.payload import wire


def valid_wire_payload(schema_name: str, **fields: Any) -> dict[str, Any]:
    schema = wire.WIRE_OBJECTS_BY_NAME[schema_name]
    payload = {
        field.name: _valid_wire_value(field.value_type)
        for field in schema.fields
        if not field.optional
    }
    payload.update(fields)
    return payload


def valid_lane_payload(**fields: Any) -> dict[str, Any]:
    if "messages" in fields:
        fields["messages"] = [
            valid_wire_payload("LaneMessage", **message)
            for message in fields["messages"]
        ]
    if "ackContexts" in fields:
        fields["ackContexts"] = [
            valid_wire_payload("AckContext", **context)
            for context in fields["ackContexts"]
        ]
    if "statusLine" in fields:
        fields["statusLine"] = valid_wire_payload(
            "StatusLine",
            **fields["statusLine"],
        )
    return valid_wire_payload("LanePayload", **fields)


def valid_metric_series_payload(**fields: Any) -> dict[str, Any]:
    return valid_wire_payload("MetricSeriesPayload", **fields)


def valid_live_bus_callback_payloads(**overrides: Any) -> dict[str, Any]:
    callbacks = {
        "work_trees_payload": lambda: valid_wire_payload("TargetsPayload"),
        "messages_payload": lambda _target, **_kwargs: valid_lane_payload(),
        "send_payload": lambda _target, _payload: (
            valid_wire_payload("WorkTreeSendResult"),
            None,
        ),
        "task_drain_payload": lambda _target, _payload: (
            valid_wire_payload("TaskDrainResult"),
            None,
        ),
        "team_snapshot_payload": lambda _since_revision: valid_wire_payload(
            "TeamSnapshotResponse"
        ),
        "team_command_payload": lambda _payload: (
            valid_wire_payload("TeamCommandResponse"),
            None,
        ),
        "metric_series_payload": lambda _query: valid_metric_series_payload(),
        "lane_metrics_payload": lambda _target: valid_wire_payload("LaneMetrics"),
    }
    callbacks.update(overrides)
    return callbacks


def _valid_wire_value(value_type: wire.WireType) -> Any:
    if value_type.kind == "reference":
        if value_type.name in wire.WIRE_ALIASES:
            return _valid_wire_value(wire.WIRE_ALIASES[value_type.name])
        return valid_wire_payload(value_type.name)
    if value_type.kind == "array":
        return []
    if value_type.kind == "record":
        return {}
    if value_type.kind == "union":
        return _valid_wire_value(value_type.items[0])
    if value_type.kind == "literal":
        return value_type.literal
    if value_type.kind == "string":
        return "fixture"
    if value_type.kind == "boolean":
        return True
    if value_type.kind == "integer":
        return 1
    if value_type.kind == "number":
        return 1.0
    if value_type.kind == "json":
        return {}
    raise AssertionError(f"unsupported wire fixture type: {value_type.kind}")


_TARGET_IDENTITY = valid_wire_payload("TargetIdentity")
_WORK_TREE = valid_wire_payload(
    "WorkTreePayload",
    targetIdentity=_TARGET_IDENTITY,
    serveAgentIdentity=valid_wire_payload("ServeAgentIdentity"),
)
_TARGETS_PAYLOAD = valid_wire_payload("TargetsPayload", workTrees=[_WORK_TREE])
_TEAM_MEMBER = valid_wire_payload(
    "TeamMemberPayload",
    agentFacts=valid_wire_payload(
        "TeamAgentIdentity",
        actorId="thread:fixture",
        targetId="target-fixture",
        threadId="fixture",
        renewalState="pending",
        renewalRevision=7,
        updatedAt=1.0,
    ),
)
_TEAM = valid_wire_payload("TeamPayload", members=[_TEAM_MEMBER])
_TEAM_SNAPSHOT = valid_wire_payload("TeamSnapshot", teams=[_TEAM])
_TEAM_RESPONSE = valid_wire_payload("TeamSnapshotResponse", snapshot=_TEAM_SNAPSHOT)
_LANE_PAYLOAD = valid_lane_payload()
_LANE_SUBSCRIPTION = valid_wire_payload(
    "LaneSubscriptionPayload",
    payload=_LANE_PAYLOAD,
)
_SERVER_TIMING = valid_wire_payload(
    "ServerTiming",
    targetResolveMs=1.0,
    totalMs=2.0,
)
_SUBMISSION = valid_wire_payload(
    "SubmissionLifecycle",
    stages={"accepted": valid_wire_payload("SubmissionStage")},
    durationsMs={"acceptedToReceived": 1.0},
)
_DIAGNOSTICS = valid_wire_payload(
    "LiveBusDiagnostics",
    frames={"lane.payload": valid_wire_payload("FrameTelemetry")},
)
_METRIC_PAYLOAD = valid_metric_series_payload(
    points=[valid_wire_payload("MetricSeriesPoint")]
)


LIVE_BUS_FRAME_FIXTURES = {
    "bus.error": valid_wire_payload("BusErrorFrame"),
    "bus.pong": valid_wire_payload("BusPongFrame", diagnostics=_DIAGNOSTICS),
    "targets.payload": valid_wire_payload(
        "TargetsFrame",
        payload=_TARGETS_PAYLOAD,
    ),
    "teams.payload": valid_wire_payload("TeamsFrame", payload=_TEAM_RESPONSE),
    "teams.commandResult": valid_wire_payload("TeamCommandFrame"),
    "lanes.payload": valid_wire_payload(
        "LanesFrame",
        lanes=[_LANE_SUBSCRIPTION],
    ),
    "lane.payload": valid_wire_payload(
        "LaneFrame",
        payload=_LANE_PAYLOAD,
        watchTiming=valid_wire_payload("WatchTiming"),
    ),
    "lane.configured": valid_wire_payload("LaneConfiguredFrame"),
    "lanes.dirty": valid_wire_payload(
        "LanesDirtyFrame",
        lanes=[valid_wire_payload("DirtyLane")],
    ),
    "lane.unsubscribed": valid_wire_payload("LaneUnsubscribedFrame"),
    "lane.sendResult": valid_wire_payload("LaneSendResultFrame"),
    "lane.sendTiming": valid_wire_payload(
        "LaneSendTimingFrame",
        serverTiming=_SERVER_TIMING,
    ),
    "lane.taskDrainResult": valid_wire_payload("LaneTaskDrainFrame"),
    "metrics.seriesResult": valid_wire_payload(
        "MetricSeriesFrame",
        result=_METRIC_PAYLOAD,
    ),
    "metrics.summaryResult": valid_wire_payload(
        "MetricsSummaryFrame",
        result=valid_wire_payload(
            "LaneMetrics",
            drained=3,
            acked=5,
            sends=7,
            toolCalls=11,
            uptimeSeconds=42,
            sparkline=[1, 2, 3],
        ),
    ),
    "lane.pending": valid_wire_payload(
        "LanePendingFrame",
        payload=valid_wire_payload("PendingLanePayload", pendingInboxCount=1),
    ),
    "lane.submission": valid_wire_payload(
        "LaneSubmissionFrame",
        submission=_SUBMISSION,
    ),
}
