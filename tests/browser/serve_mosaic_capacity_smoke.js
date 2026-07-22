const { withServePage } = require("./serve_playwright_harness");

// Mosaic structural checks not covered elsewhere in the smoke suite:
// (1) the capacity rule's four L-transition thresholds (556/840/1124/1692px
// at a 16px root -- derived from minLane=17rem against each legal base
// span) hold exactly, not approximately, at a ±1px boundary; (2) skyline
// spread across a realistic mixed-height insertion sequence stays within
// about two tallest-card heights. The wide-placement structural
// property is already covered at the pure-engine level by
// tests/fixtures/mosaic_engine.js (see test_static_mosaic_engine_helpers_
// are_pure_and_covered) -- re-deriving it against rendered pixels here
// would be a noisier version of the same fixture-level proof.

const MOSAIC_CAPACITY_THRESHOLDS = [
  { widthPx: 556, belowSpan: 12, belowL: 1, atSpan: 6, atL: 2 },
  { widthPx: 840, belowSpan: 6, belowL: 2, atSpan: 4, atL: 3 },
  { widthPx: 1124, belowSpan: 4, belowL: 3, atSpan: 3, atL: 4 },
  { widthPx: 1692, belowSpan: 3, belowL: 4, atSpan: 2, atL: 6 },
];

const MOSAIC_CAPACITY_SKYLINE_CARD_COUNT = 36;

async function measureCapacityThresholds(page) {
  return page.evaluate(
    (thresholds) =>
      thresholds.map((threshold) => {
        const below = mosaicGeometry(16, threshold.widthPx - 1);
        const at = mosaicGeometry(16, threshold.widthPx);
        return {
          widthPx: threshold.widthPx,
          below: { baseSpan: below.baseSpan, L: below.L },
          at: { baseSpan: at.baseSpan, L: at.L },
        };
      }),
    MOSAIC_CAPACITY_THRESHOLDS,
  );
}

function assertCapacityThresholds(measured) {
  measured.forEach((result, index) => {
    const expected = MOSAIC_CAPACITY_THRESHOLDS[index];
    if (result.below.baseSpan !== expected.belowSpan || result.below.L !== expected.belowL)
      throw new Error(
        expected.widthPx - 1 + "px did not resolve to L=" + expected.belowL +
          "/span=" + expected.belowSpan + ": " + JSON.stringify(result),
      );
    if (result.at.baseSpan !== expected.atSpan || result.at.L !== expected.atL)
      throw new Error(
        expected.widthPx + "px did not resolve to L=" + expected.atL +
          "/span=" + expected.atSpan + ": " + JSON.stringify(result),
      );
  });
}

// A lone lane isolated from real backend targets (mirrors
// serve_mosaic_single_column_smoke.js's addEmptyTeamLane trick): a real
// viewport resize should still wire mosaicContainerWidthPx through to a
// visibly different card span, proving the pure threshold math above is
// not disconnected from the live render path.
async function measureWideViewportWiring(page) {
  await page.setViewportSize({ width: 1800, height: 900 });
  return page.evaluate(() => {
    const teamId = "mosaic-capacity-wiring-smoke-team";
    const targetId = emptyTeamTargetId(teamId);
    // Sandbox serve discovers real sibling-agent lanes off cwd (task-backend
    // does not isolate); once init reaches ready they share #swimlanes and
    // halve this fixture's measured width. Close every other lane so the
    // fixture owns the container and its geometry reflects the viewport, not
    // a split of it -- deterministic regardless of when topology settled.
    for (const other of laneStore.lanesSnapshot())
      if (other.targetId !== targetId) closeLaneCore(other);
    if (!laneStore.hasLane(targetId))
      addEmptyTeamLane({ teamId, revision: 1, config: {} });
    const lane = laneStore.laneForId(targetId);
    lane.emptyTeam = false;
    const item = {
      ack_count: 0,
      ack_keys: [],
      index: 1,
      key: "capacity-wiring",
      kind: "assistant",
      timestamp: new Date().toISOString(),
      display_html: "<p>wiring check</p>",
      display_text: "wiring check",
      text: "wiring check",
    };
    lane.knownMessages = [item];
    lane.knownMessageKeys = new Set([item.key]);
    lane.retainedMessageLimit = 1;
    lane.renderedMessageFingerprint = "";
    renderMessagesIfChanged(lane);
    const card = lane.mosaicCards.find((candidate) => candidate.key === item.key);
    return { span: card ? card.span : null, geometryL: lane.mosaicGeometry.L };
  });
}

