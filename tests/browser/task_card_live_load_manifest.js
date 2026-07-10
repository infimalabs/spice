// Small release-runner load used while the real task-card scenario repeats.
// Both scenarios own scratch serve state and can run concurrently.
const releaseSafe = [
  { path: "serve_lanes_batch_subscribe_smoke.js" },
  { path: "serve_mosaic_single_settle_smoke.js" },
];

module.exports = { externalState: [], releaseSafe };
