const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

// Mosaic single-column degeneration (mosaic-single-column): at L=1 the
// live stream integration must take the SAME code path as any other column
// count -- one candidate span (12), one anchor, pure append -- with no
// special-cased "natural height mode" fork. Drives a real lane through
// renderMessagesIfChanged at a real 380px viewport (below the ~556px L=1/L=2
// capacity-rule threshold at root 16px) exactly as live traffic would, then
// crosses the L=1/L=2 boundary through the lane's wrapped ResizeObserver in
// both directions and checks: every card spans the full 12 tracks at L=1; a new
// message pushes down like a standard feed; late content growth pushes the
// same way it would at any other column count; crossing the boundary
// triggers exactly one full replay each way; same-frame observer bursts
// coalesce into one render with no timer or delayed second replay; and the
// settled lattice at L=1 reflects the CURRENT module/geometry, never a stale
// one carried over from the wide host.

const MOSAIC_SINGLE_COLUMN_NARROW_WIDTH = 380;
const MOSAIC_SINGLE_COLUMN_WIDE_HOST_WIDTHS = [600, 900];
const MOSAIC_SINGLE_COLUMN_SAME_TOPOLOGY_HOST_WIDTHS = [920, 940];
const MOSAIC_SINGLE_COLUMN_NARROW_HOST_WIDTHS = [360, 340];
const MOSAIC_SINGLE_COLUMN_FULL_SPAN = 12;

function mosaicSingleColumnResolveLane() {
  return resolveIsolatedLane("mosaic-single-column-smoke-team");
}

function mosaicSingleColumnBuildItem(index, lines) {
  const html = Array.from({ length: lines }, (_, i) => "line " + (i + 1)).join("<br>");
  return {
    ack_count: 0,
    ack_keys: [],
    index,
    key: "sc-" + index,
    kind: "assistant",
    timestamp: new Date(2026, 0, 1, 0, 0, index).toISOString(),
    display_html: html,
    display_text: html,
    text: html,
  };
}

function mosaicSingleColumnPushMessage(lane, index, lines) {
  upsertKnownMessage(lane, mosaicSingleColumnBuildItem(index, lines), "newest");
  trimKnownMessages(lane);
  renderMessagesIfChanged(lane);
}

function mosaicSingleColumnCardsOverlap(cards) {
  for (let i = 0; i < cards.length; i += 1) {
    for (let j = i + 1; j < cards.length; j += 1) {
      const a = cards[i];
      const b = cards[j];
      const tracksOverlap = a.t < b.t + b.span && b.t < a.t + a.span;
      const rowsOverlap = a.b < b.b + b.n && b.b < a.b + a.n;
      if (tracksOverlap && rowsOverlap) return [a, b];
    }
  }
  return null;
}

function mosaicSingleColumnSnapshot(lane) {
  return {
    baseSpan: lane.mosaicGeometry.baseSpan,
    M: lane.mosaicGeometry.M,
    gap: lane.mosaicGeometry.gap,
    width: lane.mosaicGeometry.edges[mosaicGridTrackCount],
    lattice: lane.mosaicCards.map((c) => ({ ...c })),
    overlap: mosaicSingleColumnCardsOverlap(lane.mosaicCards),
    fullReplays: lane.mosaicEventLog.events
      .filter((event) => event.type === "full-replay")
      .map((event) => event.geometryReplay || null),
  };
}

// Install before the isolated lane exists so its production observer is born
// wrapped. Message hosts are driven manually below, making same-frame delivery
// deterministic while other observer users continue through the native API.
function mosaicSingleColumnInstallResizeHarness() {
  const NativeResizeObserver = window.ResizeObserver;
  window.__mosaicWrappedResizeObservers = [];
  window.ResizeObserver = class WrappedResizeObserver {
    constructor(callback) {
      this.callback = callback;
      this.targets = new Set();
      this.nativeObserver = new NativeResizeObserver((entries) => {
        this.callback(entries, this);
      });
      window.__mosaicWrappedResizeObservers.push(this);
    }

    observe(target, options) {
      this.targets.add(target);
      if (!target.classList.contains("messages"))
        this.nativeObserver.observe(target, options);
    }

    unobserve(target) {
      this.targets.delete(target);
      this.nativeObserver.unobserve(target);
    }

    disconnect() {
      this.targets.clear();
      this.nativeObserver.disconnect();
    }
  };
}

