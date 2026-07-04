// Mosaic §7 wet tail / frozen prefix (spec .spice/mosaic-packing-spec.md §7,
// §11, §12 freezeDepth, §14). Pure functions built on the engine's
// rowFloor/anchor/commit primitives (app.mosaic-engine.js): burial depth,
// the freeze latch, and wetReplay's keep-span settling for the still-wet
// band. DOM-free and side-effect-free like the engine; mapping stream
// deltas to insert/wetReplay/ripple/fullReplay events is a
// mosaic-stream-integration concern and stays out of this module. Card
// records here add `creationIndex` (the stream's stable (epoch, index,
// key) order, per §14 -- never DOM order or arrival race order) and
// `frozen` to the engine's { t, span, b, n }.

const MOSAIC_FREEZE_DEPTH = 2;

// Burial depth of card c: the minimum, across c's covered tracks, of how
// many OTHER cards with a strictly later creationIndex also cover that
// track. A card nothing has landed after yet has depth 0 on every track it
// covers, so overall depth 0 -- never frozen.
function mosaicBurialDepth(card, cards) {
  let depth = Infinity;
  for (let track = card.t; track < card.t + card.span; track += 1) {
    let coverCount = 0;
    for (const other of cards) {
      if (other.creationIndex <= card.creationIndex) continue;
      if (track >= other.t && track < other.t + other.span) coverCount += 1;
    }
    if (coverCount < depth) depth = coverCount;
  }
  return depth;
}

// §7: freezing is a latch -- already-frozen cards are left untouched here;
// only a full replay resets frozen back to false. Returns a NEW array;
// never mutates its input or any card object in place.
function mosaicRecomputeFrozen(cards, freezeDepth) {
  const depth = Number.isFinite(freezeDepth) ? freezeDepth : MOSAIC_FREEZE_DEPTH;
  return cards.map((card) => {
    if (card.frozen) return card;
    const burial = mosaicBurialDepth(card, cards);
    return burial >= depth ? { ...card, frozen: true } : card;
  });
}

// §7 placeKeepSpan: re-anchor a card at its LATCHED span (never re-chosen —
// wetReplay settles position, not width) against the current rowFloor,
// committing the placement into rowFloor. Returns { card, rowFloor }; never
// mutates its inputs.
function mosaicPlaceKeepSpan(card, rowFloor, trackCount) {
  const anchor = mosaicAnchorFor(card.span, rowFloor, trackCount);
  const placed = { ...card, t: anchor.t, b: anchor.b };
  return { card: placed, rowFloor: mosaicCommit(rowFloor, placed, trackCount) };
}

// §7 wetReplay: derive the floor from FROZEN cards only, then re-place each
// wet card in creation order at its latched span -- spans never change
// here, only (t, b) settle. Frozen cards are untouched. Order-invariant to
// the input array's order (only creationIndex governs processing order),
// so resolving two pending cards in either arrival order produces the same
// final layout (§14). Returns a new array in the input's original order;
// never mutates its input.
function mosaicWetReplay(cards, trackCount, freezeDepth) {
  const frozenCards = cards.filter((card) => card.frozen);
  const wetCards = cards
    .filter((card) => !card.frozen)
    .slice()
    .sort((a, b) => a.creationIndex - b.creationIndex);

  let rowFloor = mosaicDeriveRowFloor(frozenCards, trackCount);
  const placedByIndex = new Map();
  for (const card of wetCards) {
    const result = mosaicPlaceKeepSpan(card, rowFloor, trackCount);
    rowFloor = result.rowFloor;
    placedByIndex.set(card.creationIndex, result.card);
  }

  const replaced = cards.map((card) =>
    card.frozen ? card : placedByIndex.get(card.creationIndex),
  );
  return mosaicRecomputeFrozen(replaced, freezeDepth);
}
