// Mosaic §5 candidate-span generation (spec .spice/mosaic-packing-spec.md §5,
// §12 tallThreshold/wideFactor, §13). No card type is special: every card
// evaluates baseSpan, and the wide tier only when ITS OWN measured height at
// base width clears tallThreshold -- never by class, kind, or content type.
// This is an explicit break from the deleted legacy grid packer's
// track-span-by-class heuristic, which granted finals 4-6 tracks,
// media-rich/image-only 4, and acked 3 by class; those type preferences have
// no successor here and must never be ported back in as biases, priors, or
// thresholds-by-class.
//
// Pure and DOM-free like the engine this builds on (app.mosaic-engine.js) and
// the sizing module (app.mosaic-sizing.js): `measure(span)` is an injected
// (span -> natural content height in px) callback, so the actual DOM read
// stays a rendering-path concern (mosaic-stream-integration) and this module
// only decides WHICH widths get read at all, and how many. mosaicDecide
// (§5's placement key) arbitrates the winner from the returned candidates;
// widening here is only ever offered as a candidate, never chosen as policy.
const MOSAIC_TALL_THRESHOLD_PX = 230;
const MOSAIC_WIDE_FACTOR = 2;

// §5/§12 wideFactor: the wide tier a card could occupy, capped at the track
// count. Equal to baseSpan (not a real widening) when baseSpan already
// covers the full width -- callers use that to skip the wide measurement
// entirely, since there is nothing wider to measure.
function mosaicWideSpan(baseSpan, trackCount) {
  return Math.min(
    baseSpan * MOSAIC_WIDE_FACTOR,
    mosaicTrackCount(trackCount),
  );
}

// §5/§13: build the candidate {span, n} list mosaicDecide arbitrates over.
// Calls measure(baseSpan) always, and measure(wideSpan) only when baseSpan
// already has somewhere wider to go AND the base measurement clears
// tallThreshold -- at most two calls total, matching §13's "at most two
// measurement reads per insertion". The gate reads only the measured pixel
// height, never anything about the card itself.
function mosaicCandidates(baseSpan, trackCount, gap, module, measure, tallThreshold) {
  const threshold = Number.isFinite(tallThreshold)
    ? tallThreshold
    : MOSAIC_TALL_THRESHOLD_PX;
  const wideSpan = mosaicWideSpan(baseSpan, trackCount);
  const basePx = measure(baseSpan);
  const candidates = [
    { span: baseSpan, n: mosaicRowsFor(basePx, gap, module) },
  ];
  if (wideSpan > baseSpan && basePx >= threshold) {
    const widePx = measure(wideSpan);
    candidates.push({ span: wideSpan, n: mosaicRowsFor(widePx, gap, module) });
  }
  return candidates;
}
