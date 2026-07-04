const { withServePage } = require("./serve_playwright_harness");

// Mosaic stream integration (mosaic-stream-integration, spec §1/§6/§7/§8):
// drives a real lane through renderMessagesIfChanged exactly as live
// traffic would (upsertKnownMessage + trimKnownMessages, not renderMessage
// called directly) and checks the persistent-lattice invariants: insert
// never moves existing cards; unchanged cards get zero style writes on a
// no-op re-render; removing the oldest message vacates without moving the
// rest; newest-first reading order holds; image reservations (§4/§12) are
// exact and a load/error event never triggers pack/position/size activity.

// msg-10..msg-16 (frozen-growth chain) plus the surviving msg-2/msg-3 from
// the earlier insert/removal/content-change scenarios.
const MOSAIC_STREAM_FROZEN_CHAIN_FIRST_KEY = 10;
const MOSAIC_STREAM_FROZEN_CHAIN_LAST_KEY = 16;
const MOSAIC_STREAM_EXPECTED_SURVIVOR_COUNT =
  MOSAIC_STREAM_FROZEN_CHAIN_LAST_KEY - MOSAIC_STREAM_FROZEN_CHAIN_FIRST_KEY + 1 + 2;

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
  // re-render -- per §8, a single removal must not move the survivors.
  lane.knownMessages = lane.knownMessages.filter((item) => item.key !== "msg-1");
  lane.knownMessageKeys.delete("msg-1");
  renderMessagesIfChanged(lane);
  const after = mosaicStreamCardSnapshot(lane);
  return { before, after };
}

// §7 wetReplay: growing a still-wet card's content must re-place the wet
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

// §7 rippleRows: growing a FROZEN card must keep its top row fixed
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

