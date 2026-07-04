const fs = require("fs");
const vm = require("vm");

const source = fs.readFileSync(process.argv[2], "utf8");
const context = { console };
vm.createContext(context);
vm.runInContext(source, context, { filename: "app.mosaic-geometry.js" });

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const ROOT = 16;
const GRID_TRACK_COUNT = 12;
const EDGE_COUNT = GRID_TRACK_COUNT + 1;

function laneCountAt(widthPx) {
  return context.mosaicGeometry(ROOT, widthPx).L;
}

// Capacity-rule thresholds at a 16px root (gap=12px, minLane=272px); the
// spec allows ±1px slop at the boundary itself, so assert a couple of
// pixels off each threshold to stay clear of that and of float rounding.
assert(laneCountAt(554) === 1, "554px should still be a single lane");
assert(laneCountAt(558) === 2, "558px should have crossed into 2 lanes");
assert(laneCountAt(838) === 2, "838px should still be 2 lanes");
assert(laneCountAt(842) === 3, "842px should have crossed into 3 lanes");
assert(laneCountAt(1122) === 3, "1122px should still be 3 lanes");
assert(laneCountAt(1126) === 4, "1126px should have crossed into 4 lanes");
assert(laneCountAt(1690) === 4, "1690px should still be 4 lanes");
assert(
  laneCountAt(1694) === 6,
  "1694px should have crossed into 6 lanes (5 lanes cannot occur)",
);

for (const width of [200, 554, 558, 838, 842, 1122, 1126, 1690, 1694, 3000]) {
  const geometry = context.mosaicGeometry(ROOT, width);
  assert(
    [1, 2, 3, 4, 6].includes(geometry.L),
    "L must be a legal lane count at width " + width,
  );
  assert(
    geometry.baseSpan * geometry.L === GRID_TRACK_COUNT,
    "baseSpan and L must partition the 12-track grid at width " + width,
  );
  assert(geometry.edges.length === EDGE_COUNT, "edges must cover tracks 0..12");
  for (let index = 1; index < geometry.edges.length; index += 1) {
    assert(
      geometry.edges[index] >= geometry.edges[index - 1],
      "edges must be non-decreasing at width " + width,
    );
  }
  const expectedLines = geometry.L >= 3 ? 3 : geometry.L === 2 ? 2 : 1;
  assert(
    geometry.lines === expectedLines,
    "module line schedule must match the lane count at width " + width,
  );
}

// Module height and gap re-resolve against the live root font size rather
// than a cached value.
const smallRoot = context.mosaicGeometry(14, 1200);
const largeRoot = context.mosaicGeometry(20, 1200);
assert(
  smallRoot.M < largeRoot.M,
  "module height must grow with the root font size",
);
assert(smallRoot.gap < largeRoot.gap, "gap must scale with the root font size");
assert(
  smallRoot.minLane < largeRoot.minLane,
  "minLane must scale with the root font size",
);

// No per-column divisor: a capacity walk over Math.floor(width / X) must
// not exist as actual code in the geometry pass (prose comments describing
// the anti-pattern are fine and expected).
assert(
  !/Math\.floor\(/.test(source),
  "geometry pass must not encode a per-column divisor",
);

// Degenerate/zero-width input still returns the safe single-lane baseline.
const empty = context.mosaicGeometry(16, 0);
assert(empty.L === 1, "zero width must degrade to a single lane");
assert(
  empty.baseSpan === GRID_TRACK_COUNT,
  "zero width must degrade to baseSpan 12",
);

console.log("mosaic geometry: all assertions passed");
