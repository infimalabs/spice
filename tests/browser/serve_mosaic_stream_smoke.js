const { withServePage } = require("./serve_playwright_harness");

// Mosaic stream integration:
// drives a real lane through renderMessagesIfChanged exactly as live
// traffic would (upsertKnownMessage + trimKnownMessages, not renderMessage
// called directly) and checks the persistent-lattice invariants: insert
// never moves existing cards; unchanged cards get zero style writes on a
// no-op re-render; removing the oldest message vacates without moving the
// rest; newest-first reading order holds; image reservations are
// exact and a load/error event never triggers pack/position/size activity.

// msg-10..msg-16 (frozen-growth chain) plus the surviving msg-2/msg-3 from
// the earlier insert/removal/content-change scenarios.
const MOSAIC_STREAM_FROZEN_CHAIN_FIRST_KEY = 10;
const MOSAIC_STREAM_FROZEN_CHAIN_LAST_KEY = 16;
const MOSAIC_STREAM_EXPECTED_SURVIVOR_COUNT =
  MOSAIC_STREAM_FROZEN_CHAIN_LAST_KEY - MOSAIC_STREAM_FROZEN_CHAIN_FIRST_KEY + 1 + 2;
// Mirrors app.mosaic-geometry.js's mosaicGridTrackCount -- this assertion
// runs on the Node side (not injected into the page), so it can't read
// that in-page global directly.
const MOSAIC_STREAM_GRID_TRACK_COUNT = 12;
const MOSAIC_STREAM_BACKFILL_EXISTING_FIRST_KEY = 100;
const MOSAIC_STREAM_BACKFILL_EXISTING_COUNT = 24;
const MOSAIC_STREAM_BACKFILL_COUNT = 3;
const MOSAIC_STREAM_BACKFILL_LINES = 8;
const MOSAIC_STREAM_SCROLL_EPSILON_PX = 2;
// Clobber scenario: 8 seeded + 2 bulk-added cards, minus any trimmed.
const MOSAIC_STREAM_CLOBBER_MIN_CARDS = 10;

function mosaicStreamResolveLane() {
  let lane = Array.from(laneStates.values()).find((item) => !item.emptyTeam);
  if (!lane && targets.length) {
    addLane(targets[0].id);
    lane = laneStates.get(targets[0].id);
  }
  if (!lane) throw new Error("no lane available for mosaic stream smoke");
  return lane;
}

function mosaicStreamBuildItem(index, lines) {
  const html = Array.from({ length: lines }, (_, i) => "line " + (i + 1)).join("<br>");
  return {
    ack_count: 0,
    ack_keys: [],
    index,
    key: "msg-" + index,
    kind: "assistant",
    timestamp: new Date(2026, 0, 1, 0, 0, index).toISOString(),
    display_html: html,
    display_text: html,
    text: html,
  };
}

function mosaicStreamPushMessage(lane, index, lines) {
  upsertKnownMessage(lane, mosaicStreamBuildItem(index, lines), "newest");
  trimKnownMessages(lane);
  renderMessagesIfChanged(lane);
}

function mosaicStreamCardSnapshot(lane) {
  const cards = Array.from(
    lane.mosaicPlaneEl.querySelectorAll("article[data-message-key]"),
  ).map((el) => ({
    key: el.dataset.messageKey,
    transform: el.style.transform,
    width: el.style.width,
    height: el.style.height,
  }));
  return { cards, lattice: lane.mosaicCards.map((c) => ({ ...c })) };
}

function mosaicStreamResetLane(lane) {
  if (lane.mosaicFrame) {
    cancelAnimationFrame(lane.mosaicFrame);
    lane.mosaicFrame = 0;
  }
  mosaicResetResizeObserver(lane);
  if (lane.mosaicPlaneEl) lane.mosaicPlaneEl.remove();
  lane.messagesEl.replaceChildren();
  lane.knownMessages = [];
  lane.knownMessageKeys = new Set();
  lane.newestMessageKey = "";
  lane.oldestMessageKey = "";
  lane.renderedMessageFingerprint = "";
  lane.mosaicCards = [];
  lane.mosaicEventLog = mosaicCreateEventLog(mosaicGridTrackCount, MOSAIC_FREEZE_DEPTH);
  lane.mosaicNextCreationIndex = 0;
  lane.mosaicNextBackfillCreationIndex = -1;
  lane.mosaicAppliedByKey = new Map();
  lane.mosaicPrevMaxRow = 0;
  lane.mosaicPrevExtent = null;
  lane.mosaicGeometry = null;
  lane.mosaicPlaneEl = null;
}

function mosaicStreamBottomDistance(lane) {
  return lane.messagesEl.scrollHeight - lane.messagesEl.clientHeight - lane.messagesEl.scrollTop;
}

function mosaicStreamInsertSequence() {
  const lane = mosaicStreamResolveLane();
  mosaicStreamPushMessage(lane, 1, 2);
  const afterFirst = mosaicStreamCardSnapshot(lane);
  mosaicStreamPushMessage(lane, 2, 2);
  const afterSecond = mosaicStreamCardSnapshot(lane);
  mosaicStreamPushMessage(lane, 3, 2);
  const afterThird = mosaicStreamCardSnapshot(lane);

  // Re-render with nothing changed: zero cards should receive a new style
  // write (transform/width/height must be identical strings, and the
  // lattice records for existing keys must be unchanged).
  const beforeNoop = afterThird.cards.map((c) => ({ ...c }));
  renderMessagesIfChanged(lane);
  const afterNoop = mosaicStreamCardSnapshot(lane);

  return { afterFirst, afterSecond, afterThird, beforeNoop, afterNoop };
}

