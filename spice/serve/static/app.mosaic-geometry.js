// Mosaic geometry pass (spec .spice/mosaic-packing-spec.md §3). All
// dimensional constants are rem-denominated and resolved against the live
// root font size once per pass. minLane is the single width tunable: the
// capacity rule's emergent per-lane upper edge (min..min*(L+1)/L before the
// next column fits) is NOT a divisor and must never be encoded as
// floor(width / X) to pick a column count — that roughly halves the
// achievable lane count. mosaicModuleLineHeightRem mirrors the reference
// computeGeometry() in .spice/mosaic-demo.html; §4's type-ramp interlock
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
  const style = getComputedStyle(host);
  return (
    host.clientWidth -
    Number.parseFloat(style.paddingLeft || "0") -
    Number.parseFloat(style.paddingRight || "0")
  );
}

function laneMosaicGeometry(lane) {
  const host = lane.messagesEl;
  return mosaicGeometry(
    mosaicRootFontSizePx(),
    host ? mosaicContainerWidthPx(host) : 0,
  );
}
