"""One executable schema for Python-to-browser serve payloads and JSDoc."""

from __future__ import annotations

import json
import math
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, TypeVar

from spice.errors import SpiceError

PayloadValue = TypeVar("PayloadValue")


@dataclass(frozen=True)
class WireType:
    kind: str
    name: str = ""
    items: tuple["WireType", ...] = ()
    literal: Any = None


@dataclass(frozen=True)
class WireField:
    name: str
    value_type: WireType
    optional: bool = False


@dataclass(frozen=True)
class WireObject:
    name: str
    fields: tuple[WireField, ...]


def _primitive(name: str) -> WireType:
    return WireType(name)


STRING = _primitive("string")
INTEGER = _primitive("integer")
NUMBER = _primitive("number")
BOOLEAN = _primitive("boolean")
JSON_VALUE = _primitive("json")


def _ref(name: str) -> WireType:
    return WireType("reference", name=name)


def _array(item: WireType) -> WireType:
    return WireType("array", items=(item,))


def _record(item: WireType) -> WireType:
    return WireType("record", items=(item,))


def _union(*items: WireType) -> WireType:
    return WireType("union", items=items)


def _literal(value: Any) -> WireType:
    return WireType("literal", literal=value)


def _field(name: str, value_type: WireType, *, optional: bool = False) -> WireField:
    return WireField(name, value_type, optional)


def _object(
    name: str,
    required: Mapping[str, WireType] | None = None,
    optional: Mapping[str, WireType] | None = None,
) -> WireObject:
    return WireObject(
        name,
        tuple(_field(key, value) for key, value in (required or {}).items())
        + tuple(
            _field(key, value, optional=True) for key, value in (optional or {}).items()
        ),
    )


STRINGS = _array(STRING)
NUMBERS = _array(NUMBER)
STRING_MAP = _record(STRING)

LANE_CHROME_FACET_AUTHORITIES = {
    "identity": "target-registry",
    "teamConfig": "team-store",
    "pendingInbox": "inbox",
    "taskBoard": "task-board",
    "lifecycle": "lifecycle-reconciler",
    "renewal": "team-store",
    "activity": "transcript",
}

LANE_CHROME_FACET_SCHEMAS = {
    "identity": "LaneChromeIdentityFacet",
    "teamConfig": "LaneChromeTeamConfigFacet",
    "pendingInbox": "LaneChromePendingInboxFacet",
    "taskBoard": "LaneChromeTaskBoardFacet",
    "lifecycle": "LaneChromeLifecycleFacet",
    "renewal": "LaneChromeRenewalFacet",
    "activity": "LaneChromeActivityFacet",
}