function mosaicStreamRemovalCheck() {
  const lane = mosaicStreamResolveLane();
  const before = mosaicStreamCardSnapshot(lane);
  // Drop the oldest known message directly (vacancy, not agent removal) and
  // re-render -- a single removal must not move the survivors.
  lane.knownMessages = lane.knownMessages.filter((item) => item.key !== "msg-1");
  lane.knownMessageKeys.delete("msg-1");
  renderMessagesIfChanged(lane);
  const after = mosaicStreamCardSnapshot(lane);
  return { before, after };
}

// wetReplay: growing a still-wet card's content must re-place the wet
// band (this card's own n grows, possibly shifting its own (t,b)) while
// never touching any FROZEN card. msg-1 was already vacated by the removal
// check, so msg-2/msg-3 are the whole wet band here.
function mosaicStreamContentChangeCheck() {
  const lane = mosaicStreamResolveLane();
  const before = mosaicStreamCardSnapshot(lane);
  const item = lane.knownMessages.find((known) => known.key === "msg-3");
  const html = Array.from({ length: 8 }, (_, i) => "grown line " + (i + 1)).join("<br>");
  item.display_html = html;
  item.display_text = html;
  item.text = html;
  renderMessagesIfChanged(lane);
  const after = mosaicStreamCardSnapshot(lane);
  const grownBefore = before.lattice.find((c) => c.key === "msg-3");
  const grownAfter = after.lattice.find((c) => c.key === "msg-3");
  return { before, after, grownBefore, grownAfter };
}

// rippleRows: growing a FROZEN card must keep its top row fixed
// (b+n invariant) and ripple only the chain resting below it in
// overlapping tracks downward -- never the cards above it, never a
// different column, never a full replay. Seven single-line messages
// settle into two columns (baseSpan=6, two 6-track lanes): column t=0 is
// msg-10 (bottom) / msg-12 / msg-14 / msg-16 (top), column t=6 is msg-11 /
// msg-13 / msg-15. msg-12 is buried by msg-14 and msg-16 (both later,
// same column) -- frozen -- and has msg-10 resting below it, giving
// rippleRows an actual chain to shift.
function mosaicStreamFrozenGrowthCheck() {
  const lane = mosaicStreamResolveLane();
  for (
    let i = MOSAIC_STREAM_FROZEN_CHAIN_FIRST_KEY;
    i <= MOSAIC_STREAM_FROZEN_CHAIN_LAST_KEY;
    i += 1
  )
    mosaicStreamPushMessage(lane, i, 1);
  const before = mosaicStreamCardSnapshot(lane);
  const target = before.lattice.find((c) => c.key === "msg-12");
  const below = before.lattice.filter(
    (c) => c.key !== target.key && c.t === target.t && c.span === target.span && c.b < target.b,
  );
  const unrelated = before.lattice.filter(
    (c) => c.key !== target.key && !(c.t === target.t && c.span === target.span && c.b < target.b),
  );

  const item = lane.knownMessages.find((known) => known.key === "msg-12");
  const html = Array.from({ length: 10 }, (_, i) => "grown line " + (i + 1)).join("<br>");
  item.display_html = html;
  item.display_text = html;
  item.text = html;
  renderMessagesIfChanged(lane);
  const after = mosaicStreamCardSnapshot(lane);
  const byKey = (list) => list.map((c) => after.lattice.find((candidate) => candidate.key === c.key));

  return {
    targetWasFrozen: target.frozen,
    grownBefore: target,
    grownAfter: after.lattice.find((c) => c.key === target.key),
    below,
    belowAfter: byKey(below),
    unrelated,
    unrelatedAfter: byKey(unrelated),
    cardCountUnchanged: before.lattice.length === after.lattice.length,
  };
}

// Full replay: a geometry change (root font-size here, cheaper to drive
// deterministically in a smoke than a real container resize) must trigger
// a full replay -- every surviving card re-decided against a fresh
// rowFloor in creation order, reading order back to clean chronological.
function mosaicStreamGeometryChangeCheck() {
  const lane = mosaicStreamResolveLane();
  const before = mosaicStreamCardSnapshot(lane);
  document.documentElement.style.fontSize = "20px";
  // Mirrors what mosaicScheduleRender's ResizeObserver callback does on
  // a real geometry change: force past the fingerprint early-return
  // (app.stream.js renderMessagesIfChanged) so the render path actually
  // runs and mosaicGeometryChanged sees the new root/M and full-replays.
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  const afterAtNewRoot = mosaicStreamCardSnapshot(lane);
  // Idempotence at a STABLE geometry (no font-size toggle in
  // between): replaying again must reproduce a byte-identical layout. A
  // toggle-and-restore round trip is deliberately NOT used for this
  // assertion -- text-metric sub-pixel rounding across a real inline
  // font-size mutation is a browser-rendering concern, not this module's,
  // and would make the assertion flaky for the wrong reason.
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  const afterNewRootAgain = mosaicStreamCardSnapshot(lane);
  document.documentElement.style.fontSize = "";
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  const afterRestored = mosaicStreamCardSnapshot(lane);
  return { before, afterAtNewRoot, afterNewRootAgain, afterRestored };
}

function mosaicStreamReadingOrderCheck() {
  const lane = mosaicStreamResolveLane();
  const cards = lane.mosaicCards.filter((c) => c.kind !== "barrier");
  // Newest-first is only an EXACT guarantee within a column chain (the
  // guarantee 3: "exact within any column chain"); across chains, cards of
  // very different heights can legitimately interleave (a tall older card
  // can end up with a higher top than a short newer one placed in a
  // shallower column) -- that is the intended packing behavior, not a
  // violation. So group by (t, span) and check monotonicity within each
  // chain, not globally across the whole board.
  const chains = new Map();
  for (const card of cards) {
    const chainKey = card.t + ":" + card.span;
    const chain = chains.get(chainKey) || [];
    chain.push(card);
    chains.set(chainKey, chain);
  }
  let monotonic = true;
  const chainTops = [];
  for (const chain of chains.values()) {
    chain.sort((a, b) => b.creationIndex - a.creationIndex);
    const tops = chain.map((c) => c.b + c.n);
    chainTops.push(tops);
    for (let i = 1; i < tops.length; i += 1) {
      if (tops[i] > tops[i - 1]) monotonic = false;
    }
  }
  return { count: cards.length, chainTops, monotonic };
}

