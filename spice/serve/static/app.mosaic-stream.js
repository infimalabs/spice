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
  lane.mosaicAppliedByKey = new Map();
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
  lane.mosaicAppliedByKey = new Map();
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

// ---- reservation classification (§4) ----------------------------------------------

// Two different lifetimes share this one lookup: an ack/quote reservation is
// genuinely pending -- real content hasn't hydrated yet, and shrink-on-
// resolve is the common case (§4) -- while an image reservation is
// permanent: images letterbox to their named cap regardless of load state
// (messages.css), so the reservation never needs to give way to a measured
// height, whether the image is loading, loaded, or broken. Both read the
// single reservation type app.render.js already computed once at build time
// (article.dataset.mosaicReservationType) rather than re-deriving it here --
// no reservation logic scattered per-call-site (§4). Barrier entries
// (dividers, rules) are never reserved -- their content is fixed at
// creation.
function mosaicPendingReservationType(lane, entry, node) {
  if (entry.kind === "barrier") return null;
  const item = entry.item;
  const pendingAck = (item.ack_keys || []).some((key) =>
    lane.missingAckContextKeys.has(key),
  );
  if (pendingAck) return "ack";
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
// lane's render-memo fields only (mosaicAppliedByKey/mosaicPrevMaxRow/
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
    const prev = lane.mosaicAppliedByKey.get(card.key) || null;
    const next = mosaicApplyCardPosition(
      node,
      card,
      prev,
      geometry.edges,
      geometry.M,
      geometry.gap,
      { reducedMotion },
    );
    lane.mosaicAppliedByKey.set(card.key, next);
  }
  for (const key of Array.from(lane.mosaicAppliedByKey.keys())) {
    if (!cards.some((card) => card.key === key)) lane.mosaicAppliedByKey.delete(key);
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
function mosaicRunContentDiffPass(lane, entries, nodesByKey, geometry) {
  let touchedWet = false;
  for (const entry of entries) {
    const card = lane.mosaicCards.find((candidate) => candidate.key === entry.key);
    if (!card) continue;
    const node = nodesByKey.get(entry.key);
    if (!node || !node.dataset.mosaicContentDirty) continue;
    delete node.dataset.mosaicContentDirty;
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
}

// ---- resize wiring (mirrors the legacy syncMessagePackObserver pattern,
// retargeted at full replay instead of a repack) --------------------------------------
//
// No image-load wiring here: images letterbox to a named cap regardless of
// load state (messages.css, mosaicPendingReservationType above), so a load
// or error event never changes a card's reserved size -- there is nothing
// for a load handler to schedule.

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

function mosaicResetResizeObserver(lane) {
  if (lane.mosaicFrame) {
    cancelAnimationFrame(lane.mosaicFrame);
    lane.mosaicFrame = 0;
  }
  if (lane.mosaicResizeObserver) lane.mosaicResizeObserver.disconnect();
  lane.mosaicResizeObserver = null;
  lane.mosaicHostResizeSize = null;
}
