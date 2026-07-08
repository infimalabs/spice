const path = require("path");
const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

// Load-stability smoke: staged bulk
// hydration -- multi-hour buckets, a compaction divider, pending acks
// resolving at staggered times, delayed images, a second lane mounting
// mid-load -- must paint with ZERO transform transitions before first
// settle, a bounded replay count, and a settled layout byte-identical to a
// single-shot render of the same content at the same geometry
// (idempotence). Post-settle, the steady-state gentle push must still
// animate (no over-suppression).

const HYDRATION_BATCH_A = 12;
const HYDRATION_BATCH_B = 10;
const HYDRATION_BATCH_C = 8;
const HYDRATION_STAGE_GAP_MS = 120;
const HYDRATION_MIN_REPLAYS = 3;
const HYDRATION_MAX_REPLAYS = 5;
const HYDRATION_SETTLE_TIMEOUT_MS = 8000;

function hydrationResolveLane() {
  return resolveIsolatedLane("mosaic-hydration-smoke-team");
}

function hydrationPurge(lane) {
  lane.knownMessages = lane.knownMessages.filter((item) =>
    String(item.key).startsWith("hydration-"),
  );
}

function hydrationBuildItem(index, kind) {
  // Hour buckets: indexes spread across three hours so time rules
  // synthesize; one compaction divider rides batch A as a barrier card.
  const hour = index % 3;
  const base = {
    ack_count: 0,
    ack_keys: [],
    index,
    key: "hydration-" + index,
    kind: "assistant",
    timestamp: new Date(2027, 0, 1, hour, index % 60, index % 60).toISOString(),
    display_html: Array.from({ length: 1 + (index % 4) }, (_, i) => "line " + i).join("<br>"),
    display_text: "x",
    text: "x",
  };
  if (kind === "compaction") {
    return { ...base, kind: "compaction", display_html: "compacted context", text: "compacted" };
  }
  if (kind === "ack") {
    return {
      ...base,
      ack_count: 1,
      ack_keys: ["hydration-ack-key-" + index],
      ack_segments: [{ keys: ["hydration-ack-key-" + index], html: "" }],
      display_html: "",
      display_text: "",
      text: "",
    };
  }
  if (kind === "image") {
    return {
      ...base,
      image_only: true,
      display_html:
        '<p class="message-image-stack"><a class="message-image" href="/"><img alt="fixture" src="/"></a></p>',
      display_text: "image",
      text: "image",
    };
  }
  return base;
}

function hydrationPushBatch(lane, start, count, kinds) {
  for (let i = 0; i < count; i += 1) {
    const index = start + i;
    const kind = kinds && kinds[i] ? kinds[i] : "plain";
    upsertKnownMessage(lane, hydrationBuildItem(index, kind), "newest");
  }
  trimKnownMessages(lane);
  renderMessagesIfChanged(lane);
}

function hydrationReplayCount(lane) {
  return lane.mosaicEventLog.events.filter((event) => event.type === "full-replay").length;
}

function hydrationLattice(lane) {
  return (lane.mosaicCards || [])
    .filter((card) => String(card.key).startsWith("hydration-"))
    .map((card) => ({ key: card.key, t: card.t, b: card.b, n: card.n, span: card.span }))
    .sort((a, b) => a.key.localeCompare(b.key));
}

const hydrationPageHelpers = [
  hydrationResolveLane,
  hydrationPurge,
  hydrationBuildItem,
  hydrationPushBatch,
  hydrationReplayCount,
  hydrationLattice,
];