// Images: a message with an inline image reserves the
// small letterbox cap ("image"); an image-only message reserves the large
// cap ("imageLarge") -- both sourced from the single dataset classification
// app.render.js computes once (article.dataset.mosaicReservationType), never
// re-derived from the .media-rich class, which is true for BOTH tiers and
// would misclassify the small case as large. Once an image resolves (loads,
// errors, or is already complete with no src), mosaicMarkImageResolved
// clears the reservation and the content-diff pass measures the card for
// real -- these fixtures carry no other content, so their real measured
// height is exactly what messages.css letterboxes the image to, proving the
// two tiers actually render at different, correct heights end to end
// (mosaic-ack-resolution-smoke separately proves the mixed text+image case,
// where the real height legitimately exceeds the image reservation).
const MOSAIC_STREAM_IMAGE_HTML =
  '<p class="message-image-stack"><a class="message-image" href="#">' +
  '<img alt="fixture" src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></a></p>';
// A message-image-stack lays its images out in a single horizontal row
// (messages.css), all at the same letterbox height -- three images here
// prove the reservation (and the final settled height) stays at exactly one
// slot, never a per-image sum.
const MOSAIC_STREAM_MULTI_IMAGE_HTML =
  '<p class="message-image-stack">' +
  '<a class="message-image" href="#"><img alt="fixture 1" src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></a>' +
  '<a class="message-image" href="#"><img alt="fixture 2" src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></a>' +
  '<a class="message-image" href="#"><img alt="fixture 3" src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></a>' +
  "</p>";
// No src at all makes the image "complete" synchronously with zero network
// activity and no console noise (mirrors mosaic-ack-resolution-smoke's
// already-complete-image fixture) -- messages.css still gives it a real,
// non-collapsed letterbox slot since height/max-height are unconditional,
// not derived from the (absent) image content.
const MOSAIC_STREAM_BROKEN_IMAGE_HTML =
  '<p class="message-image-stack"><a class="message-image" href="#">' +
  '<img alt="broken fixture"></a></p>';

function mosaicStreamImageItem(key, imageOnly, html) {
  return {
    ack_count: 0,
    ack_keys: [],
    display_html: html,
    display_text: "image",
    image_only: imageOnly,
    index: 100,
    key,
    kind: "assistant",
    text: "image",
    timestamp: new Date(2026, 0, 2).toISOString(),
  };
}

function mosaicStreamImageWaitForResolution(images) {
  return Promise.all(
    images.map(
      (image) =>
        new Promise((resolve) => {
          if (image.complete) {
            resolve();
            return;
          }
          const done = () => resolve();
          image.addEventListener("load", done, { once: true });
          image.addEventListener("error", done, { once: true });
        }),
    ),
  );
}

async function mosaicStreamImageReservationCheck() {
  const lane = mosaicStreamResolveLane();
  upsertKnownMessage(
    lane,
    mosaicStreamImageItem("img-regular", false, MOSAIC_STREAM_IMAGE_HTML),
    "newest",
  );
  upsertKnownMessage(
    lane,
    mosaicStreamImageItem("img-only", true, MOSAIC_STREAM_IMAGE_HTML),
    "newest",
  );
  upsertKnownMessage(
    lane,
    mosaicStreamImageItem("img-broken", false, MOSAIC_STREAM_BROKEN_IMAGE_HTML),
    "newest",
  );
  upsertKnownMessage(
    lane,
    mosaicStreamImageItem("img-multi", false, MOSAIC_STREAM_MULTI_IMAGE_HTML),
    "newest",
  );
  trimKnownMessages(lane);
  renderMessagesIfChanged(lane);

  const articleFor = (key) =>
    lane.mosaicPlaneEl.querySelector('[data-message-key="' + key + '"]');
  const cardFor = (key) => lane.mosaicCards.find((c) => c.key === key);

  const regularImg = articleFor("img-regular").querySelector("img");
  const onlyImg = articleFor("img-only").querySelector("img");
  const brokenImg = articleFor("img-broken").querySelector("img");
  const multiImgs = Array.from(articleFor("img-multi").querySelectorAll("img"));

  // Wait for every image to resolve (already-complete or a real load/error
  // event) so mosaicMarkImageResolved's clear-and-remeasure has actually run
  // for all four cards before the settled state below is snapshotted.
  await mosaicStreamImageWaitForResolution([
    regularImg,
    onlyImg,
    brokenImg,
    ...multiImgs,
  ]);
  // mosaicMarkImageResolved's own renderMessagesIfChanged call runs
  // synchronously inside the load/error handler, but that handler was
  // registered by mosaicSyncImageLoadHandlers before this check's own
  // listeners above -- give the resulting content-diff pass a frame to
  // fully settle before reading state.
  await new Promise((resolve) =>
    requestAnimationFrame(() => requestAnimationFrame(resolve)),
  );

  const settled = {
    regularReserve: articleFor("img-regular").dataset.mosaicReservationType || "",
    onlyReserve: articleFor("img-only").dataset.mosaicReservationType || "",
    brokenReserve: articleFor("img-broken").dataset.mosaicReservationType || "",
    multiReserve: articleFor("img-multi").dataset.mosaicReservationType || "",
    regularN: cardFor("img-regular").n,
    onlyN: cardFor("img-only").n,
    brokenN: cardFor("img-broken").n,
    multiN: cardFor("img-multi").n,
    multiImageCount: multiImgs.length,
  };

  // Idempotence: once resolved, further load/error events (a real image can
  // fire "load" more than once in some browsers on style/attribute churn)
  // must be true no-ops -- mosaicMarkImageResolved's WeakSet guard returns
  // false the second time, so no further dirty flag, no further re-render.
  const fingerprintBefore = lane.renderedMessageFingerprint;
  const before = mosaicStreamCardSnapshot(lane);
  for (const img of [regularImg, onlyImg, brokenImg, ...multiImgs]) {
    img.dispatchEvent(new Event("load"));
    img.dispatchEvent(new Event("error"));
  }
  await new Promise((resolve) =>
    requestAnimationFrame(() => requestAnimationFrame(resolve)),
  );

  return {
    ...settled,
    fingerprintBefore,
    fingerprintAfter: lane.renderedMessageFingerprint,
    before,
    after: mosaicStreamCardSnapshot(lane),
  };
}

