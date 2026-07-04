// Mosaic stream integration (spec .spice/mosaic-packing-spec.md §1, §6, §7,
// §8, §13). Wires the pure mosaic-*.js modules into the fingerprint-gated
// render path (app.stream.js renderMessagesIfChanged): persistent per-card
// lattice state keyed by message identity, replacing the legacy grid
// packer's whole-board repack. This module owns the DOM-facing
// glue -- node lifecycle inside .mosaic-plane, event classification,
// measurement, and calling engine/render primitives in the documented
// order -- none of the algorithm itself, which lives in the modules this
// composes.
//
// Scope note (serve-specific extension beyond the spec, which is silent on
// non-card stream elements): compaction dividers are real keyed messages
// (kind === "compaction") and get the same forced-full-span lattice
// treatment the legacy packer gave "barrier" nodes. Time rules are
// synthesized fresh per render from adjacency gaps (timeRuleBetween,
// app.stream.js) rather than being persistent server entities, so they are
// given a derived stable key ("time-rule:" + followingItem.key) to
// participate in the same persistent lattice. The history-pagination
// sentinel carries no visual meaning and stays outside the lattice
// entirely, pinned to the plane's bottom edge in CSS alone (messages.css).

const MOSAIC_RESIZE_WIDTH_EPSILON_PX = 0.5;
const MOSAIC_BACKFILL_BOTTOM_EPSILON_PX = 2;
// Below this host width the lane is not meaningfully measurable (hidden
// ancestor, mid-mount, collapsed pane): rendering would run the geometry
// pass against garbage (clientWidth 0 degenerates edges to [0..12] and a
// spurious full replay re-spans every card), and any transform written to
// an unrendered will-change element can freeze a stale scrollable-overflow
// region that outlives the reveal (probe scenarios B/C on freeze-root).
// Renders defer instead; the ResizeObserver's 0-to-real transition brings
// the deferred render back as a motion-suppressed correction.
const MOSAIC_MIN_RENDER_WIDTH_PX = 24;
// §8 settled predicate: a populated board with no structural change for this
// window (or first pane interaction) counts as settled; until then the §9
// settled-board rule suppresses every transform transition (spec §12
// settleQuiet). Settling flips a flag and never triggers a render.
const MOSAIC_SETTLE_QUIET_MS = 500;
const MOSAIC_SETTLE_INTERACTION_EVENTS = [
  "pointerdown",
  "wheel",
  "touchstart",
  "keydown",
  "focusin",
];

function mosaicHostMeasurable(host) {
  return Boolean(host) && host.clientWidth >= MOSAIC_MIN_RENDER_WIDTH_PX;
}

function mosaicSettleNow(lane) {
  lane.mosaicSettled = true;
  if (lane.mosaicSettleTimer) {
    clearTimeout(lane.mosaicSettleTimer);
    lane.mosaicSettleTimer = 0;
  }
}

// Restart the quiet window on structural change; leave it running across
// renders that changed nothing structural (content-diff only). First settle
// latches for the lane's lifetime -- post-settle events never re-gate.
function mosaicSyncSettle(lane, structural) {
  if (lane.mosaicSettled) return;
  if (!lane.mosaicCards.length) return;
  if (lane.mosaicSettleTimer && !structural) return;
  if (lane.mosaicSettleTimer) clearTimeout(lane.mosaicSettleTimer);
  lane.mosaicSettleTimer = setTimeout(() => {
    lane.mosaicSettleTimer = 0;
    lane.mosaicSettled = true;
  }, MOSAIC_SETTLE_QUIET_MS);
}

// The reader's first deliberate interaction settles immediately (§8):
// deliberate gestures only -- programmatic scrolls (compensation, backfill
// restore) must not count, so the raw scroll event is deliberately absent.
function mosaicArmSettleInteraction(lane) {
  if (lane.mosaicSettleInteractionArmed || !lane.messagesEl) return;
  lane.mosaicSettleInteractionArmed = true;
  const settle = () => mosaicSettleNow(lane);
  for (const type of MOSAIC_SETTLE_INTERACTION_EVENTS) {
    lane.messagesEl.addEventListener(type, settle, { once: true, passive: true });
  }
}

// ---- lane lattice state ----------------------------------------------------------

