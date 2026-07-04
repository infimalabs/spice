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

// §7 rippleRows: when the growing card must extend downward, every card
// resting below it in an overlapping track slides down by the same delta,
// transitively. Breadth is over the spec's set definition -- {d : d
// overlaps m in tracks and d.b < m.b} -- not insertion order; a card is
// discovered once (never re-shifted) using whichever frontier card reaches
// it first, and once discovered its OWN b becomes the threshold for
// reaching further cards below it, so the shift cascades. This mirrors
// .spice/mosaic-demo.html's rippleRows() bookkeeping exactly, including
// using each already-shifted descendant's new b (not its pre-shift b) once
// it becomes a frontier card itself. Only b moves; t and span never change
// (§7, §11). Row indices may go negative (§14) -- this never clamps or
// re-normalizes them. Returns a NEW array in the input's original order;
// cards whose b did not change keep their original object reference (no-op
// for a shallow-equality render diff, §9(c)); never mutates its input or
// any input card object.
function mosaicRippleRows(cards, growingCreationIndex, delta) {
  const growingCard = cards.find(
    (card) => card.creationIndex === growingCreationIndex,
  );
  if (!growingCard) {
    throw new Error(
      "mosaicRippleRows: no card with creationIndex " + growingCreationIndex,
    );
  }

  const shiftedB = new Map();
  const seen = new Set([growingCreationIndex]);
  const stack = [growingCard];
  while (stack.length) {
    const frontier = stack.pop();
    const frontierB =
      frontier.creationIndex === growingCreationIndex
        ? growingCard.b
        : shiftedB.get(frontier.creationIndex);
    for (const candidate of cards) {
      if (seen.has(candidate.creationIndex)) continue;
      if (mosaicTracksOverlap(candidate, frontier) && candidate.b < frontierB) {
        shiftedB.set(candidate.creationIndex, candidate.b - delta);
        seen.add(candidate.creationIndex);
        stack.push(candidate);
      }
    }
  }

  return cards.map((card) => {
    if (card.creationIndex === growingCreationIndex) {
      return { ...card, b: card.b - delta, n: card.n + delta };
    }
    if (shiftedB.has(card.creationIndex)) {
      return { ...card, b: shiftedB.get(card.creationIndex) };
    }
    return card;
  });
}

// §7 frozen resize dispatch: late content on a frozen card either grows
// past its reservation (rare by design) or resolves smaller. Growth keeps
// the card's top row fixed (b+n invariant) and ripples the chain below it
// downward via mosaicRippleRows; the card's own n and b both change to
// reflect the new row count. Shrinking keeps the reserved n exactly as-is
// -- zero movement outranks reclaiming a row (§7) -- so this returns
// `cards` completely untouched (same array, same object references),
// which is what makes "zero style writes on any other card in that frame"
// true by construction. The vacated space reconciles at the next full
// replay (§8), never here.
function mosaicResolveFrozenResize(cards, creationIndex, newN) {
  const card = cards.find((candidate) => candidate.creationIndex === creationIndex);
  if (!card) {
    throw new Error("mosaicResolveFrozenResize: no card with creationIndex " + creationIndex);
  }
  if (newN <= card.n) return cards;
  return mosaicRippleRows(cards, creationIndex, newN - card.n);
}