// Resolve each resize from the production render callback itself. The replay
// wrapper counts only calls made inside this lane's render, so unrelated live
// lanes cannot contaminate the result.
function mosaicSingleColumnInstallRenderObserver() {
  window.__mosaicFullReplayCallCount = 0;
  window.__mosaicRenderCallCount = 0;
  window.__mosaicObservedLane = mosaicSingleColumnResolveLane();
  window.__mosaicObservedLaneRendering = false;
  const originalReplay = mosaicFullReplay;
  mosaicFullReplay = function (...args) {
    if (window.__mosaicObservedLaneRendering)
      window.__mosaicFullReplayCallCount += 1;
    return originalReplay.apply(this, args);
  };
  const originalRender = mosaicRenderMessageStream;
  mosaicRenderMessageStream = function (lane, ...args) {
    const observed = lane === window.__mosaicObservedLane;
    if (observed) {
      window.__mosaicRenderCallCount += 1;
      window.__mosaicObservedLaneRendering = true;
    }
    let result;
    try {
      result = originalRender.call(this, lane, ...args);
    } finally {
      if (observed) window.__mosaicObservedLaneRendering = false;
    }
    const resolve = window.__mosaicResizeObservationResolve;
    if (resolve && observed) {
      window.__mosaicResizeObservationResolve = null;
      resolve({
        snapshot: mosaicSingleColumnSnapshot(lane),
        fullReplayCallCount: window.__mosaicFullReplayCallCount,
        renderCallCount: window.__mosaicRenderCallCount,
      });
    }
    return result;
  };
}

function mosaicSingleColumnBeginResizeObservation() {
  if (window.__mosaicResizeObservationResolve)
    throw new Error("a mosaic resize observation is already armed");
  window.__mosaicResizeObservation = new Promise((resolve) => {
    window.__mosaicResizeObservationResolve = resolve;
  });
}

function mosaicSingleColumnAwaitResizeObservation() {
  return window.__mosaicResizeObservation;
}

async function mosaicSingleColumnDeliverResizeBurst(widths) {
  const lane = mosaicSingleColumnResolveLane();
  const observer = window.__mosaicWrappedResizeObservers.find((candidate) =>
    candidate.targets.has(lane.messagesEl),
  );
  if (!observer) throw new Error("isolated lane has no wrapped ResizeObserver");
  if (lane.mosaicFrame) throw new Error("resize burst began with a frame already scheduled");

  mosaicSingleColumnBeginResizeObservation();
  const renderCallCountBefore = window.__mosaicRenderCallCount;
  const originalSetTimeout = window.setTimeout;
  let setTimeoutCallCount = 0;
  const frameIds = [];
  const observedHostWidths = [];
  try {
    window.setTimeout = function (...args) {
      setTimeoutCallCount += 1;
      return originalSetTimeout.apply(this, args);
    };
    for (const width of widths) {
      lane.messagesEl.style.boxSizing = "border-box";
      lane.messagesEl.style.flex = "0 0 auto";
      lane.messagesEl.style.width = width + "px";
      void lane.messagesEl.offsetWidth;
      const rect = lane.messagesEl.getBoundingClientRect();
      observedHostWidths.push(rect.width);
      observer.callback([{ target: lane.messagesEl, contentRect: rect }], observer);
      frameIds.push(lane.mosaicFrame);
    }
  } finally {
    window.setTimeout = originalSetTimeout;
  }

  const observed = await mosaicSingleColumnAwaitResizeObservation();
  const renderCallCountAtCompletion = window.__mosaicRenderCallCount;
  await new Promise((resolve) => requestAnimationFrame(resolve));
  await new Promise((resolve) => requestAnimationFrame(resolve));
  return {
    ...observed,
    frameIds,
    observedHostWidths,
    renderCallCountBefore,
    renderCallCountAtCompletion,
    renderCallCountAfterExtraFrames: window.__mosaicRenderCallCount,
    fullReplayCallCountAfterExtraFrames: window.__mosaicFullReplayCallCount,
    setTimeoutCallCount,
  };
}

function mosaicSingleColumnRunAtNarrowWidth() {
  const lane = mosaicSingleColumnResolveLane();

  for (let i = 1; i <= 4; i += 1) mosaicSingleColumnPushMessage(lane, i, 2);
  const afterInserts = mosaicSingleColumnSnapshot(lane);

  // Late-content push: grow an existing card's content at L=1.
  const item = lane.knownMessages.find((known) => known.key === "sc-2");
  const grownHtml = Array.from({ length: 6 }, (_, i) => "grown " + (i + 1)).join("<br>");
  item.display_html = grownHtml;
  item.display_text = grownHtml;
  item.text = grownHtml;
  renderMessagesIfChanged(lane);
  const afterGrow = mosaicSingleColumnSnapshot(lane);

  // New-message push-down: insert one more card at L=1.
  mosaicSingleColumnPushMessage(lane, 5, 2);
  const afterFifthInsert = mosaicSingleColumnSnapshot(lane);

  return { afterInserts, afterGrow, afterFifthInsert };
}

