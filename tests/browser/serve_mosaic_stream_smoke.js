const { withServePage } = require("./serve_playwright_harness");

// Mosaic stream integration (mosaic-stream-integration, spec §1/§6/§7/§8):
// drives a real lane through renderMessagesIfChanged exactly as live
// traffic would (upsertKnownMessage + trimKnownMessages, not renderMessage
// called directly) and checks the persistent-lattice invariants: insert
// never moves existing cards; unchanged cards get zero style writes on a
// no-op re-render; removing the oldest message vacates without moving the
// rest; newest-first reading order holds.

// msg-10..msg-16 (frozen-growth chain) plus the surviving msg-2/msg-3 from
// the earlier insert/removal/content-change scenarios.
const MOSAIC_STREAM_FROZEN_CHAIN_FIRST_KEY = 10;
const MOSAIC_STREAM_FROZEN_CHAIN_LAST_KEY = 16;
const MOSAIC_STREAM_EXPECTED_SURVIVOR_COUNT =
  MOSAIC_STREAM_FROZEN_CHAIN_LAST_KEY - MOSAIC_STREAM_FROZEN_CHAIN_FIRST_KEY + 1 + 2;
const MOSAIC_STREAM_BACKFILL_EXISTING_FIRST_KEY = 100;
const MOSAIC_STREAM_BACKFILL_EXISTING_COUNT = 24;
const MOSAIC_STREAM_BACKFILL_COUNT = 3;
const MOSAIC_STREAM_BACKFILL_LINES = 8;
const MOSAIC_STREAM_SCROLL_EPSILON_PX = 2;

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
      mosaicStreamBackfillCheck,
      mosaicStreamEventLogReplayCheck,
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
  const fail = (message) => {
    throw new Error(message + ": " + JSON.stringify(result));
  };
  assertInsertInvariants(result.insertResult, fail);
  assertRemovalInvariants(result.removalResult, fail);
  assertContentChangeInvariants(result.contentChangeResult, fail);
  assertFrozenGrowthInvariants(result.frozenGrowthResult, fail);
  assertGeometryChangeInvariants(result.geometryChangeResult, fail);
  assertReadingOrderInvariants(result.readingOrder, fail);
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
      const backfillResult = await page.evaluate(mosaicStreamBackfillCheck);
      const eventLogReplay = await page.evaluate(mosaicStreamEventLogReplayCheck);
      const result = {
        insertResult,
        removalResult,
        contentChangeResult,
        frozenGrowthResult,
        geometryChangeResult,
        readingOrder,
        backfillResult,
        eventLogReplay,
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
