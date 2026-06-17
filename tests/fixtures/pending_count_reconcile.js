const fs = require("fs");
const vm = require("vm");

const streamPath = process.argv[2];
const renderPath = process.argv[3];
const context = {
  console,
  WebSocket: { OPEN: 1, CONNECTING: 0 },
  window: { location: { protocol: "http:", host: "localhost" } },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(streamPath, "utf8"), context, {
  filename: "app.stream.js",
});
vm.runInContext(fs.readFileSync(renderPath, "utf8"), context, {
  filename: "app.render.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function lane(overrides = {}) {
  return {
    backendPendingInboxCount: 0,
    optimisticPendingInboxCount: 2,
    optimisticSubmittedInboxKeys: new Set(["inbox-a", "inbox-b"]),
    optimisticPendingInboxFloor: 2,
    pendingSubmissionCount: 0,
    sendAwaitingBackendCount: 0,
    knownMessages: [],
    ...overrides,
  };
}

const drained = lane();
context.syncLaneBackendPending(drained, 0);
assert(
  context.lanePendingDisplayCount(drained) === 0,
  "drained backend clears stale optimistic pending count",
);
assert(
  drained.optimisticSubmittedInboxKeys.size === 0,
  "drained backend clears unobserved submitted inbox keys",
);
assert(
  drained.optimisticPendingInboxFloor === 0,
  "drained backend clears submitted pending floor",
);

const sendInFlight = lane({ sendAwaitingBackendCount: 1 });
context.syncLaneBackendPending(sendInFlight, 0);
assert(
  context.lanePendingDisplayCount(sendInFlight) === 2,
  "in-flight send keeps optimistic pending count through stale backend zero",
);
assert(
  sendInFlight.optimisticSubmittedInboxKeys.size === 2,
  "in-flight send keeps submitted inbox keys",
);

const stillQueued = lane();
context.syncLaneBackendPending(stillQueued, 2);
assert(
  context.lanePendingDisplayCount(stillQueued) === 2,
  "nonzero backend pending count remains visible",
);
assert(
  stillQueued.optimisticSubmittedInboxKeys.size === 2,
  "nonzero backend pending count keeps submitted inbox keys",
);
