// Mosaic stream integration (spec .spice/mosaic-packing-spec.md §1, §6, §7,
// §8, §13). Wires the pure mosaic-*.js modules into the fingerprint-gated
// render path (app.stream.js renderMessagesIfChanged): persistent per-card
// lattice state keyed by message identity, replacing the legacy
// packMessageStream whole-board repack. This module owns the DOM-facing
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

// ---- lane lattice state ----------------------------------------------------------

function mosaicLaneReady(lane) {
  if (lane.mosaicCards) return;
  lane.mosaicCards = [];
  lane.mosaicNextCreationIndex = 0;
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
  if (lane.mosaicPlaneEl && lane.mosaicPlaneEl.isConnected) return lane.mosaicPlaneEl;
  const plane = document.createElement("div");
  plane.className = "mosaic-plane";
  lane.messagesEl.prepend(plane);
  lane.mosaicPlaneEl = plane;
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

// ---- entries: messages, compaction dividers, and derived time-rule markers -------

function mosaicItemIsBarrier(item) {
  return item.kind === "compaction";
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

// ---- pending-content reservation classification (§4) ------------------------------

// "Pending" means real content has not resolved yet: an ack/quote whose
// context text has not hydrated, or an image that has not finished
// loading. Named priors size these so shrink-on-resolve is the common
// case (§4); anything else measures live like normal. Barrier entries
// (dividers, rules) are never pending -- their content is fixed at
// creation.
// app.render.js already computes the correct reservation type once, at
// node-creation time (messageReservationType: "ack" while any ack_segments
// key has no resolved context yet via ackContextForKey, "imageLarge" for
// image_only items, a "image" fallback for any other card containing
// .message-image) and stamps it onto the node as data-mosaic-reservation-
// type. Re-deriving that classification here duplicated it with a
// DIFFERENT (and wrong) check -- lane.missingAckContextKeys only tracks
// confirmed-missing keys, not "not yet resolved", so a genuinely-pending
// ack (fetched but not yet answered) fell through to real measurement
// instead of reserving. Reading the dataset directly trusts the single
// source of truth instead of maintaining a second, divergent one. A fresh
// node built once its content resolves has no reservation type at all
// (messageReservationType returns "" once ackContextForKey finds a
// context), so this naturally falls through to real measurement on
// resolution with no extra branching needed here.
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
  const scrollOptions = mosaicPlaneScrollOptions(lane, undefined);
  const planeOptions = {
    ...scrollOptions,
    reducedMotion,
    replaying: Boolean(options && options.replaying),
  };
  lane.mosaicPrevMaxRow = mosaicSyncPlane(
    lane.mosaicPlaneEl,
    extent.maxRow,
    lane.mosaicPrevMaxRow,
    geometry.M,
    planeOptions,
  );
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
      { reducedMotion },
    );
    lane.mosaicAppliedByNode.set(node, next);
  }
  lane.mosaicPrevExtent = mosaicSyncHostHeight(
    lane.mosaicPlaneEl,
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
  const card = { ...placement.card, key: entry.key, creationIndex, frozen: false };
  lane.mosaicCards = lane.mosaicCards.concat([card]);
  lane.mosaicCards = mosaicRecomputeFrozen(lane.mosaicCards, MOSAIC_FREEZE_DEPTH);
}

function mosaicDropVacatedKeys(lane, desiredKeySet) {
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

function mosaicRunFullReplay(lane, entries, nodesByKey, geometry) {
  const withCandidates = entries.map((entry) => {
    const previous = lane.mosaicCards.find((card) => card.key === entry.key);
    const node = nodesByKey.get(entry.key);
    return {
      key: entry.key,
      creationIndex: mosaicCreationIndexFor(lane, entry.key),
      frozen: false,
      candidates: mosaicCandidatesFor(lane, entry, node, geometry),
      t: previous ? previous.t : 0,
      b: previous ? previous.b : 0,
      n: previous ? previous.n : 0,
      span: previous ? previous.span : geometry.baseSpan,
    };
  });
  const replayed = mosaicFullReplay(withCandidates, mosaicGridTrackCount, MOSAIC_FREEZE_DEPTH);
  lane.mosaicCards = replayed.map(({ candidates, ...card }) => card);
}

// ---- entry point --------------------------------------------------------------------

// Replaces packMessageStream(lane) in the fingerprint-gated render path.
// visibleItems is newest-first (matches app.stream.js's existing
// convention); nodes are created/reused exactly as before, just parented
// under the persistent .mosaic-plane instead of directly under
// lane.messagesEl.
function mosaicRenderMessageStream(lane, visibleItems) {
  mosaicLaneReady(lane);
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
  } else {
    mosaicRunFullReplay(lane, entries, nodesByKey, geometry);
  }

  mosaicApplyRender(lane, lane.mosaicCards, geometry, nodesByKey, {
    replaying: geometryChanged || (addedKeys.length > 1),
  });
  mosaicSyncResizeObserver(lane);
  mosaicSyncImageLoadHandlers(lane);
}

// ---- resize + image-load wiring (mirrors the legacy syncMessagePackObserver
// pattern, retargeted at full replay instead of a repack) ---------------------------

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
  const current = new Set(
    lane.messagesEl.querySelectorAll("[data-mosaic-card] img"),
  );
  for (const image of current) {
    if (lane.mosaicImageLoadHandlers.has(image)) continue;
    const handler = () => {
      const card = image.closest("[data-mosaic-card]");
      if (card) {
        // The node is never replaced by a load/error event (the item's
        // fingerprint doesn't change), so data-mosaic-reservation-type
        // would otherwise stay stamped forever and mosaicPendingReservationType
        // would keep returning the reservation instead of ever measuring
        // the real (possibly text+image) content -- clear it so the
        // content-diff pass this triggers takes the real-measurement path.
        delete card.dataset.mosaicReservationType;
        card.dataset.mosaicContentDirty = "1";
      }
      lane.renderedMessageFingerprint = "";
      renderMessagesIfChanged(lane);
    };
    image.addEventListener("load", handler);
    image.addEventListener("error", handler);
    lane.mosaicImageLoadHandlers.set(image, handler);
  }
  for (const [image, handler] of lane.mosaicImageLoadHandlers) {
    if (current.has(image)) continue;
    image.removeEventListener("load", handler);
    image.removeEventListener("error", handler);
    lane.mosaicImageLoadHandlers.delete(image);
  }
}

function mosaicResetResizeObserver(lane) {
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
}