// Span clamped to 12, no special span policy: a task-directive-stack
// quote (detected from item.display_html -- server-emitted markup,
// spice/serve/taskdirectives.py, always exactly
// `<div class="task-directive-stack">`) gets the same forced span-12
// treatment already given compaction dividers and time rules
// (mosaicItemIsBarrier). A span-12 card resets the frontier flat across
// every track with no special-cased reset logic -- it falls out of the
// shared engine's rowFloor commit for any full-width card -- so the very
// next inserted card, regardless of its own span, must anchor at exactly
// the barrier's top row. Self-contained: vacates its own messages after
// asserting so it leaves no residue for any other check.
function mosaicStreamBarrierResetCheck() {
  const lane = mosaicStreamResolveLane();
  const directiveItem = mosaicStreamBuildItem(90, 1);
  directiveItem.display_html = '<div class="task-directive-stack">directive</div>';
  directiveItem.display_text = "directive";
  directiveItem.text = "directive";
  upsertKnownMessage(lane, directiveItem, "newest");
  trimKnownMessages(lane);
  renderMessagesIfChanged(lane);
  const afterBarrier = mosaicStreamCardSnapshot(lane);
  const barrierCard = afterBarrier.lattice.find((c) => c.key === "msg-90");

  mosaicStreamPushMessage(lane, 91, 1);
  const afterNext = mosaicStreamCardSnapshot(lane);
  const nextCard = afterNext.lattice.find((c) => c.key === "msg-91");

  lane.knownMessages = lane.knownMessages.filter(
    (item) => item.key !== "msg-90" && item.key !== "msg-91",
  );
  lane.knownMessageKeys.delete("msg-90");
  lane.knownMessageKeys.delete("msg-91");
  renderMessagesIfChanged(lane);

  return { barrierCard, nextCard };
}

function mosaicStreamBackfillCheck() {
  const lane = mosaicStreamResolveLane();
  mosaicStreamResetLane(lane);
  for (let offset = 0; offset < MOSAIC_STREAM_BACKFILL_EXISTING_COUNT; offset += 1) {
    mosaicStreamPushMessage(
      lane,
      MOSAIC_STREAM_BACKFILL_EXISTING_FIRST_KEY + offset,
      MOSAIC_STREAM_BACKFILL_LINES,
    );
  }
  const before = mosaicStreamCardSnapshot(lane);
  const existingKeys = before.lattice.map((card) => card.key);
  const existingMinB = Math.min(...before.lattice.map((card) => card.b));
  lane.messagesEl.scrollTop = Math.max(
    0,
    lane.messagesEl.scrollHeight - lane.messagesEl.clientHeight,
  );
  const bottomDistanceBefore = mosaicStreamBottomDistance(lane);
  const sentinelBefore = lane.historySentinelEl;
  const olderMessages = Array.from({ length: MOSAIC_STREAM_BACKFILL_COUNT }, (_, index) =>
    mosaicStreamBuildItem(
      MOSAIC_STREAM_BACKFILL_EXISTING_FIRST_KEY - index - 1,
      MOSAIC_STREAM_BACKFILL_LINES,
    ),
  );
  mergeOlderPayloadMessages(lane, { messages: olderMessages });
  renderMessagesIfChanged(lane);
  const after = mosaicStreamCardSnapshot(lane);
  const bottomDistanceAfter = mosaicStreamBottomDistance(lane);
  const sentinelAfter = lane.historySentinelEl;
  const newKeys = after.lattice
    .map((card) => card.key)
    .filter((key) => !existingKeys.includes(key));
  const backfillKeys = olderMessages.map((item) => item.key);
  const backfillCreationIndexes = backfillKeys.map(
    (key) => after.lattice.find((card) => card.key === key)?.creationIndex,
  );
  document.documentElement.style.fontSize = "20px";
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  const afterReplay = mosaicStreamCardSnapshot(lane);
  document.documentElement.style.fontSize = "";
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  return {
    before,
    after,
    afterReplay,
    existingKeys,
    existingMinB,
    newKeys,
    backfillKeys,
    backfillCreationIndexes,
    bottomDistanceBefore,
    bottomDistanceAfter,
    sentinelStable: sentinelBefore === sentinelAfter,
    sentinelConnected: Boolean(sentinelAfter && sentinelAfter.isConnected),
    sentinelTargetId: sentinelAfter?.dataset?.historyTargetId || "",
    laneTargetId: lane.targetId,
  };
}

