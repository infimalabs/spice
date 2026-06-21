// Browser coverage for the lane metric pane. It drives the real client render
// path (renderLaneMetricsPane -> laneMetricsRenderModel -> the vanilla DOM
// renderer) in a real Chromium against the served CSS, and asserts that when a
// server-derived summary changes the way it does under a work-follows-agent
// move, the pane re-renders the new numbers with no stale or duplicated cells.
//
// The membership-derived DERIVATION itself (lane_metric_summary) is exhaustively
// unit-tested in tests/test_teams.py; this smoke covers the live render/update
// of that summary in the browser, which the unit tests cannot.
const { withServePage } = require("./serve_playwright_harness");

const SIX_HOUR_RANGE_SECONDS = "21600";
const SIX_HOUR_BUCKET_SECONDS = 900;

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-team-metrics-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await waitForMetricsPane(page);
      await installMetricsSmokeHelpers(page);
      await page.evaluate(setupMetricsSmokePage);
      const before = await page.evaluate(readInitialMetricsSmokePage);
      await page.evaluate(updateMetricsSmokePage, {
        rangeSeconds: SIX_HOUR_RANGE_SECONDS,
      });
      const after = await page.evaluate(readUpdatedMetricsSmokePage);
      await page.evaluate(cleanupMetricsSmokePage);
      const result = { before, after };
      assertMetrics(result);
      return { ...result, url: server.url };
    },
  );
}