function mosaicLaneReady(lane) {
  if (lane.mosaicCards) return;
  lane.mosaicCards = [];
  lane.mosaicEventLog = mosaicCreateEventLog(
    mosaicGridTrackCount,
    MOSAIC_FREEZE_DEPTH,
  );
  lane.mosaicNextCreationIndex = 0;
  lane.mosaicNextBackfillCreationIndex = -1;
  // Keyed by ELEMENT, not by card key: renderOrReuseMessageNode/mosaicRuleNode
  // build a fresh element whenever a message's content fingerprint changes
  // (e.g. an ack skeleton resolving to its real quote) even when the card's
  // own (t,b,n,span) stays identical. A key-keyed memo would then see
  // "unchanged" and skip mosaicApplyCardPosition entirely, leaving the FRESH
  // element with no transform/width/height ever applied -- a WeakMap keyed
  // by the element makes a first-time element always look prev-less (the
  // correct force-a-write case), and needs no manual cleanup since entries
  // vanish with their (detached, unreferenced) elements.
  lane.mosaicAppliedByNode = new WeakMap();
  lane.mosaicPrevMaxRow = 0;
  lane.mosaicPrevExtent = null;
  lane.mosaicGeometry = null;
  lane.mosaicSettled = false;
  lane.mosaicSettleTimer = 0;
}

// geometry must already be resolved for THIS render before a fresh plane
// is anchored: mosaicAnchorPlane's first-paint transform has to read the
// real M immediately, never a placeholder later overwritten by
// mosaicApplyRender's mosaicSyncPlane call -- a wrong interim value here
// is exactly the kind of stale/incorrect plane transform the documented
// call-order contract on mosaicApplyCardPosition warns against (see
// app.mosaic-render.js).
function mosaicPlane(lane, geometry) {
  mosaicLaneReady(lane);
  if (
    lane.mosaicPlaneEl &&
    lane.mosaicPlaneEl.isConnected &&
    lane.mosaicExtentEl &&
    lane.mosaicExtentEl.isConnected
  )
    return lane.mosaicPlaneEl;
  if (lane.mosaicPlaneEl) lane.mosaicPlaneEl.remove();
  if (lane.mosaicExtentEl) lane.mosaicExtentEl.remove();
  const plane = document.createElement("div");
  plane.className = "mosaic-plane";
  // Scroll extent lives on a static, untransformed spacer -- never on the
  // plane, whose rendered transform is exactly the state that can disagree
  // with layout (frozen transforms, mid-tween excursions). The spacer's
  // explicit height is the lattice extent; the plane stays heightless.
  const extentEl = document.createElement("div");
  extentEl.className = "mosaic-extent";
  lane.messagesEl.prepend(extentEl);
  lane.messagesEl.prepend(plane);
  lane.mosaicPlaneEl = plane;
  lane.mosaicExtentEl = extentEl;
  lane.mosaicAppliedByNode = new WeakMap();
  lane.mosaicPrevMaxRow = mosaicAnchorPlane(plane, geometry.M);
  lane.mosaicPrevExtent = null;
  return plane;
}

function mosaicCreationIndexFor(lane, key) {
  const existing = lane.mosaicCards.find((card) => card.key === key);
  if (existing) return existing.creationIndex;
  const index = lane.mosaicNextCreationIndex;
  lane.mosaicNextCreationIndex += 1;
  return index;
}

function mosaicBackfillCreationIndexFor(lane, key) {
  const existing = lane.mosaicCards.find((card) => card.key === key);
  if (existing) return existing.creationIndex;
  if (!Number.isFinite(lane.mosaicNextBackfillCreationIndex))
    lane.mosaicNextBackfillCreationIndex = -1;
  if (lane.mosaicCards.length) {
    const minIndex = Math.min(...lane.mosaicCards.map((card) => card.creationIndex));
    if (lane.mosaicNextBackfillCreationIndex >= minIndex)
      lane.mosaicNextBackfillCreationIndex = minIndex - 1;
  }
  const index = lane.mosaicNextBackfillCreationIndex;
  lane.mosaicNextBackfillCreationIndex -= 1;
  return index;
}

// ---- entries: messages, compaction dividers, and derived time-rule markers -------

// A card is span-12 ("barrier") for either of two reasons: its own kind
// (compaction dividers), or its rendered content (task-directive-stack
// quotes -- server-emitted markup, spice/serve/taskdirectives.py -- always
// exactly `<div class="task-directive-stack">`, so a plain substring check
// on the pre-render display_html is exact and needs no DOM node). No card
// type gets a different SPAN POLICY (§5) from this -- both still place
// through the identical decide()/insert() path with span forced to 12
// rather than measured, the same treatment already given compaction
// dividers and time rules.
function mosaicItemIsBarrier(item) {
  if (item.kind === "compaction") return true;
  return Boolean(item.display_html && item.display_html.includes("task-directive-stack"));
}

// Mirrors app.stream.js's messageStreamNodesWithHistorySentinels ordering
// exactly (same timeRuleBetween calls against the same visible sequence) so
// a rule appears in the lattice iff. it would have appeared in the legacy
// flow, keyed by the message it precedes rather than by array position.
function mosaicStreamEntries(visibleItems) {
  const entries = [];
  let previousItem = null;
  for (const item of visibleItems) {
    const rule = timeRuleBetween(previousItem, item);
    if (rule) {
      entries.push({
        key: "time-rule:" + item.key,
        kind: "barrier",
        bucketStartMs: Number(rule.dataset.messageTs),
      });
    }
    entries.push({
      key: String(item.key),
      kind: mosaicItemIsBarrier(item) ? "barrier" : "message",
      item,
    });
    previousItem = item;
  }
  return entries;
}

