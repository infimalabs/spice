const { withServePage } = require("./serve_playwright_harness");

// Ack quote resolution riding the mosaic-stream-integration resolution
// paths end to end (mosaic-ack-resolution):
// drives real lanes through renderMessagesIfChanged exactly as live ack
// hydration would (mutating lane.ackContextByKey/missingAckContextKeys
// directly, not faking the fetch), and checks that resolution takes
// exactly one of the three defined resolution branches -- wetReplay,
// frozen pad (zero writes), or frozen ripple -- plus the two safety
// properties (resolution racing removal is a no-op; simultaneous
// resolutions are creation-order stable).

// Plain-card index ranges for the frozen-fixture and order-stability
// fixtures below (mirrors serve_mosaic_stream_smoke.js's frozen-growth
// fixture: enough same-column siblings to bury the target card two deep).
const MOSAIC_ACK_FREEZE_PLAIN_FIRST_INDEX = 21;
const MOSAIC_ACK_FREEZE_PLAIN_LAST_INDEX = 26;
const MOSAIC_ACK_ORDER_FIRST_BLOCK_START = 41;
const MOSAIC_ACK_ORDER_FIRST_BLOCK_END = 44;
const MOSAIC_ACK_ORDER_SECOND_BLOCK_START = 46;
const MOSAIC_ACK_ORDER_SECOND_BLOCK_END = 47;

function mosaicAckResolveLane() {
  let lane = Array.from(laneStates.values()).find((item) => !item.emptyTeam);
  if (!lane && targets.length) {
    addLane(targets[0].id);
    lane = laneStates.get(targets[0].id);
  }
  if (!lane) throw new Error("no lane available for mosaic ack resolution smoke");
  return lane;
}

function mosaicAckBuildPlainItem(index, lines) {
  const html = Array.from({ length: lines }, (_, i) => "line " + (i + 1)).join("<br>");
  return {
    ack_count: 0,
    ack_keys: [],
    index,
    key: "ack-plain-" + index,
    kind: "assistant",
    timestamp: new Date(2026, 0, 1, 0, 0, index).toISOString(),
    display_html: html,
    display_text: html,
    text: html,
  };
}

function mosaicAckBuildCompleteImageItem(index) {
  const text = Array.from(
    { length: 24 },
    (_, i) => "already complete image line " + (i + 1),
  ).join("<br>");
  const imageHtml =
    '<p class="message-image-stack"><a class="message-image"><img alt="already complete"></a></p>';
  return {
    ack_count: 0,
    ack_keys: [],
    index,
    key: "ack-complete-image-" + index,
    kind: "assistant",
    timestamp: new Date(2026, 0, 1, 0, 0, index).toISOString(),
    display_html: text + imageHtml,
    display_text: text,
    text,
  };
}

// segment html is deliberately empty: renderMessageContent renders ONLY
// the ack quote/skeleton for a segment-driven item with no segment body
// text, so the reservation (sized only for the quote block) actually
// represents the whole card -- an ack reply with extra prose alongside it
// would need a bigger, content-aware reservation this task does not add.
function mosaicAckBuildAckItem(index, ackKey) {
  return {
    ack_count: 1,
    ack_keys: [ackKey],
    ack_segments: [{ keys: [ackKey], html: "" }],
    index,
    key: "ack-card-" + index,
    kind: "assistant",
    timestamp: new Date(2026, 0, 1, 0, 0, index).toISOString(),
    display_html: "",
    display_text: "",
    text: "",
  };
}

function mosaicAckPush(lane, item) {
  upsertKnownMessage(lane, item, "newest");
  trimKnownMessages(lane);
  renderMessagesIfChanged(lane);
}

function mosaicAckResolve(lane, ackKey, html) {
  lane.ackContextByKey.set(ackKey, {
    attachments: [],
    html,
    key: ackKey,
    priority: "",
    text: html.replace(/<[^>]+>/g, ""),
  });
  lane.missingAckContextKeys.delete(ackKey);
}

function mosaicAckSnapshot(lane) {
  const cards = Array.from(
    lane.mosaicPlaneEl.querySelectorAll("article[data-message-key]"),
  ).map((el) => ({
    key: el.dataset.messageKey,
    transform: el.style.transform,
    width: el.style.width,
    height: el.style.height,
    hasSkeleton: Boolean(el.querySelector(".ack-quote--pending")),
    hasQuote: Boolean(el.querySelector(".ack-quote:not(.ack-quote--pending)")),
  }));
  return { cards, lattice: lane.mosaicCards.map((c) => ({ ...c })) };
}

