const { withServePage } = require("./serve_playwright_harness");

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
          typeof liveBusIsOpen === "function" &&
          Array.isArray(window.__spiceSubmitLatencySamples) &&
          Array.isArray(targets) &&
          targets.length > 0,
        { timeout: 10000 },
      );
      await installSubmitLatencySmokeHelpers(page);
      const result = await page.evaluate(runSubmitLatencySmokePage);
      assertSubmitLatencyResult(result);
      return { ...result, url: server.url };
    },
  );
}

async function installSubmitLatencySmokeHelpers(page) {
  await page.addScriptTag({
    content: [
      runSubmitLatencySmokePage,
      submitLatencySmokeLane,
      submitLatencySmokeTextarea,
      submitLatencySmokeLiveBusRequest,
      cleanupSubmitLatencySmokePage,
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
