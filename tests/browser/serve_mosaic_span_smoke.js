const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

// Mosaic candidate-span generation against the live DOM: (a) short
// content never clears tallThreshold and reads the base width only; (b)
// tall content clears it and reads exactly two widths (base, then wide);
// (c) on a flat frontier the wide candidate wins the placement-key tie,
// demonstrating widening is emergent from mosaicDecide, not chosen by
// mosaicCandidates itself. tallThreshold travels through the evaluate
// config (read live from the page's own MOSAIC_TALL_THRESHOLD_PX global)
// rather than a lexically captured Node-side constant, per the established
// smoke-test convention (see serve_mosaic_render_smoke.js).

const SPAN_SMOKE_WIDTH = 1200;
const SPAN_SMOKE_SHORT_LINES = 1;
const SPAN_SMOKE_TALL_LINES = 20;
const SPAN_SMOKE_EXPECTED_TALL_THRESHOLD_PX = 230;

function mosaicSpanResolveLane() {
  return resolveIsolatedLane("mosaic-span-smoke-team");
}

function mosaicSpanBuildItem(key, html) {
  return {
    ack_count: 0,
    ack_keys: [],
    index: 1,
    key,
    kind: "assistant",
    timestamp: new Date().toISOString(),
    display_html: html,
    display_text: html,
    text: html,
  };
}

// Mirrors mosaicSizingMeasureContentPx's detachment discipline (see
// serve_mosaic_sizing_smoke.js): align-self overrides the grid's
// single-auto-row stretch-squeeze so offsetHeight reflects real content.
function mosaicSpanMeasureAt(lane, lines, widthPx) {
  const host = lane.messagesEl;
  const html = Array.from(
    { length: lines },
    (_, index) => "line " + (index + 1),
  ).join("<br>");
  const node = renderMessage(lane, mosaicSpanBuildItem("span-probe-" + lines + "-" + widthPx, html));
  node.style.minHeight = "";
  node.style.alignSelf = "start";
  node.style.width = widthPx + "px";
  host.append(node);
  const px = node.offsetHeight;
  node.remove();
  return px;
}

function mosaicSpanCountingMeasure(lane, lines, edges, gap) {
  const calls = [];
  const measure = (span) => {
    calls.push(span);
    const widthPx = mosaicCardWidthPx(edges, 0, span, gap);
    return mosaicSpanMeasureAt(lane, lines, widthPx);
  };
  measure.calls = calls;
  return measure;
}

function mosaicSpanProbe() {
  const lane = mosaicSpanResolveLane();
  const geometry = mosaicGeometry(16, SPAN_SMOKE_WIDTH);
  const tracks = 12;

  const shortMeasure = mosaicSpanCountingMeasure(lane, SPAN_SMOKE_SHORT_LINES, geometry.edges, geometry.gap);
  const shortCandidates = mosaicCandidates(
    geometry.baseSpan,
    tracks,
    geometry.gap,
    geometry.M,
    shortMeasure,
  );

  const tallMeasure = mosaicSpanCountingMeasure(lane, SPAN_SMOKE_TALL_LINES, geometry.edges, geometry.gap);
  const tallCandidates = mosaicCandidates(
    geometry.baseSpan,
    tracks,
    geometry.gap,
    geometry.M,
    tallMeasure,
  );

  const flatRowFloor = new Array(tracks).fill(0);
  const decision =
    tallCandidates.length > 1
      ? mosaicDecide(tallCandidates, flatRowFloor, tracks)
      : null;

  return {
    tallThreshold: MOSAIC_TALL_THRESHOLD_PX,
    baseSpan: geometry.baseSpan,
    wideSpan: mosaicWideSpan(geometry.baseSpan, tracks),
    shortCalls: shortMeasure.calls,
    shortCandidateCount: shortCandidates.length,
    tallCalls: tallMeasure.calls,
    tallCandidateCount: tallCandidates.length,
    decisionSpan: decision && decision.span,
  };
}

async function installMosaicSpanHelpers(page) {
  await page.addScriptTag({
    content: [
      "const SPAN_SMOKE_WIDTH = " + SPAN_SMOKE_WIDTH + ";",
      "const SPAN_SMOKE_SHORT_LINES = " + SPAN_SMOKE_SHORT_LINES + ";",
      "const SPAN_SMOKE_TALL_LINES = " + SPAN_SMOKE_TALL_LINES + ";",
      mosaicSpanResolveLane,
      mosaicSpanBuildItem,
      mosaicSpanMeasureAt,
      mosaicSpanCountingMeasure,
      mosaicSpanProbe,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

function assertMosaicSpanResult(result) {
  const fail = (message) => {
    throw new Error(message + ": " + JSON.stringify(result));
  };
  if (result.tallThreshold !== SPAN_SMOKE_EXPECTED_TALL_THRESHOLD_PX)
    fail("MOSAIC_TALL_THRESHOLD_PX drifted from the documented constant");
  if (result.wideSpan <= result.baseSpan)
    fail("fixture viewport must have room to widen (wideSpan must exceed baseSpan)");
  if (result.shortCalls.length !== 1 || result.shortCalls[0] !== result.baseSpan)
    fail("short content must read exactly one width (baseSpan)");
  if (result.shortCandidateCount !== 1)
    fail("short content must yield exactly one candidate");
  if (
    result.tallCalls.length !== 2 ||
    result.tallCalls[0] !== result.baseSpan ||
    result.tallCalls[1] !== result.wideSpan
  )
    fail("tall content must read exactly two widths, base then wide");
  if (result.tallCandidateCount !== 2)
    fail("tall content must yield exactly two candidates");
  if (result.decisionSpan !== result.wideSpan)
    fail("a flat frontier must let the wide candidate win the placement-key tie");
}

async function waitForMosaicSpanGlobals(page) {
  await installIsolatedLaneFixture(page, {
    globals: [
      "renderMessage",
      "mosaicGeometry",
      "mosaicCandidates",
      "mosaicWideSpan",
      "mosaicDecide",
      "mosaicCardWidthPx",
      "MOSAIC_TALL_THRESHOLD_PX",
    ],
  });
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-span-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 900 } },
    },
    async ({ page, server }) => {
      await waitForMosaicSpanGlobals(page);
      await installMosaicSpanHelpers(page);
      const result = await page.evaluate(mosaicSpanProbe);
      assertMosaicSpanResult(result);
      return { ...result, url: server.url };
    },
  );
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