function mosaicAckResetLane(lane) {
  if (lane.mosaicPlaneEl) lane.mosaicPlaneEl.remove();
  lane.mosaicPlaneEl = null;
  lane.mosaicCards = [];
  lane.mosaicNextCreationIndex = 0;
  lane.mosaicNextBackfillCreationIndex = -1;
  lane.mosaicEventLog = mosaicCreateEventLog(mosaicGridTrackCount, MOSAIC_FREEZE_DEPTH);
  lane.mosaicGeometry = null;
  lane.knownMessages = [];
  lane.knownMessageKeys = new Set();
  lane.renderedMessageFingerprint = "";
  lane.ackContextByKey.clear();
  lane.missingAckContextKeys.clear();
}

// A pending ack reserves its quote block instead of measuring -- the
// skeleton (.ack-quote--pending) renders and the card's row count comes
// from mosaicReservationRows("ack", ...), not from measuring "reply body".
function mosaicAckPendingReservationCheck() {
  const lane = mosaicAckResolveLane();
  mosaicAckPush(lane, mosaicAckBuildAckItem(1, "ack-key-pending"));
  const card = lane.mosaicCards.find((c) => c.key === "ack-card-1");
  const node = lane.mosaicPlaneEl.querySelector('article[data-message-key="ack-card-1"]');
  const geometry = lane.mosaicGeometry;
  const expectedRows = mosaicReservationRows(
    "ack",
    mosaicRootFontSizePx(),
    geometry.gap,
    geometry.M,
  );
  return {
    hasSkeleton: Boolean(node.querySelector(".ack-quote--pending")),
    cardN: card.n,
    expectedRows,
  };
}

// wetReplay: a still-wet ack card resolving must swap the skeleton for
// the real quote, update its own row count to reflect the real content,
// and go through wetReplay (never touching a frozen card, none exist yet).
function mosaicAckWetResolutionCheck() {
  const lane = mosaicAckResolveLane();
  mosaicAckPush(lane, mosaicAckBuildAckItem(10, "ack-key-wet"));
  const before = mosaicAckSnapshot(lane);
  const beforeCard = before.lattice.find((c) => c.key === "ack-card-10");

  mosaicAckResolve(lane, "ack-key-wet", "<p>quoted context arrives</p>");
  renderMessagesIfChanged(lane);
  const after = mosaicAckSnapshot(lane);
  const afterCard = after.lattice.find((c) => c.key === "ack-card-10");
  const node = lane.mosaicPlaneEl.querySelector('article[data-message-key="ack-card-10"]');

  return {
    beforeHasSkeleton: before.cards[0].hasSkeleton,
    afterHasSkeleton: Boolean(node.querySelector(".ack-quote--pending")),
    afterHasQuote: Boolean(node.querySelector(".ack-quote:not(.ack-quote--pending)")),
    beforeFrozen: beforeCard.frozen,
    afterFrozen: afterCard.frozen,
    transformSet: node.style.transform !== "",
    heightSet: node.style.height !== "",
  };
}

// A lookup that comes back found:false is CONFIRMED absent: the shimmer
// swaps for a same-height "unavailable" block so the reservation is honored
// with ZERO reflow -- the card keeps its exact row count, nothing below it
// moves, and the reader sees it failed rather than a spinner that never ends.
function mosaicAckMissingResolutionCheck() {
  const lane = mosaicAckResolveLane();
  mosaicAckPush(lane, mosaicAckBuildAckItem(15, "ack-key-missing"));
  const beforeCard = lane.mosaicCards.find((c) => c.key === "ack-card-15");
  const beforeNode = lane.mosaicPlaneEl.querySelector(
    'article[data-message-key="ack-card-15"]',
  );
  const beforeHasSkeleton = Boolean(beforeNode.querySelector(".ack-quote--pending"));
  const beforeRows = beforeCard.n;

  // Simulate the found:false branch of hydrateAckContextsForMessages.
  lane.missingAckContextKeys.add("ack-key-missing");
  renderMessagesIfChanged(lane);
  const afterCard = lane.mosaicCards.find((c) => c.key === "ack-card-15");
  const afterNode = lane.mosaicPlaneEl.querySelector(
    'article[data-message-key="ack-card-15"]',
  );

  return {
    beforeHasSkeleton,
    afterHasSkeleton: Boolean(afterNode.querySelector(".ack-quote--pending")),
    afterHasMissing: Boolean(afterNode.querySelector(".ack-quote--missing")),
    afterHasRealQuote: Boolean(
      afterNode.querySelector(
        ".ack-quote:not(.ack-quote--pending):not(.ack-quote--missing)",
      ),
    ),
    rowsUnchanged: afterCard.n === beforeRows,
    nodeRebuilt: afterNode !== beforeNode,
  };
}

