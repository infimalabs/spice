// Mosaic card sizing. Pure functions over already-measured pixel
// quantities — DOM measurement itself (natural height at a candidate width)
// is a rendering-path concern, out of scope here.

// The only px -> rows conversion in the codebase. Minimums are 2 units
// in both axes; span >= 2 is enforced by construction in the engine
// (mosaicClampSpan/legal spans), n >= 2 by this formula.
function mosaicRowsFor(px, gap, module) {
  return Math.max(2, Math.ceil((px + gap) / module));
}

// Rendered height for a card spanning n rows.
function mosaicCardHeightPx(n, module, gap) {
  return n * module - gap;
}

// Rendered width for a card anchored at track t with span s, from the
// shared integer edges table (seam rule) — never per-card fractional
// colW multiplication.
function mosaicCardWidthPx(edges, t, s, gap) {
  return edges[t + s] - edges[t] - gap;
}

// Interior slack the card pads to fill: n*module - gap - contentPx.
// Bounded below module by construction of the ceil in mosaicRowsFor above
// (contentPx + gap is always within one module of n*module).
function mosaicInteriorSlackPx(n, module, gap, contentPx) {
  return mosaicCardHeightPx(n, module, gap) - contentPx;
}