LANE_CHROME_EXCLUDED_FIELDS = frozenset(
    {
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
)


WIRE_OBJECTS = (
    _object(
        "MessageAttachment",
        {"name": STRING, "contentType": STRING, "size": INTEGER, "path": STRING},
        {"url": STRING},
    ),
    _object(
        "AckSegment",
        {"keys": STRINGS, "html": STRING, "disposition": STRING},
    ),
    _object("PlanItem", {"step": STRING, "status": STRING}),
    _object(
        "LaneMessage",
        {
            "key": STRING,
            "index": INTEGER,
            "timestamp": STRING,
            "kind": STRING,
            "source_kind": STRING,
            "text": STRING,
            "display_text": STRING,
            "display_html": STRING,
            "preamble_html": STRING,
            "preview": STRING,
            "image_only": BOOLEAN,
            "task_card_count": INTEGER,
            "ack_count": INTEGER,
            "ack_keys": STRINGS,
            "nack_count": INTEGER,
            "nack_keys": STRINGS,
            "ack_utterances": STRINGS,
            "ack_segments": _array(_ref("AckSegment")),
            "speech_utterances": STRINGS,
            "plan_items": _array(_ref("PlanItem")),
        },
    ),
    _object(
        "AckContext",
        {"key": STRING, "found": BOOLEAN},
        {
            "text": STRING,
            "html": STRING,
            "priority": STRING,
            "disposition": STRING,
            "attachments": _array(_ref("MessageAttachment")),
        },
    ),
    _object("DriverIdentity", {"name": STRING, "model": STRING, "effort": STRING}),
    _object("AgentIdentity", {"state": STRING}, {"name": STRING}),
    _object(
        "ThreadIdentity",
        {"state": STRING},
        {"threadId": STRING, "error": STRING},
    ),
    _object(
        "TargetIdentity",
        {
            "branch": STRING,
            "driver": _ref("DriverIdentity"),
            "agent": _ref("AgentIdentity"),
            "thread": _ref("ThreadIdentity"),
        },
        {"targetId": STRING, "worktreeName": STRING},
    ),
    _object(
        "ServeTargetIdentity",
        {"id": STRING, "worktreeName": STRING, "repoRoot": STRING, "branch": STRING},
    ),
    _object(
        "ServeAgentDriverIdentity",
        {"desired": STRING, "actual": STRING, "transcriptOwner": STRING},
    ),
    _object(
        "ServeAgentLaunchFacts",
        optional={
            "model": STRING,
            "effort": STRING,
            "serviceTier": STRING,
            "source": STRING,
        },
    ),
    _object(
        "ServeAgentLaunchIdentity",
        {
            "desired": _ref("ServeAgentLaunchFacts"),
            "actual": _ref("ServeAgentLaunchFacts"),
        },
    ),
    _object(
        "ServeRenewalIdentity",
        {
            "state": STRING,
            "teamIndex": _union(INTEGER, _literal(None)),
            "ancestorThreadId": STRING,
            "successorThreadId": STRING,
            "revision": INTEGER,
        },
    ),
    _object(
        "ServeAgentIdentity",
        {
            "driver": _ref("ServeAgentDriverIdentity"),
            "thread": _ref("ThreadIdentity"),
            "launch": _ref("ServeAgentLaunchIdentity"),
        },
        {
            "actorId": STRING,
            "target": _ref("ServeTargetIdentity"),
            "renewal": _ref("ServeRenewalIdentity"),
        },
    ),
    _object(
        "TeamIdentity",
        {"state": STRING},
        {"teamId": STRING, "teamRevision": INTEGER, "configRevision": INTEGER},
    ),
    _object("TaskFilterEntry", {"project": STRING, "source": STRING}),
    _object(
        "TeamAgentIdentity",
        optional={
            "actorId": STRING,
            "targetId": STRING,
            "threadId": STRING,
            "driverName": STRING,
            "driverModel": STRING,
            "driverEffort": STRING,
            "actualDriver": STRING,
            "actualModel": STRING,
            "actualEffort": STRING,
            "actualServiceTier": STRING,
            "desiredDriver": STRING,
            "desiredModel": STRING,
            "desiredEffort": STRING,
            "transcriptOwner": STRING,
            "renewalState": STRING,
            "renewalAncestorThreadId": STRING,
            "renewalSuccessorThreadId": STRING,
            "renewalRevision": INTEGER,
            "updatedAt": NUMBER,
        },
    ),
    _object(
        "RenewalIntentPayload",
        optional={
            "agentId": STRING,
            "requested": BOOLEAN,
            "state": STRING,
            "teamId": STRING,
            "ancestorThreadId": STRING,
            "successorAgentId": STRING,
            "successorThreadId": STRING,
            "teamSlot": _union(INTEGER, _literal(None)),
            "predecessorIdentity": _ref("TeamAgentIdentity"),
            "successorIdentity": _ref("TeamAgentIdentity"),
            "revision": INTEGER,
        },
    ),
    _object(
        "TaskFilterRecord",
        {"name": STRING, "primaryStem": STRING},
        {
            "openTaskCount": INTEGER,
            "readyTaskCount": INTEGER,
            "inFlightTaskCount": INTEGER,
            "blockedTaskCount": INTEGER,
            "deferredTaskCount": INTEGER,
        },
    ),
    _object(
        "TaskFilterStem",
        {"name": STRING, "filters": STRINGS},
        {
            "openTaskCount": INTEGER,
            "readyTaskCount": INTEGER,
            "inFlightTaskCount": INTEGER,
            "blockedTaskCount": INTEGER,
            "deferredTaskCount": INTEGER,
            "waitingTaskCount": INTEGER,
            "oopsTaskCount": INTEGER,
        },
    ),
    _object(
        "TaskFilterCatalog",
        {
            "approvedStems": STRINGS,
            "hiddenStems": STRINGS,
            "approvedPhases": STRINGS,
            "defaultFlow": STRINGS,
            "perStemFlows": _record(STRINGS),
            "hiddenProjectPrefix": STRING,
            "filterDelimiter": STRING,
            "segmentPattern": STRING,
            "segmentRuleLabel": STRING,
            "filterExamples": STRINGS,
        },
    ),
    _object(
        "TaskFilterInventory",
        optional={
            "revision": STRING,
            "filters": _array(_ref("TaskFilterRecord")),
            "primaryStems": _array(_ref("TaskFilterStem")),
            "openTaskCount": INTEGER,
            "catalog": _ref("TaskFilterCatalog"),
        },
    ),
    _object("LaneInfoRow", {"key": STRING, "value": STRING, "span": BOOLEAN}),
    _object(
        "LaneInfoMember", {"targetId": STRING, "rows": _array(_ref("LaneInfoRow"))}
    ),
    _object(
        "ReviewPressureItem",
        {
            "reviewedTask": STRING,
            "finding": STRING,
            "findingSeverity": STRING,
            "reviewer": STRING,
            "source": STRING,
            "followupCount": INTEGER,
            "reviewedAt": STRING,
        },
    ),
    _object(
        "ReviewPressure",
        {
            "count": INTEGER,
            "openFollowupCount": INTEGER,
            "items": _array(_ref("ReviewPressureItem")),
        },
    ),
    _object(
        "LaneInfo",
        {
            "summaryRows": _array(_ref("LaneInfoRow")),
            "members": _array(_ref("LaneInfoMember")),
        },
        {"reviewPressure": _ref("ReviewPressure")},
    ),
    _object(
        "ClaimedTask",
        optional={"handle": STRING, "phase": STRING, "title": STRING},
    ),
    _object(
        "StatusLine",
        optional={
            "bindingStatus": STRING,
            "bound": BOOLEAN,
            "bindingError": STRING,
            "rolloutStatus": STRING,
            "activityStatus": STRING,
            "lastAssistantAt": STRING,
            "latestActivityKind": STRING,
            "latestMessagePreview": STRING,
            "latestActivityPreview": STRING,
            "preview": STRING,
            "pendingInboxCount": INTEGER,
            "pendingInboxLabel": STRING,
            "pendingInboxKeys": STRINGS,
            "pendingInboxRevision": STRING,
            "pendingInboxVersion": INTEGER,
            "agentProcessStatus": STRING,
            "agentVisualStatus": STRING,
            "claimedTask": _ref("ClaimedTask"),
            "error": STRING,
        },
    ),
    _object(
        "LaneMetrics",
        optional={
            "drained": INTEGER,
            "acked": INTEGER,
            "sends": INTEGER,
            "toolCalls": INTEGER,
            "uptimeSeconds": NUMBER,
            "sparkline": NUMBERS,
        },
    ),
    _object(
        "AgentEnsurePayload",
        optional={
            "ok": BOOLEAN,
            "provider": STRING,
            "action": STRING,
            "status": STRING,
            "pid": INTEGER,
            "processGroupId": INTEGER,
            "threadId": STRING,
            "serviceTier": STRING,
            "readyAt": STRING,
            "startupFailure": STRING,
            "prompt": STRING,
            "logPath": STRING,
            "failure": STRING,
            "error": STRING,
            "trigger": STRING,
            "reason": STRING,
            "retryAfterSeconds": NUMBER,
            "taskHandle": STRING,
            "claimReleased": BOOLEAN,
            "restartRefusal": _ref("RestartRefusal"),
            "deadletteredInboxKeys": STRINGS,
            "deadletteredInboxKey": STRING,
            "deadletterRequeueCommand": STRING,
            "pendingInboxCount": INTEGER,
            "pendingInboxLabel": STRING,
            "pendingInboxKeys": STRINGS,
            "pendingInboxRevision": STRING,
            "pendingInboxVersion": INTEGER,
        },
    ),
    _object(
        "AgentStatusPayload",
        {
            "ok": BOOLEAN,
            "provider": STRING,
            "workTreeId": STRING,
            "status": STRING,
            "pid": INTEGER,
            "processGroupId": INTEGER,
            "threadId": STRING,
            "model": STRING,
            "effort": STRING,
            "serviceTier": STRING,
            "launchable": BOOLEAN,
            "bindingStatus": STRING,
            "bindingError": STRING,
        },
        {
            "restartRefusal": _ref("RestartRefusal"),
            "readyAt": STRING,
            "startupFailure": STRING,
        },
    ),
    _object(
        "LanePayload",
        {
            "messages": _array(_ref("LaneMessage")),
            "ackContexts": _array(_ref("AckContext")),
            "pendingInboxCount": INTEGER,
            "pendingInboxKeys": STRINGS,
            "pendingInboxRevision": STRING,
            "pendingInboxVersion": INTEGER,
            "targetIdentity": _ref("TargetIdentity"),
            "serveAgentIdentity": _ref("ServeAgentIdentity"),
            "taskFilters": STRINGS,
            "taskFilterEntries": _array(_ref("TaskFilterEntry")),
            "effectiveTaskFilters": STRINGS,
            "laneFilterVersion": STRING,
            "teamIdentity": _ref("TeamIdentity"),
            "lifetime": STRING,
            "renewalIntent": _ref("RenewalIntentPayload"),
            "taskFilterInventory": _ref("TaskFilterInventory"),
            "laneInfo": _ref("LaneInfo"),
            "agentEnsure": _ref("AgentEnsurePayload"),
            "statusLine": _ref("StatusLine"),
            "error": STRING,
        },
        {
            "pendingInboxLabel": STRING,
            "agentProcessStatus": STRING,
            "removedMessageKeys": STRINGS,
            "chrome": _ref("LaneChromePayload"),
        },
    ),
    _object(
        "PendingLanePayload",
        optional={
            "pendingInboxCount": INTEGER,
            "pendingInboxLabel": STRING,
            "pendingInboxKeys": STRINGS,
            "pendingInboxRevision": STRING,
            "pendingInboxVersion": INTEGER,
            "chrome": _ref("LaneChromePayload"),
        },
    ),
    _object(
        "LaneErrorPayload",
        {
            "error": STRING,
            "messages": _array(_ref("LaneMessage")),
            "statusLine": _ref("StatusLine"),
        },
    ),
    # The epoch names the generation of the authority that produced the facet,
    # and revisions restart within each one. Producers owe monotonicity, not a
    # particular spelling: the browser reducer compares epochs under natural
    # order -- digit runs as numbers, the text between them as text -- so a
    # decimal counter, an ISO instant, and a prefixed label all advance
    # correctly, including across the carry where plain string collation would
    # sort "10" below "9". tests/fixtures/lane_store_chrome.js holds the
    # conformance sweep that keeps the reducer honest about this.
    #
    # What producers here owe it in is a count of microseconds. Each generation
    # is the instant a store was written, and a store is only ever created after
    # every store it replaces, so the count rises across a restart and across a
    # store deleted and remade -- the one moment a revision counted within it can
    # restart lower. The task board is dated by two stores at once: its rows and
    # catalog advance the task-store generation minted by
    # spice.tasks.config.task_event_generation, while the revision it publishes is
    # this team's, counted in the team store. Neither generation carries it alone,
    # because either store can be remade while the other stands, so
    # payload.chrome.lane_chrome_generations joins both into one epoch -- the
    # board's generation first, the team's behind it. The comparator reads that as
    # the tuple it looks like: the leading generation decides and the one behind
    # it breaks ties, and since both only rise, so does the pair. Activity counts
    # the transcript instant it carries rather than spelling it out, because a
    # stamp written at one offset sorts ahead of a later stamp written at another.
    # teamConfig and renewal share the team store's generation, stamped into its
    # global settings when it is created, because both count inside that store and
    # both restart together when it is remade. Every generation here is counted in
    # microseconds so that a reader meeting more than one meets one kind of token
    # rather than one encoding per authority.
    # payload.chrome.lane_chrome_generation admits only a count, so a hash
    # identity cannot become an epoch -- it would arrive as an order the
    # reducer cannot fault and then mis-order silently behind it.
    _object(
        "LaneChromeFacetOrder",
        {"epoch": STRING, "revision": INTEGER},
    ),
    _object(
        "LaneChromeIdentity",
        {
            "displayName": STRING,
            "target": _ref("ServeTargetIdentity"),
            "driver": _ref("ServeAgentDriverIdentity"),
            "thread": _ref("ThreadIdentity"),
            "launch": _ref("ServeAgentLaunchIdentity"),
        },
        {"actorId": STRING, "agentName": STRING},
    ),
    _object(
        "LaneChromeTeamConfig",
        {"teamIdentity": _ref("TeamIdentity")},
    ),
    _object(
        "LaneChromePendingInbox",
        {"count": INTEGER, "label": STRING, "keys": STRINGS},
    ),
    _object(
        "LaneChromeTaskBoard",
        {
            "taskFilters": STRINGS,
            "taskFilterEntries": _array(_ref("TaskFilterEntry")),
            "effectiveTaskFilters": STRINGS,
            "taskFilterInventory": _ref("TaskFilterInventory"),
            "privateTaskCount": INTEGER,
        },
        {
            "reviewPressure": _ref("ReviewPressure"),
            "claimedTask": _ref("ClaimedTask"),
        },
    ),
    _object(
        "LaneChromeLifecycle",
        {"processStatus": STRING},
        optional={
            "visualStatus": STRING,
            "bindingStatus": STRING,
            "rolloutStatus": STRING,
        },
    ),
    _object(
        "LaneChromeRenewal",
        {"lifetime": STRING, "renewalIntent": _ref("RenewalIntentPayload")},
    ),
    _object(
        "LaneChromeActivity",
        {"lastAssistantAt": STRING},
        optional={
            "latestActivityKind": STRING,
            "latestMessagePreview": STRING,
            "latestActivityPreview": STRING,
            "preview": STRING,
        },
    ),
    _object(
        "LaneChromeIdentityFacet",
        {
            "authority": _literal(LANE_CHROME_FACET_AUTHORITIES["identity"]),
            "order": _ref("LaneChromeFacetOrder"),
            "value": _union(_ref("LaneChromeIdentity"), _literal(None)),
        },
    ),
    _object(
        "LaneChromeTeamConfigFacet",
        {
            "authority": _literal(LANE_CHROME_FACET_AUTHORITIES["teamConfig"]),
            "order": _ref("LaneChromeFacetOrder"),
            "value": _union(_ref("LaneChromeTeamConfig"), _literal(None)),
        },
    ),
    _object(
        "LaneChromePendingInboxFacet",
        {
            "authority": _literal(LANE_CHROME_FACET_AUTHORITIES["pendingInbox"]),
            "order": _ref("LaneChromeFacetOrder"),
            "value": _union(_ref("LaneChromePendingInbox"), _literal(None)),
        },
    ),
    _object(
        "LaneChromeTaskBoardFacet",
        {
            "authority": _literal(LANE_CHROME_FACET_AUTHORITIES["taskBoard"]),
            "order": _ref("LaneChromeFacetOrder"),
            "value": _union(_ref("LaneChromeTaskBoard"), _literal(None)),
        },
    ),
    _object(
        "LaneChromeLifecycleFacet",
        {
            "authority": _literal(LANE_CHROME_FACET_AUTHORITIES["lifecycle"]),
            "order": _ref("LaneChromeFacetOrder"),
            "value": _union(_ref("LaneChromeLifecycle"), _literal(None)),
        },
    ),
    _object(
        "LaneChromeRenewalFacet",
        {
            "authority": _literal(LANE_CHROME_FACET_AUTHORITIES["renewal"]),
            "order": _ref("LaneChromeFacetOrder"),
            "value": _union(_ref("LaneChromeRenewal"), _literal(None)),
        },
    ),
    _object(
        "LaneChromeActivityFacet",
        {
            "authority": _literal(LANE_CHROME_FACET_AUTHORITIES["activity"]),
            "order": _ref("LaneChromeFacetOrder"),
            "value": _union(_ref("LaneChromeActivity"), _literal(None)),
        },
    ),
    _object(
        "LaneChromePayload",
        {"targetId": STRING},
        {
            facet_name: _ref(schema_name)
            for facet_name, schema_name in LANE_CHROME_FACET_SCHEMAS.items()
        },
    ),
    _object(
        "LaneChromeSourcePayload",
        optional={
            "targetIdentity": _ref("TargetIdentity"),
            "serveAgentIdentity": _ref("ServeAgentIdentity"),
            "taskFilters": STRINGS,
            "taskFilterEntries": _array(_ref("TaskFilterEntry")),
            "effectiveTaskFilters": STRINGS,
            "laneFilterVersion": STRING,
            "taskFilterInventory": _ref("TaskFilterInventory"),
            "laneInfo": _ref("LaneInfo"),
            "privateTaskCount": INTEGER,
            "teamIdentity": _ref("TeamIdentity"),
            "lifetime": STRING,
            "renewalIntent": _ref("RenewalIntentPayload"),
            "statusLine": _ref("StatusLine"),
        },
    ),
    _object(
        "WorkTreePayload",
        {
            "id": STRING,
            "repoRoot": STRING,
            "displayName": STRING,
            "branch": STRING,
            "pendingCount": INTEGER,
            "privateTaskCount": INTEGER,
            "agentProcessStatus": STRING,
        },
        {
            "targetIdentity": _ref("TargetIdentity"),
            "serveAgentIdentity": _ref("ServeAgentIdentity"),
            "taskFilters": STRINGS,
            "taskFilterEntries": _array(_ref("TaskFilterEntry")),
            "effectiveTaskFilters": STRINGS,
            "laneFilterVersion": STRING,
            "teamIdentity": _ref("TeamIdentity"),
            "lifetime": STRING,
            "renewalIntent": _ref("RenewalIntentPayload"),
            "taskFilterInventory": _ref("TaskFilterInventory"),
            "laneInfo": _ref("LaneInfo"),
            "pendingInboxCount": INTEGER,
            "pendingInboxLabel": STRING,
            "pendingInboxKeys": STRINGS,
            "pendingInboxRevision": STRING,
            "pendingInboxVersion": INTEGER,
            "agentEnsure": _ref("AgentEnsurePayload"),
            "agentVisualStatus": STRING,
            "lastAssistantAt": STRING,
            "statusLine": _ref("StatusLine"),
            "chrome": _ref("LaneChromePayload"),
        },
    ),
    _object(
        "TargetsPayload",
        {
            "workTrees": _array(_ref("WorkTreePayload")),
            "defaultTargetId": STRING,
            "taskFilterInventory": _ref("TaskFilterInventory"),
        },
        {"observerErrors": STRINGS, "targetsDiscoveryErrors": STRINGS},
    ),
    _object("TeamGlobalSettings", {"fastMode": BOOLEAN}, {"observerMode": BOOLEAN}),
    _object(
        "TeamConfigPayload",
        {
            "lifetime": STRING,
            "taskFilters": STRINGS,
            "taskFilterEntries": _array(_ref("TaskFilterEntry")),
        },
        {
            "effectiveTaskFilters": STRINGS,
            "shellSettings": _ref("ShellSettings"),
            "revision": INTEGER,
        },
    ),
    _object("TeamSplitBack", {"available": BOOLEAN, "memberCount": INTEGER}),
    _object(
        "TeamMemberPayload",
        {"agentId": STRING, "renewalIntent": _ref("RenewalIntentPayload")},
        {"agentAliases": STRINGS, "agentFacts": _ref("TeamAgentIdentity")},
    ),
    _object(
        "TeamPayload",
        {
            "teamId": STRING,
            "revision": INTEGER,
            "config": _ref("TeamConfigPayload"),
            "members": _array(_ref("TeamMemberPayload")),
            "splitBack": _ref("TeamSplitBack"),
        },
        {"status": STRING, "order": INTEGER},
    ),
    _object(
        "TeamSnapshot",
        {
            "globalRevision": INTEGER,
            "globalSettings": _ref("TeamGlobalSettings"),
            "teams": _array(_ref("TeamPayload")),
        },
        {"teamCount": INTEGER, "removedTeamIds": STRINGS},
    ),
    _object(
        "TeamSnapshotResponse",
        {"revision": INTEGER, "changed": BOOLEAN},
        {
            "ok": BOOLEAN,
            "differential": BOOLEAN,
            "snapshot": _ref("TeamSnapshot"),
        },
    ),
    _object(
        "TeamCommandResponse",
        {"ok": BOOLEAN},
        {
            "revision": INTEGER,
            "differential": BOOLEAN,
            "snapshot": _ref("TeamSnapshot"),
            "error": STRING,
        },
    ),
    _object(
        "MetricSeriesSubject",
        {"agentIds": STRINGS},
        {"agentId": STRING, "teamId": STRING},
    ),
    _object(
        "MetricSeriesPoint",
        {"bucketStart": NUMBER, "value": NUMBER},
        {
            "messages": INTEGER,
            "sends": INTEGER,
            "acks": INTEGER,
            "claimed": INTEGER,
            "active": INTEGER,
            "completed": INTEGER,
            "drained": INTEGER,
            "agentId": STRING,
            "share": NUMBER,
            "work": INTEGER,
            "taskId": STRING,
            "handle": STRING,
            "title": STRING,
            "phase": STRING,
            "phaseIndex": INTEGER,
            "threadId": STRING,
            "teamId": STRING,
            "driver": STRING,
            "model": STRING,
            "effort": STRING,
            "startedAt": _union(NUMBER, _literal(None)),
            "endedAt": _union(NUMBER, _literal(None)),
            "wallSeconds": NUMBER,
            "inputTokens": INTEGER,
            "cachedInputTokens": INTEGER,
            "outputTokens": INTEGER,
            "reasoningOutputTokens": INTEGER,
            "totalTokens": INTEGER,
            "turns": INTEGER,
            "renewals": INTEGER,
            "sourceFiles": STRINGS,
            "partial": BOOLEAN,
            "partialMarkers": STRINGS,
        },
    ),
    _object(
        "MetricSeriesPayload",
        {
            "ok": BOOLEAN,
            "metric": STRING,
            "lens": STRING,
            "start": NUMBER,
            "effectiveStart": NUMBER,
            "end": NUMBER,
            "bucketSeconds": INTEGER,
            "subject": _ref("MetricSeriesSubject"),
            "points": _array(_ref("MetricSeriesPoint")),
        },
    ),
    _object(
        "SubmissionStage",
        {"at": STRING, "source": STRING, "evidence": STRING},
        {"sourceAt": STRING},
    ),
    _object(
        "SubmissionLifecycle",
        {
            "key": STRING,
            "stage": STRING,
            "disposition": STRING,
            "stages": _record(_ref("SubmissionStage")),
            "durationsMs": _record(NUMBER),
        },
    ),
    _object(
        "ServerTiming",
        optional={
            "mutationQueueMs": NUMBER,
            "targetResolveMs": NUMBER,
            "sendPayloadMs": NUMBER,
            "totalBeforeReplyMs": NUMBER,
            "replyLockWaitMs": NUMBER,
            "replyLockHoldMs": NUMBER,
            "replyWriteMs": NUMBER,
            "totalMs": NUMBER,
        },
    ),
    _object(
        "WorkTreeRoute",
        {
            "actor": STRING,
            "targetIdentity": _ref("TargetIdentity"),
            "serveAgentIdentity": _ref("ServeAgentIdentity"),
            "teamIdentity": _ref("TeamIdentity"),
            "memberAgents": STRINGS,
            "taskFilters": STRINGS,
            "effectiveTaskFilters": STRINGS,
            "taskFilterEntries": _array(_ref("TaskFilterEntry")),
            "laneFilterVersion": STRING,
            "lifetime": STRING,
            "chrome": _ref("LaneChromePayload"),
        },
    ),
    _object(
        "WorkTreeSendResult",
        {"ok": BOOLEAN},
        {
            "error": STRING,
            "key": STRING,
            "path": STRING,
            "text": STRING,
            "requestText": STRING,
            "requestControls": STRINGS,
            "requestPriority": STRING,
            "requestHtml": STRING,
            "noSay": BOOLEAN,
            "attachments": _array(_ref("MessageAttachment")),
            "agentEnsure": _ref("AgentEnsurePayload"),
            "pendingInboxCount": INTEGER,
            "pendingInboxLabel": STRING,
            "pendingInboxKeys": STRINGS,
            "pendingInboxRevision": STRING,
            "pendingInboxVersion": INTEGER,
            "renewalIntent": _ref("RenewalIntentPayload"),
            "route": _ref("WorkTreeRoute"),
            "serverTiming": _ref("ServerTiming"),
            "submission": _ref("SubmissionLifecycle"),
            "chrome": _ref("LaneChromePayload"),
        },
    ),
    _object(
        "TaskDrainResult",
        {"ok": BOOLEAN},
        {"route": _ref("WorkTreeRoute"), "error": STRING},
    ),
    _object(
        "WatchTiming",
        {
            "changeDetectedWallMs": NUMBER,
            "preSendWallMs": NUMBER,
            "detectToSendMs": NUMBER,
            "signatureMs": NUMBER,
            "payloadMs": NUMBER,
        },
    ),
    _object(
        "FrameTelemetry",
        {
            "count": INTEGER,
            "bytes": INTEGER,
            "sendLockWaitMsTotal": NUMBER,
            "sendLockWaitMsLast": NUMBER,
            "sendLockWaitMsMax": NUMBER,
            "sendLockHoldMsTotal": NUMBER,
            "sendLockHoldMsLast": NUMBER,
            "sendLockHoldMsMax": NUMBER,
        },
    ),
    _object("FrameTotals", {"count": INTEGER, "bytes": INTEGER}),
    _object(
        "LiveBusDiagnostics",
        {
            "clientId": STRING,
            "frames": _record(_ref("FrameTelemetry")),
            "totals": _ref("FrameTotals"),
        },
    ),
    _object(
        "LaneSubscriptionPayload",
        {
            "targetId": STRING,
            "payload": _ref("LaneWirePayload"),
            "subscriptionGeneration": STRING,
            "watcherActive": BOOLEAN,
            "watcherError": STRING,
        },
    ),
    _object(
        "LaneAppendPayload",
        {"messages": _array(_ref("LaneMessage"))},
        {"ackContexts": _array(_ref("AckContext")), "removedMessageKeys": STRINGS},
    ),
    _object(
        "LaneAppendFrame",
        {
            "type": _literal("lane.append"),
            "targetId": STRING,
            "payload": _ref("LaneAppendPayload"),
        },
    ),
    _object("DirtyLane", {"targetId": STRING, "subscriptionGeneration": STRING}),
    _object(
        "BusErrorFrame",
        {"type": _literal("bus.error"), "error": STRING},
        {"requestId": STRING},
    ),
    _object(
        "BusPongFrame",
        {"type": _literal("bus.pong"), "diagnostics": _ref("LiveBusDiagnostics")},
        {"requestId": STRING},
    ),
    _object(
        "TargetsFrame",
        {"type": _literal("targets.payload"), "payload": _ref("TargetsPayload")},
        {"requestId": STRING},
    ),
    _object(
        "TeamsFrame",
        {"type": _literal("teams.payload"), "payload": _ref("TeamSnapshotResponse")},
        {"requestId": STRING},
    ),
    _object(
        "TeamCommandFrame",
        {
            "type": _literal("teams.commandResult"),
            "result": _ref("TeamCommandResponse"),
        },
        {"requestId": STRING},
    ),
    _object(
        "LanesFrame",
        {
            "type": _literal("lanes.payload"),
            "lanes": _array(_ref("LaneSubscriptionPayload")),
        },
        {"requestId": STRING},
    ),
    _object(
        "LaneFrame",
        {"type": _literal("lane.payload"), "payload": _ref("LaneWirePayload")},
        {
            "requestId": STRING,
            "targetId": STRING,
            "source": STRING,
            "subscriptionGeneration": STRING,
            "watchTiming": _ref("WatchTiming"),
        },
    ),
    _object(
        "LaneConfiguredFrame",
        {"type": _literal("lane.configured")},
        {"requestId": STRING},
    ),
    _object(
        "LanesDirtyFrame",
        {"type": _literal("lanes.dirty"), "lanes": _array(_ref("DirtyLane"))},
    ),
    _object(
        "LaneUnsubscribedFrame",
        {"type": _literal("lane.unsubscribed")},
        {"requestId": STRING},
    ),
    _object(
        "LaneSendResultFrame",
        {"type": _literal("lane.sendResult"), "result": _ref("WorkTreeSendResult")},
        {"requestId": STRING},
    ),
    _object(
        "LaneSendTimingFrame",
        {
            "type": _literal("lane.sendTiming"),
            "requestId": STRING,
            "serverTiming": _ref("ServerTiming"),
        },
    ),
    _object(
        "LaneTaskDrainFrame",
        {"type": _literal("lane.taskDrainResult"), "result": _ref("TaskDrainResult")},
        {"requestId": STRING},
    ),
    _object(
        "MetricSeriesFrame",
        {
            "type": _literal("metrics.seriesResult"),
            "result": _ref("MetricSeriesPayload"),
        },
        {"requestId": STRING},
    ),
    _object(
        "MetricsSummaryFrame",
        {
            "type": _literal("metrics.summaryResult"),
            "result": _ref("LaneMetrics"),
        },
        {"requestId": STRING},
    ),
    _object(
        "LanePendingFrame",
        {
            "type": _literal("lane.pending"),
            "targetId": STRING,
            "source": STRING,
            "subscriptionGeneration": STRING,
            "payload": _ref("PendingLanePayload"),
        },
    ),
    _object(
        "LaneSubmissionFrame",
        {
            "type": _literal("lane.submission"),
            "targetId": STRING,
            "source": STRING,
            "subscriptionGeneration": STRING,
            "submission": _ref("SubmissionLifecycle"),
        },
    ),
    _object(
        "ServeBranding",
        optional={"name": STRING, "defaultLifetime": STRING, "version": STRING},
    ),
    _object(
        "ServeInitialGlobalSettings",
        optional={"fastMode": BOOLEAN, "observerMode": BOOLEAN},
    ),
)

WIRE_ALIASES = {
    "JsonValue": JSON_VALUE,
    "ShellSettings": _record(_ref("JsonValue")),
    "RestartRefusal": _record(_ref("JsonValue")),
    "LaneWirePayload": _union(_ref("LanePayload"), _ref("LaneErrorPayload")),
    "RoutedResult": _union(_ref("TaskDrainResult"), _ref("WorkTreeSendResult")),
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
    "agentapi.agent_status_payload": "AgentStatusPayload",
    "httpapi.team_command_response_payload": "TeamCommandResponse",
    "httpapi.team_snapshot_response_payload": "TeamSnapshotResponse",
    "observer.observer_agent_status_payload": "AgentStatusPayload",
    "observer.observer_messages_payload": "LanePayload",
    "observer.targets_payload": "TargetsPayload",
    "observer.team_snapshot_payload": "TeamSnapshotResponse",
    "payload.chrome.assemble_lane_chrome": "LaneChromePayload",
    "payload.message._messages_worktree_payload": "LanePayload",
    "payload.metric.metric_series_payload": "MetricSeriesPayload",
    "submissions.SubmissionLifecycle.event_payload": "SubmissionLifecycle",
    "workroutes.work_tree_send_accepted_response_payload": "WorkTreeSendResult",
    "workroutes.work_tree_send_response_payload": "WorkTreeSendResult",
    "workroutes.work_tree_task_drain_response_payload": "TaskDrainResult",
    "worktree.inventory.work_trees_payload": "TargetsPayload",
}

OPAQUE_JSON_ALLOWLIST = {
    "AgentEnsurePayload.restartRefusal": "driver-specific launch refusal facts",
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
    _validate(_ref(schema_name), payload, path=schema_name, descend_references=True)
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
        raise SpiceError(f"{path} does not match {_jsdoc_type(value_type)}")
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
    if value_type.kind in {"string", "boolean", "number"}:
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
