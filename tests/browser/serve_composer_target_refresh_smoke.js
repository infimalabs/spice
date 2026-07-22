const {
  installScript: installRefreshFixture,
  runTargetRefreshScenario,
} = require("./composer_target_refresh_fixture");
const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");
const { installScript: installPayloadFactory } = require("./payload_factory");

const LANE_CONFIGS = [
  {
    agentName: "spice-a",
    branch: "main-a",
    draft: "Keep the first draft through refresh",
    handle: "COMPOSE-1kG0CGtj",
    phase: "todo",
    teamId: "composer-refresh-a",
  },
  {
    agentName: "spice-b",
    branch: "main-b",
    draft: "Keep the second draft through refresh",
    handle: "SERVE-1kG0CHab",
    phase: "review",
    teamId: "composer-refresh-b",
  },
];

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-composer-target-refresh-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await installIsolatedLaneFixture(page, {
        globals: [
          "applyLanesSubscribePayloads",
          "applyTargetsPayload",
          "laneClaimedTask",
          "laneClaimedTaskLabel",
          "renderComposerAttachmentStrips",
          "renderComposerQuoteBands",
          "refreshTargets",
          "syncComposerShards",
        ],
      });
      await page.addScriptTag({ content: installPayloadFactory });
      await page.addScriptTag({ content: installRefreshFixture });
      const result = await page.evaluate(runTargetRefreshScenario, LANE_CONFIGS);
      assertTargetRefreshResult(result, LANE_CONFIGS);
      return { ...result, url: server.url };
    },
  );
}

function assertTargetRefreshResult(result, configs) {
  const expectedOrder = [
    "initial targets.payload",
    "global targets.refresh start",
    "targets.refresh request",
    "targets.payload applied",
    "lanes.subscribe recovery",
    "authoritative claim release",
  ];
  if (JSON.stringify(result.eventOrder) !== JSON.stringify(expectedOrder))
    throw new Error("refresh event order diverged: " + JSON.stringify(result));
  if (result.disappearanceDurationMs !== 0)
    throw new Error("task context disappeared during refresh: " + JSON.stringify(result));
  for (const phase of [result.before, result.afterTargets, result.afterSubscribe]) {
    phase.forEach((snapshot, index) => {
      const expectedAttachment = "attachment-" + index;
      const expectedQuote = "quote-" + index;
      if (
        snapshot.draft !== configs[index].draft ||
        snapshot.attachmentIds.join(",") !== expectedAttachment ||
        snapshot.quoteDraftIds.join(",") !== expectedQuote ||
        snapshot.quoteDraftText !== "follow-up " + index ||
        snapshot.quoteTextareaValue !== "follow-up " + index ||
        snapshot.claimedTaskLabel !==
          configs[index].handle + ", " + configs[index].phase ||
        snapshot.taskPresent !== true ||
        snapshot.textareaSame !== true ||
        snapshot.quoteTextareaSame !== true
      )
        throw new Error("composer state changed during refresh: " + JSON.stringify(result));
    });
  }
  if (
    result.afterRelease[0].claimedTaskLabel !== "" ||
    result.afterRelease[1].claimedTaskLabel !==
      configs[1].handle + ", " + configs[1].phase ||
    result.afterRelease.some((snapshot, index) => {
      return (
        snapshot.draft !== configs[index].draft ||
        snapshot.attachmentIds.join(",") !== "attachment-" + index ||
        snapshot.quoteDraftIds.join(",") !== "quote-" + index
      );
    })
  )
    throw new Error(
      "authoritative claim release changed unrelated composer state: " +
        JSON.stringify(result),
    );
}

if (require.main === module) {
  run()
    .then((result) => console.log(JSON.stringify(result, null, 2)))
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run };
