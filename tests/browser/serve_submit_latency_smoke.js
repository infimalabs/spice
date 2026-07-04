const { withServePage } = require("./serve_playwright_harness");

const typingMosaicInitialMessageCount = 72;
const typingMosaicInputCount = 24;
const typingMosaicLiveAppendInterval = 3;
const typingMosaicMinimumMessageCount = 80;
const typingMosaicMaxPackCount = 12;
const typingMosaicMaxInputDispatchMs = 8;
const typingMosaicMaxInputDelayMs = 50;
const typingMosaicMaxPackMs = 50;

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-submit-latency-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof submitLaneForm === "function" &&
          typeof renderMessage === "function" &&
          typeof renderMessagesIfChanged === "function" &&
          typeof mosaicRenderMessageStream === "function" &&
          typeof liveBusIsOpen === "function" &&
          Array.isArray(window.__spiceSubmitLatencySamples) &&
          Array.isArray(targets) &&
          targets.length > 0,
        { timeout: 10000 },
      );
      await installSubmitLatencySmokeHelpers(page);
      const result = await page.evaluate(runSubmitLatencySmokePage);
      assertSubmitLatencyResult(result);
      const typing = await page.evaluate(runSubmitTypingDuringMosaicSmokePage);
      assertSubmitTypingDuringMosaicResult(typing);
      return { ...result, typing, url: server.url };
    },
  );
}