function mosaicRuleNode(lane, entry, existingNodes) {
  const existing = existingNodes.get(entry.key);
  const fingerprint = String(entry.bucketStartMs);
  if (existing && existing.dataset.renderFingerprint === fingerprint) return existing;
  const node = renderTimeRule(entry.bucketStartMs);
  node.dataset.messageKey = entry.key;
  node.dataset.renderFingerprint = fingerprint;
  return node;
}

function mosaicEntryNode(lane, entry, existingNodes) {
  if (entry.item) return renderOrReuseMessageNode(lane, entry.item, existingNodes);
  return mosaicRuleNode(lane, entry, existingNodes);
}

function mosaicExistingNodesByKey(lane) {
  const nodes = new Map();
  const plane = lane.mosaicPlaneEl;
  if (!plane) return nodes;
  for (const node of plane.children) {
    const key = node.dataset ? node.dataset.messageKey || "" : "";
    if (key) nodes.set(key, node);
  }
  return nodes;
}

// ---- reservation classification (§4) ----------------------------------------------

// Two different lifetimes share this one lookup: an ack/quote reservation is
// genuinely pending -- real content hasn't hydrated yet, and shrink-on-
// resolve is the common case (§4) -- while an image reservation is
// permanent: images letterbox to their named cap regardless of load state
// (messages.css), so the reservation never needs to give way to a measured
// height, whether the image is loading, loaded, or broken. Both read the
// single reservation type app.render.js already computes once, at node-
// creation time (messageReservationType: "ack" while any ack_segments key
// has no resolved context yet via ackContextForKey, "imageLarge" for
// image_only items, an "image" fallback for any other card containing
// .message-image), stamped onto the node as data-mosaic-reservation-type --
// no reservation logic scattered per-call-site (§4), and no second,
// divergent check here (an earlier version of this function re-derived the
// ack case from lane.missingAckContextKeys, which only tracks confirmed-
// missing keys, not "not yet resolved", so a genuinely-pending ack fell
// through to real measurement instead of reserving). A fresh node built once
// its content resolves has no reservation type at all (messageReservationType
// returns "" once ackContextForKey finds a context), so resolution falls
// through to real measurement with no extra branching needed here. Barrier
// entries (dividers, rules) are never reserved -- their content is fixed at
// creation.
function mosaicPendingReservationType(lane, entry, node) {
  if (entry.kind === "barrier") return null;
  return node.dataset.mosaicReservationType || null;
}

// ---- measurement ------------------------------------------------------------------

// Natural content height at a candidate width: the node must already be a
// child of the plane (real .messages/.mosaic-plane ancestry for CSS chrome
// to match, per mosaic-sizing-interlock) with min-height cleared so
// reserved-height styling from a prior placement never shadows fresh
// content. position:absolute cards size to content by default, so no
// grid-stretch counter-measure is needed here (contrast the sizing/span
// smoke tests, which measured against the pre-integration grid).
function mosaicMeasureNodeAt(node, widthPx) {
  // Clearing minHeight alone is not enough: mosaicApplyCardPosition sets an
  // EXPLICIT height from the card's last-committed row count, which
  // overrides natural content sizing outright. Leaving it in place would
  // make every re-measurement after a card's first render just read back
  // its own stale reserved height instead of the true natural height at
  // the new width -- silently wrong on every full replay and wet resize.
  node.style.minHeight = "";
  node.style.height = "";
  node.style.width = widthPx + "px";
  return node.offsetHeight;
}

function mosaicCandidatesFor(lane, entry, node, geometry) {
  const rootFontSizePx = mosaicRootFontSizePx();
  const reservationType = mosaicPendingReservationType(lane, entry, node);
  const measure = (span) => {
    if (reservationType) return null;
    return mosaicMeasureNodeAt(node, mosaicCardWidthPx(geometry.edges, 0, span, geometry.gap));
  };
  if (entry.kind === "barrier") {
    const reservedRows = reservationType
      ? mosaicReservationRows(reservationType, rootFontSizePx, geometry.gap, geometry.M)
      : null;
    const n = reservedRows !== null
      ? reservedRows
      : mosaicRowsFor(measure(mosaicGridTrackCount), geometry.gap, geometry.M);
    return [{ span: mosaicGridTrackCount, n }];
  }
  if (reservationType) {
    const reservedRows = mosaicReservationRows(
      reservationType,
      rootFontSizePx,
      geometry.gap,
      geometry.M,
    );
    return [{ span: geometry.baseSpan, n: reservedRows }];
  }
  return mosaicCandidates(geometry.baseSpan, mosaicGridTrackCount, geometry.gap, geometry.M, measure);
}