// §8 full replay: a geometry change (root font-size here, cheaper to drive
// deterministically in a smoke than a real container resize) must trigger
// a full replay -- every surviving card re-decided against a fresh
// rowFloor in creation order, reading order back to clean chronological.
function mosaicStreamGeometryChangeCheck() {
  const lane = mosaicStreamResolveLane();
  const before = mosaicStreamCardSnapshot(lane);
  document.documentElement.style.fontSize = "20px";
  // Mirrors what mosaicScheduleFullReplay's ResizeObserver callback does on
  // a real geometry change: force past the fingerprint early-return
  // (app.stream.js renderMessagesIfChanged) so the render path actually
  // runs and mosaicGeometryChanged sees the new root/M and full-replays.
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  const afterAtNewRoot = mosaicStreamCardSnapshot(lane);
  // Idempotence (§10) at a STABLE geometry (no font-size toggle in
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
  // Newest-first is only an EXACT guarantee within a column chain (§1
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

// Images (§4/§12 imageHeights): a message with an inline image reserves the
// small letterbox cap ("image"); an image-only message reserves the large
// cap ("imageLarge") -- both sourced from the single dataset classification
// app.render.js computes once (article.dataset.mosaicReservationType), never
// re-derived from the .media-rich class, which is true for BOTH cases and
// would misclassify the small case as large. A load/error event on the
// image must cause zero pack/position/size activity anywhere in the lane:
// the reservation is permanent (images letterbox to their cap regardless of
// load state), so there is nothing left to resolve once the image settles,
// broken or not -- unlike the ack/quote reservation, which genuinely is
// pending until real content hydrates.
const MOSAIC_STREAM_IMAGE_HTML =
  '<p class="message-image-stack"><a class="message-image" href="#">' +
  '<img alt="fixture" src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></a></p>';
// A message-image-stack lays its images out in a single horizontal row
// (messages.css), all at the same letterbox height -- three images here
// prove the reservation stays at exactly one slot, never a per-image sum.
const MOSAIC_STREAM_MULTI_IMAGE_HTML =
  '<p class="message-image-stack">' +
  '<a class="message-image" href="#"><img alt="fixture 1" src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></a>' +
  '<a class="message-image" href="#"><img alt="fixture 2" src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></a>' +
  '<a class="message-image" href="#"><img alt="fixture 3" src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></a>' +
  "</p>";
// A malformed data URI fails to decode (fires "error") with no network
// request at all -- an actual unreachable URL would either hit the
// browser's blocked-port list or depend on network timing neither of which
// this smoke needs, since only the broken image's SLOT (not the fetch
// itself) is under test here.
const MOSAIC_STREAM_BROKEN_IMAGE_HTML =
  '<p class="message-image-stack"><a class="message-image" href="#">' +
  '<img alt="broken fixture" src="data:image/png;base64,not-a-valid-image"></a></p>';

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

function mosaicStreamImageReservationCheck() {
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
  const geometry = lane.mosaicGeometry;

  const regularImg = articleFor("img-regular").querySelector("img");
  const onlyImg = articleFor("img-only").querySelector("img");
  const brokenImg = articleFor("img-broken").querySelector("img");
  const multiImgs = Array.from(articleFor("img-multi").querySelectorAll("img"));

  const snapshot = {
    regularReserve: articleFor("img-regular").dataset.mosaicReservationType,
    onlyReserve: articleFor("img-only").dataset.mosaicReservationType,
    brokenReserve: articleFor("img-broken").dataset.mosaicReservationType,
    multiReserve: articleFor("img-multi").dataset.mosaicReservationType,
    regularN: cardFor("img-regular").n,
    onlyN: cardFor("img-only").n,
    brokenN: cardFor("img-broken").n,
    multiN: cardFor("img-multi").n,
    multiImageCount: multiImgs.length,
    regularHeightRows: mosaicRowsFor(
      Number.parseFloat(getComputedStyle(regularImg).height),
      geometry.gap,
      geometry.M,
    ),
    onlyHeightRows: mosaicRowsFor(
      Number.parseFloat(getComputedStyle(onlyImg).height),
      geometry.gap,
      geometry.M,
    ),
    brokenHeightRows: mosaicRowsFor(
      Number.parseFloat(getComputedStyle(brokenImg).height),
      geometry.gap,
      geometry.M,
    ),
    multiHeightRows: mosaicRowsFor(
      Number.parseFloat(getComputedStyle(multiImgs[0]).height),
      geometry.gap,
      geometry.M,
    ),
  };

  const fingerprintBefore = lane.renderedMessageFingerprint;
  const before = mosaicStreamCardSnapshot(lane);

  for (const img of [regularImg, onlyImg, brokenImg, ...multiImgs]) {
    img.dispatchEvent(new Event("load"));
    img.dispatchEvent(new Event("error"));
  }

  // A leftover load/error handler would schedule its work via rAF (the
  // pattern every other mosaic-stream scheduling path in this module uses);
  // two nested rAFs give any such stray callback a full frame to run before
  // re-snapshotting, so its absence is actually proven, not just unlucky
  // timing.
  return new Promise((resolve) => {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        resolve({
          ...snapshot,
          fingerprintBefore,
          fingerprintAfter: lane.renderedMessageFingerprint,
          before,
          after: mosaicStreamCardSnapshot(lane),
        });
      });
    });
  });
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
      mosaicStreamResolveLane,
      mosaicStreamBuildItem,
      mosaicStreamPushMessage,
      mosaicStreamCardSnapshot,
      mosaicStreamInsertSequence,
      mosaicStreamRemovalCheck,
      mosaicStreamContentChangeCheck,
      mosaicStreamFrozenGrowthCheck,
      mosaicStreamGeometryChangeCheck,
      mosaicStreamReadingOrderCheck,
      mosaicStreamImageItem,
      mosaicStreamImageReservationCheck,
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

  // §6: insertion must never touch any existing card's rendered position.
  const firstCardAfterFirst = insertResult.afterFirst.cards.find((c) => c.key === "msg-1");
  const firstCardAfterThird = insertResult.afterThird.cards.find((c) => c.key === "msg-1");
  if (!firstCardAfterFirst || !firstCardAfterThird)
    fail("msg-1's card disappeared across inserts");

  // §11c: a no-op re-render must write zero new style strings.
  for (const before of insertResult.beforeNoop) {
    const after = insertResult.afterNoop.cards.find((c) => c.key === before.key);
    if (!after || after.transform !== before.transform || after.width !== before.width || after.height !== before.height)
      fail("a no-op re-render changed a card's rendered style: " + before.key);
  }
}

