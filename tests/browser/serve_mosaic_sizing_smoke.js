const { withServePage } = require("./serve_playwright_harness");

// Mosaic card sizing / interlock. Verifies against the live DOM: (a)
// card chrome (border + padding + fixed footer strip) plus gap sums to a
// whole multiple of the resolved line-height at the default type ramp; (b)
// k whole lines of body text produce the designed zero/bounded slack
// pattern at each lane-count's module (L>=3, L=2, L=1); (c) slack never
// reaches a full module; (d) the congruence still holds after a root
// font-size change (the full-replay geometry recompute case).

const SIZING_SCENARIOS = [
  { label: "L>=3", width: 1200 },
  { label: "L=2", width: 700 },
  { label: "L=1", width: 400 },
];
const SIZING_MAX_LINES = 6;
const SIZING_CONGRUENCE_TOLERANCE_PX = 1;
const SIZING_RESCALED_ROOT_FONT_PX = 20;

function mosaicSizingResolveLane() {
  let lane = Array.from(laneStates.values()).find((item) => !item.emptyTeam);
  if (!lane && targets.length) {
    addLane(targets[0].id);
    lane = laneStates.get(targets[0].id);
  }
  if (!lane) throw new Error("no lane available for mosaic sizing smoke");
  return lane;
}

