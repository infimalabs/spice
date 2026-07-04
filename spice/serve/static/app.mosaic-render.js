// Mosaic render plane (spec .spice/mosaic-packing-spec.md §9, §11(a)(c), §12).
// Motion is split into two layers that must never share a transition: the
// uniform whole-board shift lives on a single plane element; differential
// per-card settling/ripple lives on the cards as individual transform
// tweens. Card positions are anchored to the fixed high-water constant K, so
// a card whose (t, b, n, span) did not change since the last apply receives
// no style write at all in that frame (§11c) -- that is the load-bearing
// invariant this module exists to preserve; it is stronger than "the
// computed value happens to be the same" and must be enforced by skipping
// the write, not by relying on idempotence.
//
// This module is DOM-facing (transforms, transitions, one forced reflow for
// the first-paint rule) but does not read the message stream, the card
// lattice engine, or any layout-trigger machinery -- it only turns already-
// decided (t, b, n, span) records into styles. Wiring it into the live
// render path is a separate concern (mosaic-stream-integration).

const MOSAIC_PLANE_K = 4096;
const MOSAIC_TWEEN_TRANSITION =
  "transform 340ms cubic-bezier(.25,.8,.3,1)";

// Plane-space y for a card's top edge, anchored to K so unchanged cards
// never recompute to a different value merely because maxRow rose (§9).
function mosaicCardY(card, M) {
  return (MOSAIC_PLANE_K - (card.b + card.n)) * M;
}

function mosaicCardX(card, edges) {
  return edges[card.t];
}

function mosaicCardWidth(card, edges, gap) {
  return edges[card.t + card.span] - edges[card.t] - gap;
}

function mosaicCardHeight(card, M, gap) {
  return card.n * M - gap;
}

function mosaicCardTransform(card, edges, M) {
  return `translate(${mosaicCardX(card, edges)}px, ${mosaicCardY(card, M)}px)`;
}

// Uniform plane translation: the single component that ever moves the whole
// board. maxRow is the tallest covered row across all cards.
function mosaicPlaneY(maxRow, M) {
  return -(MOSAIC_PLANE_K - maxRow) * M;
}

function mosaicPlaneTransform(maxRow, M) {
  return `translateY(${mosaicPlaneY(maxRow, M)}px)`;
}

// maxRow/minB across the current card set; host height is (maxRow-minB)*M,
// absorbing negative rows via minB (§9).
function mosaicCardsExtent(cards) {
  let maxRow = 0;
  let minB = 0;
  for (const card of cards) {
    const top = card.b + card.n;
    if (top > maxRow) maxRow = top;
    if (card.b < minB) minB = card.b;
  }
  return { maxRow, minB };
}

function mosaicHostHeight(maxRow, minB, M) {
  return (maxRow - minB) * M;
}

function mosaicCardPositionChanged(card, prev) {
  return (
    !prev ||
    prev.t !== card.t ||
    prev.b !== card.b ||
    prev.n !== card.n ||
    prev.span !== card.span
  );
}

// Writes transform+width for one card element iff (t, b, n, span) changed
// since `prev` (the last record this module returned for that element, or
// null/undefined for a card never positioned before). Returns the record to
// remember next time -- unchanged (same reference as `prev`) when no write
// occurred, so callers can tell "skipped" from "wrote" without re-deriving
// the comparison. A fresh card (no prev) gets its first transform applied
// with transitions suppressed and one forced reflow (§9 first-paint rule)
// so it never tweens in from the identity matrix.
function mosaicApplyCardPosition(el, card, prev, edges, M, gap, options) {
  if (!mosaicCardPositionChanged(card, prev)) return prev;
  const transform = mosaicCardTransform(card, edges, M);
  const width = `${mosaicCardWidth(card, edges, gap)}px`;
  const height = `${mosaicCardHeight(card, M, gap)}px`;
  const reducedMotion = Boolean(options && options.reducedMotion);
  if (!prev || reducedMotion) {
    el.style.transition = "none";
    el.style.transform = transform;
    el.style.width = width;
    el.style.height = height;
    void el.offsetWidth;
    el.style.transition = reducedMotion ? "none" : "";
  } else {
    el.style.transform = transform;
    el.style.width = width;
    el.style.height = height;
  }
  return { t: card.t, b: card.b, n: card.n, span: card.span };
}

// Applies the uniform plane component. Returns the maxRow to remember as
// `prevMaxRow` next call. No-ops (no style write) when maxRow is unchanged,
// mirroring the card-side skip invariant for the plane itself.
//
// options.scrolled + options.onCompensate implement the plane behavior rule
// (§9): scrolled down-page, the jump is applied with transitions disabled
// and the viewport is compensated by the same delta in the same frame, so
// the reader never sees the shift; at the top (or during full replay) the
// plane animates normally so the push-down is the visible signal.
function mosaicSyncPlane(planeEl, maxRow, prevMaxRow, M, options) {
  if (maxRow === prevMaxRow) return prevMaxRow;
  const transform = mosaicPlaneTransform(maxRow, M);
  const reducedMotion = Boolean(options && options.reducedMotion);
  const replaying = Boolean(options && options.replaying);
  const scrolled = Boolean(options && options.scrolled);
  if (reducedMotion) {
    planeEl.style.transition = "none";
    planeEl.style.transform = transform;
  } else if (scrolled && !replaying) {
    planeEl.style.transition = "none";
    planeEl.style.transform = transform;
    void planeEl.offsetWidth;
    if (options && typeof options.onCompensate === "function") {
      options.onCompensate((maxRow - prevMaxRow) * M);
    }
  } else {
    planeEl.style.transition = MOSAIC_TWEEN_TRANSITION;
    planeEl.style.transform = transform;
  }
  return maxRow;
}

// Sets the host's explicit height so scroll extent matches the lattice
// (§9). No-ops when the extent has not changed since `prevExtent`.
function mosaicSyncHostHeight(hostEl, maxRow, minB, M, prevExtent) {
  if (prevExtent && prevExtent.maxRow === maxRow && prevExtent.minB === minB) {
    return prevExtent;
  }
  hostEl.style.height = `${mosaicHostHeight(maxRow, minB, M)}px`;
  return { maxRow, minB };
}
