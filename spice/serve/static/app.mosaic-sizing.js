// Mosaic card sizing. Pure functions over already-measured pixel
// quantities — DOM measurement itself (natural height at a candidate width)
// is a rendering-path concern, out of scope here.

// The only px -> rows conversion in the codebase. Minimums are 2 units
// in both axes; span >= 2 is enforced by construction in the engine
// (mosaicClampSpan/legal spans), n >= 2 by this formula.
function mosaicRowsFor(px, gap, module) {
  return Math.max(2, Math.ceil((px + gap) / module));
}

// Rendered width for a card anchored at track t with span s, from the
// shared integer edges table (seam rule) — never per-card fractional
// colW multiplication.
function mosaicCardWidthPx(edges, t, s, gap) {
  return edges[t + s] - edges[t] - gap;
}