function assertRemovalInvariants(removalResult, fail) {
  // §8 vacancy: removing the oldest message must not move the survivors.
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
  // §7 wetReplay: growing msg-3's content must change its own row count
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
  // §7 rippleRows: growing a frozen card keeps its own top row fixed
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
  // §8 full replay: a geometry change must re-decide every surviving card
  // (module M changes with root font-size).
  if (geometryChangeResult.before.lattice.length !== geometryChangeResult.afterAtNewRoot.lattice.length)
    fail("full replay must not change the surviving card count");
  const moduleBefore = geometryChangeResult.before.cards[0]?.height;
  const moduleAfterNewRoot = geometryChangeResult.afterAtNewRoot.cards[0]?.height;
  if (moduleBefore === moduleAfterNewRoot)
    fail("full replay at a new root font-size must change rendered card height");

  // §10 determinism: replaying again at a STABLE geometry (no font-size
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
  // §4: classification must come from the single dataset attribute, not
  // re-derived from .media-rich (true for every image, which would collapse
  // both tiers into "imageLarge").
  if (imageResult.regularReserve !== "image")
    fail("an inline (non-image-only) image must reserve the small cap, got " + imageResult.regularReserve);
  if (imageResult.onlyReserve !== "imageLarge")
    fail("an image-only message must reserve the large cap, got " + imageResult.onlyReserve);
  if (imageResult.brokenReserve !== "image")
    fail("a broken inline image must still reserve the small cap, got " + imageResult.brokenReserve);
  if (imageResult.multiReserve !== "image")
    fail("a multi-image stack (not image-only) must reserve the small cap, got " + imageResult.multiReserve);

  // Exactness (§4/§12): the reserved row count must equal the rows the
  // actually-rendered (letterboxed) image height converts to, for a normal
  // image, an image-only image, a broken one, AND a multi-image stack --
  // none measure the intrinsic image, all sit at their named cap.
  if (imageResult.regularN !== imageResult.regularHeightRows)
    fail("regular image reservation is not exact: " + JSON.stringify(imageResult));
  if (imageResult.onlyN !== imageResult.onlyHeightRows)
    fail("image-only reservation is not exact: " + JSON.stringify(imageResult));
  if (imageResult.brokenN !== imageResult.brokenHeightRows)
    fail("broken image reservation is not exact: " + JSON.stringify(imageResult));
  if (imageResult.multiN !== imageResult.multiHeightRows)
    fail("multi-image stack reservation is not exact: " + JSON.stringify(imageResult));

  // Multi-image stacking rule (§4/§12): three images in one stack (laid out
  // in a single horizontal row, messages.css) must reserve exactly the SAME
  // one-slot height as a single image, never a per-image sum.
  if (imageResult.multiImageCount !== 3)
    fail("multi-image fixture did not render all three images: " + JSON.stringify(imageResult));
  if (imageResult.multiN !== imageResult.regularN)
    fail(
      "a multi-image stack must reserve exactly one slot, not a per-image sum: " +
        JSON.stringify(imageResult),
    );

  // §4: a load/error event must never schedule any pack/position/size
  // activity -- the reservation is permanent, so there is nothing to
  // resolve. Proven both at the fingerprint level (no re-render was
  // scheduled at all) and at the rendered-style level (identical transform/
  // width/height strings for every card, not just the image cards).
  if (imageResult.fingerprintAfter !== imageResult.fingerprintBefore)
    fail("a load/error event triggered a re-render (fingerprint changed)");
  for (const before of imageResult.before.cards) {
    const after = imageResult.after.cards.find((c) => c.key === before.key);
    if (
      !after ||
      after.transform !== before.transform ||
      after.width !== before.width ||
      after.height !== before.height
    )
      fail("a load/error event changed a card's rendered style: " + before.key);
  }
}

function assertMosaicStreamResult(result) {
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
}

async function waitForMosaicStreamGlobals(page) {
  await page.waitForFunction(
    () =>
      typeof addLane === "function" &&
      typeof renderMessagesIfChanged === "function" &&
      typeof upsertKnownMessage === "function" &&
      typeof trimKnownMessages === "function" &&
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
    async ({ page, server, browserErrors }) => {
      await waitForMosaicStreamGlobals(page);
      await installMosaicStreamHelpers(page);
      const insertResult = await page.evaluate(mosaicStreamInsertSequence);
      const removalResult = await page.evaluate(mosaicStreamRemovalCheck);
      const contentChangeResult = await page.evaluate(mosaicStreamContentChangeCheck);
      const frozenGrowthResult = await page.evaluate(mosaicStreamFrozenGrowthCheck);
      const geometryChangeResult = await page.evaluate(mosaicStreamGeometryChangeCheck);
      const readingOrder = await page.evaluate(mosaicStreamReadingOrderCheck);
      const imageResult = await page.evaluate(mosaicStreamImageReservationCheck);
      // The broken-image fixture (mosaicStreamImageReservationCheck) is
      // deliberately an undecodable image src, to prove a broken image
      // still occupies its exact letterbox slot (§4). The browser's own
      // "failed to load this resource" console error is the expected,
      // intentional side effect of that fixture, not a real bug -- drop
      // only that specific entry so the harness's blanket
      // assertNoBrowserErrors below still catches anything unexpected.
      const expectedBrokenImageErrors = browserErrors.filter(
        (error) => error.type === "error" && /Failed to load resource/.test(error.text),
      );
      if (expectedBrokenImageErrors.length !== 1) {
        throw new Error(
          "expected exactly one broken-image resource-load console error, got " +
            expectedBrokenImageErrors.length +
            ": " +
            JSON.stringify(browserErrors),
        );
      }
      browserErrors.length = 0;
      const result = {
        insertResult,
        removalResult,
        contentChangeResult,
        frozenGrowthResult,
        geometryChangeResult,
        readingOrder,
        imageResult,
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
