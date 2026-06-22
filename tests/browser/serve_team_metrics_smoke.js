// Browser coverage for the lane metric pane. It drives the real client render
// path (renderLaneMetricsPane -> laneMetricsRenderModel -> the vanilla DOM
// renderer) in a real Chromium against the served CSS. It asserts that when a
// server-derived summary changes the way it does under a work-follows-agent
// move, the pane re-renders the new numbers while preserving metric controls
// and giving the graph the remaining metrics pane area.
//
// The membership-derived DERIVATION itself (lane_metric_summary) is exhaustively
// unit-tested in tests/test_teams.py; this smoke covers the live render/update
// of that summary in the browser, which the unit tests cannot. It also asserts
// the graph-first pane order and that metric controls are reused across refreshes.
const { withServePage } = require("./serve_playwright_harness");

const SIX_HOUR_RANGE_SECONDS = "21600";
const SIX_HOUR_BUCKET_SECONDS = 900;
const MIN_METRICS_CHART_HEIGHT_PX = 96;
const MIN_METRICS_GRID_WIDTH_PX = 520;

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
      makeMetricsSmokePanel,
      readMetricsSmokeCells,
      countMetricsSmokeCells,
      readMetricsSmokeOrder,
      metricsSmokeSelects,
      readMetricsSmokeSelectState,
      readMetricsSmokeLayout,
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
  const sourcePanel = makeMetricsSmokePanel("source");
  const destPanel = makeMetricsSmokePanel("dest");
  lanesEl.append(sourcePanel.panel, destPanel.panel);
  const source = makeMetricsSmokeLane(
    sourcePanel.panel,
    sourcePanel.grid,
    sourcePanel.summary,
    11,
    22,
    33,
    [1, 2],
  );
  const dest = makeMetricsSmokeLane(
    destPanel.panel,
    destPanel.grid,
    destPanel.summary,
    4,
    5,
    6,
    [1],
  );
  window.__spiceMetricsSmoke = {
    source,
    dest,
    metricSeriesCalls,
    sourceSelects: null,
    focusStableAfterRefresh: false,
  };
  renderLaneMetricsPane(source);
  renderLaneMetricsPane(dest);
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function makeMetricsSmokePanel(label) {
  const panel = document.createElement("section");
  panel.className = "lane-view-panel lane-view-panel--active";
  panel.dataset.laneViewPanel = "metrics";
  panel.style.setProperty("--lane-pane-expanded-height", "480px");
  const head = document.createElement("div");
  head.className = "lane-pane-head";
  const title = document.createElement("span");
  title.textContent = label + " metrics";
  const summary = document.createElement("span");
  head.append(title, summary);
  const grid = document.createElement("div");
  grid.className = "lane-metrics-grid";
  panel.append(head, grid);
  return { panel, grid, summary };
}

function makeMetricsSmokeLane(
  panel,
  grid,
  summary,
  acked,
  sends,
  toolCalls,
  sparkline,
) {
  return {
    targetId: "target-" + Math.random().toString(16).slice(2),
    targetThreadId: "",
    metricsPanelEl: panel,
    metricsGridEl: grid,
    metricsSummaryEl: summary,
    laneMetrics: { acked, sends, toolCalls, sparkline },
    serverReachable: true,
  };
}

