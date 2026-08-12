const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { installScript } = require("./payload_factory");
const { withServePage } = require("./serve_playwright_harness");

const IDLE_START_MESSAGE_COUNT = 24;
const IDLE_START_COMPACTION_INDEX = 12;
const IDLE_START_SCROLL_TOP_PX = 160;
const IDLE_START_OLD_THREAD = "idle-start-thread-old";
const IDLE_START_NEW_THREAD = "idle-start-thread-new";

function idleStartMessage(config, index) {
  const line = "settled message " + index;
  const text = Array.from({ length: 5 }, () => line).join("\n");
  const kind =
    index === config.compactionIndex ? "compaction" : "assistant";
  return {
    ack_count: 0,
    ack_keys: [],
    ack_segments: [],
    display_html: text.replaceAll("\n", "<br>"),
    display_text: text,
    index,
    key: "idle-start-message-" + index,
    kind,
    producerTargetId: config.targetId,
    text,
    threadId: config.oldThread,
    timestamp: new Date(Date.UTC(2027, 0, 1, 0, 0, index)).toISOString(),
  };
}

function idleStartPayload(lane, config, threadId, status, revision) {
  const target = window.spicePayloads.targetPayload({
    id: lane.targetId,
    teamId: lane.teamId,
    teamRevision: 1,
    threadId,
  });
  return {
    chrome: {
      targetId: lane.targetId,
      lifecycle: {
        authority: "lifecycle-reconciler",
        order: { epoch: "idle-start", revision },
        value: { processStatus: status, visualStatus: status },
      },
    },
    messages: [],
    serveAgentIdentity: target.serveAgentIdentity,
    statusLine: {
      activityStatus: status === "starting" ? "active-ish" : "inactive",
      agentProcessStatus: status,
      agentVisualStatus: status,
      preview: status,
    },
    targetIdentity: target.targetIdentity,
  };
}

function idleStartRounded(value) {
  return Math.round(value * 1000) / 1000;
}

function idleStartRect(node) {
  const rect = node.getBoundingClientRect();
  return {
    bottom: idleStartRounded(rect.bottom),
    height: idleStartRounded(rect.height),
    left: idleStartRounded(rect.left),
    right: idleStartRounded(rect.right),
    top: idleStartRounded(rect.top),
    width: idleStartRounded(rect.width),
  };
}

function idleStartSnapshot(lane) {
  const nodes = Array.from(
    lane.mosaicPlaneEl.querySelectorAll("[data-message-key]"),
  );
  return {
    cards: lane.mosaicCards.map((card) => ({ ...card })),
    eventLog: lane.mosaicEventLog.events.map((event) => ({ ...event })),
    extent: idleStartRect(lane.mosaicExtentEl),
    fingerprint: lane.renderedMessageFingerprint,
    geometry: {
      M: lane.mosaicGeometry.M,
      baseSpan: lane.mosaicGeometry.baseSpan,
      edges: lane.mosaicGeometry.edges.slice(),
      gap: lane.mosaicGeometry.gap,
    },
    messageViewport: idleStartRect(lane.messagesEl),
    plane: idleStartRect(lane.mosaicPlaneEl),
    scrollTop: lane.messagesEl.scrollTop,
    settled: Boolean(lane.mosaicSettled),
    styles: nodes.map((node) => ({
      height: node.style.height,
      key: node.dataset.messageKey,
      rect: idleStartRect(node),
      transform: node.style.transform,
      transition: node.style.transition,
      width: node.style.width,
    })),
  };
}

function idleStartAfterTwoFrames() {
  return new Promise((resolve) =>
    requestAnimationFrame(() => requestAnimationFrame(resolve)),
  );
}

