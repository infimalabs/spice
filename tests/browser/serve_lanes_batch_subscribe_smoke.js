const { withServePage } = require("./serve_playwright_harness");

// Lane subscribes coalesce into ONE lanes.subscribe frame per microtask tick,
// and the batch payloads apply state-first so each fused host paints its
// merged interleaved stream in a single render pass. Guards the per-member
// arrival reshuffle the operator saw on load: N open lanes used to subscribe
// independently, and every arrival re-sorted the fused mosaic.

const BATCH_MEMBER_IDS = ["bsub-a", "bsub-b"];
const BATCH_MESSAGES_PER_MEMBER = 3;
const BATCH_SNAPSHOT_REVISION = 50;
const BATCH_SETTLE_MS = 80;

function batchTargetPayload(id, threadId) {
  return {
    id,
    name: id,
    branch: id,
    targetIdentity: {
      targetId: id,
      worktreeName: id,
      branch: id,
      driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
      agent: { state: "unconfigured" },
      thread: { state: "bound", threadId },
    },
    serveAgentIdentity: {
      actorId: "thread:" + threadId,
      target: { id },
      thread: { state: "bound", threadId },
    },
    teamIdentity: {
      state: "member",
      teamId: "team-batch",
      teamRevision: 1,
      configRevision: 1,
    },
    taskFilters: [],
    laneFilterVersion: "",
    lifetime: "Drive",
    pendingInboxCount: 0,
    pendingInboxKeys: [],
    pendingInboxRevision: "p-" + id,
    pendingInboxVersion: 1,
    statusLine: {
      pendingInboxCount: 0,
      pendingInboxKeys: [],
      pendingInboxRevision: "p-" + id,
      pendingInboxVersion: 1,
    },
  };
}

function batchTeam(ids, configRevision) {
  return {
    teamId: "team-batch",
    revision: configRevision,
    config: {
      revision: configRevision,
      lifetime: "Drive",
      taskFilters: [],
      taskFilterEntries: [],
    },
    splitBack: {},
    members: ids.map((id) => ({ agentId: "target:" + id })),
  };
}

// Interleaved indexes across the two members (a: 0/2/4, b: 1/3/5) so the
// merged host stream only reads correctly when all members land together.
function batchLaneMessages(id, memberOffset, perMember) {
  return Array.from({ length: perMember }, (unused, n) => {
    const index = memberOffset + n * 2;
    return {
      ack_count: 0,
      ack_keys: [],
      index,
      key: "m-" + id + "-" + index,
      kind: "assistant",
      threadId: id + "-th",
      timestamp: new Date(2027, 0, 1, 0, index, 0).toISOString(),
      display_html: "batch line " + index,
      display_text: "batch line " + index,
      text: "batch line " + index,
    };
  });
}

function batchFreshMessage(id) {
  return {
    ack_count: 0,
    ack_keys: [],
    index: 99,
    key: "m-fresh",
    kind: "assistant",
    threadId: id + "-th",
    timestamp: new Date(2027, 0, 2, 0, 0, 0).toISOString(),
    display_html: "fresh sibling line",
    display_text: "fresh sibling line",
    text: "fresh sibling line",
  };
}

// Real lane payloads always carry the pending-inbox identity quad; the lane
// chrome sync requires it.
function batchStatusLine(id) {
  return {
    pendingInboxCount: 0,
    pendingInboxKeys: [],
    pendingInboxRevision: "p-" + id,
    pendingInboxVersion: 1,
  };
}

function batchSettle(config) {
  return new Promise((resolve) => setTimeout(resolve, config.settleMs));
}

function batchSubscribeFrames(state) {
  return state.frames.filter((frame) => frame.type === "lanes.subscribe");
}

function batchEntryIds(frame) {
  return (frame.entries || []).map((entry) => entry.targetId).sort();
}

