const { withServePage } = require("./serve_playwright_harness");
const { installScript } = require("./payload_factory");

// Lane subscribes coalesce into ONE lanes.subscribe frame per microtask tick,
// and the batch payloads apply state-first so each fused host paints its
// merged interleaved stream in a single render pass. Guards the per-member
// arrival reshuffle the operator saw on load: N open lanes used to subscribe
// independently, and every arrival re-sorted the fused mosaic.

const BATCH_MEMBER_IDS = ["bsub-a", "bsub-b"];
const BATCH_MESSAGES_PER_MEMBER = 3;
const BATCH_SNAPSHOT_REVISION = 50;
const BATCH_SETTLE_MS = 80;
const BATCH_DEFERRED_SUBSCRIBE_ATTEMPTS = 50;

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
    if (state.deferNextSubscribe) {
      state.deferNextSubscribe = false;
      await new Promise((resolve) => {
        state.releaseDeferredSubscribe = resolve;
      });
      state.releaseDeferredSubscribe = null;
    }
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
  laneStore.replaceTargets(
    config.memberIds.map((id) =>
      window.spicePayloads.targetPayload({
        id,
        threadId: id + "-th",
        teamId: "team-batch",
        pendingPrefix: "p-",
      }),
    ),
  );
  applyTeamSnapshotPayload(
    window.spicePayloads.teamSnapshot({
      revision: config.snapshotRevision,
      teams: [
        window.spicePayloads.teamPayload({
          teamId: "team-batch",
          memberIds: config.memberIds,
          revision: 1,
        }),
      ],
    }),
    { force: true },
  );
  await window.__batchSettle(config);
  const host = laneGroupHost(laneStore.laneForId(config.memberIds[0]));
  const savedFocusedTargetId = focusedLiveBusLaneTargetId;
  focusedLiveBusLaneTargetId = "different-lane-group";
  const visibleHostLiveWithoutFocus = liveBusLaneIsFocused(host);
  focusedLiveBusLaneTargetId = savedFocusedTargetId;
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
    visibleHostLiveWithoutFocus,
    initialFrameCount: frames.length,
    initialFrameEntryIds: frames.length ? window.__batchEntryIds(frames[0]) : [],
    initialHostRenderCount: hostRenders.length,
    initialHostRenderKeys: hostRenders.length
      ? [...hostRenders[0].keys].sort()
      : [],
    expectedKeys,
  };
}

// Phase B: topology-only class changes must reconcile server activity even
// when the fused members are already adjacent and lanesEl has no child-list
// mutation. A split makes the second member genuinely offscreen; fusing it
// back into the visible host makes that same concrete subscription live again.
async function batchPhaseTopologyActivity(state, config) {
  const host = laneGroupHost(laneStore.laneForId(config.memberIds[0]));
  const member = laneStore.laneForId(config.memberIds[1]);
  const originalMemberRect = member.element.getBoundingClientRect;
  let directChildMutationCount = 0;
  const observer = new MutationObserver((records) => {
    directChildMutationCount += records.length;
  });
  observer.observe(lanesEl, { childList: true });
  member.element.getBoundingClientRect = () => ({
    bottom: 600,
    height: 600,
    left: window.innerWidth + 100,
    right: window.innerWidth + 420,
    top: 0,
    width: 320,
  });

  state.frames.length = 0;
  reconcileLaneGroups([]);
  await window.__batchSettle(config);
  const splitConfigureFrames = state.frames.filter(
    (frame) => frame.type === "lane.configure",
  );
  const splitFocusByTargetId = Object.fromEntries(
    splitConfigureFrames.map((frame) => [
      frame.targetId,
      frame.query.focused,
    ]),
  );

  state.frames.length = 0;
  reconcileLaneGroups([config.memberIds]);
  await window.__batchSettle(config);
  const fusedConfigureFrames = state.frames.filter(
    (frame) => frame.type === "lane.configure",
  );
  const fusedFocusByTargetId = Object.fromEntries(
    fusedConfigureFrames.map((frame) => [
      frame.targetId,
      frame.query.focused,
    ]),
  );

  observer.disconnect();
  member.element.getBoundingClientRect = originalMemberRect;
  return {
    topologyDirectChildMutationCount: directChildMutationCount,
    splitConfigureCount: splitConfigureFrames.length,
    splitHostFocused: splitFocusByTargetId[host.targetId],
    splitMemberFocused: splitFocusByTargetId[member.targetId],
    fusedConfigureCount: fusedConfigureFrames.length,
    fusedHostFocused: fusedFocusByTargetId[host.targetId],
    fusedMemberFocused: fusedFocusByTargetId[member.targetId],
  };
}

