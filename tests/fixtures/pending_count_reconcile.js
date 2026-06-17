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

const submissionInFlight = lane({ pendingSubmissionCount: 1 });
context.syncLaneBackendPending(submissionInFlight, 0);
assert(
  context.lanePendingDisplayCount(submissionInFlight) === 2,
  "pending submission keeps optimistic pending count through backend zero",
);
assert(
  submissionInFlight.optimisticSubmittedInboxKeys.size === 2,
  "pending submission keeps submitted inbox keys",
);

const acceptedSendRefresh = lane({ sendAwaitingBackendCount: 1 });
context.syncLaneBackendPending(acceptedSendRefresh, 0);
assert(
  context.lanePendingDisplayCount(acceptedSendRefresh) === 0,
  "accepted send refresh trusts drained backend count",
);
assert(
  acceptedSendRefresh.optimisticSubmittedInboxKeys.size === 0,
  "accepted send refresh clears submitted inbox keys",
);

const sameCountDifferentKeys = lane({
  backendPendingInboxCount: 1,
  optimisticPendingInboxCount: 1,
  optimisticSubmittedInboxKeys: new Set(["submitted-key"]),
  optimisticPendingInboxFloor: 1,
});
context.syncLaneBackendPending(sameCountDifferentKeys, {
  pendingInboxCount: 1,
  pendingInboxKeys: ["other-key"],
  pendingInboxRevision: "rev-other",
});
assert(
  context.lanePendingDisplayCount(sameCountDifferentKeys) === 1,
  "same-count backend key replacement remains visibly pending",
);
assert(
  sameCountDifferentKeys.optimisticSubmittedInboxKeys.size === 0,
  "authoritative backend keys clear stale submitted key with same count",
);
assert(
  sameCountDifferentKeys.backendPendingInboxKeys.has("other-key"),
  "authoritative backend keys are retained on the lane",
);

const submittedKeyStillPending = lane({
  backendPendingInboxCount: 1,
  optimisticPendingInboxCount: 1,
  optimisticSubmittedInboxKeys: new Set(["submitted-key"]),
  optimisticPendingInboxFloor: 1,
});
context.syncLaneBackendPending(submittedKeyStillPending, {
  pendingInboxCount: 1,
  pendingInboxKeys: ["submitted-key"],
  pendingInboxRevision: "rev-submitted",
});
assert(
  submittedKeyStillPending.optimisticSubmittedInboxKeys.has("submitted-key"),
  "authoritative backend keys preserve submitted key that is still pending",
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