// ---- geometry change detection ------------------------------------------------------

// Width comparison reads the shared edges table's total span rather than
// colW directly (§9 seam rule: only mosaic-geometry.js's edges[] table
// construction may compute with colW; every other mosaic file derives
// widths from that table alone).
function mosaicGeometryChanged(lane, geometry) {
  const previous = lane.mosaicGeometry;
  if (!previous) return true;
  const previousWidth = previous.edges[mosaicGridTrackCount];
  const nextWidth = geometry.edges[mosaicGridTrackCount];
  return (
    Math.abs(previousWidth - nextWidth) > MOSAIC_RESIZE_WIDTH_EPSILON_PX ||
    previous.M !== geometry.M ||
    previous.baseSpan !== geometry.baseSpan
  );
}

// ---- render application ------------------------------------------------------------

// Applies every card's position/size, syncing the plane BEFORE any card
// reflow per mosaicApplyCardPosition's documented call-order contract
// (app.mosaic-render.js), then syncs host height. Returns nothing; mutates
// lane's render-memo fields only (mosaicAppliedByNode/mosaicPrevMaxRow/
// mosaicPrevExtent), never the card records themselves.
function mosaicApplyRender(lane, cards, geometry, nodesByKey, options) {
  const extent = mosaicCardsExtent(cards);
  const reducedMotion = mosaicPrefersReducedMotion();
  const snap = Boolean(options && options.snap);
  const scrollOptions = mosaicPlaneScrollOptions(lane, undefined);
  const planeOptions = {
    ...scrollOptions,
    reducedMotion,
    snap,
    replaying: Boolean(options && options.replaying),
  };
  lane.mosaicPrevMaxRow = mosaicSyncPlane(
    lane.mosaicPlaneEl,
    extent.maxRow,
    lane.mosaicPrevMaxRow,
    geometry.M,
    planeOptions,
  );
  // Snap mode batches the first-paint flush: per-card forced reflows would
  // be O(n) layouts for a reveal rebuild, so suppressed writes accumulate
  // and a single flush + transition restore lands them all (§13).
  const written = [];
  for (const card of cards) {
    const node = nodesByKey.get(card.key);
    if (!node) continue;
    const prev = lane.mosaicAppliedByNode.get(node) || null;
    const next = mosaicApplyCardPosition(
      node,
      card,
      prev,
      geometry.edges,
      geometry.M,
      geometry.gap,
      { reducedMotion, snap, deferFlush: snap },
    );
    if (snap && next !== prev) written.push(node);
    lane.mosaicAppliedByNode.set(node, next);
  }
  if (written.length) {
    void lane.mosaicPlaneEl.offsetWidth;
    if (!reducedMotion) {
      for (const node of written) node.style.transition = "";
    }
  }
  lane.mosaicPrevExtent = mosaicSyncHostHeight(
    lane.mosaicExtentEl,
    extent.maxRow,
    extent.minB,
    geometry.M,
    lane.mosaicPrevExtent,
  );
}

// ---- events (§6 insert, §7 wetReplay/ripple, §8 fullReplay/vacancy) ---------------

function mosaicRunInsert(lane, entry, node, geometry) {
  const rowFloor = mosaicDeriveRowFloor(lane.mosaicCards, mosaicGridTrackCount);
  const candidates = mosaicCandidatesFor(lane, entry, node, geometry);
  const placement = mosaicInsert(lane.mosaicCards, rowFloor, candidates, mosaicGridTrackCount);
  const creationIndex = mosaicCreationIndexFor(lane, entry.key);
  mosaicRecordEventLogEvent(lane.mosaicEventLog, {
    type: "insert",
    key: entry.key,
    creationIndex,
    candidates,
  });
  const card = { ...placement.card, key: entry.key, creationIndex, frozen: false };
  lane.mosaicCards = lane.mosaicCards.concat([card]);
  lane.mosaicCards = mosaicRecomputeFrozen(lane.mosaicCards, MOSAIC_FREEZE_DEPTH);
}

function mosaicBackfillEntries(entries, previousKeySet, addedKeys) {
  if (!addedKeys.length) return null;
  const firstAddedIndex = entries.findIndex((entry) => !previousKeySet.has(entry.key));
  if (firstAddedIndex <= 0) return null;
  for (let index = 0; index < firstAddedIndex; index += 1) {
    if (!previousKeySet.has(entries[index].key)) return null;
  }
  const backfillEntries = [];
  for (let index = firstAddedIndex; index < entries.length; index += 1) {
    if (previousKeySet.has(entries[index].key)) return null;
    backfillEntries.push(entries[index]);
  }
  return backfillEntries;
}