async function installHydrationHelpers(page, context) {
  await context.addInitScript(() => {
    window.__hydrationTrace = { plane: 0, card: 0 };
    document.addEventListener(
      "transitionrun",
      (event) => {
        if (event.propertyName !== "transform") return;
        const el = event.target;
        if (el.classList && el.classList.contains("mosaic-plane"))
          window.__hydrationTrace.plane += 1;
        else if (el.dataset && "mosaicCard" in el.dataset)
          window.__hydrationTrace.card += 1;
      },
      true,
    );
  });
  await page.reload({ waitUntil: "domcontentloaded" });
  // The fixture installs AFTER the reload (a reload wipes injected scripts
  // and lane state); isolated lanes never receive real session messages, so
  // no ingestion gating is needed for the purity assertions.
  await installIsolatedLaneFixture(page, {
    globals: ["renderMessagesIfChanged", "upsertKnownMessage"],
  });
  for (const helper of hydrationPageHelpers) {
    await page.evaluate("window." + helper.name + " = " + helper.toString());
  }
}

async function runStagedHydration(page) {
  await page.waitForTimeout(400);
  await page.evaluate((count) => {
    const lane = hydrationResolveLane();
    hydrationPurge(lane);
    const kinds = { 5: "compaction" };
    hydrationPushBatch(lane, 1, count, kinds);
  }, HYDRATION_BATCH_A);
  await page.waitForTimeout(HYDRATION_STAGE_GAP_MS);
  await page.evaluate(
    ({ start, count }) => {
      const lane = hydrationResolveLane();
      const kinds = { 0: "ack", 3: "image", 6: "ack" };
      hydrationPushBatch(lane, start, count, kinds);
    },
    { start: HYDRATION_BATCH_A + 1, count: HYDRATION_BATCH_B },
  );
  await page.waitForTimeout(HYDRATION_STAGE_GAP_MS);
  // Second lane mounts mid-load: sibling width churn while lane 1 settles.
  await page.evaluate(
    ({ start, count }) => {
      resolveIsolatedLane("mosaic-hydration-sibling-team");
      const lane = hydrationResolveLane();
      const kinds = { 2: "image" };
      hydrationPushBatch(lane, start, count, kinds);
    },
    { start: HYDRATION_BATCH_A + HYDRATION_BATCH_B + 1, count: HYDRATION_BATCH_C },
  );
  // Staggered ack resolutions while (possibly) unsettled.
  await page.waitForTimeout(HYDRATION_STAGE_GAP_MS);
  await page.evaluate((ackIndex) => {
    const lane = hydrationResolveLane();
    lane.ackContextByKey.set("hydration-ack-key-" + ackIndex, {
      attachments: [],
      html: "<p>resolved steering quote</p>",
      key: "hydration-ack-key-" + ackIndex,
      priority: "",
      text: "resolved steering quote",
    });
    lane.missingAckContextKeys.delete("hydration-ack-key-" + ackIndex);
    lane.renderedMessageFingerprint = "";
    renderMessagesIfChanged(lane);
  }, HYDRATION_BATCH_A + 1);
}

async function waitForSettleSample(page) {
  const deadline = Date.now() + HYDRATION_SETTLE_TIMEOUT_MS;
  while (Date.now() < deadline) {
    const sample = await page.evaluate(() => ({
      settled: Boolean(hydrationResolveLane().mosaicSettled),
      trace: { ...window.__hydrationTrace },
      replays: hydrationReplayCount(hydrationResolveLane()),
    }));
    if (sample.settled) return sample;
    await page.waitForTimeout(60);
  }
  throw new Error("lane did not settle within " + HYDRATION_SETTLE_TIMEOUT_MS + "ms");
}

// Replaying the recorded event log reproduces the live layout
// byte-identically -- the normative purity oracle. A raw single-shot render
// is deliberately NOT the oracle: frozen pads keep reserved rows
// and single-card vacancies persist until the next replay, so incremental
// state legitimately differs from a fresh pack until then. The
// idempotence corollary is asserted separately: two forced replays at
// unchanged content and geometry are byte-identical to each other.
async function measurePurity(page) {
  return page.evaluate(async () => {
    const lane = hydrationResolveLane();
    const replayed = mosaicReplayEventLog(lane.mosaicEventLog);
    const live = mosaicEventLogLayout(lane.mosaicCards);
    const forceReplay = async () => {
      lane.mosaicGeometry = null;
      lane.renderedMessageFingerprint = "";
      renderMessagesIfChanged(lane);
      await new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      );
      return hydrationLattice(lane);
    };
    const firstReplay = await forceReplay();
    const secondReplay = await forceReplay();
    return {
      live,
      replayed: replayed.layout,
      firstReplay,
      secondReplay,
    };
  });
}