async function installSubmitLatencySmokeHelpers(page) {
  await page.addScriptTag({
    content: [
      "const typingMosaicInitialMessageCount = " +
        typingMosaicInitialMessageCount +
        ";",
      "const typingMosaicInputCount = " + typingMosaicInputCount + ";",
      "const typingMosaicLiveAppendInterval = " +
        typingMosaicLiveAppendInterval +
        ";",
      runSubmitLatencySmokePage,
      submitLatencySmokeLane,
      submitLatencySmokeTextarea,
      submitLatencySmokeLiveBusRequest,
      cleanupSubmitLatencySmokePage,
      runSubmitTypingDuringMosaicSmokePage,
      submitTypingMosaicItems,
      appendSubmitTypingMosaicMessage,
      waitForSubmitTypingFrame,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

async function runSubmitLatencySmokePage() {
  const lane = submitLatencySmokeLane();
  const textarea = submitLatencySmokeTextarea(lane);
  const smoke = {
    calls: [],
    lane,
    originalLiveBusRequest: liveBusRequest,
  };
  liveBusRequest = (type, fields = {}, timing = null) =>
    submitLatencySmokeLiveBusRequest(smoke, type, fields, timing);
  window.__spiceSubmitLatencySmoke = smoke;
  if (Array.isArray(window.__spiceSubmitLatencySamples))
    window.__spiceSubmitLatencySamples.length = 0;
  try {
    textarea.value = "submit latency smoke " + Date.now();
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    submitLaneForm(lane, new Event("submit", { cancelable: true }), lane.targetId);
    const deadline = Date.now() + 10000;
    while (Date.now() < deadline) {
      const samples = window.__spiceSubmitLatencySamples || [];
      const sample = samples[samples.length - 1];
      if (sample && sample.status === "accepted")
        return { calls: smoke.calls, sample };
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    throw new Error(
      "timed out waiting for submit latency sample: " +
        JSON.stringify({
          awaiting: lane.sendAwaitingBackendCount,
          pending: lane.pendingSubmissionCount,
          samples: window.__spiceSubmitLatencySamples || [],
          text: textarea.value,
        }),
    );
  } finally {
    cleanupSubmitLatencySmokePage();
  }
}

async function runSubmitTypingDuringMosaicSmokePage() {
  const lane = submitLatencySmokeLane();
  const textarea = submitLatencySmokeTextarea(lane);
  const host = lane.messagesEl || document.querySelector(".messages");
  if (!host) throw new Error("no message host available");
  const items = submitTypingMosaicItems(typingMosaicInitialMessageCount, 0);
  lane.knownMessages = items.slice();
  lane.knownMessageKeys = new Set(items.map((item) => item.key));
  lane.newestMessageKey = items[0].key;
  lane.oldestMessageKey = items[items.length - 1].key;
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  await waitForSubmitTypingFrame();
  await waitForSubmitTypingFrame();

  const originalRender = mosaicRenderMessageStream;
  const packDurations = [];
  mosaicRenderMessageStream = function (...args) {
    const startedAt = performance.now();
    try {
      return originalRender.apply(this, args);
    } finally {
      packDurations.push(performance.now() - startedAt);
    }
  };
  const inputDurations = [];
  const inputDelays = [];
  try {
    for (let index = 0; index < typingMosaicInputCount; index += 1) {
      const scheduledAt = performance.now();
      await new Promise((resolve) => setTimeout(resolve, 0));
      const inputStartedAt = performance.now();
      inputDelays.push(inputStartedAt - scheduledAt);
      textarea.value = "typing under mosaic load " + index;
      const beforeInput = performance.now();
      textarea.dispatchEvent(new Event("input", { bubbles: true }));
      inputDurations.push(performance.now() - beforeInput);
      if (index % typingMosaicLiveAppendInterval === 0)
        appendSubmitTypingMosaicMessage(lane, index);
      await waitForSubmitTypingFrame();
    }
  } finally {
    mosaicRenderMessageStream = originalRender;
  }
  return {
    inputCount: inputDurations.length,
    maxInputDelayMs: Math.max(...inputDelays, 0),
    maxInputDispatchMs: Math.max(...inputDurations, 0),
    messageCount: lane.knownMessages.length,
    packCount: packDurations.length,
    maxPackMs: Math.max(...packDurations, 0),
    // Mosaic observes exactly one node (the message host) by construction,
    // never per-card -- see mosaicSyncResizeObserver in app.mosaic-stream.js.
    observedCount: lane.mosaicResizeObserver ? 1 : 0,
  };
}

function submitTypingMosaicItems(count, offset) {
  const base = Date.parse("2026-07-04T00:00:00.000Z");
  return Array.from({ length: count }, (_, index) => {
    const step = offset + index;
    const paragraph =
      "<p>Typing stress card " +
      step +
      " keeps the loaded Mosaic realistic.</p>";
    const code =
      "<pre><code>mosaicRenderMessageStream(lane)\\nrow += " +
      step +
      "</code></pre>";
    return {
      ack_count: step % 11 === 0 ? 1 : 0,
      ack_keys: [],
      display_html: paragraph.repeat(1 + (step % 4)) + (step % 9 === 0 ? code : ""),
      display_text: "typing stress card " + step,
      index: step,
      key: "typing-mosaic-" + step,
      kind: step % 17 === 0 ? "final" : "assistant",
      text: "typing stress card " + step,
      timestamp: new Date(base + step * 60000).toISOString(),
    };
  });
}

function appendSubmitTypingMosaicMessage(lane, index) {
  const item = submitTypingMosaicItems(1, 1000 + index)[0];
  lane.knownMessages.unshift(item);
  lane.knownMessageKeys.add(item.key);
  lane.newestMessageKey = item.key;
  renderMessagesIfChanged(lane);
}

function waitForSubmitTypingFrame() {
  return new Promise((resolve) => requestAnimationFrame(resolve));
}

function submitLatencySmokeLane() {
  let lane = Array.from(laneStates.values()).find((item) => !item.emptyTeam);
  if (!lane && targets.length) {
    addLane(targets[0].id);
    lane = laneStates.get(targets[0].id);
  }
  if (!lane) throw new Error("no lane available for submit latency smoke");
  syncComposerShards(laneGroupHost(lane), laneGroupMemberLanes(laneGroupHost(lane)));
  return lane;
}

function submitLatencySmokeTextarea(lane) {
  const textarea =
    lane.shardTextareas.get(lane.targetId) || lane.element.querySelector("textarea");
  if (!textarea) throw new Error("no composer textarea available");
  return textarea;
}

function submitLatencySmokeLiveBusRequest(smoke, type, fields, timing) {
  const payload = fields.payload || {};
  const text = String(payload.text || "").trim();
  const call = {
    stubbed: type === "lane.send",
    targetId: fields.targetId || "",
    text,
    type,
  };
  smoke.calls.push(call);
  if (type !== "lane.send")
    return smoke.originalLiveBusRequest(type, fields, timing);
  const requestId = "submit-latency-smoke-" + smoke.calls.length;
  if (timing) {
    timing.requestId = requestId;
    markLaneSubmitLatency(timing, "requestCreatedAt");
    markLaneSubmitLatency(timing, "liveBusConnectStartAt");
    markLaneSubmitLatency(timing, "liveBusConnectReadyAt");
    markLaneSubmitLatency(timing, "liveBusFrameSentAt");
  }
  return new Promise((resolve) => {
    setTimeout(() => {
      markLaneSubmitLatency(timing, "liveBusResponseReceivedAt");
      resolve({
        result: {
          ok: true,
          agentEnsure: { ok: true, threadId: smoke.lane.targetThreadId || "" },
          key: "submit-latency-smoke-key",
          pendingInboxCount: 0,
          pendingInboxKeys: [],
          pendingInboxRevision: "submit-latency-smoke",
          pendingInboxVersion: Date.now(),
          requestText: payload.text || "",
          serverTiming: {
            targetResolveMs: 0,
            sendPayloadMs: 5,
            totalBeforeReplyMs: 5,
          },
        },
      });
    }, 5);
  });
}

function cleanupSubmitLatencySmokePage() {
  const smoke = window.__spiceSubmitLatencySmoke;
  if (!smoke) return;
  liveBusRequest = smoke.originalLiveBusRequest;
  delete window.__spiceSubmitLatencySmoke;
}

function assertSubmitLatencyResult(result) {
  const sample = result.sample || {};
  const calls = result.calls || [];
  const sendCalls = calls.filter((call) => call.type === "lane.send");
  if (sendCalls.length !== 1)
    throw new Error("expected one stubbed lane.send call");
  if (!sendCalls[0].stubbed)
    throw new Error("submit latency smoke used the real lane.send transport");
  if (!sendCalls[0].text.includes("submit latency smoke"))
    throw new Error("submit latency smoke did not submit the probe text");
  const durations = sample.durations || {};
  const marks = sample.marks || {};
  const serverTiming = sample.serverTiming || {};
  const requiredDurations = [
    "optimisticRenderMs",
    "liveBusOpenMs",
    "sendResultWaitMs",
    "responseHandlingMs",
    "totalMs",
  ];
  for (const key of requiredDurations) {
    if (!Number.isFinite(durations[key]))
      throw new Error("missing submit latency duration " + key);
  }
  for (const key of [
    "startedAt",
    "optimisticRenderedAt",
    "liveBusConnectStartAt",
    "liveBusConnectReadyAt",
    "liveBusFrameSentAt",
    "liveBusResponseReceivedAt",
    "responseResolvedAt",
    "resultAppliedAt",
    "completedAt",
  ]) {
    if (!Number.isFinite(marks[key]))
      throw new Error("missing submit latency mark " + key);
  }
  if (sample.status !== "accepted")
    throw new Error("unexpected submit latency status " + sample.status);
  if (!sample.requestId)
    throw new Error("submit latency sample did not record requestId");
  for (const key of ["targetResolveMs", "sendPayloadMs", "totalBeforeReplyMs"]) {
    if (!Number.isFinite(serverTiming[key]))
      throw new Error("missing submit latency server timing " + key);
  }
}

function assertSubmitTypingDuringMosaicResult(result) {
  if (result.inputCount !== typingMosaicInputCount)
    throw new Error("typing stress did not run all inputs");
  if (result.messageCount < typingMosaicMinimumMessageCount)
    throw new Error("typing stress did not append live messages");
  if (result.observedCount > 1)
    throw new Error("typing stress observed per-card Mosaic nodes");
  if (result.packCount > typingMosaicMaxPackCount)
    throw new Error("typing stress did excessive Mosaic packs: " + JSON.stringify(result));
  if (result.maxInputDispatchMs > typingMosaicMaxInputDispatchMs)
    throw new Error("typing dispatch was slow under Mosaic load: " + JSON.stringify(result));
  if (result.maxInputDelayMs > typingMosaicMaxInputDelayMs)
    throw new Error("typing event was delayed under Mosaic load: " + JSON.stringify(result));
  if (result.maxPackMs > typingMosaicMaxPackMs)
    throw new Error("Mosaic pack was too slow under typing load: " + JSON.stringify(result));
}

if (require.main === module) {
  run()
    .then((result) => {
      console.log(JSON.stringify(result, null, 2));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run };
