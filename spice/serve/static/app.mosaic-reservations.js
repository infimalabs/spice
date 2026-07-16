// Mosaic pending-content reservations. Insert-time row priors
// for cards whose real content arrives after insertion, chosen so a later
// resolution is exact-or-shrinking in the common case (pad-in-place, free)
// and growth (a ripple) is the rare exception. Pure and DOM-free like
// the other mosaic-*.js modules: DOM measurement, the skeleton placeholder
// itself, and applying the pad/ripple outcome once resolved are
// rendering-path concerns and stay out of this module.

// Ack and referenced quotes render as `.ack-quote` blockquotes
// (messages.css), reader-facing content that shares the same rem type ramp
// as the message body -- not fixed UI chrome. The prior therefore
// resolves against the live root font size each geometry pass, exactly like
// mosaicGeometry resolves gap/minLane/module, rather than assuming a
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

// Image letterbox caps, rem-denominated like the rest of
// the geometry pass so a root font-size change rescales the slot
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
// resolve its prior against the live root font size (the geometry pass
// governs every prior here, quote and image alike). A type absent from this
// table has no reservation -- its measured height stands, unchanged from
// today: unknown types default to no reservation.
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
