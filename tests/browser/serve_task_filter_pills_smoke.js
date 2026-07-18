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

function initialInventory() {
  return {
    revision: "9999999999999999999999999999",
    catalog: {
      approvedStems: ["serve", "studies", "cli", "tests", "lifecycle"],
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
        name: "lifecycle",
        filters: ["lifecycle.recovery"],
        openTaskCount: 2,
        readyTaskCount: 0,
        inFlightTaskCount: 2,
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
  };
}

async function readPills(page) {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll(".filter-pill")).map((pill) => ({
      label: pill.querySelector(".filter-pill-label")?.textContent || "",
      count: pill.querySelector(".filter-pill-count")?.textContent || "",
      tone: ["ready", "active", "dormant"].find((value) =>
        pill.classList.contains("filter-pill--" + value),
      ),
      implicit: pill.classList.contains("filter-pill--implicit"),
      unavailable: pill.dataset.unavailableTaskCount,
      title: pill.title,
    })),
  );
}

async function installInitialState(page) {
  await installIsolatedLaneFixture(page, {
    globals: ["applyTaskFilterInventory", "renderFilterPills"],
  });
  await page.evaluate((inventory) => {
    const lane = resolveIsolatedLane("task-filter-pill-smoke");
    lane.lifetime = "Drain";
    lane.lastRenderedStatusLine = { agentProcessStatus: "running" };
    applyTaskFilterInventory(inventory);
    renderFilterPills();
  }, initialInventory());
  return readPills(page);
}

function assertInitialPills(pills) {
  assertPill(pills, "serve", {
    count: "2r+1a+2u",
    tone: "ready",
    implicit: true,
    unavailable: "2",
  });
  assertPill(pills, "studies", {
    count: "0r+0a+3u",
    tone: "dormant",
    title:
      "0 ready, 0 active/in flight, 0 blocked, 3 deferred; 3 open across studies.*; no task currently movable",
  });
  assertPill(pills, "cli", {
    count: "0r+0a+1u",
    tone: "dormant",
    unavailable: "1",
  });
  assertPill(pills, "tests", { count: "3r+0a", tone: "ready" });
  assertPill(pills, "lifecycle", {
    count: "0r+2a",
    tone: "active",
    implicit: false,
    unavailable: "0",
  });
  const labels = pills.map((pill) => pill.label).join(",");
  if (labels !== "serve,studies,cli,tests,lifecycle")
    throw new Error("unexpected header pills: " + labels);
}

async function resolveCliBlocker(page) {
  await page.evaluate(() => {
    const lane = resolveIsolatedLane("task-filter-pill-smoke");
    const inventory = structuredClone(lane.taskFilterInventory);
    inventory.revision = "10000000000000000000000000000";
    inventory.primaryStems = inventory.primaryStems.map((stem) =>
      stem.name === "cli"
        ? { ...stem, readyTaskCount: 1, blockedTaskCount: 0 }
        : stem,
    );
    applyTaskFilterInventory(inventory);
    renderFilterPills();
  });
  return readPills(page);
}

async function clearInventory(page) {
  return page.evaluate(() => {
    const lane = resolveIsolatedLane("task-filter-pill-smoke");
    const inventory = structuredClone(lane.taskFilterInventory);
    inventory.revision = "100000000000000000000000000000";
    inventory.primaryStems = [];
    applyTaskFilterInventory(inventory);
    renderFilterPills();
    const strip = document.querySelector(".filter-strip");
    return {
      ariaHidden: strip?.getAttribute("aria-hidden"),
      pillCount: strip?.querySelectorAll(".filter-pill").length,
    };
  });
}

async function runScenario({ page }) {
  const pills = await installInitialState(page);
  assertInitialPills(pills);
  await page.screenshot({ path: SCREENSHOT_PATH });
  const resolvedPills = await resolveCliBlocker(page);
  assertPill(resolvedPills, "cli", {
    count: "1r+0a",
    tone: "ready",
    unavailable: "0",
  });
  const emptyState = await clearInventory(page);
  if (emptyState.ariaHidden !== "true" || emptyState.pillCount !== 0)
    throw new Error("empty inventory mismatch: " + JSON.stringify(emptyState));
  return { pills, resolvedPills, emptyState, screenshotPath: SCREENSHOT_PATH };
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-task-filter-pills-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    runScenario,
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
