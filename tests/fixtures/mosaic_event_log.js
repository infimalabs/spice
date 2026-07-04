const fs = require("fs");
const vm = require("vm");

const engineSource = fs.readFileSync(process.argv[2], "utf8");
const wetFrozenSource = fs.readFileSync(process.argv[3], "utf8");
const fullReplaySource = fs.readFileSync(process.argv[4], "utf8");
const eventLogSource = fs.readFileSync(process.argv[5], "utf8");
const context = { console };
vm.createContext(context);
vm.runInContext(engineSource, context, { filename: "app.mosaic-engine.js" });
vm.runInContext(wetFrozenSource, context, {
  filename: "app.mosaic-wet-frozen.js",
});
vm.runInContext(fullReplaySource, context, {
  filename: "app.mosaic-full-replay.js",
});
vm.runInContext(eventLogSource, context, {
  filename: "app.mosaic-event-log.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function deepEqual(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

const TRACKS = 12;
const FORBIDDEN_LAYOUT_READS = [
  /\bDate\s*\./,
  /\bDate\s*\(/,
  /\bMath\.random\s*\(/,
  /\bperformance\.now\s*\(/,
];

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

function candidatesFor(rng) {
  const baseSpan = pick(rng, [2, 3, 4, 6, 12]);
  const baseN = 1 + Math.floor(rng() * 6);
  const candidates = [{ span: baseSpan, n: baseN }];
  const wideSpan = Math.min(baseSpan * 2, TRACKS);
  if (wideSpan > baseSpan && baseN >= 4) {
    candidates.push({ span: wideSpan, n: Math.max(2, baseN - 1) });
  }
  return candidates;
}

function recordInserts(seed, count) {
  const rng = mulberry32(seed);
  const log = context.mosaicCreateEventLog(TRACKS, 2);
  for (let i = 0; i < count; i += 1) {
    context.mosaicRecordEventLogEvent(log, {
      type: "insert",
      key: "card-" + i,
      creationIndex: i,
      candidates: candidatesFor(rng),
    });
  }
  return log;
}

function fullReplayEvent(seed, count) {
  const rng = mulberry32(seed);
  return {
    type: "full-replay",
    trackCount: TRACKS,
    cards: Array.from({ length: count }, (_, i) => ({
      key: "card-" + i,
      creationIndex: i,
      candidates: candidatesFor(rng),
    })),
  };
}

function assertNoOverlaps(cards, label) {
  for (let i = 0; i < cards.length; i += 1) {
    for (let j = i + 1; j < cards.length; j += 1) {
      assert(
        !context.mosaicCardsOverlap(cards[i], cards[j]),
        label + " cards overlap: " + i + " / " + j,
      );
    }
  }
}

// Event-log replay is deterministic over seeded insertion sequences.
for (const seed of [3, 5, 8, 13, 21]) {
  const first = context.mosaicReplayEventLog(recordInserts(seed, 36));
  const second = context.mosaicReplayEventLog(recordInserts(seed, 36));
  assert(deepEqual(first.layout, second.layout), "insert replay diverged for seed " + seed);
  assert(deepEqual(first.rowFloor, second.rowFloor), "rowFloor diverged for seed " + seed);
  assertNoOverlaps(first.cards, "seed " + seed);
}

// Content resolution events replay through the wet/frozen dispatch and remain
// deterministic when replayed from the same log.
{
  const log = recordInserts(34, 9);
  context.mosaicRecordEventLogEvent(log, {
    type: "content-resize",
    key: "card-1",
    creationIndex: 1,
    n: 8,
  });
  context.mosaicRecordEventLogEvent(log, {
    type: "remove",
    keys: ["card-0", "card-4"],
  });
  context.mosaicRecordEventLogEvent(log, fullReplayEvent(55, 7));
  const first = context.mosaicReplayEventLog(log);
  const second = context.mosaicReplayEventLog(log);
  assert(deepEqual(first.layout, second.layout), "mixed event log replay diverged");
  assert(first.layout.length === 7, "full replay should define the surviving card set");
}

// History independence: divergent prehistories converge when the same
// full-replay event is applied, because only creation order + candidates +
// geometry are load-bearing.
for (const seed of [89, 144, 233]) {
  const replay = fullReplayEvent(seed, 18);
  const left = recordInserts(seed + 1, 8);
  const right = recordInserts(seed + 2, 14);
  context.mosaicRecordEventLogEvent(left, replay);
  context.mosaicRecordEventLogEvent(right, replay);
  const leftLayout = context.mosaicReplayEventLog(left).layout;
  const rightLayout = context.mosaicReplayEventLog(right).layout;
  assert(deepEqual(leftLayout, rightLayout), "history independence failed for seed " + seed);
}

// Replay idempotence over seeded final states: appending the same full replay
// event twice does not change the byte-identical layout.
for (const seed of [377, 610, 987]) {
  const log = recordInserts(seed, 20);
  const replay = fullReplayEvent(seed + 10, 20);
  context.mosaicRecordEventLogEvent(log, replay);
  const once = context.mosaicReplayEventLog(log).layout;
  context.mosaicRecordEventLogEvent(log, replay);
  const twice = context.mosaicReplayEventLog(log).layout;
  assert(deepEqual(once, twice), "full replay idempotence failed for seed " + seed);
}

// Layout-path source audit: the pure lattice/replay path must not read ambient
// time or entropy. Seeded fixture generation above is intentionally outside
// these shipped modules.
for (const [name, source] of [
  ["app.mosaic-engine.js", engineSource],
  ["app.mosaic-wet-frozen.js", wetFrozenSource],
  ["app.mosaic-full-replay.js", fullReplaySource],
  ["app.mosaic-event-log.js", eventLogSource],
]) {
  for (const pattern of FORBIDDEN_LAYOUT_READS) {
    assert(!pattern.test(source), name + " contains forbidden ambient read " + pattern);
  }
}

console.log("mosaic event log: all assertions passed");