function assertWideViewportWiring(result) {
  // 1800px clears the 1692px L=6/span=2 threshold with room for lane chrome.
  if (result.geometryL !== 6 || result.span !== 2)
    throw new Error(
      "wide viewport did not wire through to L=6/span=2: " + JSON.stringify(result),
    );
}

function mosaicCapacitySkylineItem(index, lines) {
  const html = Array.from({ length: lines }, (_, i) => "line " + (i + 1)).join("<br>");
  return {
    ack_count: 0,
    ack_keys: [],
    index,
    key: "skyline-" + index,
    kind: "assistant",
    timestamp: new Date(2026, 0, 1, 0, 0, index).toISOString(),
    display_html: html,
    display_text: html,
    text: html,
  };
}

// Skyline spread (max(rowFloor) - min(rowFloor)) across a realistic
// mixed-height insertion sequence stays within about two tallest-card
// heights. Mostly short cards (1-2 lines) with occasional tall ones (18
// lines, clearing tallThreshold to trigger wide candidates) mirrors a
// realistic chat stream and exercises the packer's natural frontier
// behavior rather than a contrived worst case.
async function measureSkylineSpread(page) {
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.addScriptTag({
    content: mosaicCapacitySkylineItem.toString(),
  });
  return page.evaluate((cardCount) => {
    const teamId = "mosaic-capacity-skyline-smoke-team";
    const targetId = emptyTeamTargetId(teamId);
    // Same sandbox-lane isolation as measureWideViewportWiring above: the
    // fixture must own #swimlanes so its render span is the viewport's alone.
    for (const other of laneStore.lanesSnapshot())
      if (other.targetId !== targetId) closeLaneCore(other);
    if (!laneStore.hasLane(targetId))
      addEmptyTeamLane({ teamId, revision: 1, config: {} });
    const lane = laneStore.laneForId(targetId);
    lane.emptyTeam = false;
    const items = [];
    for (let index = 0; index < cardCount; index += 1) {
      const lines = index % 6 === 0 ? 18 : 1 + (index % 3);
      items.push(mosaicCapacitySkylineItem(index, lines));
    }
    lane.knownMessages = items.slice();
    lane.knownMessageKeys = new Set(items.map((item) => item.key));
    lane.retainedMessageLimit = items.length;
    lane.renderedMessageFingerprint = "";
    renderMessagesIfChanged(lane);
    const rowFloor = mosaicDeriveRowFloor(lane.mosaicCards, mosaicGridTrackCount);
    const tallestN = Math.max(...lane.mosaicCards.map((card) => card.n));
    return {
      rowFloor,
      spread: Math.max(...rowFloor) - Math.min(...rowFloor),
      tallestN,
      cardCount: lane.mosaicCards.length,
    };
  }, MOSAIC_CAPACITY_SKYLINE_CARD_COUNT);
}

function assertSkylineSpread(result) {
  if (result.cardCount !== MOSAIC_CAPACITY_SKYLINE_CARD_COUNT)
    throw new Error("skyline smoke did not place every card: " + JSON.stringify(result));
  if (result.spread > 2 * result.tallestN)
    throw new Error(
      "skyline spread exceeded twice the tallest card's rows: " + JSON.stringify(result),
    );
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-capacity-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 900 } },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof mosaicGeometry === "function" &&
          typeof mosaicDeriveRowFloor === "function" &&
          typeof addEmptyTeamLane === "function" &&
          typeof renderMessagesIfChanged === "function",
        { timeout: 10000 },
      );
      const thresholds = await measureCapacityThresholds(page);
      const wiring = await measureWideViewportWiring(page);
      const skyline = await measureSkylineSpread(page);
      return { thresholds, wiring, skyline, url: server.url };
    },
  );
}

function assertCapacitySmokeResult(result) {
  assertCapacityThresholds(result.thresholds);
  assertWideViewportWiring(result.wiring);
  assertSkylineSpread(result.skyline);
}

if (require.main === module) {
  run()
    .then((result) => {
      assertCapacitySmokeResult(result);
      console.log(JSON.stringify(result, null, 2));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run, assertCapacitySmokeResult };
