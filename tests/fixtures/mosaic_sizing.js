const fs = require("fs");
const vm = require("vm");

const source = fs.readFileSync(process.argv[2], "utf8");
const context = { console };
vm.createContext(context);
vm.runInContext(source, context, { filename: "app.mosaic-sizing.js" });

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const EXPECTED_ROWS_FOR_1000PX = 17; // ceil((1000+12)/60)
const EXPECTED_CARD_HEIGHT_N3 = 168; // 3*60 - 12
const SLACK_SWEEP_MAX_CONTENT_PX = 400;

// rowsFor formula and the 2-row minimum.
assert(context.mosaicRowsFor(0, 12, 60) === 2, "near-empty content must floor at 2 rows");
assert(
  context.mosaicRowsFor(1000, 12, 60) === EXPECTED_ROWS_FOR_1000PX,
  "rowsFor must ceil-divide by the module",
);
assert(
  context.mosaicRowsFor(48, 12, 60) === 1 || context.mosaicRowsFor(48, 12, 60) === 2,
  "rowsFor must never go below the 2-row floor",
);
assert(context.mosaicRowsFor(48, 12, 60) === 2, "sub-module content still floors at 2 rows");

// Rendered height/width formulas.
assert(
  context.mosaicCardHeightPx(3, 60, 12) === EXPECTED_CARD_HEIGHT_N3,
  "cardHeightPx = n*M - gap",
);
const edges = [0, 59, 118, 177, 236, 295, 354, 413, 472, 531, 590, 649, 708];
assert(
  context.mosaicCardWidthPx(edges, 0, 4, 12) === edges[4] - edges[0] - 12,
  "cardWidthPx must read from the shared edges table",
);

// Interior slack bounded below the module: property check across a
// spread of realistic content heights (px is the whole card's natural
// height, chrome included -- messages.css chrome alone is ~68px at the
// default root, so real cards never sit near the synthetic near-empty
// range where the n>=2 floor -- not the ceil division -- would dominate
// and legitimately push slack past a module).
for (const module of [20, 40, 60]) {
  for (
    let contentPx = 60;
    contentPx <= SLACK_SWEEP_MAX_CONTENT_PX;
    contentPx += 7
  ) {
    const gap = 12;
    const n = context.mosaicRowsFor(contentPx, gap, module);
    const slack = context.mosaicInteriorSlackPx(n, module, gap, contentPx);
    assert(slack >= 0, "slack must never be negative: " + JSON.stringify({ module, contentPx, n, slack }));
    assert(
      slack < module,
      "slack must never reach a full module: " + JSON.stringify({ module, contentPx, n, slack }),
    );
  }
}

// The designed interlock: chrome + gap = 4*lineHeight at the default
// type ramp (messages.css: border 0.0625rem*2 + padding 0.5625rem*2 +
// footer margin 0.5rem + footer height 2.5rem = 4.25rem; +gap 0.75rem =
// 5rem = 4*1.25rem). At 16px root: chrome=68px, gap=12px, lineHeight=20px.
// lines(L) = 3 at L>=3 (M=60), 2 at L=2 (M=40), 1 at L=1 (M=20) -- k whole
// lines of body text land at contentPx = chrome + k*lineHeight.
{
  const chromePx = 68;
  const gapPx = 12;
  const lineHeightPx = 20;
  const moduleByLines = { 3: 60, 2: 40, 1: 20 };
  const zeroSlackPeriod = { 3: 3, 2: 2, 1: 1 };
  // (chrome+gap)/lineHeight = 4, so contentPx+gap = (4+k)*lineHeight;
  // zero slack exactly when (4+k) is divisible by lines(L).
  for (const lines of [3, 2, 1]) {
    const module = moduleByLines[lines];
    for (let k = 1; k <= 6; k += 1) {
      const contentPx = chromePx + k * lineHeightPx;
      const n = context.mosaicRowsFor(contentPx, gapPx, module);
      const slack = context.mosaicInteriorSlackPx(n, module, gapPx, contentPx);
      const expectZero = (4 + k) % zeroSlackPeriod[lines] === 0;
      if (expectZero) {
        assert(
          slack === 0,
          "expected zero slack at lines=" + lines + " k=" + k + " but got " + slack,
        );
      } else {
        assert(
          slack > 0 && slack % lineHeightPx === 0,
          "expected a whole-line slack remainder at lines=" +
            lines + " k=" + k + " but got " + slack,
        );
      }
      assert(slack < module, "slack must stay below the module at lines=" + lines + " k=" + k);
    }
  }
}

console.log("mosaic sizing: all assertions passed");
