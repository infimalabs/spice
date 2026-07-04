const fs = require("fs");
const vm = require("vm");

const engineSource = fs.readFileSync(process.argv[2], "utf8");
const wetFrozenSource = fs.readFileSync(process.argv[3], "utf8");
const fullReplaySource = fs.readFileSync(process.argv[4], "utf8");
const context = { console };
vm.createContext(context);
vm.runInContext(engineSource, context, { filename: "app.mosaic-engine.js" });
vm.runInContext(wetFrozenSource, context, {
  filename: "app.mosaic-wet-frozen.js",
});
vm.runInContext(fullReplaySource, context, {
  filename: "app.mosaic-full-replay.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function deepEqual(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

const TRACKS = 12;

function card(creationIndex, candidates, extra) {
  return Object.assign(
    { t: 0, span: 0, b: 0, n: 0, creationIndex, frozen: false, candidates },
    extra,
  );
}

// --- creation-order re-decision against a fresh rowFloor ----------------

// Two base-span-4 cards: replay must place them side by side (leftmost
// first) at the shared floor, in creationIndex order -- exactly the same
// outcome insert() would give from empty, regardless of the array's input
// order.
{
  const a = card(0, [{ span: 4, n: 2 }]);
  const b = card(1, [{ span: 4, n: 3 }]);

  const replayed = context.mosaicFullReplay([b, a], TRACKS, 2);
  const byIndex = new Map(replayed.map((c) => [c.creationIndex, c]));

  assert(byIndex.get(0).t === 0, "first-created card must anchor t=0, got " + byIndex.get(0).t);
  assert(byIndex.get(0).b === 0, "first-created card must land at b=0");
  assert(byIndex.get(1).t === 4, "second-created card must anchor next to the first, got " + byIndex.get(1).t);
  assert(byIndex.get(1).b === 0, "second-created card must land at b=0 alongside the first");
}

// --- history independence ------------------------------------------------

// Same cards (creationIndex + candidates) but wildly different prior
// t/b/span/n/frozen "history" must converge to the identical replayed
// layout -- full replay is a pure function of (creation order, content,
// trackCount) alone, never of what came before.
{
  const candidatesA = [{ span: 4, n: 2 }];
  const candidatesB = [{ span: 4, n: 3 }];
  const candidatesC = [{ span: 12, n: 2 }];

  const historyOne = [
    card(0, candidatesA, { t: 9, b: 40, span: 3, n: 9, frozen: true }),
    card(1, candidatesB, { t: 1, b: 0, span: 1, n: 1, frozen: false }),
    card(2, candidatesC, { t: 5, b: 5, span: 5, n: 5, frozen: true }),
  ];
  const historyTwo = [
    card(0, candidatesA, { t: 0, b: 0, span: 0, n: 0, frozen: false }),
    card(1, candidatesB, { t: 11, b: 200, span: 1, n: 1, frozen: true }),
    card(2, candidatesC, { t: 2, b: 2, span: 2, n: 2, frozen: false }),
  ];

  const replayedOne = context.mosaicFullReplay(historyOne, TRACKS, 2);
  const replayedTwo = context.mosaicFullReplay(historyTwo, TRACKS, 2);

  const strip = (cards) =>
    cards
      .slice()
      .sort((x, y) => x.creationIndex - y.creationIndex)
      .map((c) => ({ t: c.t, b: c.b, n: c.n, span: c.span, frozen: c.frozen }));

  assert(
    deepEqual(strip(replayedOne), strip(replayedTwo)),
    "full replay must be independent of prior layout/freeze history",
  );
}

// --- idempotence ----------------------------------------------------------

// Replaying twice with unchanged candidates reproduces a byte-identical
// layout -- an explicit "compact" control would do
// nothing, because this is already true.
{
  const cards = [
    card(0, [{ span: 6, n: 3 }]),
    card(1, [{ span: 4, n: 2 }]),
    card(2, [{ span: 4, n: 4 }]),
    card(3, [{ span: 12, n: 2 }]),
  ];

  const first = context.mosaicFullReplay(cards, TRACKS, 2);
  const second = context.mosaicFullReplay(first, TRACKS, 2);

  assert(deepEqual(first, second), "replaying twice with no change must be byte-identical");
}

// --- freeze latches are cleared and recomputed, not preserved --------------

// All three candidates are span-12 (full width), so anchorFor has only one
// legal anchor (t=0) and the cards are forced to stack in the same tracks
// rather than spread out -- D (index 0) ends up covered by both E and F
// (depth 2 -> frozen), E is covered only by F (depth 1 -> wet). Seed the
// OPPOSITE frozen flags before replay (D wet, E frozen) to prove recompute
// overrides stale flags rather than trusting them.
{
  const d = card(0, [{ span: 12, n: 2 }], { frozen: false });
  const e = card(1, [{ span: 12, n: 2 }], { frozen: true });
  const f = card(2, [{ span: 12, n: 2 }], { frozen: false });

  const replayed = context.mosaicFullReplay([d, e, f], TRACKS, 2);
  const byIndex = new Map(replayed.map((c) => [c.creationIndex, c]));

  assert(byIndex.get(0).frozen === true, "depth-2 card must freeze on replay even though its stale flag was false");
  assert(byIndex.get(1).frozen === false, "depth-1 card must stay wet on replay even though its stale flag was true");
}

console.log("mosaic full replay: all assertions passed");
