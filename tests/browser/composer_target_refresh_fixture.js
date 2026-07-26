"use strict";

function refreshClaimedTask(config) {
  return {
    handle: config.handle,
    phase: config.phase,
    title: "Task context for " + config.agentName,
  };
}

function refreshTargetPayload(lane, config, version) {
  return window.spicePayloads.targetPayload(
    {
      id: lane.targetId,
      threadId: config.teamId + "-thread",
      teamId: config.teamId,
      branch: config.branch,
      pendingRevision: version,
      taskBoardEpoch: version,
    },
    {
      targetIdentity: {
        agent: { state: "configured", name: config.agentName },
      },
      statusLine: {
        activityStatus: "active",
        agentProcessStatus: "running",
        agentVisualStatus: "running",
        claimedTask: refreshClaimedTask(config),
        latestActivityKind: "assistant",
      },
      chrome: {
        activity: window.spicePayloads.chromeFacet(
          "transcript",
          0,
          { lastAssistantAt: new Date().toISOString() },
          version,
        ),
      },
    },
  );
}

function refreshLanePayload(config, task, version) {
  return {
    messages: [],
    statusLine: {
      activityStatus: "active",
      agentProcessStatus: "running",
      agentVisualStatus: "running",
      claimedTask: task,
      latestActivityKind: "assistant",
    },
    chrome: {
      targetId: config.teamId,
      pendingInbox: window.spicePayloads.pendingInbox(version),
      activity: window.spicePayloads.chromeFacet(
        "transcript",
        0,
        { lastAssistantAt: new Date().toISOString() },
        version,
      ),
    },
  };
}

function refreshSnapshotLane(lane, config, original) {
  const textarea = lane.shardTextareas.get(lane.targetId);
  const quoteTextarea = lane.element.querySelector("textarea[data-quote-draft-id]");
  const attachments = lane.shardAttachments.get(lane.targetId) || [];
  const quotes = lane.quoteDrafts.get(lane.targetId) || [];
  return {
    attachmentIds: attachments.map((item) => item.id),
    claimedTaskLabel: laneClaimedTaskLabel(laneClaimedTask(lane)),
    draft: textarea ? textarea.value : "",
    placeholder: textarea ? textarea.placeholder : "",
    quoteDraftIds: quotes.map((item) => item.id),
    quoteDraftText: quotes.map((item) => item.text).join("|"),
    quoteTextareaSame: quoteTextarea === original.quoteTextarea,
    quoteTextareaValue: quoteTextarea ? quoteTextarea.value : "",
    taskPresent: Boolean(
      textarea && textarea.placeholder.includes(config.handle + ", " + config.phase),
    ),
    textareaSame: textarea === original.textarea,
  };
}

function refreshSeedDraft(lane, config, index) {
  const textarea = lane.shardTextareas.get(lane.targetId);
  textarea.value = config.draft;
  lane.shardAttachments.set(lane.targetId, [
    {
      id: "attachment-" + index,
      name: "diagram-" + index + ".png",
      contentType: "image/png",
      size: 4,
      dataUrl: "data:image/png;base64,aW1hZ2U=",
    },
  ]);
  lane.quoteDrafts.set(lane.targetId, [
    {
      id: "quote-" + index,
      messageKey: "message-" + index,
      href: "#message-" + index,
      timestamp: new Date().toISOString(),
      preview: "quoted context " + index,
      quoteText: "quoted context " + index,
      text: "follow-up " + index,
    },
  ]);
  renderComposerAttachmentStrips(lane);
  renderComposerQuoteBands(lane);
  return {
    quoteTextarea: lane.element.querySelector("textarea[data-quote-draft-id]"),
    textarea,
  };
}

function refreshCreateLanes(configs) {
  return configs.map((config) => {
    const lane = resolveIsolatedLane(config.teamId);
    lane.agentName = config.agentName;
    lane.branchName = config.branch;
    return lane;
  });
}

function refreshSnapshots(lanes, configs, originals) {
  return lanes.map((lane, index) =>
    refreshSnapshotLane(lane, configs[index], originals[index]),
  );
}

async function refreshTargetsThroughRequest(targets, eventOrder) {
  eventOrder.push("global targets.refresh start");
  const originalLiveBusRequest = liveBusRequest;
  // eslint-disable-next-line no-global-assign
  liveBusRequest = async (type, fields = {}) => {
    if (type !== "targets.refresh") return originalLiveBusRequest(type, fields);
    eventOrder.push("targets.refresh request");
    return {
      type: "targets.payload",
      payload: { workTrees: targets, observerErrors: [] },
    };
  };
  try {
    await refreshTargets();
  } finally {
    // eslint-disable-next-line no-global-assign
    liveBusRequest = originalLiveBusRequest;
  }
  eventOrder.push("targets.payload applied");
  await Promise.resolve();
}

async function refreshSubscribeClaims(lanes, configs, eventOrder) {
  eventOrder.push("lanes.subscribe recovery");
  applyLanesSubscribePayloads(
    lanes,
    lanes.map((lane, index) => ({
      targetId: lane.targetId,
      subscriptionGeneration: "composer-refresh-" + index,
      watcherActive: true,
      watcherError: "",
      payload: refreshLanePayload(
        configs[index],
        refreshClaimedTask(configs[index]),
        20 + index,
      ),
    })),
  );
  await Promise.resolve();
}

async function refreshReleaseClaim(lane, config, eventOrder) {
  eventOrder.push("authoritative claim release");
  applyLanesSubscribePayloads([lane], [
    {
      targetId: lane.targetId,
      subscriptionGeneration: "composer-refresh-release",
      watcherActive: true,
      watcherError: "",
      payload: refreshLanePayload(config, {}, 30),
    },
  ]);
  await Promise.resolve();
}

async function runTargetRefreshScenario(configs) {
  const lanes = refreshCreateLanes(configs);
  const targets = lanes.map((lane, index) =>
    refreshTargetPayload(lane, configs[index], 10 + index),
  );
  const eventOrder = ["initial targets.payload"];
  applyTargetsPayload({ workTrees: targets, observerErrors: [] });
  const originals = lanes.map((lane, index) =>
    refreshSeedDraft(lane, configs[index], index),
  );
  const before = refreshSnapshots(lanes, configs, originals);
  const refreshStartedAt = performance.now();
  await refreshTargetsThroughRequest(targets, eventOrder);
  const afterTargets = refreshSnapshots(lanes, configs, originals);
  await refreshSubscribeClaims(lanes, configs, eventOrder);
  const afterSubscribe = refreshSnapshots(lanes, configs, originals);
  const disappearanceDurationMs = afterTargets.concat(afterSubscribe).every(
    (snapshot) => snapshot.taskPresent,
  )
    ? 0
    : performance.now() - refreshStartedAt;
  await refreshReleaseClaim(lanes[0], configs[0], eventOrder);
  return {
    afterRelease: refreshSnapshots(lanes, configs, originals),
    afterSubscribe,
    afterTargets,
    before,
    disappearanceDurationMs,
    eventOrder,
  };
}

const PAGE_HELPERS = [
  refreshClaimedTask,
  refreshTargetPayload,
  refreshLanePayload,
  refreshSnapshotLane,
  refreshSeedDraft,
  refreshCreateLanes,
  refreshSnapshots,
  refreshTargetsThroughRequest,
  refreshSubscribeClaims,
  refreshReleaseClaim,
];
const installScript = PAGE_HELPERS.map((helper) => helper.toString()).join("\n");

module.exports = { installScript, runTargetRefreshScenario };
