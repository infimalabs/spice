const os = require("os");
const path = require("path");
const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const SCREENSHOT_PATH = path.join(os.tmpdir(), "spice-task-filter-pills.png");

function assertPill(pills, label, expected) {
  const pill = pills.find((item) => item.label === label);
  if (!pill) throw new Error("missing " + label + " pill: " + JSON.stringify(pills));
  for (const [key, value] of Object.entries(expected)) {
    if (pill[key] !== value)
      throw new Error(
        label + " " + key + " mismatch: " + JSON.stringify(pill),
      );
  }
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-task-filter-pills-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page }) => {
      await installIsolatedLaneFixture(page, {
        globals: ["applyTaskFilterInventory", "renderFilterPills"],
      });
      const pills = await page.evaluate(() => {
        const lane = resolveIsolatedLane("task-filter-pill-smoke");
        lane.lifetime = "Drain";
        lane.lastRenderedStatusLine = { agentProcessStatus: "running" };
        applyTaskFilterInventory({
          revision: "9999999999999999999999999999",
          catalog: {
            approvedStems: ["serve", "studies", "cli", "tests"],
          },
          filters: [],
          primaryStems: [
            {
              name: "serve",
              filters: ["serve.ui"],
              openTaskCount: 5,
              readyTaskCount: 2,
              inFlightTaskCount: 1,
              blockedTaskCount: 1,
              deferredTaskCount: 1,
            },
            {
              name: "studies",
              filters: ["studies.design"],
              openTaskCount: 3,
              readyTaskCount: 0,
              inFlightTaskCount: 0,
              blockedTaskCount: 0,
              deferredTaskCount: 3,
            },
            {
              name: "cli",
              filters: ["cli.config"],
              openTaskCount: 1,
              readyTaskCount: 0,
              inFlightTaskCount: 0,
              blockedTaskCount: 1,
              deferredTaskCount: 0,
            },
            {
              name: "tests",
              filters: ["tests.compat"],
              openTaskCount: 3,
              readyTaskCount: 3,
              inFlightTaskCount: 0,
              blockedTaskCount: 0,
              deferredTaskCount: 0,
            },
            {
              name: "waiting",
              filters: [],
              openTaskCount: 3,
              readyTaskCount: 0,
              inFlightTaskCount: 0,
              blockedTaskCount: 0,
              deferredTaskCount: 3,
              waitingTaskCount: 3,
            },
          ],
        });
        renderFilterPills();
        return Array.from(document.querySelectorAll(".filter-pill")).map(
          (pill) => ({
            label: pill.querySelector(".filter-pill-label")?.textContent || "",
            count: pill.querySelector(".filter-pill-count")?.textContent || "",
            lit: pill.classList.contains("filter-pill--drainable"),
            dim: pill.classList.contains("filter-pill--undrainable"),
            implicit: pill.classList.contains("filter-pill--implicit"),
            title: pill.title,
          }),
        );
      });

      assertPill(pills, "serve", {
        count: "2/5",
        lit: true,
        dim: false,
        implicit: true,
      });
      assertPill(pills, "studies", {
        count: "0/3",
        dim: true,
        title:
          "0 ready / 3 open across studies.*; 0 in flight, 0 blocked, 3 deferred; route covered by 1",
      });
      assertPill(pills, "cli", {
        count: "0/1",
        dim: true,
      });
      assertPill(pills, "tests", {
        count: "3/3",
        lit: true,
        dim: false,
      });
      const labels = pills.map((pill) => pill.label).join(",");
      if (labels !== "serve,studies,cli,tests")
        throw new Error("unexpected header pills: " + labels);
      await page.screenshot({ path: SCREENSHOT_PATH });
      return { pills, screenshotPath: SCREENSHOT_PATH };
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