function mosaicSizingBuildItem(key, html) {
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

function mosaicSizingMeasureChrome(lane) {
  const host = lane.messagesEl;
  host.innerHTML = "";
  const node = renderMessage(lane, mosaicSizingBuildItem("chrome-probe", "probe"));
  host.append(node);
  const articleStyle = getComputedStyle(node);
  const footer = node.querySelector(".message-footer");
  const footerStyle = getComputedStyle(footer);
  const chromePx =
    Number.parseFloat(articleStyle.borderTopWidth) +
    Number.parseFloat(articleStyle.borderBottomWidth) +
    Number.parseFloat(articleStyle.paddingTop) +
    Number.parseFloat(articleStyle.paddingBottom) +
    Number.parseFloat(footerStyle.marginTop) +
    footer.getBoundingClientRect().height;
  const lineHeightPx = Number.parseFloat(articleStyle.lineHeight);
  host.innerHTML = "";
  return { chromePx, lineHeightPx };
}

// Measurement discipline (demo measureAt()): the card's natural height must
// be read with min-height cleared and the fixed-length grid-auto-rows
// stretch neutralized -- .messages article's align-self: stretch otherwise
// squeezes the article into its single 4px auto-row and corrupts
// scrollHeight (confirmed against an unsqueezed measurement: same markup,
// offsetHeight matches the hand-derived chrome+content sum exactly once
// alignSelf overrides the squeeze; this is a pre-existing quirk of the grid
// rendering the new pass replaces, not something introduced here). The
// node must stay a real .messages article for the chrome CSS to match at
// all -- detaching to document.body drops every rule scoped by that
// ancestor selector.
function mosaicSizingMeasureContentPx(lane, lines) {
  const host = lane.messagesEl;
  const html = Array.from(
    { length: lines },
    (_, index) => "line " + (index + 1),
  ).join("<br>");
  const node = renderMessage(lane, mosaicSizingBuildItem("k-probe-" + lines, html));
  node.style.minHeight = "";
  node.style.alignSelf = "start";
  node.style.width = "600px";
  host.append(node);
  const px = node.offsetHeight;
  node.remove();
  return px;
}

function mosaicSizingSpotCheck(lane, scenario, k) {
  const geometry = mosaicGeometry(16, scenario.width);
  const contentPx = mosaicSizingMeasureContentPx(lane, k);
  const n = mosaicRowsFor(contentPx, geometry.gap, geometry.M);
  const slack = mosaicInteriorSlackPx(n, geometry.M, geometry.gap, contentPx);
  return { label: scenario.label, k, n, module: geometry.M, slack, contentPx };
}

function mosaicSizingCollectSpotChecks(lane) {
  const spotChecks = [];
  for (const scenario of SIZING_SCENARIOS) {
    for (let k = 1; k <= SIZING_MAX_LINES; k += 1) {
      spotChecks.push(mosaicSizingSpotCheck(lane, scenario, k));
    }
  }
  return spotChecks;
}

function mosaicSizingProbe() {
  const lane = mosaicSizingResolveLane();
  const chrome = mosaicSizingMeasureChrome(lane);
  const gap = mosaicGeometry(16, SIZING_SCENARIOS[0].width).gap;
  const spotChecks = mosaicSizingCollectSpotChecks(lane);

  // Root font-size change (full replay): the congruence must still hold
  // once every rem-denominated constant re-resolves.
  document.documentElement.style.fontSize = mosaicSizingRescaledRootFontPx + "px";
  const rescaledChrome = mosaicSizingMeasureChrome(lane);
  const rescaledGap = mosaicGeometry(mosaicSizingRescaledRootFontPx, 1200).gap;
  document.documentElement.style.fontSize = "";

  return { chrome, gap, spotChecks, rescaledChrome, rescaledGap };
}

async function installMosaicSizingHelpers(page) {
  await page.addScriptTag({
    content: [
      "const mosaicSizingRescaledRootFontPx = " +
        SIZING_RESCALED_ROOT_FONT_PX +
        ";",
      "const SIZING_SCENARIOS = " + JSON.stringify(SIZING_SCENARIOS) + ";",
      "const SIZING_MAX_LINES = " + SIZING_MAX_LINES + ";",
      mosaicSizingResolveLane,
      mosaicSizingBuildItem,
      mosaicSizingMeasureChrome,
      mosaicSizingMeasureContentPx,
      mosaicSizingSpotCheck,
      mosaicSizingCollectSpotChecks,
      mosaicSizingProbe,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

function sizingRemainder(chromePx, gapPx, lineHeightPx) {
  const total = chromePx + gapPx;
  const mod = ((total % lineHeightPx) + lineHeightPx) % lineHeightPx;
  return Math.min(mod, lineHeightPx - mod);
}

function assertChromeCongruence(result) {
  const chromeDrift = sizingRemainder(
    result.chrome.chromePx,
    result.gap,
    result.chrome.lineHeightPx,
  );
  if (chromeDrift > SIZING_CONGRUENCE_TOLERANCE_PX)
    throw new Error(
      "chrome + gap is not a whole multiple of lineHeight at the default type ramp: " +
        JSON.stringify(result.chrome) +
        " gap=" +
        result.gap +
        " drift=" +
        chromeDrift,
    );

  const rescaledDrift = sizingRemainder(
    result.rescaledChrome.chromePx,
    result.rescaledGap,
    result.rescaledChrome.lineHeightPx,
  );
  if (rescaledDrift > SIZING_CONGRUENCE_TOLERANCE_PX)
    throw new Error(
      "congruence broke after a root font-size change: " +
        JSON.stringify(result.rescaledChrome) +
        " gap=" +
        result.rescaledGap +
        " drift=" +
        rescaledDrift,
    );
}

function assertSpotChecks(result) {
  const lineHeightPx = result.chrome.lineHeightPx;
  for (const check of result.spotChecks) {
    if (check.slack < 0)
      throw new Error("negative slack: " + JSON.stringify(check));
    if (check.slack >= check.module)
      throw new Error("slack reached a full module: " + JSON.stringify(check));
    const lineRemainder = Math.min(
      check.slack % lineHeightPx,
      lineHeightPx - (check.slack % lineHeightPx),
    );
    if (lineRemainder > SIZING_CONGRUENCE_TOLERANCE_PX)
      throw new Error(
        "slack is not a whole-line remainder: " + JSON.stringify(check),
      );
  }

  // At L=1 (lines=1), every k lands with near-zero slack -- the
  // claim that a single lane has nothing to align across.
  const l1 = result.spotChecks.filter((check) => check.label === "L=1");
  if (!l1.every((check) => check.slack <= SIZING_CONGRUENCE_TOLERANCE_PX))
    throw new Error(
      "L=1 did not achieve near-zero slack for every k: " + JSON.stringify(l1),
    );
}

async function waitForMosaicSizingGlobals(page) {
  await page.waitForFunction(
    () =>
      typeof renderMessage === "function" &&
      typeof mosaicGeometry === "function" &&
      typeof mosaicRowsFor === "function" &&
      typeof mosaicInteriorSlackPx === "function" &&
      Array.isArray(targets) &&
      targets.length > 0,
    { timeout: 10000 },
  );
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-sizing-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 900 } },
    },
    async ({ page, server }) => {
      await waitForMosaicSizingGlobals(page);
      await installMosaicSizingHelpers(page);
      const result = await page.evaluate(mosaicSizingProbe);
      assertChromeCongruence(result);
      assertSpotChecks(result);
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