// Measurement must never leave a card wearing its probe styles: a
// same-geometry bulk replay re-measures every node (tall cards at the wide
// tier LAST), and any card whose lattice position is unchanged would keep
// that measurement width -- a double-wide card jammed in a single slot --
// plus a cleared height, until some later write happened to land. After
// any render, every card's inline width/height must equal its lattice
// values exactly.
function mosaicStreamMeasureClobberCheck() {
  const lane = mosaicStreamResolveLane();
  const tallHtml = Array.from({ length: 20 }, (_, i) => "clobber line " + i).join("<br>");
  const shortHtml = "clobber line";
  const item = (i, html) => ({
    ack_count: 0,
    ack_keys: [],
    index: 700 + i,
    key: "clobber-" + i,
    kind: "assistant",
    timestamp: new Date(2026, 0, 1, 3, i, 0).toISOString(),
    display_html: html,
    display_text: "x",
    text: "x",
  });
  for (let i = 1; i <= 8; i += 1) {
    upsertKnownMessage(lane, item(i, i % 3 === 0 ? tallHtml : shortHtml), "newest");
  }
  trimKnownMessages(lane);
  renderMessagesIfChanged(lane);
  // A mixed-age bulk (one older than every seeded card, one newest) forces
  // a same-geometry FULL REPLAY -- a strictly-newer bulk would take the
  // zero-movement append branch and never re-measure: placements mostly
  // unchanged, every node re-measured.
  const older = item(60, shortHtml);
  older.timestamp = new Date(2026, 0, 1, 2, 0, 0).toISOString();
  upsertKnownMessage(lane, older, "newest");
  upsertKnownMessage(lane, item(61, tallHtml), "newest");
  trimKnownMessages(lane);
  renderMessagesIfChanged(lane);
  const g = lane.mosaicGeometry;
  const mismatches = [];
  for (const card of lane.mosaicCards) {
    const node = lane.mosaicPlaneEl.querySelector(
      'article[data-message-key="' + CSS.escape(card.key) + '"]',
    );
    if (!node) continue;
    const wantW = g.edges[card.t + card.span] - g.edges[card.t] - g.gap + "px";
    const wantH = card.n * g.M - g.gap + "px";
    if (node.style.width !== wantW || node.style.height !== wantH)
      mismatches.push({
        key: card.key,
        span: card.span,
        inlineW: node.style.width,
        wantW,
        inlineH: node.style.height,
        wantH,
      });
  }
  return { cardCount: lane.mosaicCards.length, mismatches };
}

// A bulk of strictly-newer messages arriving in ONE render must place by
// sequential insert: every existing card's lattice record AND inline styles
// stay byte-identical (no re-decide, no re-measure, no style writes), and
// the new cards land in true stream order.
function mosaicStreamBulkAppendCheck() {
  const lane = mosaicStreamResolveLane();
  const item = (i, lines) => ({
    ack_count: 0,
    ack_keys: [],
    index: 800 + i,
    key: "bulk-" + i,
    kind: "assistant",
    timestamp: new Date(2026, 0, 1, 5, i, 0).toISOString(),
    display_html: Array.from({ length: lines }, (_, j) => "bulk line " + j).join("<br>"),
    display_text: "x",
    text: "x",
  });
  const snapshotExisting = () =>
    lane.mosaicCards
      .map((card) => {
        const node = lane.mosaicPlaneEl.querySelector(
          'article[data-message-key="' + CSS.escape(card.key) + '"]',
        );
        return {
          key: card.key,
          t: card.t,
          b: card.b,
          n: card.n,
          span: card.span,
          transform: node ? node.style.transform : "",
          width: node ? node.style.width : "",
          height: node ? node.style.height : "",
        };
      })
      .sort((a, b) => a.key.localeCompare(b.key));
  const before = snapshotExisting();
  const beforeKeys = new Set(lane.mosaicCards.map((card) => card.key));
  for (let i = 1; i <= 3; i += 1) upsertKnownMessage(lane, item(i, 1 + i), "newest");
  trimKnownMessages(lane);
  renderMessagesIfChanged(lane);
  const after = snapshotExisting().filter((card) => beforeKeys.has(card.key));
  const placed = lane.mosaicCards.filter((card) => card.key.startsWith("bulk-"));
  return {
    beforeCount: before.length,
    identical: JSON.stringify(before) === JSON.stringify(after),
    placedCount: placed.length,
    placedInOrder:
      placed.length === 3 &&
      placed
        .slice()
        .sort((a, b) => a.creationIndex - b.creationIndex)
        .map((card) => card.key)
        .join(",") === "bulk-1,bulk-2,bulk-3",
  };
}

function mosaicStreamEventLogReplayCheck() {
  const lane = mosaicStreamResolveLane();
  const replayed = mosaicReplayEventLog(lane.mosaicEventLog);
  return {
    eventCount: lane.mosaicEventLog.events.length,
    live: mosaicEventLogLayout(lane.mosaicCards),
    replayed: replayed.layout,
  };
}