async function installMetricsSmokeHelpers(page) {
  await page.addScriptTag({
    content: [
      makeMetricsSmokeLane,
      readMetricsSmokeCells,
      countMetricsSmokeCells,
      readDistributionSmokeGroups,
      readDistributionSmokeDots,
      readDistributionSmokeLines,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

async function waitForMetricsPane(page) {
  await page.waitForFunction(
    () =>
      typeof renderLaneMetricsPane === "function" &&
      typeof laneMetricsRenderModel === "function" &&
      typeof lanesEl !== "undefined",
    { timeout: 10000 },
  );
}

async function setupMetricsSmokePage() {
  const metricSeriesCalls = [];
  const distributionPoints = [
    ["agent-a", 60, 0.75, 1, 2],
    ["agent-b", 60, 0.25, 1, 0],
    ["agent-a", 120, 0.5, 0, 1],
    ["agent-b", 120, 0.5, 0, 1],
  ].map(([agentId, bucketStart, share, claimed, active]) => ({
    bucketStart,
    agentId,
    value: share,
    share,
    claimed,
    active,
    work: claimed + active,
  }));
  liveBusRequest = (type, fields) => {
    metricSeriesCalls.push({ type, fields });
    if (fields.query.metric === "distribution")
      return Promise.resolve({
        result: { metric: "distribution", points: distributionPoints },
      });
    const value = metricSeriesCalls.length;
    return Promise.resolve({
      result: {
        points: [
          { bucketStart: 60, value },
          { bucketStart: 120, value: value + 1 },
        ],
      },
    });
  };
  const sourceGrid = document.createElement("div");
  const destGrid = document.createElement("div");
  sourceGrid.className = "lane-metric-grid";
  destGrid.className = "lane-metric-grid";
  lanesEl.append(sourceGrid, destGrid);
  const sourceSummary = document.createElement("span");
  const destSummary = document.createElement("span");
  const source = makeMetricsSmokeLane(sourceGrid, sourceSummary, 11, 22, 33, [1, 2]);
  const dest = makeMetricsSmokeLane(destGrid, destSummary, 4, 5, 6, [1]);
  window.__spiceMetricsSmoke = { source, dest, metricSeriesCalls };
  renderLaneMetricsPane(source);
  renderLaneMetricsPane(dest);
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function makeMetricsSmokeLane(grid, summary, acked, sends, toolCalls, sparkline) {
  return {
    targetId: "target-" + Math.random().toString(16).slice(2),
    targetThreadId: "",
    metricsGridEl: grid,
    metricsSummaryEl: summary,
    laneMetrics: { acked, sends, toolCalls, sparkline },
    serverReachable: true,
  };
}

function readInitialMetricsSmokePage() {
  const { source, dest, metricSeriesCalls } = window.__spiceMetricsSmoke;
  return {
    source: readMetricsSmokeCells(source.metricsGridEl),
    dest: readMetricsSmokeCells(dest.metricsGridEl),
    sourceCells: countMetricsSmokeCells(source.metricsGridEl),
    destCells: countMetricsSmokeCells(dest.metricsGridEl),
    sourceStatus: source.metricsSummaryEl.textContent,
    seriesSvg: Boolean(source.metricsGridEl.querySelector(".lane-metric-series-svg")),
    firstMetricQuery: metricSeriesCalls[0] && metricSeriesCalls[0].fields.query,
  };
}

function readMetricsSmokeCells(grid) {
  const map = {};
  for (const cell of grid.querySelectorAll(".lane-metric-cell")) {
    const label = cell.querySelector(".lane-metric-label");
    const value = cell.querySelector(".lane-metric-value");
    if (label && label.textContent) map[label.textContent] = value.textContent;
  }
  return map;
}

function countMetricsSmokeCells(grid) {
  return grid.querySelectorAll(".lane-metric-cell").length;
}

async function updateMetricsSmokePage({ rangeSeconds }) {
  const { source, dest } = window.__spiceMetricsSmoke;
  source.laneMetrics = { acked: 1, sends: 2, toolCalls: 3, sparkline: [1] };
  dest.laneMetrics = { acked: 14, sends: 25, toolCalls: 36, sparkline: [2, 1] };
  renderLaneMetricsPane(source);
  renderLaneMetricsPane(dest);
  const lensSelect = source.metricsGridEl.querySelector(
    'select[aria-label="Metric lens"]',
  );
  lensSelect.value = "perSession";
  lensSelect.dispatchEvent(new Event("change", { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 0));
  const metricSelect = source.metricsGridEl.querySelector(
    'select[aria-label="Metric metric"]',
  );
  metricSelect.value = "distribution";
  metricSelect.dispatchEvent(new Event("change", { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 0));
  const rangeSelect = source.metricsGridEl.querySelector(
    'select[aria-label="Metric rangeSeconds"]',
  );
  rangeSelect.value = rangeSeconds;
  rangeSelect.dispatchEvent(new Event("change", { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function readUpdatedMetricsSmokePage() {
  const { source, dest, metricSeriesCalls } = window.__spiceMetricsSmoke;
  return {
    source: readMetricsSmokeCells(source.metricsGridEl),
    dest: readMetricsSmokeCells(dest.metricsGridEl),
    sourceCells: countMetricsSmokeCells(source.metricsGridEl),
    destCells: countMetricsSmokeCells(dest.metricsGridEl),
    distributionGroups: readDistributionSmokeGroups(source.metricsGridEl),
    distributionDots: readDistributionSmokeDots(source.metricsGridEl),
    distributionLineCount: source.metricsGridEl.querySelectorAll(
      ".lane-metric-series-line",
    ).length,
    distributionLines: readDistributionSmokeLines(source.metricsGridEl),
    metricSeriesCalls,
  };
}

function readDistributionSmokeGroups(grid) {
  return [...grid.querySelectorAll(".lane-metric-series-group")].map((group) =>
    group.getAttribute("data-agent-id"),
  );
}

function readDistributionSmokeDots(grid) {
  return [...grid.querySelectorAll(".lane-metric-series-dot")].map((dot) =>
    dot.getAttribute("data-agent-id"),
  );
}

function readDistributionSmokeLines(grid) {
  const distributionLines = {};
  for (const group of grid.querySelectorAll(".lane-metric-series-group")) {
    const line = group.querySelector(".lane-metric-series-line");
    distributionLines[group.getAttribute("data-agent-id")] =
      line && line.getAttribute("points");
  }
  return distributionLines;
}

function cleanupMetricsSmokePage() {
  const { source, dest } = window.__spiceMetricsSmoke;
  source.metricsGridEl.remove();
  dest.metricsGridEl.remove();
  delete window.__spiceMetricsSmoke;
}

function assertMetrics(result) {
  const { before, after } = result;
  if (before.sourceStatus !== "live")
    throw new Error("expected live status, got " + before.sourceStatus);
  if (!before.seriesSvg) throw new Error("expected metric series SVG to render");
  if (!before.firstMetricQuery || before.firstMetricQuery.metric !== "activity")
    throw new Error("expected initial activity metric query");
  // Before the move: source shows agent-a + agent-c; destination shows agent-b.
  expect(before.source, { acked: "11", sends: "22", "tool calls": "33" }, "before.source");
  expect(before.dest, { acked: "4", sends: "5", "tool calls": "6" }, "before.dest");
  // After the move: agent-a's counters left the source and followed to the dest.
  expect(after.source, { acked: "1", sends: "2", "tool calls": "3" }, "after.source");
  expect(after.dest, { acked: "14", sends: "25", "tool calls": "36" }, "after.dest");
  // The vanilla renderer uses replaceChildren, so re-render must not leave stale
  // or duplicated cells: the cell count is identical before and after.
  if (before.sourceCells !== after.sourceCells || before.destCells !== after.destCells)
    throw new Error(
      "cell count changed across re-render (stale/duplicate cells): " +
        JSON.stringify({ before, after }),
    );
  if (!after.metricSeriesCalls.some((call) => call.fields.query.lens === "perSession"))
    throw new Error("lens toggle did not re-query perSession series");
  expectArray(
    after.distributionGroups,
    ["agent-a", "agent-b"],
    "distributionGroups",
  );
  expectArray(
    after.distributionDots,
    ["agent-a", "agent-a", "agent-b", "agent-b"],
    "distributionDots",
  );
  if (after.distributionLineCount !== 2)
    throw new Error("expected one distribution line per agent");
  expect(
    after.distributionLines,
    {
      "agent-a": "0.0,10.0 120.0,18.0",
      "agent-b": "0.0,26.0 120.0,18.0",
    },
    "distributionLines",
  );
  const distributionCalls = after.metricSeriesCalls.filter(
    (call) => call.fields.query.metric === "distribution",
  );
  if (!distributionCalls.length) throw new Error("distribution metric was not queried");
  if (!distributionCalls.some((call) => call.fields.query.lens === "perSession"))
    throw new Error("distribution lens query did not keep perSession");
  if (
    !distributionCalls.some(
      (call) => call.fields.query.bucketSeconds === SIX_HOUR_BUCKET_SECONDS,
    )
  )
    throw new Error("range toggle did not re-query distribution at 6h bucket size");
}

function expect(actual, expected, label) {
  for (const [key, value] of Object.entries(expected)) {
    if (actual[key] !== value)
      throw new Error(
        label + "." + key + " = " + actual[key] + ", expected " + value,
      );
  }
}

function expectArray(actual, expected, label) {
  const serializedActual = JSON.stringify(actual);
  const serializedExpected = JSON.stringify(expected);
  if (serializedActual !== serializedExpected)
    throw new Error(label + " = " + serializedActual + ", expected " + serializedExpected);
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
