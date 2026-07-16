const fs = require("fs");
const vm = require("vm");

const geometrySource = fs.readFileSync(process.argv[2], "utf8");
const sizingSource = fs.readFileSync(process.argv[3], "utf8");
const reservationsSource = fs.readFileSync(process.argv[4], "utf8");
const context = { console };
vm.createContext(context);
vm.runInContext(geometrySource, context, {
  filename: "app.mosaic-geometry.js",
});
vm.runInContext(sizingSource, context, { filename: "app.mosaic-sizing.js" });
vm.runInContext(reservationsSource, context, {
  filename: "app.mosaic-reservations.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function fixtureReservationRows(type, rootFontSizePx, gap, module) {
  const priorPx = context.mosaicReservationPriorPx(type, rootFontSizePx);
  return priorPx === null ? null : context.mosaicRowsFor(priorPx, gap, module);
}

function fixtureReservationOutcome(reservedRows, resolvedRows) {
  const delta = resolvedRows - reservedRows;
  return {
    kind: delta > 0 ? "growth" : delta < 0 ? "shrink" : "exact",
    delta,
  };
}

const DEFAULT_ROOT_FONT_PX = 16;
const RESCALED_ROOT_FONT_PX = 20;
const WIDE_CONTAINER_PX = 1200; // L>=3, the common wide-lane case

// Every reservation-bearing type is enumerated with a positive named prior;
// an unknown type has none ("unknown types default to no
// reservation").
for (const type of ["ack", "quote", "image", "imageLarge"]) {
  const priorPx = context.mosaicReservationPriorPx(type, DEFAULT_ROOT_FONT_PX);
  assert(
    Number.isFinite(priorPx) && priorPx > 0,
    "expected a positive named prior for type=" + type + ", got " + priorPx,
  );
}
assert(
  context.mosaicReservationPriorPx("bogus-type", DEFAULT_ROOT_FONT_PX) === null,
  "an unknown card type must have no reservation",
);
assert(
  fixtureReservationRows("bogus-type", DEFAULT_ROOT_FONT_PX, 12, 60) === null,
  "an unknown card type must have no reserved row count either",
);

// Image reservations are exact by construction: the
// reserved row count, derived purely from the letterbox cap constant, is
// what a resolved image -- always letterboxed to that same cap -- will also
// measure to. Sweep every module the card sizing lattice actually uses
// (3/2/1 lines at L>=3/2/1) plus a non-lattice module, and both the
// default and a rescaled root font size: image caps are rem-denominated
// so a root font-size change rescales the cap coherently, exactly
// like the quote priors below -- the letterboxed <img> rescales right along
// with it (messages.css), so exactness holds at any root font size.
for (const rootFontSizePx of [DEFAULT_ROOT_FONT_PX, RESCALED_ROOT_FONT_PX]) {
  for (const module of [60, 40, 20, 37]) {
    const gap = 12;
    for (const [type, heightRem] of [
      ["image", 8.75],
      ["imageLarge", 15.75],
    ]) {
      const reservedRows = fixtureReservationRows(
        type,
        rootFontSizePx,
        gap,
        module,
      );
      const resolvedRows = context.mosaicRowsFor(
        heightRem * rootFontSizePx,
        gap,
        module,
      );
      assert(
        reservedRows === resolvedRows,
        "image reservation must equal the resolved rows exactly: type=" +
          type +
          " rootFontSizePx=" +
          rootFontSizePx +
          " module=" +
          module +
          " reserved=" +
          reservedRows +
          " resolved=" +
          resolvedRows,
      );
    }
  }
}

// The ack/quote prior is reader-facing content sharing the message body's
// rem type ramp, not fixed UI chrome -- it must resolve against the
// live root font size the same way mosaicGeometry resolves gap/module,
// and must actually change when the root font changes (this is exactly the
// bug a hardcoded-px prior would have: silently drifting from the real
// `.ack-quote` render once a user rescales the root font).
{
  const expectedPriorPx = (rootFontSizePx) => {
    const fontSizePx = 0.8125 * rootFontSizePx;
    const lineHeightPx = fontSizePx * 1.34;
    const chromePx = 1.25 * rootFontSizePx;
    return chromePx + 2 * lineHeightPx;
  };

  const defaultPrior = context.mosaicReservationPriorPx(
    "ack",
    DEFAULT_ROOT_FONT_PX,
  );
  assert(
    Math.abs(defaultPrior - expectedPriorPx(DEFAULT_ROOT_FONT_PX)) < 1e-9,
    "ack prior must match the documented chrome+lines derivation at the default root, got " +
      defaultPrior,
  );

  const rescaledPrior = context.mosaicReservationPriorPx(
    "ack",
    RESCALED_ROOT_FONT_PX,
  );
  assert(
    Math.abs(rescaledPrior - expectedPriorPx(RESCALED_ROOT_FONT_PX)) < 1e-9,
    "ack prior must match the documented chrome+lines derivation at a rescaled root, got " +
      rescaledPrior,
  );
  assert(
    rescaledPrior > defaultPrior,
    "the ack prior must grow when the root font size grows, not stay fixed",
  );

  assert(
    context.mosaicReservationPriorPx("quote", DEFAULT_ROOT_FONT_PX) ===
      context.mosaicReservationPriorPx("ack", DEFAULT_ROOT_FONT_PX),
    "a referenced quote shares the ack quote's rendered chrome and so its prior",
  );
}

// Resolution outcome classification: shrink and exact both pad in place
// (no ripple); only growth needs one. This module only classifies the
// outcome -- applying a ripple is app.mosaic-wet-frozen.js's concern.
{
  assert(
    fixtureReservationOutcome(3, 2).kind === "shrink",
    "resolving smaller than reserved must classify as shrink",
  );
  assert(
    fixtureReservationOutcome(3, 3).kind === "exact",
    "resolving to exactly the reservation must classify as exact",
  );
  const growth = fixtureReservationOutcome(2, 3);
  assert(growth.kind === "growth", "resolving larger than reserved must classify as growth");
  assert(growth.delta === 1, "growth delta must be resolvedRows - reservedRows");
}

// Priors are empirically shrink-biased: across a realistic sample of
// pending ack-quote resolutions (short acknowledgments, 1-5 rendered lines
// -- true outliers run far longer but are rare by construction), the
// 2-line prior at the real L>=3 geometry never produces a growth ripple, at
// the default root font size or a rescaled one (gap and module resolved via
// the real mosaicGeometry, so the ratio between quote content and module
// stays realistic as the root font changes).
for (const rootFontSizePx of [DEFAULT_ROOT_FONT_PX, RESCALED_ROOT_FONT_PX]) {
  const geometry = context.mosaicGeometry(rootFontSizePx, WIDE_CONTAINER_PX);
  const { gap, M: module } = geometry;
  const reservedRows = fixtureReservationRows(
    "ack",
    rootFontSizePx,
    gap,
    module,
  );
  const fontSizePx = 0.8125 * rootFontSizePx;
  const lineHeightPx = fontSizePx * 1.34;
  const chromePx = 1.25 * rootFontSizePx;
  const sampleLineCounts = [1, 1, 2, 2, 1, 3, 2, 1, 4, 2, 1, 5];
  let growthCount = 0;
  for (const lines of sampleLineCounts) {
    const contentPx = chromePx + lines * lineHeightPx;
    const resolvedRows = context.mosaicRowsFor(contentPx, gap, module);
    const outcome = fixtureReservationOutcome(reservedRows, resolvedRows);
    if (outcome.kind === "growth") growthCount += 1;
  }
  assert(
    growthCount === 0,
    "expected zero growth ripples across the in-fixture pending-resolution sample at rootFontSizePx=" +
      rootFontSizePx +
      ", got " +
      growthCount,
  );
}

console.log("mosaic reservations: all assertions passed");
