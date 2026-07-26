"""Every object the serve wire contract declares, in the order it renders.

Declarations only. The vocabulary they are written in lives in wiretypes.py,
and everything that reads them -- payload validation, the JSDoc render, the
frame and emitter maps -- lives in wire.py. Order is load-bearing here: the
generated app.types.js is a walk of WIRE_OBJECTS, so moving an entry moves a
line of browser-visible output.
"""

from __future__ import annotations

from spice.serve.payload.wiretypes import (
    BOOLEAN,
    INTEGER,
    NUMBER,
    NUMBERS,
    STRING,
    STRINGS,
    absent,
    array,
    literal,
    record,
    ref,
    union,
    wire_object,
)

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


# The three groups an ensure answer is assembled from. Splitting the answer in
# two means each arm has to say what the other one holds, and naming the groups
# once is what keeps that symmetric: a field added here lands on one arm and is
# denied on the other in the same edit. Stamps ride whichever answer came back,
# so both arms carry them. The process facts are what only a running agent has,
# and stay optional the way AgentStatusPayload leaves the same facts optional --
# the thread is what makes an answer a launch, and a reader that wants the pid
# still has to ask whether there is one. The excuses say why there is no agent,
# down to the inbox items parked because none could be started.
AGENT_ENSURE_STAMPS = {"action": STRING, "trigger": STRING, "taskHandle": STRING}
AGENT_ENSURE_PROCESS_FACTS = {
    "provider": STRING,
    "status": STRING,
    "pid": INTEGER,
    "processGroupId": INTEGER,
    "serviceTier": STRING,
    "readyAt": STRING,
    "startupFailure": STRING,
    "prompt": STRING,
    "logPath": STRING,
}
AGENT_ENSURE_EXCUSES = {
    "reason": STRING,
    "retryAfterSeconds": NUMBER,
    "claimReleased": BOOLEAN,
    "failure": STRING,
    "error": STRING,
    "restartRefusal": ref("RestartRefusal"),
    "deadletteredInboxKeys": STRINGS,
    "deadletteredInboxKey": STRING,
    "deadletterRequeueCommand": STRING,
}