// Builds a frozen ack scenario: the ack card (msg-20) plus 6 short cards
// settling into 2 columns (mirrors serve_mosaic_stream_smoke.js's frozen-
// growth fixture) so msg-20 is buried by 2 later same-column cards and
// freezes before its ack ever resolves.
function mosaicAckFreezeFixture(lane, ackKey) {
  mosaicAckPush(lane, mosaicAckBuildAckItem(20, ackKey));
  for (let i = MOSAIC_ACK_FREEZE_PLAIN_FIRST_INDEX; i <= MOSAIC_ACK_FREEZE_PLAIN_LAST_INDEX; i += 1)
    mosaicAckPush(lane, mosaicAckBuildPlainItem(i, 1));
}

// A frozen ack resolving to content that still fits within (or exactly
// matches) its reservation must pad in place -- zero position/size writes
// on ANY card, including the resolving one itself (this is exactly the
// case the node-identity fix in mosaicApplyRender exists for: the DOM node
// is replaced, but the lattice record is not, so the fresh node must still
// receive its transform/width/height at least once).
function mosaicAckFrozenPadCheck() {
  const lane = mosaicAckResolveLane();
  mosaicAckFreezeFixture(lane, "ack-key-frozen-pad");
  const card = lane.mosaicCards.find((c) => c.key === "ack-card-20");
  if (!card.frozen) throw new Error("ack-card-20 did not freeze as expected by the fixture");
  const before = mosaicAckSnapshot(lane);

  mosaicAckResolve(lane, "ack-key-frozen-pad", "<p>hi</p>");
  renderMessagesIfChanged(lane);
  const after = mosaicAckSnapshot(lane);
  const node = lane.mosaicPlaneEl.querySelector('article[data-message-key="ack-card-20"]');

  return {
    before,
    after,
    nodeStillHasTransform: node.style.transform !== "",
    nodeHasQuote: Boolean(node.querySelector(".ack-quote:not(.ack-quote--pending)")),
  };
}

// rippleRows: a frozen ack resolving to content TALLER than its
// reservation must extend downward (keep b+n fixed on itself) and ripple
// the same-column chain resting below it by exactly the growth delta,
// touching nothing above it or in the other column.
function mosaicAckFrozenRippleCheck() {
  const lane = mosaicAckResolveLane();
  mosaicAckFreezeFixture(lane, "ack-key-frozen-ripple");
  const before = mosaicAckSnapshot(lane);
  const target = before.lattice.find((c) => c.key === "ack-card-20");
  const below = before.lattice.filter(
    (c) => c.key !== target.key && c.t === target.t && c.span === target.span && c.b < target.b,
  );
  const unrelated = before.lattice.filter(
    (c) => c.key !== target.key && !(c.t === target.t && c.span === target.span && c.b < target.b),
  );

  const longHtml =
    "<p>" +
    Array.from({ length: 10 }, (_, i) => "grown reply line " + (i + 1)).join("<br>") +
    "</p>";
  mosaicAckResolve(lane, "ack-key-frozen-ripple", longHtml);
  renderMessagesIfChanged(lane);
  const after = mosaicAckSnapshot(lane);
  const byKey = (list) => list.map((c) => after.lattice.find((candidate) => candidate.key === c.key));

  return {
    grownBefore: target,
    grownAfter: after.lattice.find((c) => c.key === target.key),
    below,
    belowAfter: byKey(below),
    unrelated,
    unrelatedAfter: byKey(unrelated),
    cardCountUnchanged: before.lattice.length === after.lattice.length,
  };
}

