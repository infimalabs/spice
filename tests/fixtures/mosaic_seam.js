const fs = require("fs");
const vm = require("vm");

const geometrySource = fs.readFileSync(process.argv[2], "utf8");
const renderSource = fs.readFileSync(process.argv[3], "utf8");
const context = { console };
vm.createContext(context);
vm.runInContext(geometrySource, context, {
  filename: "app.mosaic-geometry.js",
});
vm.runInContext(renderSource, context, { filename: "app.mosaic-render.js" });

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const TRACK_COUNT = 12;
const LEGAL_SPANS = [2, 3, 4, 6, TRACK_COUNT];

// Seam rule: for every adjacent (leftSpan, rightSpan) pair that
// fits side by side on the 12-track grid, the left card's right edge plus
// gap must equal the right card's left edge EXACTLY -- both must derive
// from the same shared integer edges[] table, never from independent
// per-card colW multiplication (which would only diverge at fractional
// colW, hence the odd widths below).
function assertSeamsHoldAt(rootFontSizePx, containerWidthPx) {
  const geometry = context.mosaicGeometry(rootFontSizePx, containerWidthPx);
  const { edges, gap } = geometry;

  assert(
    edges.every((edge) => Number.isInteger(edge)),
    "edges[] must be all-integer at width " + containerWidthPx + ": " + JSON.stringify(edges),
  );

  for (const leftSpan of LEGAL_SPANS) {
    for (const rightSpan of LEGAL_SPANS) {
      if (leftSpan + rightSpan > TRACK_COUNT) continue;
      const left = { t: 0, span: leftSpan };
      const right = { t: leftSpan, span: rightSpan };
      const leftRightEdge =
        context.mosaicCardX(left, edges) + context.mosaicCardWidth(left, edges, gap) + gap;
      const rightLeftEdge = context.mosaicCardX(right, edges);
      assert(
        leftRightEdge === rightLeftEdge,
        "seam mismatch at width " +
          containerWidthPx +
          " for spans " +
          leftSpan +
          "/" +
          rightSpan +
          ": left-right+gap=" +
          leftRightEdge +
          " right-left=" +
          rightLeftEdge,
      );
    }
  }
}

// 1281 and 977 are the odd widths named in the task: at root 16px, gap
// rounds to 12px, so colW = (width - 11*gap)/12 lands on a non-integer
// (95.75 and 70.41666... respectively) -- exactly the fractional-colW
// regime where per-card rounding (colW * span, rounded independently per
// card) would produce a boundary off by up to a pixel, unlike the shared
// edges[] table used here.
const TEST_WIDTHS = [1281, 977, 960, 1200, 640, 375, 1920];
const TEST_ROOTS = [16, 18, 20];

assert(
  ((1281 - 11 * 12) / 12) % 1 !== 0,
  "1281 must produce a fractional colW at root 16px (test setup assumption)",
);
assert(
  ((977 - 11 * 12) / 12) % 1 !== 0,
  "977 must produce a fractional colW at root 16px (test setup assumption)",
);

for (const rootFontSizePx of TEST_ROOTS) {
  for (const containerWidthPx of TEST_WIDTHS) {
    assertSeamsHoldAt(rootFontSizePx, containerWidthPx);
  }
}

console.log("mosaic seam rule: all assertions passed");