async function idleStartRunScenario(config) {
  const lane = resolveIsolatedLane("mosaic-idle-start-smoke-team");
  config.targetId = lane.targetId;
  const originalSubscribe = subscribeLaneToLiveBus;
  subscribeLaneToLiveBus = function () {};
  try {
    await applyLaneBusPayload(
      lane,
      idleStartPayload(lane, config, config.oldThread, "idle", 1),
      "bus",
    );
  } finally {
    subscribeLaneToLiveBus = originalSubscribe;
  }

  lane.knownMessages = Array.from(
    { length: config.messageCount },
    (_, offset) => idleStartMessage(config, config.messageCount - offset),
  );
  lane.knownMessageKeys = new Set(lane.knownMessages.map((item) => item.key));
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  mosaicSettleNow(lane);
  await idleStartAfterTwoFrames();

  const maxScrollTop = lane.messagesEl.scrollHeight - lane.messagesEl.clientHeight;
  if (maxScrollTop < config.scrollTop)
    throw new Error("idle-start fixture did not create a scrollable mosaic");
  lane.messagesEl.scrollTop = config.scrollTop;
  await idleStartAfterTwoFrames();

  const planeBefore = lane.mosaicPlaneEl;
  const nodeByKeyBefore = new Map(
    Array.from(planeBefore.querySelectorAll("[data-message-key]"), (node) => [
      node.dataset.messageKey,
      node,
    ]),
  );
  const before = idleStartSnapshot(lane);
  const trace = {
    fullReplays: 0,
    messageNodeRenders: 0,
    mosaicRenders: 0,
    planeMutations: 0,
    positionCalls: 0,
    subscriptions: 0,
  };

  const observer = new MutationObserver((records) => {
    trace.planeMutations += records.length;
  });
  observer.observe(planeBefore, { childList: true, subtree: true });
  const originalMosaicRender = mosaicRenderMessageStream;
  const originalFullReplay = mosaicFullReplay;
  const originalPosition = mosaicApplyCardPosition;
  const originalMessageRender = renderMessage;
  const transitionSubscribe = subscribeLaneToLiveBus;
  mosaicRenderMessageStream = function (...args) {
    trace.mosaicRenders += 1;
    return originalMosaicRender.apply(this, args);
  };
  mosaicFullReplay = function (...args) {
    trace.fullReplays += 1;
    return originalFullReplay.apply(this, args);
  };
  mosaicApplyCardPosition = function (...args) {
    trace.positionCalls += 1;
    return originalPosition.apply(this, args);
  };
  renderMessage = function (...args) {
    trace.messageNodeRenders += 1;
    return originalMessageRender.apply(this, args);
  };
  subscribeLaneToLiveBus = function () {
    trace.subscriptions += 1;
  };

  try {
    await applyLaneBusPayload(
      lane,
      idleStartPayload(lane, config, config.newThread, "starting", 2),
      "bus",
    );
    await idleStartAfterTwoFrames();
  } finally {
    mosaicRenderMessageStream = originalMosaicRender;
    mosaicFullReplay = originalFullReplay;
    mosaicApplyCardPosition = originalPosition;
    renderMessage = originalMessageRender;
    subscribeLaneToLiveBus = transitionSubscribe;
    trace.planeMutations += observer.takeRecords().length;
    observer.disconnect();
  }

  const nodesAfter = new Map(
    Array.from(lane.mosaicPlaneEl.querySelectorAll("[data-message-key]"), (node) => [
      node.dataset.messageKey,
      node,
    ]),
  );
  return {
    activeThreadId: lane.activeThreadId,
    after: idleStartSnapshot(lane),
    before,
    nodeIdentityStable: Array.from(nodeByKeyBefore).every(
      ([key, node]) => nodesAfter.get(key) === node,
    ),
    nodeInventoryStable:
      nodeByKeyBefore.size === nodesAfter.size &&
      Array.from(nodesAfter.keys()).every((key) => nodeByKeyBefore.has(key)),
    planeIdentityStable: lane.mosaicPlaneEl === planeBefore,
    processStatus: lane.lastRenderedStatusLine.agentProcessStatus,
    targetThreadId: lane.targetThreadId,
    trace,
    visualStatus: lane.lastRenderedStatusLine.agentVisualStatus,
  };
}

function assertIdleStartResult(result, config) {
  const fail = (message) => {
    throw new Error(message + ": " + JSON.stringify(result));
  };
  if (
    result.activeThreadId !== config.newThread ||
    result.targetThreadId !== config.newThread
  )
    fail("agent startup did not bind the renewed thread");
  if (result.processStatus !== "starting" || result.visualStatus !== "starting")
    fail("agent startup did not update lane status chrome");
  if (result.trace.subscriptions !== 1)
    fail("renewed thread did not trigger exactly one subscription");
  const unnecessary = Object.entries(result.trace).filter(
    ([name, count]) => name !== "subscriptions" && count !== 0,
  );
  if (unnecessary.length)
    fail("idle agent startup entered mosaic rebuild paths");
  if (!result.planeIdentityStable || !result.nodeIdentityStable)
    fail("idle agent startup replaced settled mosaic nodes");
  if (!result.nodeInventoryStable)
    fail("idle agent startup changed the settled card inventory");
  if (JSON.stringify(result.before) !== JSON.stringify(result.after))
    fail("idle agent startup changed settled mosaic geometry or scroll state");
}

async function run() {
  return withServePage(
    {
      contextOptions: { viewport: { height: 800, width: 1280 } },
      path: "/?smoke=mosaic-idle-start-" + Date.now(),
    },
    async ({ page, server }) => {
      await installIsolatedLaneFixture(page, {
        globals: [
          "applyLaneBusPayload",
          "mosaicApplyCardPosition",
          "mosaicFullReplay",
          "mosaicRenderMessageStream",
          "mosaicSettleNow",
          "renderMessage",
          "renderMessagesIfChanged",
          "subscribeLaneToLiveBus",
        ],
      });
      await page.addScriptTag({ content: installScript });
      await page.addScriptTag({
        content: [
          idleStartMessage,
          idleStartPayload,
          idleStartRounded,
          idleStartRect,
          idleStartSnapshot,
          idleStartAfterTwoFrames,
        ]
          .map((helper) => helper.toString())
          .join("\n"),
      });
      const config = {
        compactionIndex: IDLE_START_COMPACTION_INDEX,
        messageCount: IDLE_START_MESSAGE_COUNT,
        newThread: IDLE_START_NEW_THREAD,
        oldThread: IDLE_START_OLD_THREAD,
        scrollTop: IDLE_START_SCROLL_TOP_PX,
      };
      const result = await page.evaluate(idleStartRunScenario, config);
      assertIdleStartResult(result, config);
      return { ...result, url: server.url };
    },
  );
}

if (require.main === module) {
  run()
    .then((result) => {
      console.log("serve_mosaic_idle_start_smoke ok " + JSON.stringify(result));
    })
    .catch((error) => {
      console.error(error);
      process.exitCode = 1;
    });
}

module.exports = { run };
