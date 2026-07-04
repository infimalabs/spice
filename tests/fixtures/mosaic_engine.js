const fs = require("fs");
const vm = require("vm");

const source = fs.readFileSync(process.argv[2], "utf8");
const context = { console };
vm.createContext(context);
vm.runInContext(source, context, { filename: "app.mosaic-engine.js" });

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function deepEqual(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

const TRACKS = 12;
const LEGAL_BASE_SPANS = [2, 3, 4, 6, 12];
const TALL_ROW_COUNT_THRESHOLD = 4;

// mulberry32: deterministic seeded PRNG so "randomized" sequences replay
// identically across runs and across the byte-identical replay check below.
function mulberry32(seed) {
  let state = seed >>> 0;
  return function next() {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function pick(rng, list) {
  return list[Math.floor(rng() * list.length)];
}

// Mirrors the real candidate generation: baseSpan always, plus the wide
// tier only when tall enough -- but driven by a seeded RNG instead of DOM
// measurement, since measurement is a sizing concern out of scope here.
function randomCandidates(rng) {
  const baseSpan = pick(rng, LEGAL_BASE_SPANS);
  const baseN = 1 + Math.floor(rng() * 6);
  const candidates = [{ span: baseSpan, n: baseN }];
  const wideSpan = Math.min(baseSpan * 2, TRACKS);
  const isTall = baseN >= TALL_ROW_COUNT_THRESHOLD;
  if (wideSpan > baseSpan && isTall) {
    const wideN = Math.max(2, baseN - 1 - Math.floor(rng() * 2));
    candidates.push({ span: wideSpan, n: wideN });
  }
  return candidates;
}

function runSequence(seed, count) {
  const rng = mulberry32(seed);
  let cards = [];
  let rowFloor = context.mosaicDeriveRowFloor(cards, TRACKS);
  const snapshots = [];
  for (let step = 0; step < count; step += 1) {
    const candidates = randomCandidates(rng);
    const before = cards.map((card) => ({ ...card }));
    const result = context.mosaicInsert(cards, rowFloor, candidates, TRACKS);
    // Non-mutation: insertion must never touch any prior card.
    for (let index = 0; index < before.length; index += 1) {
      assert(
        deepEqual(before[index], result.cards[index]),
        "insertion mutated an existing card at step " + step,
      );
    }
    // Wide-placement property: a wide placement only wins if it
    // lands strictly lower than, or ties with no more waste than, the best
    // base placement.
    if (candidates.length > 1 && result.card.span === candidates[1].span) {
      const baseAnchor = context.mosaicAnchorFor(
        candidates[0].span,
        rowFloor,
        TRACKS,
      );
      const wideAnchor = context.mosaicAnchorFor(
        candidates[1].span,
        rowFloor,
        TRACKS,
      );
      const landsLower = wideAnchor.b < baseAnchor.b;
      const tiesWithNoMoreWaste =
        wideAnchor.b === baseAnchor.b && wideAnchor.waste <= baseAnchor.waste;
      assert(
        landsLower || tiesWithNoMoreWaste,
        "wide placement dug a hole at step " +
          step +
          ": " +
          JSON.stringify({ baseAnchor, wideAnchor }),
      );
    }
    cards = result.cards;
    rowFloor = result.rowFloor;
    snapshots.push(cards.map((card) => ({ ...card })));
  }
  return { cards, rowFloor, snapshots };
}

function assertNoOverlaps(cards, seed) {
  for (let i = 0; i < cards.length; i += 1) {
    for (let j = i + 1; j < cards.length; j += 1) {
      assert(
        !context.mosaicCardsOverlap(cards[i], cards[j]),
        "cards " + i + " and " + j + " overlap in both axes (seed " + seed + ")",
      );
    }
  }
}

for (const seed of [1, 2, 3, 4, 5, 42, 1337]) {
  const first = runSequence(seed, 40);
  assertNoOverlaps(first.cards, seed);

  // Determinism: replaying the identical sequence reproduces
  // byte-identical (t, b, n, span) for every card.
  const second = runSequence(seed, 40);
  assert(
    deepEqual(first.cards, second.cards),
    "replay diverged for seed " + seed,
  );
  assert(
    deepEqual(first.rowFloor, second.rowFloor),
    "replay rowFloor diverged for seed " + seed,
  );
}

// The first card anchors track 0, row 0, regardless of span.
{
  const rowFloor = context.mosaicDeriveRowFloor([], TRACKS);
  const result = context.mosaicInsert(
    [],
    rowFloor,
    [{ span: 4, n: 3 }],
    TRACKS,
  );
  assert(result.card.t === 0, "first card must anchor track 0");
  assert(result.card.b === 0, "first card must anchor row 0");
}

// A span request wider than 12 clamps to 12.
{
  const rowFloor = context.mosaicDeriveRowFloor([], TRACKS);
  const anchor = context.mosaicAnchorFor(15, rowFloor, TRACKS);
  assert(anchor.span === TRACKS, "oversized span must clamp to track count");
  assert(anchor.t === 0, "a full-width span has exactly one legal anchor");
}

// rowFloor derivation matches an independent hand-placed fixture.
{
  const cards = [
    { t: 0, span: 4, b: 0, n: 2 },
    { t: 4, span: 4, b: 0, n: 3 },
    { t: 8, span: 4, b: 0, n: 1 },
  ];
  const rowFloor = context.mosaicDeriveRowFloor(cards, TRACKS);
  const expected = [2, 2, 2, 2, 3, 3, 3, 3, 1, 1, 1, 1];
  assert(
    deepEqual(rowFloor, expected),
    "rowFloor derivation mismatch: " + JSON.stringify(rowFloor),
  );
}

// mosaicCommit never mutates its input rowFloor.
{
  const before = [0, 0, 0, 0];
  const before_copy = before.slice();
  const after = context.mosaicCommit(
    before,
    { t: 0, span: 2, b: 0, n: 3 },
    4,
  );
  assert(deepEqual(before, before_copy), "mosaicCommit mutated its input");
  assert(deepEqual(after, [3, 3, 0, 0]), "mosaicCommit advanced tracks wrong");
}

console.log("mosaic engine: all assertions passed");
