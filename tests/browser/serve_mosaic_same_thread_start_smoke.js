const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { installScript } = require("./payload_factory");
const { withServePage } = require("./serve_playwright_harness");

const SAME_THREAD_ID = "same-thread-start-thread";
const SAME_THREAD_MESSAGE_COUNT = 24;
const SAME_THREAD_COMPACTION_INDEX = 12;
const SAME_THREAD_SCROLL_TOP_PX = 160;
const SAME_THREAD_STARTUP_KEY = "same-thread-start-first-message";

function sameThreadMessage(config, index) {
  const line = "same-thread settled message " + index;
  const text = Array.from({ length: 5 }, () => line).join("\n");
  return {
    ack_count: 0,
    ack_keys: [],
    ack_segments: [],
    display_html: text.replaceAll("\n", "<br>"),
    display_text: text,
    index,
    key: "same-thread-start-message-" + index,
    kind: index === config.compactionIndex ? "compaction" : "assistant",
    producerTargetId: config.targetId,
    text,
    threadId: config.threadId,
    timestamp: new Date(Date.UTC(2027, 0, 1, 0, 0, index)).toISOString(),
  };
}

function sameThreadStartupMessage(config) {
  const index = config.messageCount + 1;
  const item = sameThreadMessage(config, index);
  item.key = config.startupKey;
  delete item.producerTargetId;
  delete item.threadId;
  return item;
}

function sameThreadPayload(lane, config, status, revision, messages = []) {
  const target = window.spicePayloads.targetPayload({
    id: lane.targetId,
    teamId: lane.teamId,
    teamRevision: 1,
    threadId: config.threadId,
  });
  return {
    chrome: {
      targetId: lane.targetId,
      lifecycle: {
        authority: "lifecycle-reconciler",
        order: { epoch: "same-thread-start", revision },
        value: { processStatus: status, visualStatus: status },
      },
    },
    messages,
    serveAgentIdentity: target.serveAgentIdentity,
    statusLine: {
      activityStatus: status === "idle" ? "inactive" : "active-ish",
      agentProcessStatus: status,
      agentVisualStatus: status,
      preview: status,
    },
    targetIdentity: target.targetIdentity,
  };
}

function sameThreadRounded(value) {
  return Math.round(value * 1000) / 1000;
}

function sameThreadRect(node) {
  const rect = node.getBoundingClientRect();
  return {
    height: sameThreadRounded(rect.height),
    left: sameThreadRounded(rect.left),
    top: sameThreadRounded(rect.top),
    width: sameThreadRounded(rect.width),
  };
}

function sameThreadExistingSnapshot(lane, excludedKey = "") {
  return lane.mosaicCards
    .filter((card) => card.key !== excludedKey)
    .map((card) => {
      const node = lane.mosaicPlaneEl.querySelector(
        '[data-message-key="' + CSS.escape(card.key) + '"]',
      );
      return {
        card: {
          b: card.b,
          creationIndex: card.creationIndex,
          key: card.key,
          n: card.n,
          span: card.span,
          t: card.t,
        },
        key: card.key,
        rect: sameThreadRect(node),
        style: {
          height: node.style.height,
          transform: node.style.transform,
          transition: node.style.transition,
          width: node.style.width,
        },
      };
    })
    .sort((left, right) => left.key.localeCompare(right.key));
}

function sameThreadStableSnapshot(lane) {
  return {
    cards: sameThreadExistingSnapshot(lane),
    eventLog: lane.mosaicEventLog.events.map((event) => ({ ...event })),
    fingerprint: lane.renderedMessageFingerprint,
    geometry: {
      M: lane.mosaicGeometry.M,
      baseSpan: lane.mosaicGeometry.baseSpan,
      edges: lane.mosaicGeometry.edges.slice(),
      gap: lane.mosaicGeometry.gap,
    },
    messageViewport: sameThreadRect(lane.messagesEl),
    plane: sameThreadRect(lane.mosaicPlaneEl),
    planeTransform: lane.mosaicPlaneEl.style.transform,
    scrollTop: lane.messagesEl.scrollTop,
    settled: Boolean(lane.mosaicSettled),
  };
}

function sameThreadAfterTwoFrames() {
  return new Promise((resolve) =>
    requestAnimationFrame(() => requestAnimationFrame(resolve)),
  );
}

function sameThreadMutationContainsSentinel(nodes) {
  return Array.from(nodes).some(
    (node) =>
      node.nodeType === Node.ELEMENT_NODE &&
      (node.matches("[data-history-sentinel]") ||
        node.querySelector("[data-history-sentinel]")),
  );
}

function sameThreadTraceBucket() {
  return {
    existingPositionWrites: 0,
    fullReplays: 0,
    messageNodeRenders: 0,
    mosaicRenders: 0,
    scheduledRenders: 0,
    sentinelMutations: 0,
    subscriptions: 0,
  };
}

