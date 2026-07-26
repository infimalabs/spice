// Mosaic pure lattice engine. DOM-free and side-effect-free: layout is a
// pure function of the
// ordered insertion sequence and current geometry, nothing else — no
// randomness, no wall-clock reads, no resize-history dependence. Card
// records are plain objects: { t: anchor track, span: track span, b: base
// row, n: row count }. Rows count upward — a larger b is nearer the top.
// Measuring a candidate's row count from rendered content is a sizing concern
// and stays out of this module; candidates arrive pre-measured as
// { span, n } pairs.

// Every other function in the lattice treats this as "at least one track", so
// the guard has to be >= 1 rather than > 0: a fractional count like 0.5 passes
// `> 0` and then floors to zero, which yields an empty rowFloor, a placement
// range of nothing, and an anchor search with no candidates at all.
function mosaicTrackCount(trackCount) {
  return Number.isFinite(trackCount) && trackCount >= 1
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
  // tracks >= 1 and clampedSpan is clamped into [1, tracks], so track 0 is
  // always a legal placement. The search is seeded with it rather than with
  // null: this function is total, and saying so is what lets its callers read
  // the anchor without asking whether there was one.
  let best = mosaicAnchorAt(0, clampedSpan, rowFloor);
  for (let t = 1; t <= tracks - clampedSpan; t += 1) {
    const candidate = mosaicAnchorAt(t, clampedSpan, rowFloor);
    if (
      mosaicKeyLess(
        [candidate.b, candidate.waste, candidate.t],
        [best.b, best.waste, best.t],
      )
    )
      best = candidate;
  }
  return best;
}

// The frontier a span would sit on at one track: the highest floor it covers,
// and the dead space left under it.
function mosaicAnchorAt(t, clampedSpan, rowFloor) {
  let b = 0;
  for (let i = t; i < t + clampedSpan; i += 1) {
    if (rowFloor[i] > b) b = rowFloor[i];
  }
  let waste = 0;
  for (let i = t; i < t + clampedSpan; i += 1) waste += b - rowFloor[i];
  return { b, waste, t, span: clampedSpan };
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
  // At least one candidate is guaranteed above, so the first one seeds the
  // search and the winner is never absent.
  let best = mosaicDecision(candidates[0], rowFloor, tracks);
  for (let index = 1; index < candidates.length; index += 1) {
    const decision = mosaicDecision(candidates[index], rowFloor, tracks);
    if (mosaicKeyLess(decision.key, best.key)) best = decision;
  }
  return { span: best.span, n: best.n, b: best.b, t: best.t };
}

function mosaicDecision(candidate, rowFloor, tracks) {
  const anchor = mosaicAnchorFor(candidate.span, rowFloor, tracks);
  return {
    span: anchor.span,
    n: candidate.n,
    b: anchor.b,
    t: anchor.t,
    key: [anchor.b, anchor.waste, -anchor.span, anchor.t],
  };
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
