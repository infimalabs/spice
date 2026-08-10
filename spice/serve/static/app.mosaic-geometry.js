// Mosaic geometry pass. All
// dimensional constants are rem-denominated and resolved against the live
// root font size once per pass. minLane is the single width tunable: the
// capacity rule's emergent per-lane upper edge (min..min*(L+1)/L before the
// next column fits) is NOT a divisor and must never be encoded as
// floor(width / X) to pick a column count — that roughly halves the
// achievable lane count. mosaicModuleLineHeightRem mirrors the reference
// the type-ramp interlock
// owns the real card-chrome CSS this must ultimately agree with.
const mosaicGridTrackCount = 12;
const mosaicLegalBaseSpans = [2, 3, 4, 6, 12];
const mosaicGapRem = 0.75;
const mosaicMinLaneRem = 17;
const mosaicModuleLineHeightRem = 1.25;

function mosaicGeometry(rootFontSizePx, containerWidthPx) {
  const rem =
    Number.isFinite(rootFontSizePx) && rootFontSizePx > 0
      ? rootFontSizePx
      : 16;
  const width =
    Number.isFinite(containerWidthPx) && containerWidthPx > 0
      ? containerWidthPx
      : 0;
  const gap = Math.round(mosaicGapRem * rem);
  const minLane = mosaicMinLaneRem * rem;
  const colW =
    (width - (mosaicGridTrackCount - 1) * gap) / mosaicGridTrackCount;
  const baseSpan = mosaicBaseSpan(colW, gap, minLane);
  const L = mosaicGridTrackCount / baseSpan;
  const lines = mosaicModuleLines(L);
  const lineHeight = mosaicModuleLineHeightRem * rem;
  const M = Math.round(lines * lineHeight);
  const edges = mosaicEdges(colW, gap);
  return { gap, minLane, colW, baseSpan, L, lines, M, edges };
}

// Smallest legal span whose lane width already clears minLane; larger spans
// only make the inequality easier, so the first hit in ascending order wins.
// If none clear it even at a full-width single lane, baseSpan falls back to
// the grid track count (L = 1) — the same emergent floor the single-column
// degeneration relies on elsewhere, not a special case.
function mosaicBaseSpan(colW, gap, minLane) {
  for (const span of mosaicLegalBaseSpans) {
    if (span * colW + (span - 1) * gap >= minLane) return span;
  }
  return mosaicGridTrackCount;
}

function mosaicModuleLines(laneCount) {
  if (laneCount >= 3) return 3;
  if (laneCount === 2) return 2;
  return 1;
}

function mosaicEdges(colW, gap) {
  const edges = [];
  for (let track = 0; track <= mosaicGridTrackCount; track += 1) {
    edges.push(Math.round(track * (colW + gap)));
  }
  return edges;
}

function mosaicRootFontSizePx() {
  const parsed = Number.parseFloat(
    getComputedStyle(document.documentElement).fontSize,
  );
  return Number.isFinite(parsed) ? parsed : 16;
}

// Width must be read after the scrollbar gutter is reserved so it never
// depends on content height (see mosaic-scrollbar-gutter); this only mirrors
// the host's own padding box, it does not reserve the gutter itself.
function mosaicContainerWidthPx(host) {
  if (!host) throw new Error("mosaic content width requires a host element");
  const clientWidth = host.clientWidth;
  if (!Number.isFinite(clientWidth) || clientWidth < 0)
    throw new Error("mosaic host clientWidth must be finite and nonnegative");
  // display:none collapses clientWidth to zero even though computed padding
  // still reports its authored value. Zero is the real unmeasurable content
  // width in that state; subtracting the dormant padding would invent a
  // contradictory negative measurement.
  if (clientWidth === 0) return 0;
  const style = getComputedStyle(host);
  const paddingLeft = Number.parseFloat(style.paddingLeft);
  const paddingRight = Number.parseFloat(style.paddingRight);
  if (
    !Number.isFinite(paddingLeft) ||
    !Number.isFinite(paddingRight) ||
    paddingLeft < 0 ||
    paddingRight < 0
  )
    throw new Error("mosaic host padding must be finite and nonnegative");
  const contentWidth = clientWidth - paddingLeft - paddingRight;
  if (!Number.isFinite(contentWidth) || contentWidth < 0)
    throw new Error("mosaic host padding exceeds its measurable client width");
  return contentWidth;
}
