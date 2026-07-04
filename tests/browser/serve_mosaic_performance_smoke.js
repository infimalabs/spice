const { withServePage } = require("./serve_playwright_harness");

// Mosaic §13 complexity contract: insert is O(candidates x 12 x span) --
// constant -- with at most two measurement reads (base and wide widths);
// full replay is O(n x 12) plus one measurement pass; no per-frame work
// happens outside real events (the live-text tick touches zero mosaic
// machinery). Instruments the actual forced-layout reads
// (offsetHeight/getBoundingClientRect/getComputedStyle) rather than trusting
// the code's own documented bound, against a 64-card board (comfortably
// past the "60+ card" floor the acceptance names).

const MOSAIC_PERF_BOARD_CARD_COUNT = 64;
const MOSAIC_PERF_MAX_READS_PER_INSERT = 2;
const MOSAIC_PERF_STEADY_STATE_TICKS = 3;

async function installMosaicPerfInstrumentation(page) {
  await page.addScriptTag({
    content: `
      window.__mosaicPerfInstall = function () {
        const counts = { offsetHeightMosaic: 0, offsetHeightOther: 0, getBoundingClientRect: 0, getComputedStyle: 0 };
        const isMosaicNode = (node) =>
          Boolean(node && node.dataset && (node.dataset.mosaicCard !== undefined || node.classList.contains("mosaic-plane")));
        const offsetHeightDescriptor = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");
        Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
          configurable: true,
          get() {
            if (isMosaicNode(this)) counts.offsetHeightMosaic += 1;
            else counts.offsetHeightOther += 1;
            return offsetHeightDescriptor.get.call(this);
          },
        });
        const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
        Element.prototype.getBoundingClientRect = function (...args) {
          counts.getBoundingClientRect += 1;
          return originalGetBoundingClientRect.apply(this, args);
        };
        const originalGetComputedStyle = window.getComputedStyle;
        window.getComputedStyle = function (...args) {
          counts.getComputedStyle += 1;
          return originalGetComputedStyle.apply(this, args);
        };
        window.__mosaicPerfCounts = counts;
        window.__mosaicPerfRestore = function () {
          Object.defineProperty(HTMLElement.prototype, "offsetHeight", offsetHeightDescriptor);
          Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
          window.getComputedStyle = originalGetComputedStyle;
        };
      };
      window.__mosaicPerfReset = function () {
        for (const key of Object.keys(window.__mosaicPerfCounts)) window.__mosaicPerfCounts[key] = 0;
      };
      window.__mosaicPerfSnapshot = function () {
        return { ...window.__mosaicPerfCounts };
      };
    `,
  });
  await page.evaluate(() => window.__mosaicPerfInstall());
}

function mosaicPerfBoardItem(index, lines) {
  const html = Array.from({ length: lines }, (_, i) => "line " + (i + 1)).join("<br>");
  return {
    ack_count: 0,
    ack_keys: [],
    index,
    key: "perf-" + index,
    kind: "assistant",
    timestamp: new Date(2026, 0, 1, 0, 0, index).toISOString(),
    display_html: html,
    display_text: html,
    text: html,
  };
}

async function buildMosaicPerfBoard(page, cardCount) {
  await page.addScriptTag({ content: mosaicPerfBoardItem.toString() });
  return page.evaluate((count) => {
    const teamId = "mosaic-perf-smoke-team";
    const targetId = emptyTeamTargetId(teamId);
    if (!laneStates.has(targetId)) addEmptyTeamLane({ teamId, revision: 1, config: {} });
    const lane = laneStates.get(targetId);
    lane.emptyTeam = false;
    const items = [];
    for (let index = 0; index < count; index += 1) {
      items.push(mosaicPerfBoardItem(index, index % 5 === 0 ? 18 : 1 + (index % 3)));
    }
    lane.knownMessages = items.slice();
    lane.knownMessageKeys = new Set(items.map((item) => item.key));
    lane.retainedMessageLimit = items.length;
    lane.renderedMessageFingerprint = "";
    renderMessagesIfChanged(lane);
    window.__mosaicPerfLaneTargetId = targetId;
    return { cardCount: lane.mosaicCards.length };
  }, cardCount);
}

