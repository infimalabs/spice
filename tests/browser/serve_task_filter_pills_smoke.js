const os = require("os");
const path = require("path");
const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const SCREENSHOT_PATH = path.join(os.tmpdir(), "spice-task-filter-pills.png");

// The tone classes the ramp can land on, ordered from the fully-saturated
// coverage-plus-ready top down to the neutral idle floor.
const PILL_TONES = ["saturated", "active", "assigned", "dormant", "idle"];

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
// saturation ramp can be checked in hue/saturation terms.
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
  if (!pill) throw new Error("missing " + label + " pill for saturation ramp");
  return rgbToHsl(pill.color);
}

// Saturation encodes agent coverage layered with work flow. A running,
// boundary-dissolving Drain lane covers every public stem, so under it the ramp
// climbs saturated (covered + ready) -> active (covered + in flight) -> assigned
// (covered, all blocked/deferred) -> idle (uncovered but ready) purely by
// stepping saturation down a constant --good hue: the ready green washes toward
// the neutral gray with no green->cyan hue shift. `serve` renders saturated,
// `lifecycle` active, `studies` assigned, and the private `agent` channel (which
// sits out boundary dissolution) renders the neutral idle endpoint. A
// regression that reintroduces an off-hue accent moves a step's hue off the
// ready hue and fails here.
const RAMP_HUE_TOLERANCE_DEG = 8;
const IDLE_MAX_SATURATION = 0.2;
// Hidden/system stems recover the warn accent: an orange/red hue well away from
// the green ramp, with real saturation rather than the desaturated gray floor.
const WARN_HUE_MAX_DEG = 60;
const WARN_MIN_SATURATION = 0.2;
const RAMP_HUE_DIVERGENCE_DEG = 60;

function assertCoverageSaturationRamp(pills, readyGreen) {
  const saturatedPill = pills.find((item) => item.label === "serve");
  if (
    saturatedPill.color !== readyGreen ||
    saturatedPill.countColor !== readyGreen
  )
    throw new Error(
      "fully-ready pill and count did not use the shared ready green: " +
        JSON.stringify({ saturatedPill, readyGreen }),
    );
  const saturated = toneColor(pills, "serve");
  const active = toneColor(pills, "lifecycle");
  const assigned = toneColor(pills, "studies");
  const idle = toneColor(pills, "agent");
  const ramp = { saturated, active, assigned, idle };
  const steps = [saturated, active, assigned, idle];
  // Hue is meaningful only while saturation remains on the green ramp. The
  // neutral --muted endpoint may report any nominal hue after RGB -> HSL.
  for (const step of [saturated, active, assigned]) {
    if (Math.abs(step.hue - saturated.hue) > RAMP_HUE_TOLERANCE_DEG)
      throw new Error(
        "coverage ramp step shifted hue instead of desaturating: " +
          JSON.stringify(ramp),
      );
  }
  for (let index = 1; index < steps.length; index += 1) {
    if (!(steps[index - 1].saturation > steps[index].saturation))
      throw new Error(
        "coverage ramp saturation did not fall saturated>active>assigned>idle: " +
          JSON.stringify(ramp),
      );
  }
  return ramp;
}

function assertHiddenWarnAccent(pills) {
  const green = toneColor(pills, "serve");
  const accents = {};
  for (const label of ["oops", "maxim_proposal"]) {
    const warn = toneColor(pills, label);
    accents[label] = warn;
    if (warn.saturation < WARN_MIN_SATURATION)
      throw new Error(
        label + " hidden stem desaturated to gray instead of the warn accent: " +
          JSON.stringify(warn),
      );
    if (warn.hue > WARN_HUE_MAX_DEG)
      throw new Error(
        label + " hidden stem is not in the warn/orange hue band: " +
          JSON.stringify(warn),
      );
    if (Math.abs(warn.hue - green.hue) < RAMP_HUE_DIVERGENCE_DEG)
      throw new Error(
        label + " hidden accent did not diverge from the green coverage ramp: " +
          JSON.stringify({ warn, green }),
      );
  }
  return accents;
}