// Resolution racing removal: the ack context resolves for a message that
// is simultaneously removed from lane.knownMessages in the SAME render
// pass -- must be a clean no-op (the vacancy filter drops it before the
// content-diff pass ever looks it up by key).
function mosaicAckResolutionRacingRemovalCheck() {
  const lane = mosaicAckResolveLane();
  mosaicAckPush(lane, mosaicAckBuildAckItem(30, "ack-key-racing"));
  let threw = false;
  try {
    mosaicAckResolve(lane, "ack-key-racing", "<p>too late</p>");
    lane.knownMessages = lane.knownMessages.filter((m) => m.key !== "ack-card-30");
    lane.knownMessageKeys.delete("ack-card-30");
    renderMessagesIfChanged(lane);
  } catch (error) {
    threw = true;
  }
  const stillInLattice = lane.mosaicCards.some((c) => c.key === "ack-card-30");
  const stillInDom = Boolean(
    lane.mosaicPlaneEl.querySelector('article[data-message-key="ack-card-30"]'),
  );
  return { threw, stillInLattice, stillInDom };
}

// Image completion is another late-content path. An image with no src is
// already complete before mosaicSyncImageLoadHandlers can attach listeners,
// so this catches cards that wait forever for a future load/error event
// while stuck on the image reservation prior.
async function mosaicAckCompleteImageResolutionCheck() {
  const lane = mosaicAckResolveLane();
  mosaicAckResetLane(lane);
  mosaicAckPush(lane, mosaicAckBuildCompleteImageItem(35));
  // Already-complete images resolve through the deferred render scheduler
  // (a render never re-enters itself); the re-measured row count lands on
  // the next frame, not synchronously with the insert.
  await new Promise((resolve) =>
    requestAnimationFrame(() => requestAnimationFrame(resolve)),
  );
  const node = lane.mosaicPlaneEl.querySelector('article[data-message-key="ack-complete-image-35"]');
  const image = node.querySelector(".message-image img");
  const card = lane.mosaicCards.find((c) => c.key === "ack-complete-image-35");
  const geometry = lane.mosaicGeometry;
  const reservedRows = mosaicReservationRows(
    "image",
    mosaicRootFontSizePx(),
    geometry.gap,
    geometry.M,
  );
  return {
    cardN: card.n,
    imageComplete: image.complete,
    reservationType: node.dataset.mosaicReservationType || "",
    reservedRows,
  };
}

// Two simultaneously-resolving acks must yield an IDENTICAL final
// layout regardless of which one's fetch happened to land first. Compares
// resolving both in one pass (entries visits them newest-first) against
// resolving them one at a time across two renders in the OPPOSITE
// (reversed) order, in two fresh, identically-seeded lanes.
function mosaicAckSimultaneousOrderStabilityCheck() {
  function buildFixture(lane) {
    mosaicAckPush(lane, mosaicAckBuildAckItem(40, "ack-key-first"));
    for (let i = MOSAIC_ACK_ORDER_FIRST_BLOCK_START; i <= MOSAIC_ACK_ORDER_FIRST_BLOCK_END; i += 1)
      mosaicAckPush(lane, mosaicAckBuildPlainItem(i, 1));
    mosaicAckPush(lane, mosaicAckBuildAckItem(45, "ack-key-second"));
    for (let i = MOSAIC_ACK_ORDER_SECOND_BLOCK_START; i <= MOSAIC_ACK_ORDER_SECOND_BLOCK_END; i += 1)
      mosaicAckPush(lane, mosaicAckBuildPlainItem(i, 1));
  }
  function stripLattice(lane) {
    return lane.mosaicCards
      .map((c) => ({ t: c.t, b: c.b, n: c.n, span: c.span, frozen: c.frozen, key: c.key }))
      .sort((a, b) => a.key.localeCompare(b.key));
  }
  // mosaicAckResolveLane() reuses whatever lane the EARLIER checks in this
  // same page already populated (pendingReservation/wetResolution/
  // frozenPad/frozenRipple all share it), so a fair "together vs reversed"
  // comparison needs an identical, fully-reset starting lattice before
  // EACH run -- not just before the second one.
  function resetLane(lane) {
    mosaicAckResetLane(lane);
  }

  const laneA = mosaicAckResolveLane();
  resetLane(laneA);
  buildFixture(laneA);
  mosaicAckResolve(laneA, "ack-key-first", "<p>both together one</p>");
  mosaicAckResolve(laneA, "ack-key-second", "<p>both together two</p>");
  renderMessagesIfChanged(laneA);
  const together = stripLattice(laneA);

  // Rebuild from an identically clean lane, then resolve the NEWER ack
  // (created later, index 45) before the OLDER one (index 40) -- the
  // reverse of arrival/creation order.
  resetLane(laneA);
  buildFixture(laneA);
  mosaicAckResolve(laneA, "ack-key-second", "<p>both together two</p>");
  renderMessagesIfChanged(laneA);
  mosaicAckResolve(laneA, "ack-key-first", "<p>both together one</p>");
  renderMessagesIfChanged(laneA);
  const reversed = stripLattice(laneA);

  return {
    together,
    reversed,
    identical: JSON.stringify(together) === JSON.stringify(reversed),
  };
}