function mosaicDeriveDownwardRowFloor(cards, trackCount) {
  const tracks = mosaicTrackCount(trackCount);
  const bottomByTrack = new Array(tracks).fill(0);
  for (const card of cards) {
    const start = Math.max(0, card.t);
    const end = Math.min(tracks, card.t + card.span);
    for (let track = start; track < end; track += 1) {
      if (card.b < bottomByTrack[track]) bottomByTrack[track] = card.b;
    }
  }
  const anchor = Math.max(...bottomByTrack);
  return {
    anchor,
    rowFloor: bottomByTrack.map((bottom) => anchor - bottom),
  };
}

function mosaicRunBackfill(lane, entries, nodesByKey, geometry) {
  const downward = mosaicDeriveDownwardRowFloor(lane.mosaicCards, mosaicGridTrackCount);
  let rowFloor = downward.rowFloor;
  const placedCards = [];
  const eventCards = [];
  for (const entry of entries) {
    const node = nodesByKey.get(entry.key);
    const candidates = mosaicCandidatesFor(lane, entry, node, geometry);
    const placement = mosaicInsert([], rowFloor, candidates, mosaicGridTrackCount);
    const creationIndex = mosaicBackfillCreationIndexFor(lane, entry.key);
    rowFloor = placement.rowFloor;
    eventCards.push({
      key: entry.key,
      creationIndex,
      candidates,
    });
    placedCards.push({
      ...placement.card,
      key: entry.key,
      creationIndex,
      b: downward.anchor - placement.card.b - placement.card.n,
      frozen: true,
    });
  }
  mosaicRecordEventLogEvent(lane.mosaicEventLog, {
    type: "backfill",
    trackCount: mosaicGridTrackCount,
    cards: eventCards,
  });
  lane.mosaicCards = lane.mosaicCards.concat(placedCards);
  lane.mosaicCards = mosaicRecomputeFrozen(lane.mosaicCards, MOSAIC_FREEZE_DEPTH);
}

function mosaicDropVacatedKeys(lane, desiredKeySet) {
  const removedKeys = lane.mosaicCards
    .filter((card) => !desiredKeySet.has(card.key))
    .map((card) => card.key);
  if (removedKeys.length) {
    mosaicRecordEventLogEvent(lane.mosaicEventLog, {
      type: "remove",
      keys: removedKeys,
    });
  }
  lane.mosaicCards = lane.mosaicCards.filter((card) => desiredKeySet.has(card.key));
}

// Re-measures a card at its OWN already-committed span (never a fresh
// candidate/decide pass -- span is fixed outside insert/fullReplay, per §7)
// and routes the outcome to wetReplay (wet) or mosaicResolveFrozenResize
// (frozen, which itself no-ops on shrink/exact and only ripples on true
// growth, §7). Only cards whose node was actually replaced by a fresh
// render (content genuinely changed, flagged by the caller) are touched;
// an untouched reused node must never re-enter this pass; that is what
// keeps a no-op re-render from moving anything (§11c).
//
// §14: dirty entries are resolved in CREATION-INDEX order, not entries'
// (newest-first) order -- two simultaneously-resolving cards (e.g. two acks
// racing back in reversed arrival order) must ripple/replay identically
// regardless of which happened to finish its fetch first. wetReplay already
// re-sorts internally, but mosaicResolveFrozenResize applies ripples one
// card at a time as this loop reaches them, so the loop order IS the
// ripple-application order and must be deterministic on its own.
function mosaicRunContentDiffPass(lane, entries, nodesByKey, geometry) {
  const dirty = [];
  for (const entry of entries) {
    const card = lane.mosaicCards.find((candidate) => candidate.key === entry.key);
    if (!card) continue;
    const node = nodesByKey.get(entry.key);
    if (!node || !node.dataset.mosaicContentDirty) continue;
    delete node.dataset.mosaicContentDirty;
    dirty.push({ entry, node, creationIndex: card.creationIndex });
  }
  dirty.sort((a, b) => a.creationIndex - b.creationIndex);

  let touchedWet = false;
  for (const { entry, node, creationIndex } of dirty) {
    const card = lane.mosaicCards.find((candidate) => candidate.creationIndex === creationIndex);
    if (!card) continue;
    const reservationType = mosaicPendingReservationType(lane, entry, node);
    const resolvedN = reservationType
      ? mosaicReservationRows(reservationType, mosaicRootFontSizePx(), geometry.gap, geometry.M)
      : mosaicRowsFor(
          mosaicMeasureNodeAt(node, mosaicCardWidthPx(geometry.edges, 0, card.span, geometry.gap)),
          geometry.gap,
          geometry.M,
        );
    if (resolvedN === card.n) continue;
    mosaicRecordEventLogEvent(lane.mosaicEventLog, {
      type: "content-resize",
      key: card.key,
      creationIndex: card.creationIndex,
      n: resolvedN,
    });
    if (card.frozen) {
      lane.mosaicCards = mosaicResolveFrozenResize(lane.mosaicCards, card.creationIndex, resolvedN);
    } else {
      lane.mosaicCards = lane.mosaicCards.map((candidate) =>
        candidate.creationIndex === card.creationIndex
          ? { ...candidate, n: resolvedN }
          : candidate,
      );
      touchedWet = true;
    }
  }
  if (touchedWet) {
    lane.mosaicCards = mosaicWetReplay(lane.mosaicCards, mosaicGridTrackCount, MOSAIC_FREEZE_DEPTH);
  }
}