function initialInventory() {
  return {
    revision: "9999999999999999999999999999",
    catalog: {
      approvedStems: ["serve", "studies", "cli", "tests", "lifecycle"],
      hiddenStems: ["oops", "maxim_proposal"],
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
        name: "agent",
        filters: [],
        openTaskCount: 3,
        readyTaskCount: 2,
        inFlightTaskCount: 1,
        blockedTaskCount: 0,
        deferredTaskCount: 0,
      },
      {
        name: "oops",
        filters: [],
        openTaskCount: 2,
        readyTaskCount: 0,
        inFlightTaskCount: 0,
        blockedTaskCount: 0,
        deferredTaskCount: 2,
      },
      {
        name: "maxim_proposal",
        filters: [],
        openTaskCount: 4,
        readyTaskCount: 0,
        inFlightTaskCount: 0,
        blockedTaskCount: 0,
        deferredTaskCount: 4,
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
  return page.evaluate((tones) =>
    Array.from(document.querySelectorAll(".filter-pill")).map((pill) => ({
      label: pill.querySelector(".filter-pill-label")?.textContent || "",
      count: pill.querySelector(".filter-pill-count")?.textContent || "",
      tone: tones.find((value) =>
        pill.classList.contains("filter-pill--" + value),
      ),
      implicit: pill.classList.contains("filter-pill--implicit"),
      unavailable: pill.dataset.unavailableTaskCount,
      title: pill.title,
      color: getComputedStyle(pill).color,
      countColor: getComputedStyle(
        pill.querySelector(".filter-pill-count"),
      ).backgroundColor,
      borderStyle: getComputedStyle(pill).borderTopStyle,
    })),
  PILL_TONES);
}

async function readReadyGreen(page) {
  return page.evaluate(() => {
    const probe = document.createElement("span");
    probe.style.color = "var(--good)";
    document.body.append(probe);
    const color = getComputedStyle(probe).color;
    probe.remove();
    return color;
  });
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
  // Covered: the running Drain lane camps on every public stem, so the ramp
  // climbs with how much of each stem's work is moving.
  assertPill(pills, "serve", {
    count: "2·1·2",
    tone: "saturated",
    implicit: true,
    unavailable: "2",
  });
  // A covered stem with only deferred work stays lit as legitimately assigned,
  // not dormant -- the operator can see an agent is on it even when nothing is
  // ready to pull.
  assertPill(pills, "studies", {
    count: "0·0·3",
    tone: "assigned",
    title:
      "0 ready, 0 active/in flight, 0 blocked, 3 deferred; 3 open across studies.*; no task currently movable",
  });
  // A covered but fully-blocked stem is likewise assigned, distinguishing a
  // legitimately-blocked agent from one with no work assigned at all.
  assertPill(pills, "cli", {
    count: "0·0·1",
    tone: "assigned",
    unavailable: "1",
  });
  assertPill(pills, "tests", { count: "3·0·0", tone: "saturated" });
  assertPill(pills, "lifecycle", {
    count: "0·2·0",
    tone: "active",
    implicit: false,
    unavailable: "0",
  });
  // The private agent channel is handed out, so its tasks move through phases and
  // it keeps the public triple; it sits out boundary dissolution, so the public
  // Drain lane does not cover it and its ready work reads as the neutral idle
  // floor rather than saturated, behind the dashed private marker.
  assertPill(pills, "agent", {
    count: "2·1·0",
    tone: "idle",
    borderStyle: "dashed",
  });
  // Hidden stems (built-in oops and project-defined maxim_proposal) are never
  // handed out, so their badges collapse to a single open count and they rest on
  // the dormant tone -- but the dashed system marker repaints them in the warn
  // accent so unattended triage still stands out.
  assertPill(pills, "oops", {
    count: "2",
    tone: "dormant",
    borderStyle: "dashed",
  });
  assertPill(pills, "maxim_proposal", {
    count: "4",
    tone: "dormant",
    borderStyle: "dashed",
  });
  const serveBorder = pills.find((pill) => pill.label === "serve")?.borderStyle;
  if (serveBorder !== "solid")
    throw new Error("public serve pill lost its solid border: " + serveBorder);
  const labels = pills.map((pill) => pill.label).join(",");
  if (labels !== "serve,studies,cli,tests,lifecycle,agent,oops,maxim_proposal")
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

// Stop the covering lane so no agent is assigned to any public stem. Coverage,
// not task state, is what changes: the same ready work that read saturated now
// desaturates to the neutral idle floor, while a stem with nothing movable rests
// on the low dormant step.
async function dropCoverage(page) {
  await page.evaluate(() => {
    const lane = resolveIsolatedLane("task-filter-pill-smoke");
    lane.lastRenderedStatusLine = { agentProcessStatus: "stopped" };
    renderFilterPills();
  });
  return readPills(page);
}

function assertUncoveredPills(pills, coveredRamp, warnAccents) {
  // Ready work with no agent on it drops from saturated to the idle waiting step.
  assertPill(pills, "serve", { tone: "idle", implicit: false });
  assertPill(pills, "tests", { tone: "idle" });
  // The cli blocker was resolved earlier, so it has ready work but, uncovered,
  // reads idle rather than saturated.
  assertPill(pills, "cli", { tone: "idle" });
  // Nothing movable and no agent assigned rests one step above idle: no
  // handleable work is being left unattended.
  assertPill(pills, "studies", { tone: "dormant" });
  assertPill(pills, "lifecycle", { tone: "dormant" });

  const serveIdle = toneColor(pills, "serve");
  if (!(serveIdle.saturation < coveredRamp.saturated.saturation))
    throw new Error(
      "uncovered serve did not desaturate below its covered saturated tone: " +
        JSON.stringify({ serveIdle, covered: coveredRamp.saturated }),
    );
  if (serveIdle.saturation > IDLE_MAX_SATURATION)
    throw new Error(
      "uncovered ready stem is not the neutral idle endpoint: " +
        JSON.stringify(serveIdle),
    );
  const studiesDormant = toneColor(pills, "studies");
  if (
    Math.abs(studiesDormant.hue - coveredRamp.saturated.hue) >
    RAMP_HUE_TOLERANCE_DEG
  )
    throw new Error(
      "dormant step shifted hue instead of desaturating: " +
        JSON.stringify({ studiesDormant, coveredRamp }),
    );
  if (
    !(
      coveredRamp.assigned.saturation > studiesDormant.saturation &&
      studiesDormant.saturation > serveIdle.saturation
    )
  )
    throw new Error(
      "dormant saturation did not fall between assigned and idle: " +
        JSON.stringify({ studiesDormant, serveIdle, coveredRamp }),
    );
  // Hidden stems keep the warn accent regardless of coverage; it must stay
  // distinct from the low dormant tone a public stem falls to.
  const stillWarn = assertHiddenWarnAccent(pills);
  const oopsColor = pills.find((pill) => pill.label === "oops")?.color;
  const studiesColor = pills.find((pill) => pill.label === "studies")?.color;
  if (oopsColor === studiesColor)
    throw new Error(
      "hidden oops pill collapsed onto the public dormant tone instead of warn: " +
        JSON.stringify({ oopsColor, studiesColor }),
    );
  return { stillWarn, warnAccents };
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
  const readyGreen = await readReadyGreen(page);
  assertInitialPills(pills);
  const coverageRamp = assertCoverageSaturationRamp(pills, readyGreen);
  const warnAccents = assertHiddenWarnAccent(pills);
  await page.screenshot({ path: SCREENSHOT_PATH });
  const resolvedPills = await resolveCliBlocker(page);
  assertPill(resolvedPills, "cli", {
    count: "1·0·0",
    tone: "saturated",
    unavailable: "0",
  });
  const uncoveredPills = await dropCoverage(page);
  const uncovered = assertUncoveredPills(uncoveredPills, coverageRamp, warnAccents);
  const emptyState = await clearInventory(page);
  if (emptyState.ariaHidden !== "true" || emptyState.pillCount !== 0)
    throw new Error("empty inventory mismatch: " + JSON.stringify(emptyState));
  return {
    pills,
    resolvedPills,
    uncoveredPills,
    uncovered,
    emptyState,
    coverageRamp,
    readyGreen,
    warnAccents,
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
