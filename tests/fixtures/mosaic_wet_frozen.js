const fs = require("fs");
const vm = require("vm");

const engineSource = fs.readFileSync(process.argv[2], "utf8");
const wetFrozenSource = fs.readFileSync(process.argv[3], "utf8");
const context = { console };
vm.createContext(context);
vm.runInContext(engineSource, context, { filename: "app.mosaic-engine.js" });
vm.runInContext(wetFrozenSource, context, {
  filename: "app.mosaic-wet-frozen.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const TRACKS = 12;

function card(creationIndex, t, span, n, frozen, b = 0) {
  return { t, span, n, b, creationIndex, frozen: Boolean(frozen) };
}

// --- burial depth + freeze latch -------------------------------------

// A (tracks 0-3) is covered by B (2-5, index 1) and C (2-5, index 2) on its
// overlapping tracks 2-3, and by nothing on tracks 0-1 -- so its minimum
// cover count across ALL its tracks (0,1,2,3) is 0 (tracks 0-1 have no
// later cover), not 2. Burial depth takes the MIN across the card's own
// span, so a partially-buried card is not buried.
{
  const a = card(0, 0, 4, 2);
  const b = card(1, 2, 4, 2);
  const c = card(2, 2, 4, 2);
  const depth = context.mosaicBurialDepth(a, [a, b, c]);
  assert(depth === 0, "partial cover across a's span should floor depth at 0, got " + depth);
}

// D (tracks 0-3) fully covered on every track by E and F (both later,
// indices 1 and 2, both spanning 0-3) -- depth 2, freezes at freezeDepth=2.
{
  const d = card(0, 0, 4, 2);
  const e = card(1, 0, 4, 2);
  const f = card(2, 0, 4, 2);
  const depth = context.mosaicBurialDepth(d, [d, e, f]);
  assert(depth === 2, "fully double-covered card should have depth 2, got " + depth);

  const oneLayer = context.mosaicRecomputeFrozen([d, e], 2);
  assert(oneLayer[0].frozen === false, "depth 1 (one later cover) must not freeze");

  const twoLayers = context.mosaicRecomputeFrozen([d, e, f], 2);
  assert(twoLayers[0].frozen === true, "depth 2 must freeze at freezeDepth=2");
  assert(twoLayers[1].frozen === false, "e (nothing later covers it) must stay wet");
}

// Freeze is a latch: once frozen, recompute leaves it alone even though a
// literal burial-depth recompute would now read low (simulating "later
// cards were removed" -- recomputeFrozen itself never re-derives an
// already-frozen card, by construction).
{
  const already = card(0, 0, 4, 2, true);
  const alone = context.mosaicRecomputeFrozen([already], 2);
  assert(alone[0].frozen === true, "an already-frozen card must never unfreeze via recompute");
  assert(alone[0] === already, "an already-frozen card must be returned unchanged (no new object)");
}

// --- wetReplay: floor from frozen only, spans latch --------------------

{
  const frozenCard = card(0, 0, 4, 3, true); // occupies rows [0,3) on tracks 0-3
  const wetLow = card(1, 4, 4, 2, false); // creationIndex 1, tracks 4-7
  const wetHigh = card(2, 8, 4, 2, false); // creationIndex 2, tracks 8-11
  const before = JSON.stringify(frozenCard);

  const replayed = context.mosaicWetReplay([frozenCard, wetLow, wetHigh], TRACKS, 2);
  const [replayedFrozen, replayedLow, replayedHigh] = replayed;

  assert(JSON.stringify(replayedFrozen) === before, "frozen card must be untouched by wetReplay");
  assert(replayedLow.span === wetLow.span, "wetReplay must never change a card's span");
  assert(replayedHigh.span === wetHigh.span, "wetReplay must never change a card's span");
}

// Sharper floor-source discriminator: full-width (span 12) cards force a
// SINGLE possible anchor (t=0), so the only free variable is b. A frozen
// card sets the true floor at 2; a wet card carries an enormous STALE b
// from before this replay. If wetReplay's floor were (wrongly) derived
// from all cards instead of frozen-only, the wet card's own stale b=90
// would poison its own floor and it would re-place at b=93 instead of
// the correct b=2 -- this fails if `frozenCards` is ever widened to
// include wet cards.
{
  const frozenFull = card(0, 0, 12, 2, true); // tracks 0-11, rows [0,2)
  const staleWet = card(1, 0, 12, 3, false, 90); // tracks 0-11, STALE rows [90,93)

  const replayed = context.mosaicWetReplay([frozenFull, staleWet], TRACKS, 2);
  const placedWet = replayed[1];

  assert(placedWet.t === 0, "span-12 card has only one legal anchor: t=0");
  assert(
    placedWet.b === 2,
    "floor must come from the frozen card (top=2) only, not the wet card's own stale position; got b=" +
      placedWet.b,
  );
}

// Two wet cards resolving in either arrival order produce the same final
// (t, b) per creationIndex -- order invariance (§14).
{
  const frozenBase = card(0, 0, 12, 2, true);
  const wetA = card(1, 0, 4, 2, false);
  const wetB = card(2, 4, 4, 3, false);

  const orderOne = context.mosaicWetReplay([frozenBase, wetA, wetB], TRACKS, 2);
  const orderTwo = context.mosaicWetReplay([frozenBase, wetB, wetA], TRACKS, 2);

  const byIndex = (cards) => {
    const map = new Map();
    for (const c of cards) map.set(c.creationIndex, { t: c.t, b: c.b, n: c.n, span: c.span });
    return map;
  };
  const mapOne = byIndex(orderOne);
  const mapTwo = byIndex(orderTwo);
  for (const index of [1, 2]) {
    assert(
      JSON.stringify(mapOne.get(index)) === JSON.stringify(mapTwo.get(index)),
      "card " + index + " must place identically regardless of arrival order",
    );
  }
}

// freezeDepth default matches the named constant when omitted.
{
  const d = card(0, 0, 4, 2);
  const e = card(1, 0, 4, 2);
  const f = card(2, 0, 4, 2);
  const withDefault = context.mosaicRecomputeFrozen([d, e, f]);
  assert(withDefault[0].frozen === true, "default freezeDepth must equal MOSAIC_FREEZE_DEPTH (2)");
  // MOSAIC_FREEZE_DEPTH is a top-level `const`, so (matching real browser
  // <script> semantics, verified separately) it is NOT a property of the vm
  // context object -- it lives in the shared script-scope lexical
  // environment instead, readable only via a follow-up expression
  // evaluation in the same context. This mirrors how a future browser
  // page.evaluate(() => MOSAIC_FREEZE_DEPTH) would read it too.
  const freezeDepthConst = vm.runInContext("MOSAIC_FREEZE_DEPTH", context);
  assert(freezeDepthConst === 2, "MOSAIC_FREEZE_DEPTH must be the named constant 2");
}

console.log("mosaic wet/frozen: all assertions passed");