// Stub the transport before any lane mounts so every subscribe rides it, and
// instrument mosaic renders per host.
function batchInstallStubs(state, config) {
  // eslint-disable-next-line no-global-assign
  liveBusRequest = async (type, fields = {}) => {
    state.frames.push({ type, ...fields });
    if (type !== "lanes.subscribe") return { type: "bus.ok", payload: {} };
    return {
      type: "lanes.payload",
      lanes: (fields.entries || []).map((entry) => {
        return {
          targetId: entry.targetId,
          subscriptionGeneration: "batch:" + entry.targetId,
          watcherActive: true,
          watcherError: "",
          payload:
            state.payloadOverrides[entry.targetId] ||
            {
              messages: window.__batchLaneMessages(
                entry.targetId,
                config.memberIds.indexOf(entry.targetId),
                config.perMember,
              ),
              statusLine: window.__batchStatusLine(entry.targetId),
            },
        };
      }),
    };
  };
  state.originalMosaicRender = mosaicRenderMessageStream;
  // eslint-disable-next-line no-global-assign
  mosaicRenderMessageStream = function (lane, items) {
    state.mosaicRenders.push({
      targetId: lane.targetId,
      keys: items.map((item) => item.key),
    });
    return state.originalMosaicRender(lane, items);
  };
}

// Phase A: initial fused mount -- one frame, one message-bearing host render.
async function batchPhaseInitial(state, config) {
  targets = config.memberIds.map((id) => window.__batchTarget(id, id + "-th"));
  targetById = new Map(targets.map((target) => [target.id, target]));
  applyTeamSnapshotPayload(
    {
      revision: config.snapshotRevision,
      changed: true,
      snapshot: {
        globalSettings: { fastMode: false },
        teams: [window.__batchTeam(config.memberIds, 1)],
      },
    },
    { force: true },
  );
  await window.__batchSettle(config);
  const host = laneGroupHost(laneStates.get(config.memberIds[0]));
  const frames = window.__batchSubscribeFrames(state);
  const hostRenders = state.mosaicRenders.filter((render) => {
    return render.targetId === host.targetId && render.keys.length > 0;
  });
  const expectedKeys = config.memberIds
    .flatMap((id, offset) => {
      return window
        .__batchLaneMessages(id, offset, config.perMember)
        .map((item) => item.key);
    })
    .sort();
  return {
    hostTargetId: host.targetId,
    initialFrameCount: frames.length,
    initialFrameEntryIds: frames.length ? window.__batchEntryIds(frames[0]) : [],
    initialHostRenderCount: hostRenders.length,
    initialHostRenderKeys: hostRenders.length
      ? [...hostRenders[0].keys].sort()
      : [],
    expectedKeys,
  };
}

// Phase B: reconnect resync -- one frame covering every open lane.
async function batchPhaseResync(state, config) {
  state.frames.length = 0;
  resubscribeLiveBusLanes();
  await window.__batchSettle(config);
  const frames = window.__batchSubscribeFrames(state);
  return {
    resyncFrameCount: frames.length,
    resyncFrameEntryIds: frames.length ? window.__batchEntryIds(frames[0]) : [],
  };
}

// Phase C: a thread change (bus payload with a renewed bound thread) and a
// config-revision advance in the same tick coalesce into one batch flush.
// Both triggers stay in one synchronous block -- applyLaneBusPayload is not
// awaited -- so their marks land before the microtask flush fires.
async function batchPhaseCoalesce(state, config) {
  state.frames.length = 0;
  applyLaneBusPayload(
    laneStates.get(config.memberIds[0]),
    {
      targetIdentity: window.__batchTarget(
        config.memberIds[0],
        config.memberIds[0] + "-th2",
      ).targetIdentity,
      messages: [],
      statusLine: window.__batchStatusLine(config.memberIds[0]),
    },
    "bus",
  );
  ensureTeamMemberLane(
    config.memberIds[1],
    window.__batchTeam(config.memberIds, 2),
  );
  await window.__batchSettle(config);
  const frames = window.__batchSubscribeFrames(state);
  return {
    coalescedFrameCount: frames.length,
    coalescedFrameEntryIds: frames.length
      ? window.__batchEntryIds(frames[0])
      : [],
  };
}

// Phase D: a failed lane inside the batch surfaces its transient status while
// the sibling's fresh message still renders.
async function batchPhaseFailedLane(state, config, hostTargetId) {
  state.frames.length = 0;
  state.mosaicRenders.length = 0;
  state.payloadOverrides[config.memberIds[0]] = {
    error: "boom from batch",
    messages: [],
    statusLine: {},
  };
  state.payloadOverrides[config.memberIds[1]] = {
    messages: [window.__batchFreshMessage(config.memberIds[1])],
    statusLine: window.__batchStatusLine(config.memberIds[1]),
  };
  subscribeLaneToLiveBus(laneStates.get(config.memberIds[0]));
  subscribeLaneToLiveBus(laneStates.get(config.memberIds[1]));
  await window.__batchSettle(config);
  const failedLane = laneStates.get(config.memberIds[0]);
  const freshRenders = state.mosaicRenders.filter((render) => {
    return render.targetId === hostTargetId && render.keys.includes("m-fresh");
  });
  return {
    failedLaneStatus: failedLane.statusErrorEl.textContent,
    siblingFreshRenderCount: freshRenders.length,
  };
}