// §8/§14: creationIndex must be the stream's stable (epoch, index, key)
// order (app.mosaic-wet-frozen.js), never DOM/arrival-observation order.
// `entries` is already in that true order (newest-first, mirroring
// laneGroupMergedMessages/knownMessages) -- deriving creationIndex from
// its reversed position covers every surviving card, old and newly-merged
// alike, in one pass, so no backfillKeySet special-case is needed here:
// unlike mosaicRunBackfill (which must avoid disturbing already-placed
// cards' indices to skip a full replay), this function is already
// recomputing everyone's position from scratch, so it can just read the
// truth entries already encodes. mosaicCreationIndexFor's incremental
// counter is right for a single live append (mosaicRunInsert, where the
// new key really is the newest thing ever observed) but wrong here: a bulk
// batch of previously-unseen keys -- a team fuse pulling in another
// member's whole backlog, or a history-pagination hydration landing
// several older messages at once -- would otherwise get stapled onto the
// END of the counter in newest-first iteration order, backwards, and
// stamped as newer than every already-known card regardless of true age.
function mosaicRunFullReplay(lane, entries, nodesByKey, geometry) {
  const withCandidates = entries.map((entry, index) => {
    const previous = lane.mosaicCards.find((card) => card.key === entry.key);
    const node = nodesByKey.get(entry.key);
    return {
      key: entry.key,
      creationIndex: entries.length - 1 - index,
      frozen: false,
      candidates: mosaicCandidatesFor(lane, entry, node, geometry),
      t: previous ? previous.t : 0,
      b: previous ? previous.b : 0,
      n: previous ? previous.n : 0,
      span: previous ? previous.span : geometry.baseSpan,
    };
  });
  mosaicRecordEventLogEvent(lane.mosaicEventLog, {
    type: "full-replay",
    trackCount: mosaicGridTrackCount,
    cards: withCandidates,
  });
  const replayed = mosaicFullReplay(withCandidates, mosaicGridTrackCount, MOSAIC_FREEZE_DEPTH);
  lane.mosaicCards = replayed.map(({ candidates, ...card }) => card);
  lane.mosaicNextCreationIndex = entries.length;
}

function mosaicCaptureBackfillViewport(lane) {
  const scroller = lane.messagesEl;
  if (!scroller) return null;
  const distanceFromBottom = scroller.scrollHeight - scroller.clientHeight - scroller.scrollTop;
  if (distanceFromBottom > MOSAIC_BACKFILL_BOTTOM_EPSILON_PX) return null;
  return { distanceFromBottom: Math.max(0, distanceFromBottom) };
}

function mosaicRestoreBackfillViewport(lane, renderResult) {
  const viewport = renderResult ? renderResult.backfillViewport : null;
  if (!viewport || !lane.messagesEl) return;
  const scroller = lane.messagesEl;
  const nextTop =
    scroller.scrollHeight - scroller.clientHeight - viewport.distanceFromBottom;
  setLaneScrollTopWithoutPaneIntent(lane, Math.max(0, nextTop));
}

// ---- entry point --------------------------------------------------------------------

