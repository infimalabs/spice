const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const typingLatencyLaneCount = 4;
const typingLatencyCardsPerLane = 48;
const typingLatencyFrameBudgetMs = 16;
const typingLatencyInputText =
  "typing latency under mosaic load keeps the composer responsive";

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-composer-typing-latency-" + Date.now(),
      contextOptions: { viewport: { width: 1440, height: 900 } },
    },
    async ({ page, server }) => {
      await installIsolatedLaneFixture(page, {
        globals: ["mosaicRenderMessageStream", "renderMessagesIfChanged"],
      });
      await page.addScriptTag({
        content: [
          runTypingLatencySmokePage,
          typingLatencySmokeItems,
          typingLatencySmokeBodyHtml,
        ]
          .map((helper) => helper.toString())
          .join("\n"),
      });
      const result = await page.evaluate(runTypingLatencySmokePage, {
        cardCount: typingLatencyCardsPerLane,
        inputText: typingLatencyInputText,
        laneCount: typingLatencyLaneCount,
      });
      assertTypingLatencyResult(result);
      return { ...result, url: server.url };
    },
  );
}

async function runTypingLatencySmokePage(config) {
  const lanes = Array.from({ length: config.laneCount }, (_, index) =>
    resolveIsolatedLane("composer-typing-latency-smoke-team-" + index),
  );
  for (const lane of lanes) {
    const items = typingLatencySmokeItems(lane, config.cardCount);
    lane.knownMessages = items.slice();
    lane.knownMessageKeys = new Set(items.map((item) => item.key));
    lane.retainedMessageLimit = config.cardCount;
    lane.renderedMessageFingerprint = "";
    renderMessagesIfChanged(lane);
    syncComposerShards(laneGroupHost(lane), laneGroupMemberLanes(laneGroupHost(lane)));
  }
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const lane = lanes[0];
  const textarea =
    lane.shardTextareas.get(lane.targetId) || lane.element.querySelector("textarea");
  if (!textarea) throw new Error("no composer textarea for typing latency smoke");
  textarea.focus({ preventScroll: true });
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const originalSync = syncLanePaneMetrics;
  const originalRender = mosaicRenderMessageStream;
  const durations = [];
  let metricSyncCount = 0;
  let packCount = 0;
  syncLanePaneMetrics = function (...args) {
    metricSyncCount += 1;
    return originalSync.apply(this, args);
  };
  mosaicRenderMessageStream = function (...args) {
    packCount += 1;
    return originalRender.apply(this, args);
  };
  try {
    textarea.value = "";
    for (const char of config.inputText) {
      const start = performance.now();
      textarea.value += char;
      textarea.dispatchEvent(
        new InputEvent("input", {
          bubbles: true,
          data: char,
          inputType: "insertText",
        }),
      );
      durations.push(performance.now() - start);
    }
  } finally {
    syncLanePaneMetrics = originalSync;
    mosaicRenderMessageStream = originalRender;
  }
  return {
    inputLength: config.inputText.length,
    laneCount: lanes.length,
    maxInputMs: Math.max(...durations),
    metricSyncCount,
    packCount,
    valueLength: textarea.value.length,
  };
}

function typingLatencySmokeItems(lane, count) {
  const base = Date.parse("2026-07-03T10:00:00.000Z");
  return Array.from({ length: count }, (_, index) => ({
    ack_count: 0,
    ack_keys: [],
    display_html: typingLatencySmokeBodyHtml(index),
    display_text: "typing latency card " + index,
    index,
    key: "typing-latency-" + lane.targetId + "-" + index,
    kind: index % 13 === 0 ? "final" : "assistant",
    text: "typing latency card " + index,
    timestamp: new Date(base + index * 60000).toISOString(),
  }));
}

function typingLatencySmokeBodyHtml(index) {
  const sentence =
    "<p>Typing latency smoke card " +
    index +
    " carries enough prose to exercise Mosaic layout.</p>";
  if (index % 7 === 0)
    return sentence + "<pre><code>mosaicRenderMessageStream(lane)</code></pre>";
  if (index % 5 === 0) return sentence + sentence + sentence + sentence;
  return sentence + sentence;
}

function assertTypingLatencyResult(result) {
  if (result.laneCount < typingLatencyLaneCount)
    throw new Error("typing latency smoke did not create enough lanes");
  if (result.valueLength !== result.inputLength)
    throw new Error("typing latency smoke lost input text: " + JSON.stringify(result));
  if (result.metricSyncCount > 1)
    throw new Error("typing triggered pane metric syncs: " + JSON.stringify(result));
  if (result.packCount > 0)
    throw new Error("typing triggered message packing: " + JSON.stringify(result));
  if (result.maxInputMs > typingLatencyFrameBudgetMs)
    throw new Error("typing input handler exceeded frame budget: " + JSON.stringify(result));
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