function readInitialMetricsSmokePage() {
  const { source, dest, metricSeriesCalls } = window.__spiceMetricsSmoke;
  window.__spiceMetricsSmoke.sourceSelects = metricsSmokeSelects(source.metricsGridEl);
  return {
    source: readMetricsSmokeCells(source.metricsGridEl),
    dest: readMetricsSmokeCells(dest.metricsGridEl),
    sourceCells: countMetricsSmokeCells(source.metricsGridEl),
    destCells: countMetricsSmokeCells(dest.metricsGridEl),
    sourceOrder: readMetricsSmokeOrder(source.metricsGridEl),
    sourceStatus: source.metricsSummaryEl.textContent,
    seriesSvg: Boolean(source.metricsGridEl.querySelector(".lane-metric-series-svg")),
    firstMetricQuery: metricSeriesCalls[0] && metricSeriesCalls[0].fields.query,
    selectState: readMetricsSmokeSelectState(source.metricsGridEl),
    layout: readMetricsSmokeLayout(source.metricsGridEl),
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

function readMetricsSmokeOrder(grid) {
  return [...grid.children].map((child) => {
    if (child.classList.contains("lane-metric-series-chart"))
      return child.querySelector(".lane-metric-series-svg")
        ? "series-chart:svg"
        : "series-chart:empty";
    if (child.classList.contains("lane-metric-series-controls"))
      return "series-controls";
    const label = child.querySelector(".lane-metric-label");
    return "cell:" + (label ? label.textContent : child.className);
  });
}

async function updateMetricsSmokePage({ rangeSeconds }) {
  const { source, dest } = window.__spiceMetricsSmoke;
  const initialSelects = window.__spiceMetricsSmoke.sourceSelects;
  initialSelects.lens.focus();
  source.laneMetrics = { acked: 1, sends: 2, toolCalls: 3, sparkline: [1] };
  dest.laneMetrics = { acked: 14, sends: 25, toolCalls: 36, sparkline: [2, 1] };
  renderLaneMetricsPane(source);
  renderLaneMetricsPane(dest);
  window.__spiceMetricsSmoke.focusStableAfterRefresh =
    document.activeElement === initialSelects.lens;
  const lensSelect = metricsSmokeSelects(source.metricsGridEl).lens;
  lensSelect.value = "perSession";
  lensSelect.dispatchEvent(new Event("change", { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 0));
  const metricSelect = metricsSmokeSelects(source.metricsGridEl).metric;
  metricSelect.value = "distribution";
  metricSelect.dispatchEvent(new Event("change", { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 0));
  const rangeSelect = metricsSmokeSelects(source.metricsGridEl).rangeSeconds;
  rangeSelect.value = rangeSeconds;
  rangeSelect.dispatchEvent(new Event("change", { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function readUpdatedMetricsSmokePage() {
  const {
    source,
    dest,
    metricSeriesCalls,
    sourceSelects,
    focusStableAfterRefresh,
  } = window.__spiceMetricsSmoke;
  const currentSelects = metricsSmokeSelects(source.metricsGridEl);
  return {
    source: readMetricsSmokeCells(source.metricsGridEl),
    dest: readMetricsSmokeCells(dest.metricsGridEl),
    sourceCells: countMetricsSmokeCells(source.metricsGridEl),
    destCells: countMetricsSmokeCells(dest.metricsGridEl),
    sourceOrder: readMetricsSmokeOrder(source.metricsGridEl),
    distributionGroups: readDistributionSmokeGroups(source.metricsGridEl),
    distributionDots: readDistributionSmokeDots(source.metricsGridEl),
    distributionLineCount: source.metricsGridEl.querySelectorAll(
      ".lane-metric-series-line",
    ).length,
    distributionLines: readDistributionSmokeLines(source.metricsGridEl),
    metricSeriesCalls,
    selectState: readMetricsSmokeSelectState(source.metricsGridEl),
    selectsStable:
      currentSelects.metric === sourceSelects.metric &&
      currentSelects.lens === sourceSelects.lens &&
      currentSelects.rangeSeconds === sourceSelects.rangeSeconds,
    focusStableAfterRefresh,
    layout: readMetricsSmokeLayout(source.metricsGridEl),
  };
}

function metricsSmokeSelects(grid) {
  return {
    metric: grid.querySelector('select[aria-label="Metric metric"]'),
    lens: grid.querySelector('select[aria-label="Metric lens"]'),
    rangeSeconds: grid.querySelector('select[aria-label="Metric rangeSeconds"]'),
  };
}

function readMetricsSmokeSelectState(grid) {
  const selects = metricsSmokeSelects(grid);
  return {
    metric: selects.metric && selects.metric.value,
    lens: selects.lens && selects.lens.value,
    rangeSeconds: selects.rangeSeconds && selects.rangeSeconds.value,
  };
}

function readMetricsSmokeLayout(grid) {
  const controls = grid.querySelector(".lane-metric-series-controls");
  const chart = grid.querySelector(".lane-metric-series-chart");
  const svg = grid.querySelector(".lane-metric-series-svg");
  const gridRect = grid.getBoundingClientRect();
  const controlsRect = controls.getBoundingClientRect();
  const chartRect = chart.getBoundingClientRect();
  const svgRect = svg.getBoundingClientRect();
  const summaryCells = [...grid.querySelectorAll(".lane-metric-cell")];
  return {
    gridHeight: Math.round(gridRect.height),
    gridWidth: Math.round(gridRect.width),
    chartBeforeControls: chartRect.bottom <= controlsRect.top,
    summaryBelowControls: summaryCells.every(
      (cell) => cell.getBoundingClientRect().top >= controlsRect.bottom - 1,
    ),
    chartHeight: Math.round(chartRect.height),
    chartWidth: Math.round(chartRect.width),
    chartTopGap: Math.round(chartRect.top - gridRect.top),
    svgHeight: Math.round(svgRect.height),
    svgWidth: Math.round(svgRect.width),
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
  source.metricsPanelEl.remove();
  dest.metricsPanelEl.remove();
  delete window.__spiceMetricsSmoke;
}

function assertMetrics(result) {
  const { before, after } = result;
  if (before.sourceStatus !== "live")
    throw new Error("expected live status, got " + before.sourceStatus);
  if (!before.seriesSvg) throw new Error("expected metric series SVG to render");
  if (!before.firstMetricQuery || before.firstMetricQuery.metric !== "activity")
    throw new Error("expected initial activity metric query");
  const expectedSourceOrder = [
    "series-chart:svg",
    "series-controls",
    "cell:drained",
    "cell:acked",
    "cell:sends",
    "cell:tool calls",
    "cell:uptime",
    "cell:activity",
  ];
  expectArray(before.sourceOrder, expectedSourceOrder, "before.sourceOrder");
  expectArray(after.sourceOrder, expectedSourceOrder, "after.sourceOrder");
  expect(
    before.selectState,
    { metric: "activity", lens: "lineage", rangeSeconds: "3600" },
    "before.selectState",
  );
  assertMetricsLayout(before.layout, "before.layout");
  // Before the move: source shows agent-a + agent-c; destination shows agent-b.
  expect(before.source, { acked: "11", sends: "22", "tool calls": "33" }, "before.source");
  expect(before.dest, { acked: "4", sends: "5", "tool calls": "6" }, "before.dest");
  // After the move: agent-a's counters left the source and followed to the dest.
  expect(after.source, { acked: "1", sends: "2", "tool calls": "3" }, "after.source");
  expect(after.dest, { acked: "14", sends: "25", "tool calls": "36" }, "after.dest");
  // The vanilla renderer reconciles children by slot, so re-render must not leave
  // stale or duplicated cells: the cell count is identical before and after.
  if (before.sourceCells !== after.sourceCells || before.destCells !== after.destCells)
    throw new Error(
      "cell count changed across re-render (stale/duplicate cells): " +
        JSON.stringify({ before, after }),
    );
  if (!after.selectsStable)
    throw new Error("metric/lens/range selects were replaced across refresh");
  if (!after.focusStableAfterRefresh)
    throw new Error("focused metric lens select did not survive refresh");
  expect(
    after.selectState,
    { metric: "distribution", lens: "perSession", rangeSeconds: SIX_HOUR_RANGE_SECONDS },
    "after.selectState",
  );
  assertMetricsLayout(after.layout, "after.layout");
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

function assertMetricsLayout(layout, label) {
  if (!layout.chartBeforeControls)
    throw new Error(
      label + " chart overlaps or follows the controls: " + JSON.stringify(layout),
    );
  if (!layout.summaryBelowControls)
    throw new Error(label + " summary cells are not below controls: " + JSON.stringify(layout));
  if (layout.chartHeight < MIN_METRICS_CHART_HEIGHT_PX)
    throw new Error(label + " chart did not use top metrics area: " + JSON.stringify(layout));
  if (layout.gridWidth < MIN_METRICS_GRID_WIDTH_PX)
    throw new Error(
      label + " grid did not use available horizontal space: " + JSON.stringify(layout),
    );
  if (Math.abs(layout.chartTopGap) > 2)
    throw new Error(label + " chart does not start at grid top: " + JSON.stringify(layout));
  if (layout.svgHeight < layout.chartHeight - 2)
    throw new Error(label + " svg does not fill chart height: " + JSON.stringify(layout));
  if (layout.chartWidth < layout.gridWidth - 2 || layout.svgWidth < layout.chartWidth - 2)
    throw new Error(label + " chart does not fill grid width: " + JSON.stringify(layout));
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