async function batchMeasure(config) {
  const state = { frames: [], mosaicRenders: [], payloadOverrides: {} };
  window.__batchInstallStubs(state, config);
  const initial = await window.__batchPhaseInitial(state, config);
  const resync = await window.__batchPhaseResync(state, config);
  const coalesced = await window.__batchPhaseCoalesce(state, config);
  const failed = await window.__batchPhaseFailedLane(
    state,
    config,
    initial.hostTargetId,
  );
  // eslint-disable-next-line no-global-assign
  mosaicRenderMessageStream = state.originalMosaicRender;
  return { ...initial, ...resync, ...coalesced, ...failed };
}

const BATCH_PAGE_HELPERS = {
  __batchTarget: batchTargetPayload,
  __batchTeam: batchTeam,
  __batchLaneMessages: batchLaneMessages,
  __batchFreshMessage: batchFreshMessage,
  __batchStatusLine: batchStatusLine,
  __batchSettle: batchSettle,
  __batchSubscribeFrames: batchSubscribeFrames,
  __batchEntryIds: batchEntryIds,
  __batchInstallStubs: batchInstallStubs,
  __batchPhaseInitial: batchPhaseInitial,
  __batchPhaseResync: batchPhaseResync,
  __batchPhaseCoalesce: batchPhaseCoalesce,
  __batchPhaseFailedLane: batchPhaseFailedLane,
};

function batchPageHelperScript() {
  return Object.entries(BATCH_PAGE_HELPERS)
    .map(([name, helper]) => "window." + name + " = " + helper.toString() + ";")
    .join("\n");
}

function assertBatchResult(result) {
  const failures = {
    "initial mount did not coalesce into one lanes.subscribe frame":
      result.initialFrameCount !== 1,
    "initial batch frame missed open lanes":
      result.initialFrameEntryIds.join(",") !== BATCH_MEMBER_IDS.join(","),
    "fused host painted more than one message-bearing render on load":
      result.initialHostRenderCount !== 1,
    "single host render did not cover every member's initial messages":
      result.initialHostRenderKeys.join(",") !== result.expectedKeys.join(","),
    "reconnect resync did not issue exactly one lanes.subscribe frame":
      result.resyncFrameCount !== 1,
    "resync frame did not cover every open lane":
      result.resyncFrameEntryIds.join(",") !== BATCH_MEMBER_IDS.join(","),
    "thread-change + config-revision resubscribes did not coalesce":
      result.coalescedFrameCount !== 1,
    "coalesced flush missed a resubscribing lane":
      result.coalescedFrameEntryIds.join(",") !== BATCH_MEMBER_IDS.join(","),
    "failed batch lane did not surface its transient status":
      result.failedLaneStatus !== "boom from batch",
    "sibling lane did not render normally beside the failed lane":
      result.siblingFreshRenderCount !== 1,
  };
  for (const [message, failed] of Object.entries(failures)) {
    if (failed) throw new Error(message + ": " + JSON.stringify(result));
  }
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=lanes-batch-subscribe-" + Date.now(),
      contextOptions: { viewport: { width: 1900, height: 1000 } },
    },
    async ({ page }) => {
      await page.waitForFunction(
        () =>
          typeof applyTeamSnapshotPayload === "function" &&
          typeof subscribeLaneToLiveBus === "function" &&
          typeof mosaicRenderMessageStream === "function" &&
          typeof ensureTeamMemberLane === "function",
        { timeout: 10000 },
      );
      await page.addScriptTag({ content: batchPageHelperScript() });
      const result = await page.evaluate(batchMeasure, {
        memberIds: BATCH_MEMBER_IDS,
        perMember: BATCH_MESSAGES_PER_MEMBER,
        snapshotRevision: BATCH_SNAPSHOT_REVISION,
        settleMs: BATCH_SETTLE_MS,
      });
      assertBatchResult(result);
      return result;
    },
  );
}

run().then(
  (result) => {
    console.log("serve_lanes_batch_subscribe_smoke ok " + JSON.stringify(result));
  },
  (error) => {
    console.error(error);
    process.exit(1);
  },
);
