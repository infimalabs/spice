// Mosaic full replay. Pure function built on the engine's decide/commit primitives
// (app.mosaic-engine.js) and the freeze latch (app.mosaic-wet-frozen.js):
// clears every freeze latch, re-decides each surviving card's span/anchor
// in creation order against a fresh rowFloor, then recomputes freeze
// latches from the new positions. DOM-free and side-effect-free like its
// dependencies; recomputing geometry, re-measuring content, animating with
// FLIP, and mapping resize/agent-removal events to this call are
// mosaic-stream-integration concerns and stay out of this module. Card
// records add `candidates` (this pass's pre-measured { span, n } options,
// (sizing and span policy) -- content may have been re-measured since the card was first
// placed) to the creationIndex/frozen shape from app.mosaic-wet-frozen.js.

// Recompute geometry (caller's concern), clear freeze latches, and for
// every surviving card in creation order re-run decide() against a fresh
// rowFloor. Because the procedure reads only creationIndex and candidates
// from each card -- never its prior t/b/span/n/frozen -- the outcome is a
// pure function of (creation order, content, trackCount) alone, identical
// regardless of layout or freeze history and idempotent when
// replayed again with unchanged candidates. Returns a new array in the
// input's original order; never mutates its input.
function mosaicFullReplay(cards, trackCount, freezeDepth) {
  const tracks = mosaicTrackCount(trackCount);
  const ordered = cards
    .slice()
    .sort((a, b) => a.creationIndex - b.creationIndex);

  let rowFloor = new Array(tracks).fill(0);
  const decidedByIndex = new Map();
  for (const card of ordered) {
    const placement = mosaicDecide(card.candidates, rowFloor, tracks);
    const placed = {
      ...card,
      span: placement.span,
      n: placement.n,
      b: placement.b,
      t: placement.t,
      frozen: false,
    };
    rowFloor = mosaicCommit(rowFloor, placed, tracks);
    decidedByIndex.set(card.creationIndex, placed);
  }

  const replayed = cards.map((card) => decidedByIndex.get(card.creationIndex));
  return mosaicRecomputeFrozen(replayed, freezeDepth);
}

// Reconcile one structural epoch while retaining every card outside it
// byte-for-byte. `replacedKeys` names the old cards owned by the epoch;
// `cards` is the epoch's complete desired card set after the structural
// change. Creation indexes on those desired cards place them strictly
// between the surrounding stable epochs. The older outside cards seed the
// skyline, then only the desired epoch is re-decided. Freeze recomputation
// is observed for the replayed cards alone: an interleaved arrival must not
// mutate even the bookkeeping fields of an outside card.
function mosaicEpochReplay(
  previousCards,
  replacedKeys,
  cards,
  trackCount,
  freezeDepth,
) {
  const tracks = mosaicTrackCount(trackCount);
  const replaced = new Set(replacedKeys || []);
  const outside = previousCards.filter((card) => !replaced.has(card.key));
  const ordered = cards
    .slice()
    .sort((a, b) => a.creationIndex - b.creationIndex);
  const firstCreationIndex = ordered.length
    ? ordered[0].creationIndex
    : Infinity;
  const olderOutside = outside.filter(
    (card) => card.creationIndex < firstCreationIndex,
  );

  let rowFloor = mosaicDeriveRowFloor(olderOutside, tracks);
  const decidedByIndex = new Map();
  for (const card of ordered) {
    const placement = mosaicDecide(card.candidates, rowFloor, tracks);
    const placed = {
      ...card,
      span: placement.span,
      n: placement.n,
      b: placement.b,
      t: placement.t,
      frozen: false,
    };
    rowFloor = mosaicCommit(rowFloor, placed, tracks);
    decidedByIndex.set(card.creationIndex, placed);
  }

  const replayed = cards.map((card) => decidedByIndex.get(card.creationIndex));
  const combined = outside.concat(replayed);
  const recomputed = mosaicRecomputeFrozen(combined, freezeDepth);
  const replayedKeys = new Set(replayed.map((card) => card.key));
  return combined.map((card, index) =>
    replayedKeys.has(card.key) ? recomputed[index] : card,
  );
}