// Replaces the legacy grid packer in the fingerprint-gated render path.
// visibleItems is newest-first (matches app.stream.js's existing
// convention); nodes are created/reused exactly as before, just parented
// under the persistent .mosaic-plane instead of directly under
// lane.messagesEl.
function mosaicRenderMessageStream(lane, visibleItems) {
  mosaicLaneReady(lane);
  // Unmeasurable host: touch nothing -- not the lattice, not the plane, not
  // a single style. The caller must not commit its render fingerprint, so
  // the content this render would have painted stays pending; the reveal
  // ResizeObserver transition re-schedules it against real geometry.
  if (!mosaicHostMeasurable(lane.messagesEl)) {
    lane.mosaicRenderDeferred = true;
    // The observer is the reveal hook: it must exist even when the very
    // first render defers, or the 0-to-real width transition would never
    // re-schedule this lane and it would stay blank until new content.
    mosaicSyncResizeObserver(lane);
    return { deferred: true, backfillViewport: null };
  }
  const revealing = Boolean(lane.mosaicRenderDeferred);
  lane.mosaicRenderDeferred = false;
  if (revealing) {
    // First-paint discipline board-wide: whatever the engine rendered (or
    // froze) while the lane was unmeasurable, no tween may start from that
    // baseline. Dropping the applied-position memo makes every card look
    // fresh to mosaicApplyCardPosition, and the NaN/null sentinels force
    // unconditional plane-transform and extent rewrites -- all of which the
    // snap option below applies with transitions suppressed in one flush.
    lane.mosaicAppliedByNode = new WeakMap();
    lane.mosaicPrevMaxRow = NaN;
    lane.mosaicPrevExtent = null;
  }
  // Geometry first: the plane is position:absolute, so creating it cannot
  // affect lane.messagesEl's own clientWidth, and mosaicPlane needs the
  // real M immediately if it has to anchor a fresh plane (see mosaicPlane).
  const geometry = mosaicGeometry(mosaicRootFontSizePx(), mosaicContainerWidthPx(lane.messagesEl));
  const geometryChanged = mosaicGeometryChanged(lane, geometry);
  lane.mosaicGeometry = geometry;
  const plane = mosaicPlane(lane, geometry);

  const entries = mosaicStreamEntries(visibleItems);
  const desiredKeys = entries.map((entry) => entry.key);
  const desiredKeySet = new Set(desiredKeys);
  const previousKeySet = new Set(lane.mosaicCards.map((card) => card.key));
  const existingNodes = mosaicExistingNodesByKey(lane);

  const nodesByKey = new Map();
  for (const entry of entries) {
    const node = mosaicEntryNode(lane, entry, existingNodes);
    if (node) nodesByKey.set(entry.key, node);
  }
  plane.replaceChildren(...entries.map((entry) => nodesByKey.get(entry.key)).filter(Boolean));

  // A node is "content dirty" only when renderOrReuseMessageNode/
  // mosaicRuleNode decided its fingerprint changed and built a FRESH
  // element for an ALREADY-KNOWN key -- reuse (same node reference) means
  // nothing changed, and a brand-new key is handled by insert/fullReplay
  // below, not the content-diff pass. Marking every render dirty here was
  // the earlier bug: it re-measured and re-placed every wet card on every
  // no-op render.
  for (const [key, node] of nodesByKey) {
    node.dataset.mosaicCard = "";
    const reused = existingNodes.get(key) === node;
    if (!reused && previousKeySet.has(key)) node.dataset.mosaicContentDirty = "1";
  }

  const addedKeys = desiredKeys.filter((key) => !previousKeySet.has(key));
  const removedKeys = [...previousKeySet].filter((key) => !desiredKeySet.has(key));
  const backfillEntries = mosaicBackfillEntries(entries, previousKeySet, addedKeys);
  const backfillViewport = backfillEntries ? mosaicCaptureBackfillViewport(lane) : null;
  let replaying = geometryChanged;

  if (geometryChanged) {
    mosaicRunFullReplay(lane, entries, nodesByKey, geometry);
  } else if (!addedKeys.length) {
    if (removedKeys.length) mosaicDropVacatedKeys(lane, desiredKeySet);
    mosaicRunContentDiffPass(lane, entries, nodesByKey, geometry);
  } else if (addedKeys.length === 1 && addedKeys[0] === desiredKeys[0]) {
    if (removedKeys.length) mosaicDropVacatedKeys(lane, desiredKeySet);
    const entry = entries.find((candidate) => candidate.key === addedKeys[0]);
    mosaicRunInsert(lane, entry, nodesByKey.get(entry.key), geometry);
    mosaicRunContentDiffPass(lane, entries, nodesByKey, geometry);
  } else if (backfillEntries) {
    if (removedKeys.length) mosaicDropVacatedKeys(lane, desiredKeySet);
    mosaicRunBackfill(lane, backfillEntries, nodesByKey, geometry);
    mosaicRunContentDiffPass(lane, entries, nodesByKey, geometry);
  } else {
    replaying = true;
    mosaicRunFullReplay(lane, entries, nodesByKey, geometry);
  }

  // Motion suppression unions three sources: reveal corrections, the §9
  // settled-board gate (nothing animates until first settle), and -- inside
  // the apply -- prefers-reduced-motion. Layout is identical either way.
  mosaicApplyRender(lane, lane.mosaicCards, geometry, nodesByKey, {
    replaying,
    snap: revealing || !lane.mosaicSettled,
  });
  mosaicSyncResizeObserver(lane);
  mosaicSyncImageLoadHandlers(lane);
  mosaicArmSettleInteraction(lane);
  mosaicSyncSettle(
    lane,
    geometryChanged ||
      addedKeys.length > 0 ||
      removedKeys.length > 0 ||
      Boolean(backfillEntries),
  );
  return { backfillViewport };
}

