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

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-team-metrics-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof renderLaneMetricsPane === "function" &&
          typeof laneMetricsRenderModel === "function" &&
          typeof lanesEl !== "undefined",
        { timeout: 10000 },
      );

      const result = await page.evaluate(async () => {
        const metricSeriesCalls = [];
        liveBusRequest = (type, fields) => {
          metricSeriesCalls.push({ type, fields });
          return Promise.resolve({
            result: {
              points: [
                { bucketStart: 60, value: metricSeriesCalls.length },
                { bucketStart: 120, value: metricSeriesCalls.length + 1 },
              ],
            },
          });
        };
        function makeLane(metrics) {
          const grid = document.createElement("div");
          grid.className = "lane-metric-grid";
          const summary = document.createElement("span");
          lanesEl.append(grid);
          return {
            targetId: "target-" + Math.random().toString(16).slice(2),
            targetThreadId: "",
            metricsGridEl: grid,
            metricsSummaryEl: summary,
            laneMetrics: metrics,
            serverReachable: true,
          };
        }
        function readCells(grid) {
          const map = {};
          for (const cell of grid.querySelectorAll(".lane-metric-cell")) {
            const label = cell.querySelector(".lane-metric-label");
            const value = cell.querySelector(".lane-metric-value");
            if (label && label.textContent) map[label.textContent] = value.textContent;
          }
          return map;
        }
        function cellCount(grid) {
          return grid.querySelectorAll(".lane-metric-cell").length;
        }

        // Source holds agent-a (10/20/30) + agent-c (1/2/3); destination holds
        // agent-b (4/5/6). Numbers mirror the composer-move unit test.
        const source = makeLane({ acked: 11, sends: 22, toolCalls: 33, sparkline: [1, 2] });
        const dest = makeLane({ acked: 4, sends: 5, toolCalls: 6, sparkline: [1] });
        renderLaneMetricsPane(source);
        renderLaneMetricsPane(dest);
        await new Promise((resolve) => setTimeout(resolve, 0));
        const before = {
          source: readCells(source.metricsGridEl),
          dest: readCells(dest.metricsGridEl),
          sourceCells: cellCount(source.metricsGridEl),
          destCells: cellCount(dest.metricsGridEl),
          sourceStatus: source.metricsSummaryEl.textContent,
          seriesSvg: Boolean(source.metricsGridEl.querySelector(".lane-metric-series-svg")),
          firstMetricQuery: metricSeriesCalls[0] && metricSeriesCalls[0].fields.query,
        };

        // agent-a moves to the destination: its counters leave the source lane
        // and land on the destination lane (work follows the agent).
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
        const after = {
          source: readCells(source.metricsGridEl),
          dest: readCells(dest.metricsGridEl),
          sourceCells: cellCount(source.metricsGridEl),
          destCells: cellCount(dest.metricsGridEl),
          metricSeriesCalls,
        };

        source.metricsGridEl.remove();
        dest.metricsGridEl.remove();
        return { before, after };
      });

      assertMetrics(result);
      return { ...result, url: server.url };
    },
  );
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
}

function expect(actual, expected, label) {
  for (const [key, value] of Object.entries(expected)) {
    if (actual[key] !== value)
      throw new Error(
        label + "." + key + " = " + actual[key] + ", expected " + value,
      );
  }
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
