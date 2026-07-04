// Mosaic pure lattice engine. DOM-free and side-effect-free: layout is a
// pure function of the
// ordered insertion sequence and current geometry, nothing else — no
// randomness, no wall-clock reads, no resize-history dependence. Card
// records are plain objects: { t: anchor track, span: track span, b: base
// row, n: row count }. Rows count upward — a larger b is nearer the top.
// Measuring a candidate's row count from rendered content is a sizing concern
// and stays out of this module; candidates arrive pre-measured as
// { span, n } pairs.

function mosaicTrackCount(trackCount) {
  return Number.isFinite(trackCount) && trackCount > 0
    ? Math.floor(trackCount)
    : 12;
}

function mosaicClampSpan(span, trackCount) {
  const tracks = mosaicTrackCount(trackCount);
  return Math.max(1, Math.min(Math.floor(span) || 1, tracks));
}

// rowFloor[i] is the integer skyline of track i: max(b + n) over cards
// covering i, or 0. Derived fresh from a card list — the shape full
// replay needs against a clean skyline.
function mosaicDeriveRowFloor(cards, trackCount) {
  const tracks = mosaicTrackCount(trackCount);
  const rowFloor = new Array(tracks).fill(0);
  for (const card of cards) {
    const top = card.b + card.n;
    const start = Math.max(0, card.t);
    const end = Math.min(tracks, card.t + card.span);
    for (let track = start; track < end; track += 1) {
      if (top > rowFloor[track]) rowFloor[track] = top;
    }
  }
  return rowFloor;
}

// Lexicographic comparison over equal-length numeric tuples.
function mosaicKeyLess(left, right) {
  for (let index = 0; index < left.length; index += 1) {
    if (left[index] !== right[index]) return left[index] < right[index];
  }
  return false;
}

// For a fixed span, the anchor minimizing (b, waste, t) — flattest
// frontier, then least waste, then leftmost.
function mosaicAnchorFor(span, rowFloor, trackCount) {
  const tracks = mosaicTrackCount(trackCount);
  const clampedSpan = mosaicClampSpan(span, tracks);
  let best = null;
  for (let t = 0; t <= tracks - clampedSpan; t += 1) {
    let b = 0;
    for (let i = t; i < t + clampedSpan; i += 1) {
      if (rowFloor[i] > b) b = rowFloor[i];
    }
    let waste = 0;
    for (let i = t; i < t + clampedSpan; i += 1) waste += b - rowFloor[i];
    const key = [b, waste, t];
    if (best === null || mosaicKeyLess(key, [best.b, best.waste, best.t]))
      best = { b, waste, t, span: clampedSpan };
  }
  return best;
}

// Choose across candidate spans by the lexicographic minimum of
// (b, waste, −span, t) — flattest frontier, then least waste, then widest,
// then leftmost. A wide placement can only win this key if it lands at a
// strictly lower row than the best base placement, or ties it with no more
// waste — it never digs a hole to take width.
function mosaicDecide(candidates, rowFloor, trackCount) {
  if (!candidates || !candidates.length)
    throw new Error("mosaicDecide requires at least one candidate");
  const tracks = mosaicTrackCount(trackCount);
  let best = null;
  for (const candidate of candidates) {
    const anchor = mosaicAnchorFor(candidate.span, rowFloor, tracks);
    const key = [anchor.b, anchor.waste, -anchor.span, anchor.t];
    if (best === null || mosaicKeyLess(key, best.key))
      best = { span: anchor.span, n: candidate.n, b: anchor.b, t: anchor.t, key };
  }
  return { span: best.span, n: best.n, b: best.b, t: best.t };
}

// Advance the tracks a placed card covers. Returns a new rowFloor —
// never mutates the input — so callers can compose without aliasing.
function mosaicCommit(rowFloor, card, trackCount) {
  const tracks = mosaicTrackCount(trackCount);
  const next = rowFloor.slice();
  const top = card.b + card.n;
  const start = Math.max(0, card.t);
  const end = Math.min(tracks, card.t + card.span);
  for (let track = start; track < end; track += 1) {
    if (top > next[track]) next[track] = top;
  }
  return next;
}

// insert(): decide, append, commit. Geometry recompute, freeze latches,
// and rendering are separate concerns and stay out of this
// pure step. Never touches any existing card in `cards`.
function mosaicInsert(cards, rowFloor, candidates, trackCount) {
  const tracks = mosaicTrackCount(trackCount);
  const placement = mosaicDecide(candidates, rowFloor, tracks);
  const card = {
    t: placement.t,
    span: placement.span,
    b: placement.b,
    n: placement.n,
  };
  return {
    card,
    cards: cards.concat([card]),
    rowFloor: mosaicCommit(rowFloor, card, tracks),
  };
}

function mosaicTracksOverlap(a, b) {
  return a.t < b.t + b.span && b.t < a.t + a.span;
}

function mosaicRowsOverlap(a, b) {
  return a.b < b.b + b.n && b.b < a.b + a.n;
}

// Structural invariant: no two cards overlap in both axes.
function mosaicCardsOverlap(a, b) {
  return mosaicTracksOverlap(a, b) && mosaicRowsOverlap(a, b);
}