async function sameThreadPrepareLane(config) {
  const lane = resolveIsolatedLane("mosaic-same-thread-start-smoke-team");
  config.targetId = lane.targetId;
  const originalSubscribe = subscribeLaneToLiveBus;
  subscribeLaneToLiveBus = function () {};
  try {
    await applyLaneBusPayload(
      lane,
      sameThreadPayload(lane, config, "idle", 1),
      "bus",
    );
  } finally {
    subscribeLaneToLiveBus = originalSubscribe;
  }

  lane.knownMessages = Array.from(
    { length: config.messageCount },
    (_, offset) => sameThreadMessage(config, config.messageCount - offset),
  );
  lane.knownMessageKeys = new Set(lane.knownMessages.map((item) => item.key));
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  mosaicSettleNow(lane);
  await sameThreadAfterTwoFrames();
  const maxScrollTop = lane.messagesEl.scrollHeight - lane.messagesEl.clientHeight;
  if (maxScrollTop < config.scrollTop)
    throw new Error("same-thread fixture did not create a scrollable mosaic");
  lane.messagesEl.scrollTop = config.scrollTop;
  await sameThreadAfterTwoFrames();
  return lane;
}

function sameThreadMessageNodes(lane) {
  return new Map(
    Array.from(lane.mosaicPlaneEl.querySelectorAll("[data-message-key]"), (node) => [
      node.dataset.messageKey,
      node,
    ]),
  );
}

function sameThreadInstallTracing(lane, config, trace) {
  const counts = () => trace[trace.phase];
  const observer = new MutationObserver((records) => {
    for (const record of records) {
      if (
        sameThreadMutationContainsSentinel(record.addedNodes) ||
        sameThreadMutationContainsSentinel(record.removedNodes)
      )
        counts().sentinelMutations += 1;
    }
  });
  observer.observe(lane.mosaicPlaneEl, { childList: true, subtree: true });
  const originals = {
    fullReplay: mosaicFullReplay,
    messageRender: renderMessage,
    mosaicRender: mosaicRenderMessageStream,
    position: mosaicApplyCardPosition,
    schedule: mosaicScheduleRender,
    subscribe: subscribeLaneToLiveBus,
  };
  mosaicRenderMessageStream = function (...args) {
    counts().mosaicRenders += 1;
    return originals.mosaicRender.apply(this, args);
  };
  mosaicFullReplay = function (...args) {
    counts().fullReplays += 1;
    return originals.fullReplay.apply(this, args);
  };
  mosaicApplyCardPosition = function (node, card, previous, ...args) {
    const next = originals.position.call(this, node, card, previous, ...args);
    if (card.key !== config.startupKey && next !== previous)
      counts().existingPositionWrites += 1;
    return next;
  };
  renderMessage = function (...args) {
    counts().messageNodeRenders += 1;
    return originals.messageRender.apply(this, args);
  };
  mosaicScheduleRender = function (...args) {
    counts().scheduledRenders += 1;
    return originals.schedule.apply(this, args);
  };
  subscribeLaneToLiveBus = function () {
    counts().subscriptions += 1;
  };
  return { observer, originals };
}

function sameThreadRestoreTracing(instrumentation) {
  mosaicFullReplay = instrumentation.originals.fullReplay;
  renderMessage = instrumentation.originals.messageRender;
  mosaicRenderMessageStream = instrumentation.originals.mosaicRender;
  mosaicApplyCardPosition = instrumentation.originals.position;
  mosaicScheduleRender = instrumentation.originals.schedule;
  subscribeLaneToLiveBus = instrumentation.originals.subscribe;
  instrumentation.observer.disconnect();
}

async function sameThreadExerciseTransitions(lane, config, trace) {
  await applyLaneBusPayload(
    lane,
    sameThreadPayload(lane, config, "starting", 2),
    "bus",
  );
  await sameThreadAfterTwoFrames();
  const statusAfter = sameThreadStableSnapshot(lane);
  trace.phase = "message";
  const messageBefore = sameThreadExistingSnapshot(lane);
  const eventCount = lane.mosaicEventLog.events.length;
  await applyLaneBusPayload(
    lane,
    sameThreadPayload(lane, config, "starting", 3, [
      sameThreadStartupMessage(config),
    ]),
    "bus",
  );
  await sameThreadAfterTwoFrames();
  return {
    eventDelta: lane.mosaicEventLog.events.slice(eventCount),
    messageAfter: sameThreadExistingSnapshot(lane, config.startupKey),
    messageBefore,
    statusAfter,
  };
}

async function sameThreadRunScenario(config) {
  const lane = await sameThreadPrepareLane(config);
  const planeBefore = lane.mosaicPlaneEl;
  const nodesBefore = sameThreadMessageNodes(lane);
  const statusBefore = sameThreadStableSnapshot(lane);
  const trace = {
    message: sameThreadTraceBucket(),
    phase: "status",
    status: sameThreadTraceBucket(),
  };
  const instrumentation = sameThreadInstallTracing(lane, config, trace);
  let transitions;
  try {
    transitions = await sameThreadExerciseTransitions(lane, config, trace);
  } finally {
    sameThreadRestoreTracing(instrumentation);
  }
  const nodesAfter = sameThreadMessageNodes(lane);
  return {
    activeThreadId: lane.activeThreadId,
    eventDelta: transitions.eventDelta,
    existingNodeIdentityStable: Array.from(nodesBefore).every(
      ([key, node]) => nodesAfter.get(key) === node,
    ),
    messageAfter: transitions.messageAfter,
    messageBefore: transitions.messageBefore,
    planeIdentityStable: lane.mosaicPlaneEl === planeBefore,
    processStatus: lane.lastRenderedStatusLine.agentProcessStatus,
    statusAfter: transitions.statusAfter,
    statusBefore,
    targetThreadId: lane.targetThreadId,
    trace,
    visualStatus: lane.lastRenderedStatusLine.agentVisualStatus,
  };
}