// ---- resize + image-load wiring (mirrors the legacy grid packer's resize
// observer pattern, retargeted at full replay instead of a repack) -----------------

function mosaicElementResizeSize(element) {
  const rect = element.getBoundingClientRect();
  return { height: rect.height, width: rect.width };
}

function mosaicHostResizeChanged(lane) {
  const next = mosaicElementResizeSize(lane.messagesEl);
  const previous = lane.mosaicHostResizeSize || null;
  lane.mosaicHostResizeSize = next;
  if (!previous) return false;
  return Math.abs(next.width - previous.width) > MOSAIC_RESIZE_WIDTH_EPSILON_PX;
}

function mosaicScheduleFullReplay(lane) {
  if (lane.mosaicFrame) return;
  lane.mosaicFrame = requestAnimationFrame(() => {
    lane.mosaicFrame = 0;
    if (lane.closed) return;
    lane.renderedMessageFingerprint = "";
    renderMessagesIfChanged(lane);
  });
}

function mosaicSyncResizeObserver(lane) {
  if (typeof ResizeObserver === "undefined" || !lane.messagesEl) return;
  if (!lane.mosaicResizeObserver) {
    lane.mosaicResizeObserver = new ResizeObserver(() => {
      if (!mosaicHostResizeChanged(lane)) return;
      mosaicScheduleFullReplay(lane);
    });
    lane.mosaicHostResizeSize = mosaicElementResizeSize(lane.messagesEl);
    lane.mosaicResizeObserver.observe(lane.messagesEl);
  }
}

function mosaicSyncImageLoadHandlers(lane) {
  if (!lane.messagesEl) return;
  if (!lane.mosaicImageLoadHandlers) lane.mosaicImageLoadHandlers = new Map();
  if (!lane.mosaicResolvedImages) lane.mosaicResolvedImages = new WeakSet();
  const current = new Set(
    lane.messagesEl.querySelectorAll("[data-mosaic-card] img"),
  );
  let resolvedCompleteImage = false;
  for (const image of current) {
    if (lane.mosaicImageLoadHandlers.has(image)) {
      if (image.complete && mosaicMarkImageResolved(lane, image))
        resolvedCompleteImage = true;
      continue;
    }
    const handler = () => {
      if (!mosaicMarkImageResolved(lane, image)) return;
      lane.renderedMessageFingerprint = "";
      renderMessagesIfChanged(lane);
    };
    image.addEventListener("load", handler);
    image.addEventListener("error", handler);
    lane.mosaicImageLoadHandlers.set(image, handler);
    if (image.complete && mosaicMarkImageResolved(lane, image))
      resolvedCompleteImage = true;
  }
  for (const [image, handler] of lane.mosaicImageLoadHandlers) {
    if (current.has(image)) continue;
    image.removeEventListener("load", handler);
    image.removeEventListener("error", handler);
    lane.mosaicImageLoadHandlers.delete(image);
  }
  if (resolvedCompleteImage) {
    lane.renderedMessageFingerprint = "";
    renderMessagesIfChanged(lane);
  }
}

function mosaicMarkImageResolved(lane, image) {
  if (!lane.mosaicResolvedImages) lane.mosaicResolvedImages = new WeakSet();
  if (lane.mosaicResolvedImages.has(image)) return false;
  const card = image.closest("[data-mosaic-card]");
  if (!card) return false;
  lane.mosaicResolvedImages.add(image);
  // The node is never replaced by a load/error event (the item's
  // fingerprint doesn't change), so data-mosaic-reservation-type would
  // otherwise stay stamped forever and mosaicPendingReservationType would
  // keep returning the reservation instead of ever measuring the real
  // (possibly text+image) content -- clear it so the content-diff pass this
  // triggers takes the real-measurement path. Already-complete images take
  // this same path because no future load event is guaranteed.
  delete card.dataset.mosaicReservationType;
  card.dataset.mosaicContentDirty = "1";
  return true;
}

function mosaicResetResizeObserver(lane) {
  if (lane.mosaicSettleTimer) {
    clearTimeout(lane.mosaicSettleTimer);
    lane.mosaicSettleTimer = 0;
  }
  if (lane.mosaicFrame) {
    cancelAnimationFrame(lane.mosaicFrame);
    lane.mosaicFrame = 0;
  }
  if (lane.mosaicResizeObserver) lane.mosaicResizeObserver.disconnect();
  lane.mosaicResizeObserver = null;
  lane.mosaicHostResizeSize = null;
  for (const [image, handler] of lane.mosaicImageLoadHandlers || []) {
    image.removeEventListener("load", handler);
    image.removeEventListener("error", handler);
  }
  lane.mosaicImageLoadHandlers = new Map();
  lane.mosaicResolvedImages = new WeakSet();
}