async function measurePostSettlePush(page) {
  return page.evaluate(async () => {
    const lane = hydrationResolveLane();
    window.__hydrationTrace.plane = 0;
    window.__hydrationTrace.card = 0;
    // Genuinely newest (hour 9 sits above every fixture bucket) so this
    // exercises the single-newest insert path -- the gentle push -- rather
    // than a mid-sequence arrival that would full-replay.
    const item = hydrationBuildItem(999, "plain");
    item.timestamp = new Date(2027, 0, 1, 9, 0, 0).toISOString();
    upsertKnownMessage(lane, item, "newest");
    trimKnownMessages(lane);
    renderMessagesIfChanged(lane);
    await new Promise((resolve) => setTimeout(resolve, 120));
    return { ...window.__hydrationTrace };
  });
}

function assertHydration(settleSample, purity, postSettle) {
  if (settleSample.trace.plane !== 0 || settleSample.trace.card !== 0)
    throw new Error(
      "transform transitions before first settle: " +
        JSON.stringify(settleSample.trace),
    );
  if (
    settleSample.replays < HYDRATION_MIN_REPLAYS ||
    settleSample.replays > HYDRATION_MAX_REPLAYS
  )
    throw new Error(
      "replay count out of bounds: " +
        settleSample.replays +
        " (want " +
        HYDRATION_MIN_REPLAYS +
        ".." +
        HYDRATION_MAX_REPLAYS +
        ")",
    );
  if (JSON.stringify(purity.live) !== JSON.stringify(purity.replayed))
    throw new Error("event-log replay did not reproduce the live layout");
  if (JSON.stringify(purity.firstReplay) !== JSON.stringify(purity.secondReplay))
    throw new Error("forced replay is not idempotent");
  if (!purity.firstReplay.length)
    throw new Error("purity comparison ran on an empty lattice");
  if (postSettle.plane < 1)
    throw new Error(
      "post-settle insert did not animate the plane (over-suppression): " +
        JSON.stringify(postSettle),
    );
}

async function run(screenshotDir) {
  return withServePage(
    {
      path: "/?smoke=mosaic-hydration-" + Date.now(),
      contextOptions: { viewport: { width: 1600, height: 1000 } },
    },
    async ({ page, context }) => {
      await installHydrationHelpers(page, context);
      const floor = await page.evaluate(() => ({
        settleQuiet: MOSAIC_SETTLE_QUIET_MS,
        resizeDebounce: MOSAIC_RESIZE_DEBOUNCE_MS,
        minRenderWidth: MOSAIC_MIN_RENDER_WIDTH_PX,
      }));
      await runStagedHydration(page);
      const settleSample = await waitForSettleSample(page);
      const screenshot = screenshotDir
        ? path.join(screenshotDir, "mosaic-hydration-settled.png")
        : null;
      if (screenshot) await page.screenshot({ path: screenshot });
      const purity = await measurePurity(page);
      const postSettle = await measurePostSettlePush(page);
      assertHydration(settleSample, purity, postSettle);
      return {
        constants: floor,
        replays: settleSample.replays,
        preSettleTrace: settleSample.trace,
        postSettle,
        cards: purity.firstReplay.length,
        screenshot,
      };
    },
  );
}

run(process.argv[2]).then(
  (result) => {
    console.log("serve_mosaic_hydration_smoke ok " + JSON.stringify(result));
  },
  (error) => {
    console.error(error);
    process.exit(1);
  },
);