// §13: a single new message is the fast insert path -- at most two content
// measurements (base width, wide width), never a pass over the whole board.
async function measureSingleInsert(page) {
  return page.evaluate(() => {
    const lane = laneStates.get(window.__mosaicPerfLaneTargetId);
    window.__mosaicPerfReset();
    const item = mosaicPerfBoardItem(9000, 2);
    lane.knownMessages.unshift(item);
    lane.knownMessageKeys.add(item.key);
    lane.newestMessageKey = item.key;
    renderMessagesIfChanged(lane);
    return window.__mosaicPerfSnapshot();
  });
}

// §13: a full replay (geometry actually changed) re-measures every
// surviving card exactly once -- "one measurement pass" -- never more than
// the §5 two-reads-per-card ceiling times the board size.
async function measureFullReplay(page, cardCount) {
  await page.setViewportSize({ width: 900, height: 900 });
  return page.evaluate((count) => {
    const lane = laneStates.get(window.__mosaicPerfLaneTargetId);
    window.__mosaicPerfReset();
    lane.renderedMessageFingerprint = "";
    renderMessagesIfChanged(lane);
    return { ...window.__mosaicPerfSnapshot(), cardCount: count };
  }, cardCount);
}

// §13: per-frame work outside real events is zero -- the live-text tick
// (updateLiveRelativeTimes, the real setInterval callback) must never touch
// mosaic measurement or packing machinery.
async function measureSteadyStateTicks(page, tickCount) {
  return page.evaluate((count) => {
    const lane = laneStates.get(window.__mosaicPerfLaneTargetId);
    const originalRender = mosaicRenderMessageStream;
    let renderCalls = 0;
    mosaicRenderMessageStream = function (...args) {
      renderCalls += 1;
      return originalRender.apply(this, args);
    };
    window.__mosaicPerfReset();
    try {
      for (let tick = 0; tick < count; tick += 1) updateLiveRelativeTimes();
    } finally {
      mosaicRenderMessageStream = originalRender;
    }
    return { ...window.__mosaicPerfSnapshot(), renderCalls, laneStillSettled: Boolean(lane.mosaicCards) };
  }, tickCount);
}

function assertSingleInsert(counts) {
  if (counts.offsetHeightMosaic > MOSAIC_PERF_MAX_READS_PER_INSERT)
    throw new Error(
      "single insert exceeded " + MOSAIC_PERF_MAX_READS_PER_INSERT +
        " content measurements: " + JSON.stringify(counts),
    );
}

function assertFullReplay(counts) {
  const ceiling = counts.cardCount * MOSAIC_PERF_MAX_READS_PER_INSERT;
  if (counts.offsetHeightMosaic === 0)
    throw new Error("full replay measured nothing: " + JSON.stringify(counts));
  if (counts.offsetHeightMosaic > ceiling)
    throw new Error(
      "full replay exceeded one measurement pass (>2 reads/card): " + JSON.stringify(counts),
    );
}

function assertSteadyStateTicks(counts) {
  if (counts.renderCalls !== 0)
    throw new Error("live-text tick triggered a mosaic render: " + JSON.stringify(counts));
  if (counts.offsetHeightMosaic !== 0)
    throw new Error("live-text tick read mosaic card heights: " + JSON.stringify(counts));
  if (!counts.laneStillSettled)
    throw new Error("lane lattice was disturbed by a steady-state tick: " + JSON.stringify(counts));
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-perf-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 900 } },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof addEmptyTeamLane === "function" &&
          typeof renderMessagesIfChanged === "function" &&
          typeof mosaicRenderMessageStream === "function" &&
          typeof updateLiveRelativeTimes === "function",
        { timeout: 10000 },
      );
      await installMosaicPerfInstrumentation(page);
      const board = await buildMosaicPerfBoard(page, MOSAIC_PERF_BOARD_CARD_COUNT);
      const singleInsert = await measureSingleInsert(page);
      const fullReplay = await measureFullReplay(page, board.cardCount + 1);
      const steadyState = await measureSteadyStateTicks(page, MOSAIC_PERF_STEADY_STATE_TICKS);
      return { board, singleInsert, fullReplay, steadyState, url: server.url };
    },
  );
}

function assertMosaicPerfResult(result) {
  assertSingleInsert(result.singleInsert);
  assertFullReplay(result.fullReplay);
  assertSteadyStateTicks(result.steadyState);
}

if (require.main === module) {
  run()
    .then((result) => {
      assertMosaicPerfResult(result);
      console.log(JSON.stringify(result, null, 2));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run, assertMosaicPerfResult };