WIRE_OBJECTS = (
    wire_object(
        "MessageAttachment",
        {"name": STRING, "contentType": STRING, "size": INTEGER, "path": STRING},
        {"url": STRING},
    ),
    wire_object(
        "AckSegment",
        {"keys": STRINGS, "html": STRING, "disposition": STRING},
    ),
    wire_object("PlanItem", {"step": STRING, "status": STRING}),
    wire_object(
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
            "ack_segments": array(ref("AckSegment")),
            "speech_utterances": STRINGS,
            "plan_items": array(ref("PlanItem")),
        },
    ),
    wire_object(
        "AckContext",
        {"key": STRING, "found": BOOLEAN},
        {
            "text": STRING,
            "html": STRING,
            "priority": STRING,
            "disposition": STRING,
            "attachments": array(ref("MessageAttachment")),
        },
    ),
    wire_object("DriverIdentity", {"name": STRING, "model": STRING, "effort": STRING}),
    wire_object("AgentIdentity", {"state": STRING}, {"name": STRING}),
    wire_object(
        "ThreadIdentity",
        {"state": STRING},
        {"threadId": STRING, "error": STRING},
    ),
    wire_object(
        "TargetIdentity",
        {
            "branch": STRING,
            "driver": ref("DriverIdentity"),
            "agent": ref("AgentIdentity"),
            "thread": ref("ThreadIdentity"),
        },
        {"targetId": STRING, "worktreeName": STRING},
    ),
    wire_object(
        "ServeTargetIdentity",
        {"id": STRING, "worktreeName": STRING, "repoRoot": STRING, "branch": STRING},
    ),
    wire_object(
        "ServeAgentDriverIdentity",
        {"desired": STRING, "actual": STRING, "transcriptOwner": STRING},
    ),
    wire_object(
        "ServeAgentLaunchFacts",
        optional={
            "model": STRING,
            "effort": STRING,
            "serviceTier": STRING,
            "source": STRING,
        },
    ),
    wire_object(
        "ServeAgentLaunchIdentity",
        {
            "desired": ref("ServeAgentLaunchFacts"),
            "actual": ref("ServeAgentLaunchFacts"),
        },
    ),
    wire_object(
        "ServeRenewalIdentity",
        {
            "state": STRING,
            "teamIndex": union(INTEGER, literal(None)),
            "ancestorThreadId": STRING,
            "successorThreadId": STRING,
            "revision": INTEGER,
        },
    ),
    wire_object(
        "ServeAgentIdentity",
        {
            "driver": ref("ServeAgentDriverIdentity"),
            "thread": ref("ThreadIdentity"),
            "launch": ref("ServeAgentLaunchIdentity"),
        },
        {
            "actorId": STRING,
            "target": ref("ServeTargetIdentity"),
            "renewal": ref("ServeRenewalIdentity"),
        },
    ),
    wire_object(
        "TeamIdentity",
        {"state": STRING},
        {"teamId": STRING, "teamRevision": INTEGER, "configRevision": INTEGER},
    ),
    wire_object("TaskFilterEntry", {"project": STRING, "source": STRING}),
    wire_object(
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
    wire_object(
        "RenewalIntentPayload",
        optional={
            "agentId": STRING,
            "requested": BOOLEAN,
            "state": STRING,
            "teamId": STRING,
            "ancestorThreadId": STRING,
            "successorAgentId": STRING,
            "successorThreadId": STRING,
            "teamSlot": union(INTEGER, literal(None)),
            "predecessorIdentity": ref("TeamAgentIdentity"),
            "successorIdentity": ref("TeamAgentIdentity"),
            "revision": INTEGER,
        },
    ),
    wire_object(
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
    wire_object(
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
    wire_object(
        "TaskFilterCatalog",
        {
            "approvedStems": STRINGS,
            "hiddenStems": STRINGS,
            "approvedPhases": STRINGS,
            "defaultFlow": STRINGS,
            "perStemFlows": record(STRINGS),
            "hiddenProjectPrefix": STRING,
            "filterDelimiter": STRING,
            "segmentPattern": STRING,
            "segmentRuleLabel": STRING,
            "filterExamples": STRINGS,
        },
    ),
    wire_object(
        "TaskFilterInventory",
        optional={
            "revision": STRING,
            "filters": array(ref("TaskFilterRecord")),
            "primaryStems": array(ref("TaskFilterStem")),
            "openTaskCount": INTEGER,
            "catalog": ref("TaskFilterCatalog"),
        },
    ),
    wire_object("LaneInfoRow", {"key": STRING, "value": STRING, "span": BOOLEAN}),
    wire_object(
        "LaneInfoMember", {"targetId": STRING, "rows": array(ref("LaneInfoRow"))}
    ),
    wire_object(
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
    wire_object(
        "ReviewPressure",
        {
            "count": INTEGER,
            "openFollowupCount": INTEGER,
            "items": array(ref("ReviewPressureItem")),
        },
    ),
    wire_object(
        "LaneInfo",
        {
            "summaryRows": array(ref("LaneInfoRow")),
            "members": array(ref("LaneInfoMember")),
        },
        {"reviewPressure": ref("ReviewPressure")},
    ),
    wire_object(
        "ClaimedTask",
        optional={"handle": STRING, "phase": STRING, "title": STRING},
    ),
    wire_object(
        "StatusLine",
        optional={
            "bindingStatus": STRING,
            "bound": BOOLEAN,
            "bindingError": STRING,
            "rolloutStatus": STRING,
            "activityStatus": STRING,
            "latestActivityKind": STRING,
            "latestMessagePreview": STRING,
            "latestActivityPreview": STRING,
            "preview": STRING,
            "agentProcessStatus": STRING,
            "agentVisualStatus": STRING,
            "claimedTask": ref("ClaimedTask"),
            "error": STRING,
        },
    ),
    wire_object(
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
    # An ensure either put an agent there or it did not. `ok` does not separate
    # the two: a skip answers ok true having started nothing (see
    # agentapi._available_work_skip), so the seam is the thread. A launch is the
    # only answer that can name one, and naming it is what entitles the answer to
    # the rest of the process facts -- the pid, the log to read, the tier it runs
    # under. Every other answer explains itself instead, with a reason, an error,
    # or the refusal the driver returned.
    wire_object(
        "AgentEnsureLaunched",
        {"ok": literal(True), "threadId": STRING},
        AGENT_ENSURE_STAMPS | AGENT_ENSURE_PROCESS_FACTS | absent(AGENT_ENSURE_EXCUSES),
    ),
    # No agent was started: the empty answer from a caller that never asked, a
    # skip that declined to, or a refusal that could not. Each says why instead,
    # and none of them has a process to name.
    wire_object(
        "AgentEnsureUnstarted",
        optional={"ok": BOOLEAN}
        | AGENT_ENSURE_STAMPS
        | AGENT_ENSURE_EXCUSES
        | absent(("threadId", *AGENT_ENSURE_PROCESS_FACTS)),
    ),
    wire_object(
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
            "restartRefusal": ref("RestartRefusal"),
            "readyAt": STRING,
            "startupFailure": STRING,
        },
    ),
    wire_object(
        "LanePayload",
        {
            "messages": array(ref("LaneMessage")),
            "ackContexts": array(ref("AckContext")),
            "targetIdentity": ref("TargetIdentity"),
            "serveAgentIdentity": ref("ServeAgentIdentity"),
            "laneInfo": ref("LaneInfo"),
            "agentEnsure": ref("AgentEnsurePayload"),
            "statusLine": ref("StatusLine"),
            "error": STRING,
            "chrome": ref("LaneChromePayload"),
        },
        {
            "agentProcessStatus": STRING,
            "removedMessageKeys": STRINGS,
        },
    ),
    wire_object(
        "PendingLanePayload",
        {"chrome": ref("LaneChromePayload")},
    ),
    wire_object(
        "LaneErrorPayload",
        {
            "error": STRING,
            "messages": array(ref("LaneMessage")),
            "statusLine": ref("StatusLine"),
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
    # it breaks ties, and since both only rise, so does the pair. An authority
    # with nothing to report still holds its place there, so a silent leading
    # generation is never read as the one behind it, and only when every authority
    # is silent is there no epoch at all. Activity counts the transcript instant
    # it carries rather than spelling it out, because a stamp written at one
    # offset sorts ahead of a later stamp written at another.
    # teamConfig and renewal share the team store's generation, stamped into its
    # global settings when it is created, because both count inside that store and
    # both restart together when it is remade. Every generation here is counted in
    # microseconds so that a reader meeting more than one meets one kind of token
    # rather than one encoding per authority.
    # The reducer never converts a digit run to a JavaScript number: it compares
    # normalized digit length and then lexical value, exactly matching Python's
    # unbounded integer order beyond Number.MAX_SAFE_INTEGER.
    # payload.chrome.lane_chrome_generation admits only a count, and every
    # generation in a join passes through it, so a hash identity cannot become
    # an epoch -- it would arrive as an order the reducer cannot fault and then
    # mis-order silently behind it.
    wire_object(
        "LaneChromeFacetOrder",
        {"epoch": STRING, "revision": INTEGER},
    ),
    wire_object(
        "LaneChromeIdentity",
        {
            "displayName": STRING,
            "target": ref("ServeTargetIdentity"),
            "driver": ref("ServeAgentDriverIdentity"),
            "thread": ref("ThreadIdentity"),
            "launch": ref("ServeAgentLaunchIdentity"),
        },
        {"actorId": STRING, "agentName": STRING},
    ),
    wire_object(
        "LaneChromeTeamConfig",
        {"teamIdentity": ref("TeamIdentity")},
    ),
    wire_object(
        "LaneChromePendingInbox",
        {"count": INTEGER, "label": STRING, "keys": STRINGS},
    ),
    wire_object(
        "LaneChromeTaskBoard",
        {
            "taskFilters": STRINGS,
            "taskFilterEntries": array(ref("TaskFilterEntry")),
            "effectiveTaskFilters": STRINGS,
            "taskFilterInventory": ref("TaskFilterInventory"),
            "privateTaskCount": INTEGER,
        },
        {
            "reviewPressure": ref("ReviewPressure"),
            "claimedTask": ref("ClaimedTask"),
        },
    ),
    wire_object(
        "LaneChromeLifecycle",
        {"processStatus": STRING},
        optional={
            "visualStatus": STRING,
            "bindingStatus": STRING,
            "rolloutStatus": STRING,
        },
    ),
    wire_object(
        "LaneChromeRenewal",
        {"lifetime": STRING, "renewalIntent": ref("RenewalIntentPayload")},
    ),
    wire_object(
        "LaneChromeActivity",
        {"lastAssistantAt": STRING},
        optional={
            "latestActivityKind": STRING,
            "latestMessagePreview": STRING,
            "latestActivityPreview": STRING,
            "preview": STRING,
        },
    ),
    wire_object(
        "LaneChromeIdentityFacet",
        {
            "authority": literal(LANE_CHROME_FACET_AUTHORITIES["identity"]),
            "order": ref("LaneChromeFacetOrder"),
            "value": union(ref("LaneChromeIdentity"), literal(None)),
        },
    ),
    wire_object(
        "LaneChromeTeamConfigFacet",
        {
            "authority": literal(LANE_CHROME_FACET_AUTHORITIES["teamConfig"]),
            "order": ref("LaneChromeFacetOrder"),
            "value": union(ref("LaneChromeTeamConfig"), literal(None)),
        },
    ),
    wire_object(
        "LaneChromePendingInboxFacet",
        {
            "authority": literal(LANE_CHROME_FACET_AUTHORITIES["pendingInbox"]),
            "order": ref("LaneChromeFacetOrder"),
            "value": union(ref("LaneChromePendingInbox"), literal(None)),
        },
    ),
    wire_object(
        "LaneChromeTaskBoardFacet",
        {
            "authority": literal(LANE_CHROME_FACET_AUTHORITIES["taskBoard"]),
            "order": ref("LaneChromeFacetOrder"),
            "value": union(ref("LaneChromeTaskBoard"), literal(None)),
        },
    ),
    wire_object(
        "LaneChromeLifecycleFacet",
        {
            "authority": literal(LANE_CHROME_FACET_AUTHORITIES["lifecycle"]),
            "order": ref("LaneChromeFacetOrder"),
            "value": union(ref("LaneChromeLifecycle"), literal(None)),
        },
    ),
    wire_object(
        "LaneChromeRenewalFacet",
        {
            "authority": literal(LANE_CHROME_FACET_AUTHORITIES["renewal"]),
            "order": ref("LaneChromeFacetOrder"),
            "value": union(ref("LaneChromeRenewal"), literal(None)),
        },
    ),
    wire_object(
        "LaneChromeActivityFacet",
        {
            "authority": literal(LANE_CHROME_FACET_AUTHORITIES["activity"]),
            "order": ref("LaneChromeFacetOrder"),
            "value": union(ref("LaneChromeActivity"), literal(None)),
        },
    ),
    wire_object(
        "LaneChromePayload",
        {"targetId": STRING},
        {
            facet_name: ref(schema_name)
            for facet_name, schema_name in LANE_CHROME_FACET_SCHEMAS.items()
        },
    ),
    wire_object(
        "LaneChromeSourcePayload",
        optional={
            "targetIdentity": ref("TargetIdentity"),
            "serveAgentIdentity": ref("ServeAgentIdentity"),
            "laneInfo": ref("LaneInfo"),
            "statusLine": ref("StatusLine"),
            "chrome": ref("LaneChromePayload"),
        },
    ),
    wire_object(
        "WorkTreePayload",
        {
            "id": STRING,
            "repoRoot": STRING,
            "displayName": STRING,
            "branch": STRING,
            "agentProcessStatus": STRING,
            "targetIdentity": ref("TargetIdentity"),
            "serveAgentIdentity": ref("ServeAgentIdentity"),
            "laneInfo": ref("LaneInfo"),
            "agentEnsure": ref("AgentEnsurePayload"),
            "agentVisualStatus": STRING,
            "statusLine": ref("StatusLine"),
            "chrome": ref("LaneChromePayload"),
        },
    ),
    wire_object(
        "TargetsPayload",
        {
            "workTrees": array(ref("WorkTreePayload")),
            "defaultTargetId": STRING,
        },
        {"observerErrors": STRINGS, "targetsDiscoveryErrors": STRINGS},
    ),
    wire_object("TeamGlobalSettings", {"fastMode": BOOLEAN}, {"observerMode": BOOLEAN}),
    wire_object(
        "TeamConfigPayload",
        {
            "lifetime": STRING,
            "taskFilters": STRINGS,
            "taskFilterEntries": array(ref("TaskFilterEntry")),
        },
        {
            "effectiveTaskFilters": STRINGS,
            "shellSettings": ref("ShellSettings"),
            "revision": INTEGER,
        },
    ),
    wire_object("TeamSplitBack", {"available": BOOLEAN, "memberCount": INTEGER}),
    wire_object(
        "TeamMemberPayload",
        {"agentId": STRING, "renewalIntent": ref("RenewalIntentPayload")},
        {"agentAliases": STRINGS, "agentFacts": ref("TeamAgentIdentity")},
    ),
    wire_object(
        "TeamPayload",
        {
            "teamId": STRING,
            "revision": INTEGER,
            "config": ref("TeamConfigPayload"),
            "members": array(ref("TeamMemberPayload")),
            "splitBack": ref("TeamSplitBack"),
        },
        {"status": STRING, "order": INTEGER},
    ),
    wire_object(
        "TeamSnapshot",
        {
            "globalRevision": INTEGER,
            "globalSettings": ref("TeamGlobalSettings"),
            "teams": array(ref("TeamPayload")),
        },
        {"teamCount": INTEGER, "removedTeamIds": STRINGS},
    ),
    wire_object(
        "TeamSnapshotResponse",
        {"revision": INTEGER, "changed": BOOLEAN},
        {
            "ok": BOOLEAN,
            "differential": BOOLEAN,
            "snapshot": ref("TeamSnapshot"),
        },
    ),
    wire_object(
        "TeamCommandApplied",
        {
            "ok": literal(True),
            "revision": INTEGER,
            "differential": BOOLEAN,
            "snapshot": ref("TeamSnapshot"),
        },
    ),
    wire_object(
        "TeamCommandRefused",
        {"ok": literal(False), "error": STRING},
    ),
    wire_object(
        "MetricSeriesSubject",
        {"agentIds": STRINGS},
        {"agentId": STRING, "teamId": STRING},
    ),
    wire_object(
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
            "startedAt": union(NUMBER, literal(None)),
            "endedAt": union(NUMBER, literal(None)),
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
    wire_object(
        "MetricSeriesPayload",
        {
            "ok": BOOLEAN,
            "metric": STRING,
            "lens": STRING,
            "start": NUMBER,
            "effectiveStart": NUMBER,
            "end": NUMBER,
            "bucketSeconds": INTEGER,
            "subject": ref("MetricSeriesSubject"),
            "points": array(ref("MetricSeriesPoint")),
        },
    ),
    wire_object(
        "SubmissionStage",
        {"at": STRING, "source": STRING, "evidence": STRING},
        {"sourceAt": STRING},
    ),
    wire_object(
        "SubmissionLifecycle",
        {
            "key": STRING,
            "stage": STRING,
            "disposition": STRING,
            "stages": record(ref("SubmissionStage")),
            "durationsMs": record(NUMBER),
        },
    ),
    wire_object(
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
    wire_object(
        "WorkTreeRoute",
        {
            "actor": STRING,
            "targetIdentity": ref("TargetIdentity"),
            "serveAgentIdentity": ref("ServeAgentIdentity"),
            "memberAgents": STRINGS,
            "chrome": ref("LaneChromePayload"),
        },
    ),
    wire_object(
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
            "attachments": array(ref("MessageAttachment")),
            "agentEnsure": ref("AgentEnsurePayload"),
            "route": ref("WorkTreeRoute"),
            "serverTiming": ref("ServerTiming"),
            "submission": ref("SubmissionLifecycle"),
            "chrome": ref("LaneChromePayload"),
        },
    ),
    wire_object(
        "TaskDrainResult",
        {"ok": BOOLEAN},
        {"route": ref("WorkTreeRoute"), "error": STRING},
    ),
    wire_object(
        "WatchTiming",
        {
            "changeDetectedWallMs": NUMBER,
            "preSendWallMs": NUMBER,
            "detectToSendMs": NUMBER,
            "signatureMs": NUMBER,
            "payloadMs": NUMBER,
        },
    ),
    wire_object(
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
    wire_object("FrameTotals", {"count": INTEGER, "bytes": INTEGER}),
    wire_object(
        "LiveBusDiagnostics",
        {
            "clientId": STRING,
            "frames": record(ref("FrameTelemetry")),
            "totals": ref("FrameTotals"),
        },
    ),
    wire_object(
        "LaneSubscriptionPayload",
        {
            "targetId": STRING,
            "payload": ref("LaneWirePayload"),
            "subscriptionGeneration": STRING,
            "watcherActive": BOOLEAN,
            "watcherError": STRING,
        },
    ),
    wire_object(
        "LaneAppendPayload",
        {"messages": array(ref("LaneMessage"))},
        {"ackContexts": array(ref("AckContext")), "removedMessageKeys": STRINGS},
    ),
    wire_object(
        "LaneAppendFrame",
        {
            "type": literal("lane.append"),
            "targetId": STRING,
            "payload": ref("LaneAppendPayload"),
        },
    ),
    wire_object("DirtyLane", {"targetId": STRING, "subscriptionGeneration": STRING}),
    wire_object(
        "BusErrorFrame",
        {"type": literal("bus.error"), "error": STRING},
        {"requestId": STRING},
    ),
    wire_object(
        "BusPongFrame",
        {"type": literal("bus.pong"), "diagnostics": ref("LiveBusDiagnostics")},
        {"requestId": STRING},
    ),
    wire_object(
        "TargetsFrame",
        {"type": literal("targets.payload"), "payload": ref("TargetsPayload")},
        {"requestId": STRING},
    ),
    wire_object(
        "TeamsFrame",
        {"type": literal("teams.payload"), "payload": ref("TeamSnapshotResponse")},
        {"requestId": STRING},
    ),
    wire_object(
        "TeamCommandFrame",
        {
            "type": literal("teams.commandResult"),
            "result": ref("TeamCommandResponse"),
        },
        {"requestId": STRING},
    ),
    wire_object(
        "LanesFrame",
        {
            "type": literal("lanes.payload"),
            "lanes": array(ref("LaneSubscriptionPayload")),
        },
        {"requestId": STRING},
    ),
    wire_object(
        "LaneFrame",
        {"type": literal("lane.payload"), "payload": ref("LaneWirePayload")},
        {
            "requestId": STRING,
            "targetId": STRING,
            "source": STRING,
            "subscriptionGeneration": STRING,
            "watchTiming": ref("WatchTiming"),
        },
    ),
    wire_object(
        "LaneConfiguredFrame",
        {"type": literal("lane.configured")},
        {"requestId": STRING},
    ),
    wire_object(
        "LanesDirtyFrame",
        {"type": literal("lanes.dirty"), "lanes": array(ref("DirtyLane"))},
    ),
    wire_object(
        "LaneUnsubscribedFrame",
        {"type": literal("lane.unsubscribed")},
        {"requestId": STRING},
    ),
    wire_object(
        "LaneSendResultFrame",
        {"type": literal("lane.sendResult"), "result": ref("WorkTreeSendResult")},
        {"requestId": STRING},
    ),
    wire_object(
        "LaneSendTimingFrame",
        {
            "type": literal("lane.sendTiming"),
            "requestId": STRING,
            "serverTiming": ref("ServerTiming"),
        },
    ),
    wire_object(
        "LaneTaskDrainFrame",
        {"type": literal("lane.taskDrainResult"), "result": ref("TaskDrainResult")},
        {"requestId": STRING},
    ),
    wire_object(
        "MetricSeriesFrame",
        {
            "type": literal("metrics.seriesResult"),
            "result": ref("MetricSeriesPayload"),
        },
        {"requestId": STRING},
    ),
    wire_object(
        "MetricsSummaryFrame",
        {
            "type": literal("metrics.summaryResult"),
            "result": ref("LaneMetrics"),
        },
        {"requestId": STRING},
    ),
    wire_object(
        "LanePendingFrame",
        {
            "type": literal("lane.pending"),
            "targetId": STRING,
            "source": STRING,
            "subscriptionGeneration": STRING,
            "payload": ref("PendingLanePayload"),
        },
    ),
    wire_object(
        "LaneSubmissionFrame",
        {
            "type": literal("lane.submission"),
            "targetId": STRING,
            "source": STRING,
            "subscriptionGeneration": STRING,
            "submission": ref("SubmissionLifecycle"),
        },
    ),
    wire_object(
        # Every field is built unconditionally in `spice/serve/web.py`: the name
        # and lifetime off a frozen ServeBranding whose fields are plain `str`,
        # the version off `runtime_version()`. Declaring them optional described
        # an absence the server has no way to send, and the browser paid for it
        # by reading `string | undefined` where only a string ever arrives.
        "ServeBranding",
        required={"name": STRING, "defaultLifetime": STRING, "version": STRING},
    ),
    wire_object(
        "ServeInitialGlobalSettings",
        required={"fastMode": BOOLEAN, "observerMode": BOOLEAN},
    ),
)
