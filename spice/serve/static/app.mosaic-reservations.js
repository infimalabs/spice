// Mosaic §4 pending-content reservations (spec .spice/mosaic-packing-spec.md
// §4 last paragraph, §12: imageHeights, ackReserve). Insert-time row priors
// for cards whose real content arrives after insertion, chosen so a later
// resolution is exact-or-shrinking in the common case (pad-in-place, free)
// and growth (a ripple, §7) is the rare exception. Pure and DOM-free like
// the other mosaic-*.js modules: DOM measurement, the skeleton placeholder
// itself, and applying the pad/ripple outcome once resolved are
// rendering-path/§7 concerns and stay out of this module.

// Ack and referenced quotes render as `.ack-quote` blockquotes
// (messages.css), reader-facing content that shares the same rem type ramp
// as the message body (§4) -- not fixed UI chrome. The prior therefore
// resolves against the live root font size each geometry pass, exactly like
// mosaicGeometry (§3) resolves gap/minLane/module, rather than assuming a
// fixed px height.
const MOSAIC_ACK_QUOTE_FONT_SIZE_REM = 0.8125; // .ack-quote font-size
const MOSAIC_ACK_QUOTE_LINE_HEIGHT_RATIO = 1.34; // .ack-quote line-height (unitless: already tracks font-size)
const MOSAIC_ACK_QUOTE_CHROME_REM = 1.25; // .ack-quotes margin (3+5) + .ack-quote padding-block (6+6), at the 16px default root
const MOSAIC_ACK_QUOTE_TYPICAL_LINES = 2; // the "standard type ramp" typical quote length

function mosaicQuoteReservePx(rootFontSizePx, lines) {
  const rem =
    Number.isFinite(rootFontSizePx) && rootFontSizePx > 0
      ? rootFontSizePx
      : 16;
  const typicalLines = Number.isFinite(lines)
    ? lines
    : MOSAIC_ACK_QUOTE_TYPICAL_LINES;
  const fontSizePx = MOSAIC_ACK_QUOTE_FONT_SIZE_REM * rem;
  const lineHeightPx = fontSizePx * MOSAIC_ACK_QUOTE_LINE_HEIGHT_RATIO;
  const chromePx = MOSAIC_ACK_QUOTE_CHROME_REM * rem;
  return chromePx + typicalLines * lineHeightPx;
}

// Image letterbox caps (§12 imageHeights), rem-denominated like the rest of
// §3's geometry pass so a root font-size change rescales the slot
// coherently through full replay -- messages.css's --mosaic-image-height /
// --mosaic-image-large-height custom properties must stay in sync with
// these. The rendered image is letterboxed to this exact cap regardless of
// its intrinsic size or load state (messages.css), so an image reservation
// is exact by construction and never depends on measurement.
const MOSAIC_IMAGE_HEIGHT_REM = 8.75; // --mosaic-image-height: 140px at the 16px default root
const MOSAIC_IMAGE_HEIGHT_LARGE_REM = 15.75; // --mosaic-image-large-height: 252px at the 16px default root

function mosaicImageReservePx(heightRem, rootFontSizePx) {
  const rem =
    Number.isFinite(rootFontSizePx) && rootFontSizePx > 0
      ? rootFontSizePx
      : 16;
  return heightRem * rem;
}

// One table, one place: every reservation-bearing card type and how to
// resolve its prior against the live root font size (§3's geometry pass
// governs every prior here, quote and image alike). A type absent from this
// table has no reservation -- its measured height stands, unchanged from
// today (§4 last paragraph: "unknown types default to no reservation").
const MOSAIC_RESERVATION_PRIORS = {
  ack: (rootFontSizePx) =>
    mosaicQuoteReservePx(rootFontSizePx, MOSAIC_ACK_QUOTE_TYPICAL_LINES),
  quote: (rootFontSizePx) =>
    mosaicQuoteReservePx(rootFontSizePx, MOSAIC_ACK_QUOTE_TYPICAL_LINES),
  image: (rootFontSizePx) =>
    mosaicImageReservePx(MOSAIC_IMAGE_HEIGHT_REM, rootFontSizePx),
  imageLarge: (rootFontSizePx) =>
    mosaicImageReservePx(MOSAIC_IMAGE_HEIGHT_LARGE_REM, rootFontSizePx),
};

function mosaicReservationPriorPx(type, rootFontSizePx) {
  const resolve = Object.prototype.hasOwnProperty.call(
    MOSAIC_RESERVATION_PRIORS,
    type,
  )
    ? MOSAIC_RESERVATION_PRIORS[type]
    : null;
  return resolve ? resolve(rootFontSizePx) : null;
}

// Reserved row count for a card type at insert time (§4), or null when the
// type has no named prior and the caller must measure instead.
function mosaicReservationRows(type, rootFontSizePx, gap, module) {
  const priorPx = mosaicReservationPriorPx(type, rootFontSizePx);
  return priorPx === null ? null : mosaicRowsFor(priorPx, gap, module);
}

// §7 resolution outcome: compares the reserved row count against the row
// count once real content is measured. "shrink" and "exact" both pad in
// place (zero movement, free, §7); only "growth" requires a ripple -- rare
// by construction when the prior is sized correctly (§4).
function mosaicReservationOutcome(reservedRows, resolvedRows) {
  const delta = resolvedRows - reservedRows;
  return {
    kind: delta > 0 ? "growth" : delta < 0 ? "shrink" : "exact",
    delta,
  };
}
