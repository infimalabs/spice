const { withServePage } = require("./serve_playwright_harness");

// Hidden-render deferral (freeze-hidden-guard): a lane whose host is
// unmeasurable must not touch the lattice, the plane, or any style; the
// reveal runs one motion-suppressed correction render at real geometry.
// Guards the probe-confirmed defect where a clientWidth-0 render degenerated
// edges to [0..12] and full-replayed every card into a 12px stack, arming
// the transform-freeze on reveal.

const HIDDEN_REVEAL_SEED_COUNT = 12;

function hiddenRevealBuildItem(index, lines) {
  const html = Array.from({ length: lines }, (_, i) => "line " + (i + 1)).join("<br>");
  return {
    ack_count: 0,
    ack_keys: [],
    index,
    key: "hidden-reveal-" + index,
    kind: "assistant",
    timestamp: new Date(2027, 0, 1, 0, 0, index).toISOString(),
    display_html: html,
    display_text: html,
    text: html,
  };
}

function hiddenRevealResolveLane() {
  let lane = Array.from(laneStates.values()).find((item) => !item.emptyTeam);
  if (!lane && targets.length) {
    addLane(targets[0].id);
    lane = laneStates.get(targets[0].id);
  }
  if (!lane) throw new Error("no lane available");
  return lane;
}

function hiddenRevealSnapshot(lane) {
  return {
    geometry: lane.mosaicGeometry
      ? { M: lane.mosaicGeometry.M, edges12: lane.mosaicGeometry.edges[12] }
      : null,
    lattice: (lane.mosaicCards || [])
      .filter((c) => String(c.key).startsWith("hidden-reveal-"))
      .map((c) => ({ key: c.key, t: c.t, b: c.b, n: c.n, span: c.span })),
    planeTransform: lane.mosaicPlaneEl ? lane.mosaicPlaneEl.style.transform : null,
    styles: Array.from(
      lane.mosaicPlaneEl ? lane.mosaicPlaneEl.querySelectorAll("[data-mosaic-card]") : [],
    )
      .filter((el) => (el.dataset.messageKey || "").startsWith("hidden-reveal-"))
      .map((el) => ({
        key: el.dataset.messageKey || "",
        transform: el.style.transform,
        width: el.style.width,
        height: el.style.height,
      })),
  };
}

function hiddenRevealPurgeLiveTraffic(lane) {
  // The harness serve streams real session messages; this smoke asserts
  // exact lattice identity, so controlled renders only see synthetic keys.
  lane.knownMessages = lane.knownMessages.filter((item) =>
    String(item.key).startsWith("hidden-reveal-"),
  );
}

// Rendered-vs-inline agreement: the frozen-transform defect manifests as
// getBoundingClientRect permanently disagreeing with the inline transform
// after a reveal. Cards are compared against the plane's own rect so the
// check holds regardless of scroll position or host padding.
function hiddenRevealRectAgreement(lane) {
  const plane = lane.mosaicPlaneEl;
  const planeRect = plane.getBoundingClientRect();
  const parse = (transform) => {
    const match = /translate\((-?[\d.]+)px,\s*(-?[\d.]+)px\)/.exec(transform || "");
    return match ? { x: Number(match[1]), y: Number(match[2]) } : null;
  };
  const planeMatch = /translateY\((-?[\d.]+)px\)/.exec(plane.style.transform || "");
  const planeInlineY = planeMatch ? Number(planeMatch[1]) : NaN;
  const computed = new DOMMatrixReadOnly(getComputedStyle(plane).transform);
  const disagreements = [];
  if (Math.abs(computed.m42 - planeInlineY) > 1)
    disagreements.push("plane rendered " + computed.m42 + " vs inline " + planeInlineY);
  for (const el of plane.querySelectorAll("[data-mosaic-card]")) {
    if (!(el.dataset.messageKey || "").startsWith("hidden-reveal-")) continue;
    const inline = parse(el.style.transform);
    if (!inline) continue;
    const rect = el.getBoundingClientRect();
    const dx = rect.left - planeRect.left - inline.x;
    const dy = rect.top - planeRect.top - inline.y;
    if (Math.abs(dx) > 1 || Math.abs(dy) > 1)
      disagreements.push(el.dataset.messageKey + " off by " + dx + "," + dy);
  }
  return {
    disagreements,
    scrollDelta: lane.messagesEl.scrollWidth - lane.messagesEl.clientWidth,
    scrollHeight: lane.messagesEl.scrollHeight,
    planeHeight: plane.getBoundingClientRect().height,
  };
}

