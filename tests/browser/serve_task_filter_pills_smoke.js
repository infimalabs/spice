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

// getComputedStyle returns the resolved tone color as `rgb(...)`/`rgba(...)`
// (or a `color(srgb ...)` fallback with 0-1 channels); recover its HSL so the
// drain ramp can be checked in hue/saturation terms.
function parseRgb(value) {
  const channels = (value.match(/[\d.]+/g) || []).map(Number).slice(0, 3);
  if (channels.length < 3) throw new Error("unparseable color: " + value);
  const scale = /color\(|srgb/i.test(value) ? 255 : 1;
  return channels.map((channel) => channel * scale);
}

function rgbToHsl(value) {
  const [r, g, b] = parseRgb(value).map((channel) => channel / 255);
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const delta = max - min;
  let hue = 0;
  if (delta !== 0) {
    if (max === r) hue = ((g - b) / delta) % 6;
    else if (max === g) hue = (b - r) / delta + 2;
    else hue = (r - g) / delta + 4;
    hue = (hue * 60 + 360) % 360;
  }
  const lightness = (max + min) / 2;
  const saturation =
    delta === 0 ? 0 : delta / (1 - Math.abs(2 * lightness - 1));
  return { hue, saturation, lightness };
}

function toneColor(pills, label) {
  const pill = pills.find((item) => item.label === label);
  if (!pill) throw new Error("missing " + label + " pill for drain ramp");
  return rgbToHsl(pill.color);
}

// The drain pill's ready -> active(draining) -> dormant(idle) progression must
// vary saturation only at a constant hue: the ready green washes out toward the
// neutral idle gray with no green->cyan hue shift. `serve`/`tests` render ready
// (green), `lifecycle` renders active/draining, `studies`/`cli` render
// dormant/idle. A regression that reintroduces an off-hue accent (e.g. the old
// teal) moves the active hue away from the ready hue and fails here.
const DRAIN_RAMP_HUE_TOLERANCE_DEG = 8;
const DRAIN_RAMP_IDLE_MAX_SATURATION = 0.2;

function assertDrainColorRamp(pills) {
  const ready = toneColor(pills, "serve");
  const active = toneColor(pills, "lifecycle");
  const dormant = toneColor(pills, "studies");
  const ramp = { ready, active, dormant };
  if (Math.abs(active.hue - ready.hue) > DRAIN_RAMP_HUE_TOLERANCE_DEG)
    throw new Error(
      "draining pill shifted hue instead of desaturating: " +
        JSON.stringify(ramp),
    );
  if (!(ready.saturation > active.saturation && active.saturation > dormant.saturation))
    throw new Error(
      "drain ramp saturation did not fall ready>active>dormant: " +
        JSON.stringify(ramp),
    );
  if (dormant.saturation > DRAIN_RAMP_IDLE_MAX_SATURATION)
    throw new Error(
      "idle/dormant pill is not a neutral gray endpoint: " +
        JSON.stringify(ramp),
    );
  return ramp;
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
      color: getComputedStyle(pill).color,
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
    count: "2/1/2",
    tone: "ready",
    implicit: true,
    unavailable: "2",
  });
  assertPill(pills, "studies", {
    count: "0/0/3",
    tone: "dormant",
    title:
      "0 ready, 0 active/in flight, 0 blocked, 3 deferred; 3 open across studies.*; no task currently movable",
  });
  assertPill(pills, "cli", {
    count: "0/0/1",
    tone: "dormant",
    unavailable: "1",
  });
  assertPill(pills, "tests", { count: "3", tone: "ready" });
  assertPill(pills, "lifecycle", {
    count: "0/2",
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
  const drainRamp = assertDrainColorRamp(pills);
  await page.screenshot({ path: SCREENSHOT_PATH });
  const resolvedPills = await resolveCliBlocker(page);
  assertPill(resolvedPills, "cli", {
    count: "1",
    tone: "ready",
    unavailable: "0",
  });
  const emptyState = await clearInventory(page);
  if (emptyState.ariaHidden !== "true" || emptyState.pillCount !== 0)
    throw new Error("empty inventory mismatch: " + JSON.stringify(emptyState));
  return {
    pills,
    resolvedPills,
    emptyState,
    drainRamp,
    screenshotPath: SCREENSHOT_PATH,
  };
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