function sameThreadResultSummary(result) {
  return {
    activeThreadId: result.activeThreadId,
    eventDelta: result.eventDelta,
    existingNodeIdentityStable: result.existingNodeIdentityStable,
    planeIdentityStable: result.planeIdentityStable,
    processStatus: result.processStatus,
    targetThreadId: result.targetThreadId,
    trace: result.trace,
    visualStatus: result.visualStatus,
  };
}

function assertSameThreadResult(result, config) {
  const fail = (message) => {
    throw new Error(message + ": " + JSON.stringify(sameThreadResultSummary(result)));
  };
  if (
    result.activeThreadId !== config.threadId ||
    result.targetThreadId !== config.threadId
  )
    fail("same-thread startup changed the bound thread");
  if (result.processStatus !== "starting" || result.visualStatus !== "starting")
    fail("same-thread startup did not update lifecycle chrome");
  if (Object.values(result.trace.status).some((count) => count !== 0))
    fail("same-thread status transition entered mosaic or subscription paths");
  if (JSON.stringify(result.statusBefore) !== JSON.stringify(result.statusAfter))
    fail("same-thread status transition changed settled mosaic state");
  if (!result.planeIdentityStable || !result.existingNodeIdentityStable)
    fail("same-thread startup replaced settled mosaic nodes");
  if (
    result.trace.message.mosaicRenders !== 1 ||
    result.trace.message.messageNodeRenders !== 1 ||
    result.trace.message.fullReplays !== 0 ||
    result.trace.message.existingPositionWrites !== 0 ||
    result.trace.message.scheduledRenders !== 0 ||
    result.trace.message.subscriptions !== 0
  )
    fail("first same-thread message did not use one incremental insert");
  if (result.trace.message.sentinelMutations !== 0)
    fail("first same-thread message detached an unchanged history sentinel");
  if (JSON.stringify(result.messageBefore) !== JSON.stringify(result.messageAfter))
    fail("first same-thread message moved or restyled an existing card");
  if (
    result.eventDelta.length !== 1 ||
    result.eventDelta[0].type !== "insert" ||
    result.eventDelta[0].key !== config.startupKey
  )
    fail("first same-thread message did not record exactly one insert event");
}

async function run() {
  return withServePage(
    {
      contextOptions: { viewport: { height: 800, width: 1280 } },
      path: "/?smoke=mosaic-same-thread-start-" + Date.now(),
    },
    async ({ page, server }) => {
      await installIsolatedLaneFixture(page, {
        globals: [
          "applyLaneBusPayload",
          "mosaicApplyCardPosition",
          "mosaicFullReplay",
          "mosaicRenderMessageStream",
          "mosaicScheduleRender",
          "mosaicSettleNow",
          "renderMessage",
          "renderMessagesIfChanged",
          "subscribeLaneToLiveBus",
        ],
      });
      await page.addScriptTag({ content: installScript });
      await page.addScriptTag({
        content: [
          sameThreadMessage,
          sameThreadStartupMessage,
          sameThreadPayload,
          sameThreadRounded,
          sameThreadRect,
          sameThreadExistingSnapshot,
          sameThreadStableSnapshot,
          sameThreadAfterTwoFrames,
          sameThreadMutationContainsSentinel,
          sameThreadTraceBucket,
          sameThreadPrepareLane,
          sameThreadMessageNodes,
          sameThreadInstallTracing,
          sameThreadRestoreTracing,
          sameThreadExerciseTransitions,
        ]
          .map((helper) => helper.toString())
          .join("\n"),
      });
      const config = {
        compactionIndex: SAME_THREAD_COMPACTION_INDEX,
        messageCount: SAME_THREAD_MESSAGE_COUNT,
        scrollTop: SAME_THREAD_SCROLL_TOP_PX,
        startupKey: SAME_THREAD_STARTUP_KEY,
        threadId: SAME_THREAD_ID,
      };
      const result = await page.evaluate(sameThreadRunScenario, config);
      assertSameThreadResult(result, config);
      return { ...result, url: server.url };
    },
  );
}

if (require.main === module) {
  run()
    .then((result) => {
      console.log(
        "serve_mosaic_same_thread_start_smoke ok " +
          JSON.stringify({ ...sameThreadResultSummary(result), url: result.url }),
      );
    })
    .catch((error) => {
      console.error(error);
      process.exitCode = 1;
    });
}

module.exports = { run };