// Phase C: focus moves while the replacement batch is awaiting its response.
// The pending lanes must configure immediately, before the held subscribe
// response can release server watchers under their stale focus queries.
async function batchPhaseFocusWhilePending(state, config) {
  state.frames.length = 0;
  state.deferNextSubscribe = true;
  resubscribeLiveBusLanes();
  for (
    let attempt = 0;
    attempt < config.deferredSubscribeAttempts &&
    typeof state.releaseDeferredSubscribe !== "function";
    attempt += 1
  ) {
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  if (typeof state.releaseDeferredSubscribe !== "function") {
    throw new Error("deferred lanes.subscribe did not reach the transport");
  }
  const priorId = config.memberIds[0];
  const selectedId = config.memberIds[1];
  setFocusedLiveBusLane(laneStore.laneForId(selectedId));
  const subscribeFrame = state.frames.find(
    (frame) => frame.type === "lanes.subscribe",
  );
  const requestedFocus = Object.fromEntries(
    (subscribeFrame?.entries || []).map((entry) => [
      entry.targetId,
      entry.query.focused,
    ]),
  );
  const configureFramesBeforeRelease = state.frames.filter(
    (frame) => frame.type === "lane.configure",
  );
  const configuredFocusBeforeRelease = Object.fromEntries(
    configureFramesBeforeRelease.map((frame) => [
      frame.targetId,
      frame.query.focused,
    ]),
  );
  state.releaseDeferredSubscribe();
  await window.__batchSettle(config);
  const configureFrames = state.frames.filter(
    (frame) => frame.type === "lane.configure",
  );
  return {
    pendingPriorFocus: requestedFocus[priorId],
    pendingSelectedFocus: requestedFocus[selectedId],
    preReleaseFocusConfigureCount: configureFramesBeforeRelease.length,
    preReleasePriorFocus: configuredFocusBeforeRelease[priorId],
    preReleaseSelectedFocus: configuredFocusBeforeRelease[selectedId],
    focusConfigureCount: configureFrames.length,
  };
}

// Phase D: a dirty notification for a member of the focused fused host must
// refresh that concrete member immediately. The server may still coalesce the
// member's burst into one lanes.dirty frame, but the browser cannot leave a
// visible part of the merged stream stale until a later composer submit.
async function batchPhaseFocusedMemberDirty(state, config) {
  state.frames.length = 0;
  const member = laneStore.laneForId(config.memberIds[1]);
  handleBackgroundLanesDirtyPush({
    type: "lanes.dirty",
    lanes: [
      {
        targetId: member.targetId,
        subscriptionGeneration: member.liveBusSubscriptionGeneration,
      },
    ],
  });
  await window.__batchSettle(config);
  const frames = window.__batchSubscribeFrames(state);
  return {
    dirtyMemberFocused: liveBusLaneIsFocused(member),
    dirtyMemberFrameCount: frames.length,
    dirtyMemberFrameEntryIds: frames.length
      ? window.__batchEntryIds(frames[0])
      : [],
    dirtyMemberCleared: member.liveBusDirty === false,
  };
}

// Phase E: reconnect resync -- one frame covering every open lane.
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

// Phase F: a thread change (bus payload with a renewed bound thread) and a
// config-revision advance in the same tick coalesce into one batch flush.
// Both triggers stay in one synchronous block -- applyLaneBusPayload is not
// awaited -- so their marks land before the microtask flush fires.
async function batchPhaseCoalesce(state, config) {
  state.frames.length = 0;
  applyLaneBusPayload(
    laneStore.laneForId(config.memberIds[0]),
    {
      targetIdentity: window.spicePayloads.targetPayload({
        id: config.memberIds[0],
        threadId: config.memberIds[0] + "-th2",
        teamId: "team-batch",
        pendingPrefix: "p-",
      }).targetIdentity,
      messages: [],
      statusLine: window.__batchStatusLine(config.memberIds[0]),
    },
    "bus",
  );
  ensureTeamMemberLane(
    config.memberIds[1],
    window.spicePayloads.teamPayload({
      teamId: "team-batch",
      memberIds: config.memberIds,
      revision: 2,
    }),
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

// Phase G: a failed lane inside the batch surfaces its transient status while
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
  subscribeLaneToLiveBus(laneStore.laneForId(config.memberIds[0]));
  subscribeLaneToLiveBus(laneStore.laneForId(config.memberIds[1]));
  await window.__batchSettle(config);
  const failedLane = laneStore.laneForId(config.memberIds[0]);
  const freshRenders = state.mosaicRenders.filter((render) => {
    return render.targetId === hostTargetId && render.keys.includes("m-fresh");
  });
  return {
    failedLaneStatus: failedLane.statusErrorEl.textContent,
    siblingFreshRenderCount: freshRenders.length,
  };
}

async function batchMeasure(config) {
  const state = {
    frames: [],
    mosaicRenders: [],
    payloadOverrides: {},
    deferNextSubscribe: false,
    releaseDeferredSubscribe: null,
  };
  window.__batchInstallStubs(state, config);
  const initial = await window.__batchPhaseInitial(state, config);
  const topology = await window.__batchPhaseTopologyActivity(state, config);
  const dirty = await window.__batchPhaseFocusedMemberDirty(state, config);
  const focus = await window.__batchPhaseFocusWhilePending(state, config);
  const resync = await window.__batchPhaseResync(state, config);
  const coalesced = await window.__batchPhaseCoalesce(state, config);
  const failed = await window.__batchPhaseFailedLane(
    state,
    config,
    initial.hostTargetId,
  );
  // eslint-disable-next-line no-global-assign
  mosaicRenderMessageStream = state.originalMosaicRender;
  return {
    ...initial,
    ...topology,
    ...focus,
    ...dirty,
    ...resync,
    ...coalesced,
    ...failed,
  };
}

const BATCH_PAGE_HELPERS = {
  __batchLaneMessages: batchLaneMessages,
  __batchFreshMessage: batchFreshMessage,
  __batchStatusLine: batchStatusLine,
  __batchSettle: batchSettle,
  __batchSubscribeFrames: batchSubscribeFrames,
  __batchEntryIds: batchEntryIds,
  __batchInstallStubs: batchInstallStubs,
  __batchPhaseInitial: batchPhaseInitial,
  __batchPhaseTopologyActivity: batchPhaseTopologyActivity,
  __batchPhaseFocusWhilePending: batchPhaseFocusWhilePending,
  __batchPhaseFocusedMemberDirty: batchPhaseFocusedMemberDirty,
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
    "visible lane group required click focus for live delivery":
      result.visibleHostLiveWithoutFocus !== true,
    "topology fixture accidentally relied on a direct child-list mutation":
      result.topologyDirectChildMutationCount !== 0,
    "split topology did not configure every concrete lane exactly once":
      result.splitConfigureCount !== BATCH_MEMBER_IDS.length,
    "visible split host was configured as background":
      result.splitHostFocused !== true,
    "offscreen split member retained live-push delivery":
      result.splitMemberFocused !== false,
    "fused topology did not configure every concrete lane exactly once":
      result.fusedConfigureCount !== BATCH_MEMBER_IDS.length,
    "visible fused host was configured as background":
      result.fusedHostFocused !== true,
    "newly visible fused member retained dirty-only delivery":
      result.fusedMemberFocused !== true,
    "pending subscribe did not capture the prior lane as focused":
      result.pendingPriorFocus !== true,
    "focused fused host did not keep its selected member live":
      result.pendingSelectedFocus !== true,
    "selecting a member of the focused fused host reconfigured the same group":
      result.preReleaseFocusConfigureCount !== 0 ||
      result.focusConfigureCount !== 0,
    "focused fused member did not use the live-push path":
      result.dirtyMemberFocused !== true,
    "dirty focused member did not batch exactly one resubscribe":
      result.dirtyMemberFrameCount !== 1,
    "dirty focused member resubscribe addressed the wrong lane":
      result.dirtyMemberFrameEntryIds.join(",") !== BATCH_MEMBER_IDS[1],
    "dirty focused member stayed stale after its payload applied":
      result.dirtyMemberCleared !== true,
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
      await page.addScriptTag({ content: installScript });
      await page.addScriptTag({ content: batchPageHelperScript() });
      const result = await page.evaluate(batchMeasure, {
        memberIds: BATCH_MEMBER_IDS,
        perMember: BATCH_MESSAGES_PER_MEMBER,
        snapshotRevision: BATCH_SNAPSHOT_REVISION,
        settleMs: BATCH_SETTLE_MS,
        deferredSubscribeAttempts: BATCH_DEFERRED_SUBSCRIBE_ATTEMPTS,
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