async function installMosaicAckHelpers(page) {
  await page.addScriptTag({
    content: [
      "const MOSAIC_ACK_FREEZE_PLAIN_FIRST_INDEX = " +
        MOSAIC_ACK_FREEZE_PLAIN_FIRST_INDEX +
        ";",
      "const MOSAIC_ACK_FREEZE_PLAIN_LAST_INDEX = " +
        MOSAIC_ACK_FREEZE_PLAIN_LAST_INDEX +
        ";",
      "const MOSAIC_ACK_ORDER_FIRST_BLOCK_START = " +
        MOSAIC_ACK_ORDER_FIRST_BLOCK_START +
        ";",
      "const MOSAIC_ACK_ORDER_FIRST_BLOCK_END = " +
        MOSAIC_ACK_ORDER_FIRST_BLOCK_END +
        ";",
      "const MOSAIC_ACK_ORDER_SECOND_BLOCK_START = " +
        MOSAIC_ACK_ORDER_SECOND_BLOCK_START +
        ";",
      "const MOSAIC_ACK_ORDER_SECOND_BLOCK_END = " +
        MOSAIC_ACK_ORDER_SECOND_BLOCK_END +
        ";",
      mosaicAckResolveLane,
      mosaicAckBuildPlainItem,
      mosaicAckBuildCompleteImageItem,
      mosaicAckBuildAckItem,
      mosaicAckPush,
      mosaicAckResolve,
      mosaicAckSnapshot,
      mosaicAckResetLane,
      mosaicAckPendingReservationCheck,
      mosaicAckWetResolutionCheck,
      mosaicAckMissingResolutionCheck,
      mosaicAckFreezeFixture,
      mosaicAckFrozenPadCheck,
      mosaicAckFrozenRippleCheck,
      mosaicAckResolutionRacingRemovalCheck,
      mosaicAckCompleteImageResolutionCheck,
      mosaicAckSimultaneousOrderStabilityCheck,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

function assertPendingReservation(result, fail) {
  if (!result.hasSkeleton) fail("pending ack must render the skeleton, not the real quote");
  if (result.cardN !== result.expectedRows)
    fail("pending ack card row count must come from the reservation, not measurement");
}

function assertWetResolution(result, fail) {
  if (!result.beforeHasSkeleton) fail("wet fixture must start with a skeleton");
  if (result.afterHasSkeleton) fail("resolved wet ack must remove the skeleton");
  if (!result.afterHasQuote) fail("resolved wet ack must render the real quote");
  if (result.beforeFrozen || result.afterFrozen) fail("wet fixture card unexpectedly frozen");
  if (!result.transformSet) fail("resolved card's fresh node must receive a transform");
  if (!result.heightSet) fail("resolved card's fresh node must receive a height");
}

function assertMissingResolution(result, fail) {
  if (!result.beforeHasSkeleton) fail("missing fixture must start with a skeleton");
  if (!result.nodeRebuilt) fail("a found:false resolution must re-render the card");
  if (result.afterHasSkeleton) fail("a found:false ack must drop the shimmer skeleton");
  if (!result.afterHasMissing)
    fail("a found:false ack must render the same-height unavailable block");
  if (result.afterHasRealQuote) fail("a found:false ack must not render a real quote");
  if (!result.rowsUnchanged)
    fail("a found:false ack must not change the card's row count (no reflow)");
}

function assertFrozenPad(result, fail) {
  if (!result.nodeHasQuote) fail("frozen-pad card must render the real quote after resolving");
  if (!result.nodeStillHasTransform)
    fail("frozen-pad card's fresh node must still receive its transform (node-identity fix)");
  for (const before of result.before.cards) {
    const after = result.after.cards.find((c) => c.key === before.key);
    if (!after) fail("a card disappeared across ack resolution: " + before.key);
    if (after.transform !== before.transform || after.width !== before.width || after.height !== before.height)
      fail("frozen pad (resolve <= reservation) must write zero position/size changes: " + before.key);
  }
}

function assertFrozenRipple(result, fail) {
  if (!result.cardCountUnchanged) fail("frozen ripple must not change the surviving card count");
  const before = result.grownBefore;
  const after = result.grownAfter;
  if (!(after.n > before.n)) fail("resolving past the reservation must increase the row count");
  if (before.b + before.n !== after.b + after.n)
    fail("a growing frozen ack must keep its top row (b+n) fixed");
  const delta = after.n - before.n;
  for (let i = 0; i < result.below.length; i += 1) {
    const b = result.below[i];
    const a = result.belowAfter[i];
    if (a.b !== b.b - delta)
      fail("a card below the growing frozen ack did not ripple by the exact delta: " + b.key);
  }
  for (let i = 0; i < result.unrelated.length; i += 1) {
    if (JSON.stringify(result.unrelated[i]) !== JSON.stringify(result.unrelatedAfter[i]))
      fail("ripple touched a card outside the growing ack's column/chain: " + result.unrelated[i].key);
  }
}

function assertRacingRemoval(result, fail) {
  if (result.threw) fail("resolution racing removal must not throw");
  if (result.stillInLattice) fail("a removed message's ack resolution must not leave orphan lattice state");
  if (result.stillInDom) fail("a removed message's ack resolution must not leave an orphan DOM node");
}

function assertCompleteImageResolution(result, fail) {
  if (!result.imageComplete) fail("complete-image fixture image was not already complete");
  if (result.reservationType)
    fail("already-complete image card kept its reservation type: " + result.reservationType);
  if (!(result.cardN > result.reservedRows))
    fail("already-complete text+image card stayed at the image reservation instead of measuring");
}

function assertOrderStability(result, fail) {
  if (!result.identical)
    fail(
      "two acks resolved in reversed order must yield an identical final layout: " +
        JSON.stringify(result),
    );
}

async function waitForMosaicAckGlobals(page) {
  await page.waitForFunction(
    () =>
      typeof addLane === "function" &&
      typeof renderMessagesIfChanged === "function" &&
      typeof upsertKnownMessage === "function" &&
      typeof mosaicReservationRows === "function" &&
      Array.isArray(targets) &&
      targets.length > 0,
    { timeout: 10000 },
  );
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-ack-resolution-" + Date.now(),
      // 1920px (M=60) is the narrowest width at which a real, minimal
      // resolved ack quote's chrome still fits within the ack reservation's
      // 2-row floor -- needed so the frozen-pad scenario is a genuine
      // resolve-at-or-under-reservation case, not an accidental ripple.
      contextOptions: { viewport: { width: 1920, height: 900 } },
    },
    async ({ page, server }) => {
      await waitForMosaicAckGlobals(page);
      await installMosaicAckHelpers(page);

      const pendingReservation = await page.evaluate(mosaicAckPendingReservationCheck);
      const wetResolution = await page.evaluate(mosaicAckWetResolutionCheck);
  const missingResolution = await page.evaluate(mosaicAckMissingResolutionCheck);
      const frozenPad = await page.evaluate(mosaicAckFrozenPadCheck);
      const frozenRipple = await page.evaluate(mosaicAckFrozenRippleCheck);
      const racingRemoval = await page.evaluate(mosaicAckResolutionRacingRemovalCheck);
      const completeImageResolution = await page.evaluate(mosaicAckCompleteImageResolutionCheck);
      const orderStability = await page.evaluate(mosaicAckSimultaneousOrderStabilityCheck);

      const fail = (message) => {
        throw new Error(
          message +
            ": " +
            JSON.stringify({
              pendingReservation,
              wetResolution,
    missingResolution,
              frozenPad,
              frozenRipple,
              racingRemoval,
              completeImageResolution,
              orderStability,
            }),
        );
      };
      assertPendingReservation(pendingReservation, fail);
      assertWetResolution(wetResolution, fail);
  assertMissingResolution(missingResolution, fail);
      assertFrozenPad(frozenPad, fail);
      assertFrozenRipple(frozenRipple, fail);
      assertRacingRemoval(racingRemoval, fail);
      assertCompleteImageResolution(completeImageResolution, fail);
      assertOrderStability(orderStability, fail);

      return {
        pendingReservation,
        wetResolution,
        frozenPad,
        frozenRipple,
        racingRemoval,
        completeImageResolution,
        orderStability,
        url: server.url,
      };
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
