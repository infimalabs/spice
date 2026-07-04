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
