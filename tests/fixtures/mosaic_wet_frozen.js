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

function fixtureCardsOverlap(a, b) {
  const tracksOverlap = a.t < b.t + b.span && b.t < a.t + a.span;
  const rowsOverlap = a.b < b.b + b.n && b.b < a.b + a.n;
  return tracksOverlap && rowsOverlap;
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
  // "Touches only wet cards" is a stronger claim than "values happen
  // to match" -- the frozen card must be the SAME object, never even
  // reconstructed, let alone re-decided.
  assert(replayedFrozen === frozenCard, "frozen card must be the same object reference, proving wetReplay never touched it");
  assert(replayedLow.span === wetLow.span, "wetReplay must never change a card's span");
  assert(replayedHigh.span === wetHigh.span, "wetReplay must never change a card's span");
}

// The wet tail stays bounded by freezeDepth regardless of total board
// size. Build a straight vertical stack of N cards (each resting directly
// on the one before, single track so every card buries every earlier one)
// and recompute freeze latches after each append, mirroring how
// mosaicRunInsert really drives this over a live stream. Only the last
// freezeDepth cards can still be wet; wetReplay must return every other
// (frozen) card by the same object reference.
{
  const freezeDepth = 2;
  const boardSize = 50;
  let cards = [];
  for (let i = 0; i < boardSize; i += 1) {
    cards = cards.concat([card(i, 0, 12, 1, false, i)]);
    cards = context.mosaicRecomputeFrozen(cards, freezeDepth);
  }
  const wetCountBefore = cards.filter((c) => !c.frozen).length;
  assert(
    wetCountBefore <= freezeDepth,
    "wet tail must stay bounded by freezeDepth (" + freezeDepth + ") regardless of board size (" +
      boardSize + "), got " + wetCountBefore,
  );

  const replayed = context.mosaicWetReplay(cards, TRACKS, freezeDepth);
  let untouchedFrozenCount = 0;
  for (let i = 0; i < cards.length; i += 1) {
    if (cards[i].frozen) {
      assert(
        replayed[i] === cards[i],
        "frozen card at index " + i + " must be untouched (same reference) in a " + boardSize + "-card board",
      );
      untouchedFrozenCount += 1;
    }
  }
  assert(
    untouchedFrozenCount === boardSize - wetCountBefore,
    "every frozen card in the large board must come back untouched, got " +
      untouchedFrozenCount + " of " + (boardSize - wetCountBefore),
  );
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
// (t, b) per creationIndex -- order invariance.
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

// --- rippleRows: frozen growth ----------------------------------------

// A grows by delta=2 (b+n invariant at the top edge); B rests directly
// below A in overlapping tracks (b < A.b) and must shift down by delta; C
// shares no tracks with A and must not move; D overlaps A's tracks but
// sits ABOVE A (b > A.b) and must not move either.
{
  const a = card(0, 0, 4, 3, true, 10); // tracks 0-3, rows [10,13)
  const b = card(1, 2, 4, 2, true, 5); // tracks 2-5 (overlaps a), rows [5,7)
  const c = card(2, 8, 4, 2, true, 5); // tracks 8-11 (disjoint from a)
  const d = card(3, 0, 4, 2, true, 20); // tracks 0-3 (overlaps a), ABOVE a

  const topBefore = a.b + a.n;
  const replayed = context.mosaicRippleRows([a, b, c, d], 0, 2);
  const [ra, rb, rc, rd] = replayed;

  assert(ra.b + ra.n === topBefore, "growing card's top row (b+n) must be invariant, got " + (ra.b + ra.n));
  assert(ra.n === a.n + 2, "growing card's n must increase by delta");
  assert(ra.b === a.b - 2, "growing card's b must decrease by delta to hold the top row");
  assert(ra.t === a.t && ra.span === a.span, "ripple must never change t or span");

  assert(rb.b === b.b - 2, "a card resting below the grower in overlapping tracks must shift down by delta, got " + rb.b);
  assert(rb.t === b.t && rb.n === b.n && rb.span === b.span, "ripple only moves b, nothing else");

  assert(rc === c, "a card sharing no tracks with the grower must be untouched (same reference)");
  assert(rd === d, "a card above the grower (b > grower.b) must not move even if tracks overlap");
}

// Transitive chain: A grows, B rests below A, C rests below B in tracks
// that do NOT overlap A at all -- C is only reachable via B, discovered
// using B's ORIGINAL (not shifted) b as the frontier threshold for that
// second hop.
{
  const a = card(0, 0, 4, 2, true, 10); // tracks 0-3
  const b = card(1, 2, 4, 2, true, 5); // tracks 2-5 (overlaps a on 2,3), below a
  const c = card(2, 4, 4, 2, true, -3); // tracks 4-7 (overlaps b on 4,5; disjoint from a's 0-3)
  const delta = 4;

  const replayed = context.mosaicRippleRows([a, b, c], 0, delta);
  const [ra, rb, rc] = replayed;
  assert(ra.b === a.b - delta, "root grower shifts by exactly delta (as its own top-row bookkeeping)");
  assert(rb.b === b.b - delta, "direct child shifts by delta");
  assert(rc.b === c.b - delta, "transitive grandchild (reachable only via the shifted child) shifts by delta too, got " + rc.b);
}

// Regression: a staggered multi-hop chain (E reachable only via D, not
// directly via A) with delta LARGER than the original gap between D and E
// (gap=2, delta=3). Discovering E via D's shifted b (8-3=5) instead of D's
// original b (8) would wrongly require E.b(6) < 5, which is false, so E
// would be left behind -- overlapping D after the ripple. Using D's
// original b (8) correctly finds E.b(6) < 8.
{
  const a = card(0, 0, 4, 2, true, 10); // tracks 0-3, rows [10,12)
  const d = card(1, 2, 4, 2, true, 8); // tracks 2-5 (overlaps a), rows [8,10) touching a's bottom
  const e = card(2, 4, 4, 2, true, 6); // tracks 4-7 (overlaps d only, not a), rows [6,8) touching d's bottom
  const delta = 3; // > the original gap between d and e (8-6=2)

  const replayed = context.mosaicRippleRows([a, d, e], 0, delta);
  const [ra, rd, re] = replayed;

  assert(rd.b === d.b - delta, "direct child (d) shifts by delta, got " + rd.b);
  assert(
    re.b === e.b - delta,
    "grandchild (e) reachable only via d must still shift by the full delta even though delta exceeds the original d/e gap, got " +
      re.b,
  );
  for (let i = 0; i < replayed.length; i += 1) {
    for (let j = i + 1; j < replayed.length; j += 1) {
      assert(
        !fixtureCardsOverlap(replayed[i], replayed[j]),
        "no two replayed cards may overlap in both axes after a ripple, cards " + i + " and " + j,
      );
    }
  }
}

// A deep stack with a large delta drives b negative; nothing clamps it.
{
  const deep = [card(0, 0, 12, 2, true, 3)];
  for (let i = 1; i <= 5; i += 1) deep.push(card(i, 0, 12, 1, true, 3 - i));
  const replayed = context.mosaicRippleRows(deep, 0, 50);
  const bottom = replayed[replayed.length - 1];
  assert(bottom.b < 0, "a large enough delta over a deep stack must be able to drive b negative, got " + bottom.b);
  assert(bottom.b === deep[deep.length - 1].b - 50, "the negative result must still be exactly original b minus delta");
}

// --- frozen resize dispatch: growth ripples, shrink pads in place ------

{
  const a = card(0, 0, 4, 3, true, 10);
  const below = card(1, 0, 4, 2, true, 5);

  const grown = context.mosaicResolveFrozenResize([a, below], 0, 5); // n: 3 -> 5, delta 2
  assert(grown[0].n === 5, "growth path must adopt the new row count");
  assert(grown[0].b === a.b - 2, "growth path must ripple via the same top-row-invariant bookkeeping");
  assert(grown[1].b === below.b - 2, "growth path must cascade the ripple to cards below");

  const shrunkSame = context.mosaicResolveFrozenResize([a, below], 0, 3); // n unchanged
  assert(shrunkSame[0] === a && shrunkSame[1] === below, "no-op resize (newN === n) must return cards untouched, same references");

  const shrunk = context.mosaicResolveFrozenResize([a, below], 0, 1); // newN < n: pad, don't shrink
  assert(shrunk[0] === a, "shrink must keep the reserved n and return the SAME object reference (pad-in-place, zero style writes)");
  assert(shrunk[0].n === a.n, "shrink must never actually reduce the reserved row count");
  assert(shrunk[1] === below, "shrink must leave every other card untouched too");
}

console.log("mosaic wet/frozen: all assertions passed");
