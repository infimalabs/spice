const fs = require("fs");
const vm = require("vm");

const engineSource = fs.readFileSync(process.argv[2], "utf8");
const sizingSource = fs.readFileSync(process.argv[3], "utf8");
const spanSource = fs.readFileSync(process.argv[4], "utf8");
const context = { console };
vm.createContext(context);
vm.runInContext(engineSource, context, { filename: "app.mosaic-engine.js" });
vm.runInContext(sizingSource, context, { filename: "app.mosaic-sizing.js" });
vm.runInContext(spanSource, context, { filename: "app.mosaic-span.js" });

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function deepEqual(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

const TRACKS = 12;
const GAP = 12;
const MODULE = 60;
const TALL = 230;

function countingMeasure(heightBySpan) {
  const calls = [];
  const measure = (span) => {
    calls.push(span);
    if (!(span in heightBySpan))
      throw new Error("unexpected measure(" + span + ")");
    return heightBySpan[span];
  };
  measure.calls = calls;
  return measure;
}

// Short content never clears tallThreshold -- exactly one candidate,
// exactly one measurement read.
{
  const measure = countingMeasure({ 4: 100 });
  const candidates = context.mosaicCandidates(4, TRACKS, GAP, MODULE, measure, TALL);
  assert(measure.calls.length === 1, "short content must read exactly once, got " + measure.calls.length);
  assert(deepEqual(measure.calls, [4]), "short content must only measure baseSpan");
  assert(candidates.length === 1, "short content must yield exactly one candidate");
  assert(candidates[0].span === 4, "sole candidate must be baseSpan");
  assert(
    candidates[0].n === context.mosaicRowsFor(100, GAP, MODULE),
    "candidate n must come from mosaicRowsFor",
  );
}

// Content at or above tallThreshold adds the wide tier -- exactly
// two measurement reads, one per candidate width.
{
  const measure = countingMeasure({ 4: 230, 8: 340 });
  const candidates = context.mosaicCandidates(4, TRACKS, GAP, MODULE, measure, TALL);
  assert(measure.calls.length === 2, "tall content must read exactly twice, got " + measure.calls.length);
  assert(deepEqual(measure.calls, [4, 8]), "tall content must measure baseSpan then wideSpan");
  assert(candidates.length === 2, "tall content must yield two candidates");
  assert(candidates[0].span === 4 && candidates[1].span === 8, "candidates must be [base, wide]");
  assert(
    candidates[1].n === context.mosaicRowsFor(340, GAP, MODULE),
    "wide candidate n must come from its OWN measurement, not the base one",
  );
}

// Exactly-at-threshold is inclusive (`>=`).
{
  const measure = countingMeasure({ 4: 230, 8: 50 });
  const candidates = context.mosaicCandidates(4, TRACKS, GAP, MODULE, measure, TALL);
  assert(candidates.length === 2, "height exactly at tallThreshold must still widen");
}

// One px under threshold must not widen, and must not even read the wide
// width (the whole point of the gate: skip the second measurement).
{
  const measure = countingMeasure({ 4: 229 });
  const candidates = context.mosaicCandidates(4, TRACKS, GAP, MODULE, measure, TALL);
  assert(measure.calls.length === 1, "just-under-threshold must not read the wide width");
  assert(candidates.length === 1, "just-under-threshold must not widen");
}

// A baseSpan that already fills the grid has nowhere to widen into --
// never measured twice, never offered a wide candidate, no matter how tall.
{
  const measure = countingMeasure({ 12: 5000 });
  const candidates = context.mosaicCandidates(12, TRACKS, GAP, MODULE, measure, TALL);
  assert(measure.calls.length === 1, "full-width baseSpan must read only once");
  assert(candidates.length === 1, "full-width baseSpan must never offer a wide candidate");
}

// mosaicWideSpan formula and the "nothing wider to measure" degenerate case.
{
  assert(context.mosaicWideSpan(4, TRACKS) === 8, "wideSpan must be min(2*baseSpan, tracks)");
  assert(context.mosaicWideSpan(6, TRACKS) === TRACKS, "wideSpan must clamp to the track count");
  assert(context.mosaicWideSpan(TRACKS, TRACKS) === TRACKS, "full-width baseSpan has no wider tier");
}

// Default tallThreshold (no override) matches the documented constant.
{
  const measure = countingMeasure({ 4: 229 });
  const below = context.mosaicCandidates(4, TRACKS, GAP, MODULE, measure);
  assert(below.length === 1, "default threshold must reject 229px");
  const measureAt = countingMeasure({ 4: 230, 8: 100 });
  const at = context.mosaicCandidates(4, TRACKS, GAP, MODULE, measureAt);
  assert(at.length === 2, "default threshold must accept the documented 230px");
}

// End-to-end: on a flat frontier, exact integer ties are common, and
// the placement key's -span term means the wide candidate wins a tie rather
// than the base one -- widening is emergent from the key, not chosen here.
{
  const flatRowFloor = context.mosaicDeriveRowFloor([], TRACKS);
  const measure = countingMeasure({ 4: 230, 8: 230 });
  const candidates = context.mosaicCandidates(4, TRACKS, GAP, MODULE, measure, TALL);
  const decision = context.mosaicDecide(candidates, flatRowFloor, TRACKS);
  assert(decision.span === 8, "a flat frontier with a tied key must pick the wider candidate");
}

// End-to-end: the wide candidate must never win by digging a hole --
// when every wide anchor straddles a bump that a base-width card can dodge,
// base must win even though the card measured tall enough to be offered wide.
{
  // A bump on tracks 4-7 (row 2); tracks 0-3 and 8-11 are flat at row 0. A
  // baseSpan=4 card fits entirely within the flat run at t=0 (b=0, waste=0).
  // Every wideSpan=8 anchor (t=0..4) straddles the bump and lands at b=2 with
  // waste=8 -- strictly worse on the leading `b` term alone.
  const bumpyRowFloor = [0, 0, 0, 0, 2, 2, 2, 2, 0, 0, 0, 0];
  const measure = countingMeasure({ 4: 230, 8: 230 });
  const candidates = context.mosaicCandidates(4, TRACKS, GAP, MODULE, measure, TALL);
  const decision = context.mosaicDecide(candidates, bumpyRowFloor, TRACKS);
  assert(decision.span === 4, "wide must not win by landing higher/wastier than the best base placement");
  assert(decision.b === 0, "base placement must land on the flat run");
  assert(decision.t === 0, "base placement must anchor at the flat run");
}

console.log("mosaic span: all assertions passed");