function hiddenRevealWatchWrites(lane) {
  window.__hiddenRevealWrites = [];
  window.__hiddenRevealTransitions = [];
  const observer = new MutationObserver((records) => {
    for (const record of records) {
      const key = record.target.dataset
        ? record.target.dataset.messageKey || record.target.className
        : "?";
      window.__hiddenRevealWrites.push(String(key));
    }
  });
  observer.observe(lane.messagesEl, {
    subtree: true,
    attributes: true,
    attributeFilter: ["style"],
  });
  window.__hiddenRevealWriteObserver = observer;
  lane.messagesEl.addEventListener(
    "transitionrun",
    (event) => {
      if (event.propertyName === "transform")
        window.__hiddenRevealTransitions.push(event.target.className);
    },
    true,
  );
}

const hiddenRevealPageHelpers = [
  hiddenRevealBuildItem,
  hiddenRevealResolveLane,
  hiddenRevealSnapshot,
  hiddenRevealPurgeLiveTraffic,
  hiddenRevealRectAgreement,
  hiddenRevealWatchWrites,
];

async function installHiddenRevealHelpers(page) {
  await page.waitForFunction(
    () =>
      typeof renderMessagesIfChanged === "function" &&
      typeof upsertKnownMessage === "function" &&
      Array.isArray(targets) &&
      targets.length > 0,
    { timeout: 10000 },
  );
  for (const helper of hiddenRevealPageHelpers) {
    await page.evaluate("window." + helper.name + " = " + helper.toString());
  }
}

function hiddenRevealWaitFrames(page) {
  return page.evaluate(
    () =>
      new Promise((resolve) =>
        requestAnimationFrame(() =>
          requestAnimationFrame(() => requestAnimationFrame(resolve)),
        ),
      ),
  );
}

async function measureHiddenRender(page, config) {
  await page.waitForTimeout(800);
  return page.evaluate((cfg) => {
    const lane = hiddenRevealResolveLane();
    hiddenRevealPurgeLiveTraffic(lane);
    for (let i = 1; i <= cfg.seedCount; i += 1) {
      upsertKnownMessage(lane, hiddenRevealBuildItem(i, 1 + (i % 4)), "newest");
      trimKnownMessages(lane);
      renderMessagesIfChanged(lane);
    }
    hiddenRevealPurgeLiveTraffic(lane);
    lane.renderedMessageFingerprint = "";
    renderMessagesIfChanged(lane);
    const before = hiddenRevealSnapshot(lane);
    lane.element.style.display = "none";
    hiddenRevealWatchWrites(lane);
    upsertKnownMessage(lane, hiddenRevealBuildItem(cfg.seedCount + 1, 3), "newest");
    trimKnownMessages(lane);
    renderMessagesIfChanged(lane);
    return {
      before,
      after: hiddenRevealSnapshot(lane),
      deferred: Boolean(lane.mosaicRenderDeferred),
      writes: window.__hiddenRevealWrites.slice(),
    };
  }, config);
}

function assertHiddenRender(hidden) {
  if (!hidden.deferred) throw new Error("hidden render did not defer");
  if (hidden.writes.length)
    throw new Error("hidden render wrote styles: " + hidden.writes.join(","));
  if (JSON.stringify(hidden.before) !== JSON.stringify(hidden.after))
    throw new Error("hidden render mutated lattice/styles");
}

async function measureReveal(page, config) {
  await page.evaluate(() => {
    const lane = hiddenRevealResolveLane();
    hiddenRevealPurgeLiveTraffic(lane);
    window.__hiddenRevealWrites.length = 0;
    window.__hiddenRevealTransitions.length = 0;
    lane.element.style.display = "";
  });
  await hiddenRevealWaitFrames(page);
  return page.evaluate((cfg) => {
    const lane = hiddenRevealResolveLane();
    const snapshot = hiddenRevealSnapshot(lane);
    const queuedKey = "hidden-reveal-" + (cfg.seedCount + 1);
    return {
      snapshot,
      queued: snapshot.lattice.find((card) => card.key === queuedKey),
      deferred: Boolean(lane.mosaicRenderDeferred),
      writes: window.__hiddenRevealWrites.slice(),
      transitions: window.__hiddenRevealTransitions.slice(),
      scrollDelta: lane.messagesEl.scrollWidth - lane.messagesEl.clientWidth,
    };
  }, config);
}