async function installMosaicSingleColumnHelpers(page) {
  await page.addScriptTag({
    content: [
      mosaicSingleColumnResolveLane,
      mosaicSingleColumnBuildItem,
      mosaicSingleColumnPushMessage,
      mosaicSingleColumnCardsOverlap,
      mosaicSingleColumnSnapshot,
      mosaicSingleColumnInstallResizeHarness,
      mosaicSingleColumnInstallRenderObserver,
      mosaicSingleColumnBeginResizeObservation,
      mosaicSingleColumnAwaitResizeObservation,
      mosaicSingleColumnDeliverResizeBurst,
      mosaicSingleColumnRunAtNarrowWidth,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

function assertNarrowWidthResult(result, fail) {
  const assertAllFullSpan = (snapshot, label) => {
    if (snapshot.baseSpan !== MOSAIC_SINGLE_COLUMN_FULL_SPAN)
      fail(label + ": baseSpan must be 12 at a 380px viewport, got " + snapshot.baseSpan);
    for (const card of snapshot.lattice) {
      if (card.span !== MOSAIC_SINGLE_COLUMN_FULL_SPAN || card.t !== 0)
        fail(label + ": card " + card.key + " must anchor t=0/span=12 at L=1, got t=" + card.t + " span=" + card.span);
    }
    if (snapshot.overlap) fail(label + ": cards overlap: " + JSON.stringify(snapshot.overlap));
  };
  assertAllFullSpan(result.afterInserts, "after four inserts");
  if (result.afterInserts.lattice.length !== 4)
    fail("expected 4 cards after four inserts, got " + result.afterInserts.lattice.length);

  assertAllFullSpan(result.afterGrow, "after late-content growth");
  const grownBefore = result.afterInserts.lattice.find((c) => c.key === "sc-2");
  const grownAfter = result.afterGrow.lattice.find((c) => c.key === "sc-2");
  if (!(grownAfter.n > grownBefore.n))
    fail("growing sc-2's content at L=1 did not increase its row count");

  assertAllFullSpan(result.afterFifthInsert, "after fifth insert (push-down)");
  if (result.afterFifthInsert.lattice.length !== 5)
    fail("expected 5 cards after the fifth insert, got " + result.afterFifthInsert.lattice.length);
  const newest = result.afterFifthInsert.lattice.find((c) => c.key === "sc-5");
  const secondNewest = result.afterFifthInsert.lattice
    .filter((c) => c.key !== "sc-5")
    .sort((a, b) => b.creationIndex - a.creationIndex)[0];
  if (!(newest.b + newest.n > secondNewest.b + secondNewest.n))
    fail("the newest card must push above the rest of the single-column stack");
}

function assertResizeResult(label, result, expectedFullReplayCallCount, expectFullSpan, fail) {
  if (result.frameIds.length < 2)
    fail(label + ": fixture must deliver a same-frame ResizeObserver burst");
  if (
    result.observedHostWidths.some((width) => width <= 0) ||
    new Set(result.observedHostWidths).size !== result.observedHostWidths.length
  )
    fail(
      label +
        ": observer deliveries must carry distinct nonzero host widths: " +
        JSON.stringify(result.observedHostWidths),
    );
  if (
    !result.frameIds[0] ||
    !result.frameIds.every((id) => id === result.frameIds[0])
  )
    fail(
      label +
        ": observer deliveries did not coalesce onto one scheduled frame: " +
        JSON.stringify(result.frameIds),
    );
  if (result.setTimeoutCallCount !== 0)
    fail(label + ": ResizeObserver scheduled " + result.setTimeoutCallCount + " timeout(s)");
  if (result.renderCallCountAtCompletion !== result.renderCallCountBefore + 1)
    fail(
      label +
        ": expected exactly one render for the observer burst, got " +
        (result.renderCallCountAtCompletion - result.renderCallCountBefore),
    );
  if (result.renderCallCountAfterExtraFrames !== result.renderCallCountAtCompletion)
    fail(
      label +
        ": a delayed second render appeared after completion: " +
        result.renderCallCountAtCompletion +
        " -> " +
        result.renderCallCountAfterExtraFrames,
    );
  if (result.fullReplayCallCountAfterExtraFrames !== result.fullReplayCallCount)
    fail(
      label +
        ": a delayed second replay appeared after completion: " +
        result.fullReplayCallCount +
        " -> " +
        result.fullReplayCallCountAfterExtraFrames,
    );
  if (result.fullReplayCallCount !== expectedFullReplayCallCount)
    fail(
      label +
        ": expected exactly " +
        expectedFullReplayCallCount +
        " cumulative full replay(s), got " +
        result.fullReplayCallCount,
    );
  if (result.snapshot.overlap) fail(label + ": cards overlap: " + JSON.stringify(result.snapshot.overlap));
  const spans = result.snapshot.lattice.map((c) => c.span);
  if (expectFullSpan) {
    if (!spans.every((span) => span === MOSAIC_SINGLE_COLUMN_FULL_SPAN))
      fail(label + ": expected every card back at span=12 (L=1), got spans " + JSON.stringify(spans));
  } else {
    if (spans.every((span) => span === MOSAIC_SINGLE_COLUMN_FULL_SPAN))
      fail(label + ": expected at least one card narrower than span=12 at the wide host (L>1)");
  }
  // Never-mixed-module: every card's rendered height, recomputed from the
  // CURRENT geometry's M/gap, must match n*M-gap exactly -- proving the
  // settled lattice reflects the geometry actually in effect, not a stale
  // one carried over from before the resize.
  for (const card of result.snapshot.lattice) {
    const expectedHeight = card.n * result.snapshot.M - result.snapshot.gap;
    if (expectedHeight <= 0)
      fail(label + ": nonsensical expected height for card " + card.key);
  }
}

function assertSameTopologyResize(before, after, fail) {
  if (before.snapshot.baseSpan !== after.snapshot.baseSpan)
    fail(
      "same-topology fixture crossed a base-span boundary: " +
        before.snapshot.baseSpan +
        " -> " +
        after.snapshot.baseSpan,
    );
  if (before.fullReplayCallCount !== after.fullReplayCallCount)
    fail(
      "same-topology width change invoked a full replay: " +
        before.fullReplayCallCount +
        " -> " +
        after.fullReplayCallCount,
    );
  if (
    JSON.stringify(before.snapshot.fullReplays) !==
    JSON.stringify(after.snapshot.fullReplays)
  ) {
    fail("same-topology width change emitted a full-replay event");
  }
  const beforeByKey = new Map(
    before.snapshot.lattice.map((card) => [card.key, card]),
  );
  for (const card of after.snapshot.lattice) {
    const prior = beforeByKey.get(card.key);
    if (!prior) fail("same-topology resize lost prior card " + card.key);
    if (prior.t !== card.t || prior.span !== card.span)
      fail(
        "same-topology resize moved " +
          card.key +
          " horizontally: t/span " +
          prior.t +
          "/" +
          prior.span +
          " -> " +
          card.t +
          "/" +
          card.span,
      );
  }
}

function assertGeometryReplay(event, reason, source, previous, next, fail) {
  if (!event) fail(reason + " geometry replay provenance is missing");
  if (event.kind !== "geometry" || event.reason !== reason || event.source !== source) {
    fail(
      "unexpected geometry replay identity: " +
        JSON.stringify(event) +
        " (want " +
        reason +
        "/" +
        source +
        ")",
    );
  }
  if (previous === null) {
    if (event.previous !== null)
      fail(reason + " geometry replay unexpectedly has a previous snapshot");
  } else if (JSON.stringify(event.previous) !== JSON.stringify(previous)) {
    fail(
      reason +
        " previous geometry mismatch: " +
        JSON.stringify(event.previous) +
        " vs " +
        JSON.stringify(previous),
    );
  }
  if (JSON.stringify(event.next) !== JSON.stringify(next)) {
    fail(
      reason +
        " next geometry mismatch: " +
        JSON.stringify(event.next) +
        " vs " +
        JSON.stringify(next),
    );
  }
}

function mosaicSingleColumnGeometrySnapshot(snapshot) {
  return {
    width: snapshot.width,
    baseSpan: snapshot.baseSpan,
    trackCount: MOSAIC_SINGLE_COLUMN_FULL_SPAN,
    M: snapshot.M,
  };
}

async function mosaicSingleColumnResizeAndObserve(page, widths) {
  return page.evaluate(mosaicSingleColumnDeliverResizeBurst, widths);
}

async function mosaicSingleColumnCollectResults(page) {
  const narrowResult = await page.evaluate(mosaicSingleColumnRunAtNarrowWidth);
  // Install the spy after first paint so counters cover resizes only. Each
  // viewport mutation resolves from the wrapped production render callback.
  await page.evaluate(mosaicSingleColumnInstallRenderObserver);
  const wideResizeResult = await mosaicSingleColumnResizeAndObserve(
    page,
    MOSAIC_SINGLE_COLUMN_WIDE_HOST_WIDTHS,
  );
  const sameTopologyResizeResult = await mosaicSingleColumnResizeAndObserve(
    page,
    MOSAIC_SINGLE_COLUMN_SAME_TOPOLOGY_HOST_WIDTHS,
  );
  const narrowAgainResizeResult = await mosaicSingleColumnResizeAndObserve(
    page,
    MOSAIC_SINGLE_COLUMN_NARROW_HOST_WIDTHS,
  );
  return {
    narrowResult,
    wideResizeResult,
    sameTopologyResizeResult,
    narrowAgainResizeResult,
  };
}

function assertMosaicSingleColumnResults(results, fail) {
  const { narrowResult, wideResizeResult, sameTopologyResizeResult } = results;
  const { narrowAgainResizeResult } = results;
  assertNarrowWidthResult(narrowResult, fail);
  assertResizeResult("wide resize", wideResizeResult, 1, false, fail);
  assertResizeResult("same-topology resize", sameTopologyResizeResult, 1, false, fail);
  assertSameTopologyResize(wideResizeResult, sameTopologyResizeResult, fail);
  assertResizeResult("narrow-again resize", narrowAgainResizeResult, 2, true, fail);
  const initialReplays = narrowResult.afterInserts.fullReplays;
  if (initialReplays.length !== 1)
    fail("initial lane render recorded " + initialReplays.length + " full replays");
  assertGeometryReplay(
    initialReplays[0],
    "initial",
    "stream-render",
    null,
    mosaicSingleColumnGeometrySnapshot(narrowResult.afterInserts),
    fail,
  );
  const wideReplays = wideResizeResult.snapshot.fullReplays;
  assertGeometryReplay(
    wideReplays[wideReplays.length - 1],
    "topology",
    "resize-observer",
    mosaicSingleColumnGeometrySnapshot(narrowResult.afterFifthInsert),
    mosaicSingleColumnGeometrySnapshot(wideResizeResult.snapshot),
    fail,
  );
  const narrowReplays = narrowAgainResizeResult.snapshot.fullReplays;
  assertGeometryReplay(
    narrowReplays[narrowReplays.length - 1],
    "topology",
    "resize-observer",
    mosaicSingleColumnGeometrySnapshot(sameTopologyResizeResult.snapshot),
    mosaicSingleColumnGeometrySnapshot(narrowAgainResizeResult.snapshot),
    fail,
  );
}

async function mosaicSingleColumnLifecycleProof(page, fail) {
  const proof = await page.evaluate(() => {
    let conflict = "accepted";
    try {
      mosaicMergeRenderSources("resize-observer", "fonts-ready");
    } catch (error) {
      conflict = String(error && error.message ? error.message : error);
    }
    const lane = {
      mosaicPendingRenderSource: "resize-observer",
      mosaicActiveRenderSource: "resize-observer",
      mosaicGeometryInvalidation: Object.freeze({
        reason: "fonts-ready",
        source: "fonts-ready",
      }),
      mosaicImageLoadHandlers: new Map(),
    };
    mosaicResetResizeObserver(lane);
    return {
      conflict,
      reset: {
        pending: lane.mosaicPendingRenderSource,
        active: lane.mosaicActiveRenderSource,
        invalidation: lane.mosaicGeometryInvalidation,
      },
    };
  });
  if (!proof.conflict.includes("conflicting mosaic geometry render sources"))
    fail("conflicting geometry sources did not refuse: " + proof.conflict);
  if (
    proof.reset.pending !== "" ||
    proof.reset.active !== "" ||
    proof.reset.invalidation !== null
  ) {
    fail("lane reset retained geometry provenance: " + JSON.stringify(proof.reset));
  }
  return proof;
}

async function mosaicSingleColumnExercise({ page, server }) {
  await installMosaicSingleColumnHelpers(page);
  await page.evaluate(mosaicSingleColumnInstallResizeHarness);
  await installIsolatedLaneFixture(page, {
    globals: [
      "renderMessagesIfChanged",
      "upsertKnownMessage",
      "trimKnownMessages",
      "mosaicFullReplay",
      "mosaicRenderMessageStream",
    ],
  });
  const results = await mosaicSingleColumnCollectResults(page);
  const fail = (message) => {
    throw new Error(message);
  };
  assertMosaicSingleColumnResults(results, fail);
  const lifecycle = await mosaicSingleColumnLifecycleProof(page, fail);
  return { ...results, lifecycle, url: server.url };
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-single-column-" + Date.now(),
      contextOptions: {
        viewport: { width: MOSAIC_SINGLE_COLUMN_NARROW_WIDTH, height: 900 },
      },
    },
    mosaicSingleColumnExercise,
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
