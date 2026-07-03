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
      const sample = await page.evaluate(runSubmitLatencySmokePage);
      assertSubmitLatencySample(sample);
      return { sample, url: server.url };
    },
  );
}

async function installSubmitLatencySmokeHelpers(page) {
  await page.addScriptTag({
    content: [
      runSubmitLatencySmokePage,
      submitLatencySmokeLane,
      submitLatencySmokeTextarea,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

async function runSubmitLatencySmokePage() {
  const lane = submitLatencySmokeLane();
  const textarea = submitLatencySmokeTextarea(lane);
  if (Array.isArray(window.__spiceSubmitLatencySamples))
    window.__spiceSubmitLatencySamples.length = 0;
  textarea.value = "submit latency smoke " + Date.now();
  textarea.dispatchEvent(new Event("input", { bubbles: true }));
  submitLaneForm(lane, new Event("submit", { cancelable: true }), lane.targetId);
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    const samples = window.__spiceSubmitLatencySamples || [];
    const sample = samples[samples.length - 1];
    if (sample && sample.status === "accepted") return sample;
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

function assertSubmitLatencySample(sample) {
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
