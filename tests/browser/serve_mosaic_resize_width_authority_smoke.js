const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

// The ResizeObserver and the geometry pass must observe one width: the
// mosaic host's content box. Chromium rounds clientWidth at a boundary that
// a fractional border-box delta can cross by much less than half a pixel.
// This fixture manually delivers the captured observer callback so the
// assertions resolve from production render completion, never elapsed time.

const WIDTH_AUTHORITY_BEFORE_PX = 600.484375;
const WIDTH_AUTHORITY_AFTER_PX = 600.5;
const WIDTH_AUTHORITY_SAME_CONTENT_PX = 600.515625;

function widthAuthorityResolveLane() {
  return resolveIsolatedLane("mosaic-resize-width-authority-smoke-team");
}

function widthAuthorityItem(index) {
  const text = "width authority " + index;
  return {
    ack_count: 0,
    ack_keys: [],
    display_html: text,
    display_text: text,
    index,
    key: "width-authority-" + index,
    kind: "assistant",
    text,
    timestamp: new Date(2026, 0, 1, 0, 0, index).toISOString(),
  };
}

function widthAuthorityMeasurement(lane) {
  return {
    borderWidth: lane.messagesEl.getBoundingClientRect().width,
    clientWidth: lane.messagesEl.clientWidth,
    contentWidth: mosaicContainerWidthPx(lane.messagesEl),
  };
}

function widthAuthorityInstallHarness() {
  window.__widthAuthorityTrace = {
    fullReplayCount: 0,
    geometryReasons: [],
    geometryWidths: [],
    observer: null,
    pendingRender: null,
  };
  window.ResizeObserver = class {
    constructor(callback) {
      this.callback = callback;
      this.target = null;
      window.__widthAuthorityTrace.observer = this;
    }
    observe(target) {
      this.target = target;
    }
    disconnect() {
      this.target = null;
    }
    deliver() {
      this.callback([], this);
    }
  };

  const originalGeometry = mosaicGeometry;
  mosaicGeometry = function (rootFontSizePx, containerWidthPx) {
    window.__widthAuthorityTrace.geometryWidths.push(containerWidthPx);
    return originalGeometry(rootFontSizePx, containerWidthPx);
  };
  const originalGeometryChange = mosaicGeometryChange;
  mosaicGeometryChange = function (...args) {
    const change = originalGeometryChange.apply(this, args);
    window.__widthAuthorityTrace.geometryReasons.push(change.reason);
    return change;
  };
  const originalReplay = mosaicFullReplay;
  mosaicFullReplay = function (...args) {
    window.__widthAuthorityTrace.fullReplayCount += 1;
    return originalReplay.apply(this, args);
  };
  const originalRender = mosaicRenderMessageStream;
  mosaicRenderMessageStream = function (lane, ...args) {
    const result = originalRender.call(this, lane, ...args);
    const pending = window.__widthAuthorityTrace.pendingRender;
    if (pending && pending.lane === lane) {
      window.__widthAuthorityTrace.pendingRender = null;
      pending.resolve();
    }
    return result;
  };
}

function widthAuthorityNextRender(lane) {
  if (window.__widthAuthorityTrace.pendingRender)
    throw new Error("a width-authority render is already pending");
  return new Promise((resolve) => {
    window.__widthAuthorityTrace.pendingRender = { lane, resolve };
  });
}

function widthAuthoritySeedLane() {
  const lane = widthAuthorityResolveLane();
  Object.assign(lane.messagesEl.style, {
    borderLeft: "1px solid transparent",
    borderRight: "1px solid transparent",
    boxSizing: "border-box",
    flex: "none",
    width: WIDTH_AUTHORITY_BEFORE_PX + "px",
  });
  const item = widthAuthorityItem(1);
  lane.knownMessages = [item];
  lane.knownMessageKeys = new Set([item.key]);
  lane.retainedMessageLimit = 10;
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  const initial = widthAuthorityMeasurement(lane);
  const trace = window.__widthAuthorityTrace;
  trace.fullReplayCount = 0;
  trace.geometryReasons = [];
  trace.geometryWidths = [];
  return { lane, initial };
}

function widthAuthorityMeasurementFailures() {
  const failures = [];
  const capture = (label, callback) => {
    try {
      callback();
    } catch (error) {
      failures.push({ label, message: String(error.message || error) });
    }
  };
  capture("missing", () => mosaicContainerWidthPx(null));
  const nonFinite = document.createElement("div");
  Object.defineProperty(nonFinite, "clientWidth", { value: Number.NaN });
  capture("non-finite", () => mosaicContainerWidthPx(nonFinite));
  const contradictory = document.createElement("div");
  contradictory.style.padding = "10px";
  document.body.append(contradictory);
  Object.defineProperty(contradictory, "clientWidth", { value: 5 });
  capture("contradictory", () => mosaicContainerWidthPx(contradictory));
  contradictory.remove();
  return failures;
}