async function installMosaicStreamHelpers(page) {
  await page.addScriptTag({
    content: [
      "const MOSAIC_STREAM_FROZEN_CHAIN_FIRST_KEY = " +
        MOSAIC_STREAM_FROZEN_CHAIN_FIRST_KEY +
        ";",
      "const MOSAIC_STREAM_FROZEN_CHAIN_LAST_KEY = " +
        MOSAIC_STREAM_FROZEN_CHAIN_LAST_KEY +
        ";",
      "const MOSAIC_STREAM_IMAGE_HTML = " +
        JSON.stringify(MOSAIC_STREAM_IMAGE_HTML) +
        ";",
      "const MOSAIC_STREAM_BROKEN_IMAGE_HTML = " +
        JSON.stringify(MOSAIC_STREAM_BROKEN_IMAGE_HTML) +
        ";",
      "const MOSAIC_STREAM_MULTI_IMAGE_HTML = " +
        JSON.stringify(MOSAIC_STREAM_MULTI_IMAGE_HTML) +
        ";",
      "const MOSAIC_STREAM_BACKFILL_EXISTING_FIRST_KEY = " +
        MOSAIC_STREAM_BACKFILL_EXISTING_FIRST_KEY +
        ";",
      "const MOSAIC_STREAM_BACKFILL_EXISTING_COUNT = " +
        MOSAIC_STREAM_BACKFILL_EXISTING_COUNT +
        ";",
      "const MOSAIC_STREAM_BACKFILL_COUNT = " +
        MOSAIC_STREAM_BACKFILL_COUNT +
        ";",
      "const MOSAIC_STREAM_BACKFILL_LINES = " +
        MOSAIC_STREAM_BACKFILL_LINES +
        ";",
      mosaicStreamResolveLane,
      mosaicStreamBuildItem,
      mosaicStreamPushMessage,
      mosaicStreamCardSnapshot,
      mosaicStreamResetLane,
      mosaicStreamBottomDistance,
      mosaicStreamInsertSequence,
      mosaicStreamRemovalCheck,
      mosaicStreamContentChangeCheck,
      mosaicStreamFrozenGrowthCheck,
      mosaicStreamGeometryChangeCheck,
      mosaicStreamReadingOrderCheck,
      mosaicStreamImageItem,
      mosaicStreamImageWaitForResolution,
      mosaicStreamImageReservationCheck,
      mosaicStreamBarrierResetCheck,
      mosaicStreamBackfillCheck,
      mosaicStreamEventLogReplayCheck,
      mosaicStreamMeasureClobberCheck,
      mosaicStreamBulkAppendCheck,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

function mosaicStreamCardsOverlap(cards) {
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

function assertInsertInvariants(insertResult, fail) {
  if (insertResult.afterFirst.cards.length !== 1)
    fail("first insert must produce exactly one card");
  if (insertResult.afterSecond.cards.length !== 2)
    fail("second insert must produce exactly two cards");
  if (insertResult.afterThird.cards.length !== 3)
    fail("third insert must produce exactly three cards");

  // Insertion must never touch any existing card's rendered position.
  const firstCardAfterFirst = insertResult.afterFirst.cards.find((c) => c.key === "msg-1");
  const firstCardAfterThird = insertResult.afterThird.cards.find((c) => c.key === "msg-1");
  if (!firstCardAfterFirst || !firstCardAfterThird)
    fail("msg-1's card disappeared across inserts");

  // A no-op re-render must write zero new style strings.
  for (const before of insertResult.beforeNoop) {
    const after = insertResult.afterNoop.cards.find((c) => c.key === before.key);
    if (!after || after.transform !== before.transform || after.width !== before.width || after.height !== before.height)
      fail("a no-op re-render changed a card's rendered style: " + before.key);
  }
}

function assertRemovalInvariants(removalResult, fail) {
  // Vacancy: removing the oldest message must not move the survivors.
  const survivorsBefore = removalResult.before.cards.filter((c) => c.key !== "msg-1");
  for (const before of survivorsBefore) {
    const after = removalResult.after.cards.find((c) => c.key === before.key);
    if (!after || after.transform !== before.transform)
      fail("removing msg-1 moved a surviving card: " + before.key);
  }
  if (removalResult.after.cards.some((c) => c.key === "msg-1"))
    fail("msg-1 should have been vacated from the lattice");
}

function assertContentChangeInvariants(contentChangeResult, fail) {
  // wetReplay: growing msg-3's content must change its own row count
  // (n), and must not touch msg-2 unless the wet band's relative order
  // actually shifted -- here msg-3 (newer) growing means it may now
  // legitimately move msg-2, so we only assert msg-3 itself actually grew
  // and the lattice stayed internally consistent (no overlap).
  if (!(contentChangeResult.grownAfter.n > contentChangeResult.grownBefore.n))
    fail("growing msg-3's content did not increase its row count");
  const overlap = mosaicStreamCardsOverlap(contentChangeResult.after.lattice);
  if (overlap) fail("cards overlap after wet content growth: " + JSON.stringify(overlap));
}

function assertFrozenGrowthInvariants(frozenGrowthResult, fail) {
  // rippleRows: growing a frozen card keeps its own top row fixed
  // (b+n invariant) and ripples only the same-column chain resting below
  // it downward by exactly the growth delta -- never the cards above it or
  // in another column, and never a full replay (card count unchanged).
  if (!frozenGrowthResult.targetWasFrozen)
    fail("msg-12 should have frozen (buried by two later same-column cards)");
  if (!frozenGrowthResult.cardCountUnchanged)
    fail("frozen growth must not change the surviving card count (no full replay)");
  const before = frozenGrowthResult.grownBefore;
  const after = frozenGrowthResult.grownAfter;
  if (!(after.n > before.n)) fail("growing a frozen card must increase its row count");
  if (before.b + before.n !== after.b + after.n)
    fail("a growing frozen card must keep its top row (b+n) fixed");
  if (after.t !== before.t || after.span !== before.span)
    fail("ripple must never change the growing card's own t/span");
  const delta = after.n - before.n;
  for (let i = 0; i < frozenGrowthResult.below.length; i += 1) {
    const b = frozenGrowthResult.below[i];
    const a = frozenGrowthResult.belowAfter[i];
    if (a.b !== b.b - delta)
      fail("a card resting below the growing frozen card did not ripple by the exact delta: " + b.key);
    if (a.t !== b.t || a.n !== b.n || a.span !== b.span)
      fail("ripple must only move b, never t/n/span, on a shifted card: " + b.key);
  }
  for (let i = 0; i < frozenGrowthResult.unrelated.length; i += 1) {
    const u = frozenGrowthResult.unrelated[i];
    const uAfter = frozenGrowthResult.unrelatedAfter[i];
    if (JSON.stringify(u) !== JSON.stringify(uAfter))
      fail("ripple touched a card outside the growing card's column/chain: " + u.key);
  }
}

function assertGeometryChangeInvariants(geometryChangeResult, fail) {
  // Full replay: a geometry change must re-decide every surviving card
  // (module M changes with root font-size).
  if (geometryChangeResult.before.lattice.length !== geometryChangeResult.afterAtNewRoot.lattice.length)
    fail("full replay must not change the surviving card count");
  const moduleBefore = geometryChangeResult.before.cards[0]?.height;
  const moduleAfterNewRoot = geometryChangeResult.afterAtNewRoot.cards[0]?.height;
  if (moduleBefore === moduleAfterNewRoot)
    fail("full replay at a new root font-size must change rendered card height");

  // Determinism: replaying again at a STABLE geometry (no font-size
  // toggle in between) must reproduce a byte-identical layout.
  const byKey = (a, b) => a.key.localeCompare(b.key);
  const atNewRootLattice = JSON.stringify(geometryChangeResult.afterAtNewRoot.lattice.slice().sort(byKey));
  const newRootAgainLattice = JSON.stringify(geometryChangeResult.afterNewRootAgain.lattice.slice().sort(byKey));
  if (atNewRootLattice !== newRootAgainLattice)
    fail("replaying twice at the same settled geometry must be byte-identical");

  // Restoring the original geometry must still yield a valid, non-
  // overlapping layout with the same surviving card count (exact
  // byte-identity to the pre-toggle layout is not asserted here -- a real
  // inline font-size mutate-and-restore round trip can shift browser text
  // metrics by a sub-pixel, which is a rendering concern, not this
  // module's determinism to prove).
  if (geometryChangeResult.afterRestored.lattice.length !== geometryChangeResult.before.lattice.length)
    fail("restoring geometry must not change the surviving card count");
  const overlap = mosaicStreamCardsOverlap(geometryChangeResult.afterRestored.lattice);
  if (overlap) fail("cards overlap after restoring geometry: " + JSON.stringify(overlap));
}

function assertReadingOrderInvariants(readingOrder, fail) {
  if (!readingOrder.monotonic)
    fail("newest-first reading order violated (plane-space tops not monotonic by creation order)");
  if (readingOrder.count !== MOSAIC_STREAM_EXPECTED_SURVIVOR_COUNT)
    fail(
      "expected " + MOSAIC_STREAM_EXPECTED_SURVIVOR_COUNT +
        " surviving cards (msg-2/msg-3 + msg-10..msg-16), got " + readingOrder.count,
    );
}

function assertImageReservationInvariants(imageResult, fail) {
  // Once resolved (mosaicMarkImageResolved), the reservation type clears
  // unconditionally so the content-diff pass measures the real (letterboxed)
  // height instead -- true for a pure image-only/image-stack card as much as
  // a mixed text+image one (mosaic-ack-resolution-smoke covers the mixed
  // case; these fixtures carry no other content).
  for (const key of ["regularReserve", "onlyReserve", "brokenReserve", "multiReserve"]) {
    if (imageResult[key])
      fail("reservation type did not clear after resolution: " + key + "=" + imageResult[key]);
  }

  // Tier proportionality: an image-only card letterboxes to the
  // large cap, a regular inline image to the small cap -- the large tier
  // must actually settle taller, proving the two CSS caps (and the fixed
  // classification read from app.render.js's dataset, not the .media-rich
  // class which is true for both tiers) really do produce different,
  // correct heights end to end.
  if (!(imageResult.onlyN > imageResult.regularN))
    fail("an image-only card must settle taller than a regular inline image: " + JSON.stringify(imageResult));

  // A broken (no-src) image still gets a real, non-collapsed letterbox slot
  // (messages.css height/max-height are unconditional) -- identical to a
  // loaded regular image at the same (small) tier.
  if (imageResult.brokenN !== imageResult.regularN)
    fail("a broken image must settle at the same height as a loaded one: " + JSON.stringify(imageResult));

  // Multi-image stacking rule: three images in one stack (laid out
  // in a single horizontal row, messages.css) must settle at exactly the
  // SAME one-slot height as a single image, never a per-image sum.
  if (imageResult.multiImageCount !== 3)
    fail("multi-image fixture did not render all three images: " + JSON.stringify(imageResult));
  if (imageResult.multiN !== imageResult.regularN)
    fail(
      "a multi-image stack must settle at exactly one slot, not a per-image sum: " +
        JSON.stringify(imageResult),
    );

  // Idempotence: once every image has already resolved, a further load/error
  // event must be a true no-op (mosaicMarkImageResolved's WeakSet guard) --
  // zero re-render, zero style write on any card in the lane.
  if (imageResult.fingerprintAfter !== imageResult.fingerprintBefore)
    fail("a load/error event after resolution triggered another re-render (fingerprint changed)");
  for (const before of imageResult.before.cards) {
    const after = imageResult.after.cards.find((c) => c.key === before.key);
    if (
      !after ||
      after.transform !== before.transform ||
      after.width !== before.width ||
      after.height !== before.height
    )
      fail("a load/error event after resolution changed a card's rendered style: " + before.key);
  }
}


function assertBarrierResetInvariants(barrierResult, fail) {
  if (!barrierResult.barrierCard)
    fail("task-directive-stack message did not produce a lattice card");
  if (barrierResult.barrierCard.span !== MOSAIC_STREAM_GRID_TRACK_COUNT)
    fail("task-directive-stack message must force span 12, got " + barrierResult.barrierCard.span);
  const barrierTop = barrierResult.barrierCard.b + barrierResult.barrierCard.n;
  if (!barrierResult.nextCard)
    fail("card following the barrier did not exist");
  if (barrierResult.nextCard.b !== barrierTop)
    fail(
      "a span-12 card must reset the frontier flat across every track -- the next card must anchor exactly at the barrier's top row (" +
        barrierTop + "), got " + barrierResult.nextCard.b,
    );
}

function assertBackfillInvariants(backfillResult, fail) {
  const beforeCardsByKey = new Map(backfillResult.before.cards.map((card) => [card.key, card]));
  const beforeLatticeByKey = new Map(
    backfillResult.before.lattice.map((card) => [card.key, card]),
  );
  for (const key of backfillResult.existingKeys) {
    const beforeCard = beforeCardsByKey.get(key);
    const afterCard = backfillResult.after.cards.find((card) => card.key === key);
    if (
      !beforeCard ||
      !afterCard ||
      beforeCard.transform !== afterCard.transform ||
      beforeCard.width !== afterCard.width ||
      beforeCard.height !== afterCard.height
    )
      fail("history backfill changed an existing card's rendered style: " + key);
    const before = beforeLatticeByKey.get(key);
    const after = backfillResult.after.lattice.find((card) => card.key === key);
    if (
      !before ||
      !after ||
      before.t !== after.t ||
      before.b !== after.b ||
      before.n !== after.n ||
      before.span !== after.span
    )
      fail("history backfill changed an existing card lattice record: " + key);
  }

  for (const key of backfillResult.backfillKeys) {
    const card = backfillResult.after.lattice.find((candidate) => candidate.key === key);
    if (!card) fail("history backfill did not create card " + key);
    if (card.b + card.n > backfillResult.existingMinB)
      fail("history backfill card did not land below the existing archive: " + key);
  }
  if (!backfillResult.newKeys.some((key) => backfillResult.backfillKeys.includes(key)))
    fail("history backfill did not add any message keys");
  if (!backfillResult.backfillCreationIndexes.every((index) => Number.isFinite(index) && index < 0))
    fail("history backfill cards were not assigned earlier creation indexes");
  if (
    Math.abs(backfillResult.bottomDistanceAfter - backfillResult.bottomDistanceBefore) >
    MOSAIC_STREAM_SCROLL_EPSILON_PX
  )
    fail("history backfill moved the bottom seam viewport");
  if (!backfillResult.sentinelStable)
    fail("history sentinel identity changed during backfill");
  if (!backfillResult.sentinelConnected)
    fail("history sentinel was detached after backfill");
  if (backfillResult.sentinelTargetId !== backfillResult.laneTargetId)
    fail("history sentinel target identity changed during backfill");

  const replayOverlap = mosaicStreamCardsOverlap(backfillResult.afterReplay.lattice);
  if (replayOverlap)
    fail("full replay after history backfill produced overlapping cards: " + JSON.stringify(replayOverlap));
  for (const key of backfillResult.backfillKeys) {
    if (!backfillResult.afterReplay.lattice.some((card) => card.key === key))
      fail("full replay dropped backfilled card " + key);
  }
}

function assertEventLogReplayInvariants(eventLogReplay, fail) {
  if (eventLogReplay.eventCount <= 0)
    fail("mosaic stream did not record any event-log events");
  if (JSON.stringify(eventLogReplay.live) !== JSON.stringify(eventLogReplay.replayed))
    fail("mosaic event-log replay did not reproduce the live lattice byte-identically");
}

function assertMosaicStreamResult(result) {
  if (result.measureClobber.mismatches.length)
    throw new Error(
      "measurement clobbered card styles: " +
        JSON.stringify(result.measureClobber.mismatches),
    );
  if (result.measureClobber.cardCount < MOSAIC_STREAM_CLOBBER_MIN_CARDS)
    throw new Error("clobber scenario ran on too few cards");
  if (!result.bulkAppend.identical)
    throw new Error(
      "bulk-newer batch moved or re-styled existing cards: " +
        JSON.stringify(result.bulkAppend),
    );
  if (!result.bulkAppend.placedInOrder)
    throw new Error(
      "bulk-newer batch placed out of order: " + JSON.stringify(result.bulkAppend),
    );
  const fail = (message) => {
    throw new Error(message + ": " + JSON.stringify(result));
  };
  assertInsertInvariants(result.insertResult, fail);
  assertRemovalInvariants(result.removalResult, fail);
  assertContentChangeInvariants(result.contentChangeResult, fail);
  assertFrozenGrowthInvariants(result.frozenGrowthResult, fail);
  assertGeometryChangeInvariants(result.geometryChangeResult, fail);
  assertReadingOrderInvariants(result.readingOrder, fail);
  assertImageReservationInvariants(result.imageResult, fail);
  assertBarrierResetInvariants(result.barrierResult, fail);
  assertBackfillInvariants(result.backfillResult, fail);
  assertEventLogReplayInvariants(result.eventLogReplay, fail);
}

async function waitForMosaicStreamGlobals(page) {
  await page.waitForFunction(
    () =>
      typeof addLane === "function" &&
      typeof renderMessagesIfChanged === "function" &&
      typeof upsertKnownMessage === "function" &&
      typeof trimKnownMessages === "function" &&
      typeof mosaicReplayEventLog === "function" &&
      typeof mosaicEventLogLayout === "function" &&
      Array.isArray(targets) &&
      targets.length > 0,
    { timeout: 10000 },
  );
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-stream-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 900 } },
    },
    async ({ page, server }) => {
      await waitForMosaicStreamGlobals(page);
      await installMosaicStreamHelpers(page);
      const insertResult = await page.evaluate(mosaicStreamInsertSequence);
      const removalResult = await page.evaluate(mosaicStreamRemovalCheck);
      const contentChangeResult = await page.evaluate(mosaicStreamContentChangeCheck);
      const frozenGrowthResult = await page.evaluate(mosaicStreamFrozenGrowthCheck);
      const geometryChangeResult = await page.evaluate(mosaicStreamGeometryChangeCheck);
      const readingOrder = await page.evaluate(mosaicStreamReadingOrderCheck);
      const imageResult = await page.evaluate(mosaicStreamImageReservationCheck);
      const barrierResult = await page.evaluate(mosaicStreamBarrierResetCheck);
      const backfillResult = await page.evaluate(mosaicStreamBackfillCheck);
      const eventLogReplay = await page.evaluate(mosaicStreamEventLogReplayCheck);
      const measureClobber = await page.evaluate(mosaicStreamMeasureClobberCheck);
      const bulkAppend = await page.evaluate(mosaicStreamBulkAppendCheck);
      const result = {
        insertResult,
        removalResult,
        contentChangeResult,
        frozenGrowthResult,
        geometryChangeResult,
        readingOrder,
        imageResult,
        barrierResult,
        backfillResult,
        eventLogReplay,
        measureClobber,
        bulkAppend,
      };
      assertMosaicStreamResult(result);
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