function assertReveal(revealed, hidden) {
  if (revealed.deferred) throw new Error("reveal did not clear deferral");
  if (!revealed.queued)
    throw new Error("queued hidden-arrival message missing after reveal");
  if (revealed.transitions.length)
    throw new Error(
      "transform transitions during reveal: " + revealed.transitions.join(","),
    );
  if (revealed.scrollDelta > 0)
    throw new Error("horizontal overflow after reveal: " + revealed.scrollDelta);
  const beforeByKey = new Map(
    hidden.before.lattice.map((card) => [card.key, JSON.stringify(card)]),
  );
  for (const card of revealed.snapshot.lattice) {
    if (card.key === revealed.queued.key) continue;
    if (beforeByKey.get(card.key) !== JSON.stringify(card))
      throw new Error("pre-hide card moved across hide/reveal: " + card.key);
  }
  if (revealed.snapshot.geometry.edges12 !== hidden.before.geometry.edges12)
    throw new Error("geometry drifted across hide/reveal round-trip");
}

// Mid-tween interrupt: hide the lane while a plane tween is in flight
// (fresh insert at top animates the push-down), reveal, and require the
// rebuild to land rendered == inline with sane scroll extents.
async function measureMidTweenInterrupt(page, config) {
  await page.evaluate((cfg) => {
    const lane = hiddenRevealResolveLane();
    hiddenRevealPurgeLiveTraffic(lane);
    upsertKnownMessage(lane, hiddenRevealBuildItem(cfg.seedCount + 2, 4), "newest");
    trimKnownMessages(lane);
    renderMessagesIfChanged(lane);
    lane.element.style.display = "none";
    lane.renderedMessageFingerprint = "";
    renderMessagesIfChanged(lane);
    lane.element.style.display = "";
  }, config);
  await hiddenRevealWaitFrames(page);
  return page.evaluate(() => hiddenRevealRectAgreement(hiddenRevealResolveLane()));
}

function assertAgreement(agreement, label) {
  if (agreement.disagreements.length)
    throw new Error(
      label + " rendered/inline disagreement: " + agreement.disagreements.join("; "),
    );
  if (agreement.scrollDelta > 0)
    throw new Error(label + " horizontal overflow: " + agreement.scrollDelta);
  if (agreement.scrollHeight > agreement.planeHeight + 64)
    throw new Error(
      label +
        " scroll extent inflated: scrollHeight " +
        agreement.scrollHeight +
        " vs plane " +
        agreement.planeHeight,
    );
}

async function measureRoundTrip(page) {
  const started = await page.evaluate(() => {
    const lane = hiddenRevealResolveLane();
    hiddenRevealPurgeLiveTraffic(lane);
    const before = hiddenRevealSnapshot(lane);
    lane.element.style.display = "none";
    window.__hiddenRevealWrites.length = 0;
    lane.renderedMessageFingerprint = "";
    renderMessagesIfChanged(lane);
    hiddenRevealPurgeLiveTraffic(lane);
    lane.element.style.display = "";
    return { before, writes: window.__hiddenRevealWrites.slice() };
  });
  await hiddenRevealWaitFrames(page);
  const after = await page.evaluate(() => ({
    snapshot: hiddenRevealSnapshot(hiddenRevealResolveLane()),
    writes: window.__hiddenRevealWrites.slice(),
  }));
  return { started, after };
}

function assertRoundTrip(roundTrip) {
  if (roundTrip.started.writes.length)
    throw new Error("no-change hidden render wrote styles");
  if (
    JSON.stringify(roundTrip.after.snapshot.lattice) !==
    JSON.stringify(roundTrip.started.before.lattice)
  )
    throw new Error("no-change round-trip changed the lattice");
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=mosaic-hidden-reveal-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 900 } },
    },
    async ({ page }) => {
      await installHiddenRevealHelpers(page);
      const config = { seedCount: HIDDEN_REVEAL_SEED_COUNT };
      const floor = await page.evaluate(() => MOSAIC_MIN_RENDER_WIDTH_PX);
      if (!Number.isFinite(floor) || floor <= 0)
        throw new Error("MOSAIC_MIN_RENDER_WIDTH_PX not surfaced: " + floor);
      const hidden = await measureHiddenRender(page, config);
      assertHiddenRender(hidden);
      const revealed = await measureReveal(page, config);
      assertReveal(revealed, hidden);
      const revealAgreement = await page.evaluate(() =>
        hiddenRevealRectAgreement(hiddenRevealResolveLane()),
      );
      assertAgreement(revealAgreement, "reveal");
      const roundTrip = await measureRoundTrip(page);
      assertRoundTrip(roundTrip);
      const midTween = await measureMidTweenInterrupt(page, config);
      assertAgreement(midTween, "mid-tween interrupt");
      return {
        floor,
        queued: revealed.queued,
        revealWrites: revealed.writes.length,
        roundTripWrites: roundTrip.after.writes.length,
      };
    },
  );
}

run().then(
  (result) => {
    console.log("serve_mosaic_hidden_reveal_smoke ok " + JSON.stringify(result));
  },
  (error) => {
    console.error(error);
    process.exit(1);
  },
);