async function widthAuthorityRun() {
  const { lane, initial } = widthAuthoritySeedLane();
  const trace = window.__widthAuthorityTrace;
  const observer = trace.observer;
  if (!observer || observer.target !== lane.messagesEl)
    throw new Error("production ResizeObserver was not captured");

  const boundaryRender = widthAuthorityNextRender(lane);
  lane.messagesEl.style.width = WIDTH_AUTHORITY_AFTER_PX + "px";
  const boundary = widthAuthorityMeasurement(lane);
  observer.deliver();
  const cachedAtDelivery = lane.mosaicHostContentWidth;
  await boundaryRender;
  const boundaryResult = {
    cachedAtDelivery,
    geometryReason: trace.geometryReasons.at(-1),
    geometryWidth: trace.geometryWidths.at(-1),
    fullReplayCount: trace.fullReplayCount,
  };

  const geometryReasonCount = trace.geometryReasons.length;
  lane.messagesEl.style.width = WIDTH_AUTHORITY_SAME_CONTENT_PX + "px";
  const sameContent = widthAuthorityMeasurement(lane);
  observer.deliver();
  const sameContentResult = {
    frame: lane.mosaicFrame || 0,
    geometryReasonCount: trace.geometryReasons.length,
    resizeDebounce: lane.mosaicResizeDebounce || 0,
  };

  const later = widthAuthorityItem(2);
  upsertKnownMessage(lane, later, "newest");
  trimKnownMessages(lane);
  renderMessagesIfChanged(lane);
  const unrelatedRender = {
    fullReplayCount: trace.fullReplayCount,
    geometryReason: trace.geometryReasons.at(-1),
    geometryReasonCountBefore: geometryReasonCount,
    geometryWidth: trace.geometryWidths.at(-1),
  };

  return {
    boundary,
    boundaryResult,
    failures: widthAuthorityMeasurementFailures(),
    initial,
    sameContent,
    sameContentResult,
    unrelatedRender,
  };
}

const widthAuthorityPageHelpers = [
  widthAuthorityResolveLane,
  widthAuthorityItem,
  widthAuthorityMeasurement,
  widthAuthorityInstallHarness,
  widthAuthorityNextRender,
  widthAuthoritySeedLane,
  widthAuthorityMeasurementFailures,
  widthAuthorityRun,
];

function assertResult(result) {
  const borderDelta = result.boundary.borderWidth - result.initial.borderWidth;
  const clientDelta = result.boundary.clientWidth - result.initial.clientWidth;
  const contentDelta = result.boundary.contentWidth - result.initial.contentWidth;
  if (!(borderDelta > 0 && borderDelta < 0.5))
    throw new Error("fixture did not produce a sub-0.5px border-box delta: " + JSON.stringify(result));
  if (clientDelta !== 1 || contentDelta !== 1)
    throw new Error("fractional boundary did not cross content-width rounding: " + JSON.stringify(result));
  if (
    result.boundaryResult.cachedAtDelivery !== result.boundary.contentWidth ||
    result.boundaryResult.geometryWidth !== result.boundary.contentWidth
  )
    throw new Error("observer and geometry consumed different widths: " + JSON.stringify(result));
  if (result.boundaryResult.geometryReason !== "width")
    throw new Error("content-width boundary did not reconcile geometry: " + JSON.stringify(result));
  if (result.boundaryResult.fullReplayCount !== 0)
    throw new Error("same-topology content-width change entered full replay: " + JSON.stringify(result));

  if (result.sameContent.borderWidth === result.boundary.borderWidth)
    throw new Error("same-content fixture did not change the border box: " + JSON.stringify(result));
  if (
    result.sameContent.clientWidth !== result.boundary.clientWidth ||
    result.sameContent.contentWidth !== result.boundary.contentWidth
  )
    throw new Error("same-content fixture changed mosaic content width: " + JSON.stringify(result));
  if (
    result.sameContentResult.frame ||
    result.sameContentResult.resizeDebounce ||
    result.sameContentResult.geometryReasonCount !==
      result.unrelatedRender.geometryReasonCountBefore
  )
    throw new Error("border-only delta scheduled mosaic reconciliation: " + JSON.stringify(result));

  if (
    result.unrelatedRender.geometryReason !== "stable" ||
    result.unrelatedRender.geometryWidth !== result.boundary.contentWidth ||
    result.unrelatedRender.fullReplayCount !== 0
  )
    throw new Error("later message render corrected stale geometry: " + JSON.stringify(result));
  if (result.failures.map((failure) => failure.label).join(",") !== "missing,non-finite,contradictory")
    throw new Error("invalid measurements did not fail loudly: " + JSON.stringify(result));
}

async function installWidthAuthorityHelpers(page) {
  await page.addScriptTag({
    content:
      "const WIDTH_AUTHORITY_BEFORE_PX = " +
      WIDTH_AUTHORITY_BEFORE_PX +
      ";\nconst WIDTH_AUTHORITY_AFTER_PX = " +
      WIDTH_AUTHORITY_AFTER_PX +
      ";\nconst WIDTH_AUTHORITY_SAME_CONTENT_PX = " +
      WIDTH_AUTHORITY_SAME_CONTENT_PX +
      ";\n" +
      widthAuthorityPageHelpers.map((helper) => helper.toString()).join("\n"),
  });
  await page.evaluate(widthAuthorityInstallHarness);
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-resize-width-authority-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 900 } },
    },
    async ({ page }) => {
      await installIsolatedLaneFixture(page, {
        globals: [
          "mosaicContainerWidthPx",
          "mosaicFullReplay",
          "mosaicGeometry",
          "mosaicGeometryChange",
          "mosaicRenderMessageStream",
          "renderMessagesIfChanged",
          "trimKnownMessages",
          "upsertKnownMessage",
        ],
      });
      await installWidthAuthorityHelpers(page);
      return page.evaluate(() => widthAuthorityRun());
    },
  );
}

if (require.main === module) {
  run()
    .then((result) => {
      assertResult(result);
      console.log(JSON.stringify(result, null, 2));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { assertResult, run };
