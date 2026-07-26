const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const renderPath = process.argv[3];
let placeholderSyncs = 0;
let badgeRenders = 0;
const context = {
  console,
  laneGroupHost: (lane) => lane,
  laneIsFusedHost: () => false,
  laneGroupMemberLanes: (lane) => [lane],
  renderLaneViewShell: () => {},
  renderLaneViewBadge: () => {
    badgeRenders += 1;
  },
  syncComposerPlaceholders: () => {
    placeholderSyncs += 1;
  },
  syncComposerShards: () => {},
  renderLaneFiltersPane: () => {},
  renderFilterPills: () => {},
  syncFusedLaneLights: () => {},
  syncFusedLaneStatusLine: () => {},
  syncLaneTeamMenuButton: () => {},
  applyTaskFilterInventory: () => false,
  applyServerLaneLifetime: () => false,
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});
vm.runInContext(fs.readFileSync(renderPath, "utf8"), context, {
  filename: "app.render.js",
});
const laneStore = vm.runInContext("laneStore", context);

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

let laneIndex = 0;
function lane(overrides = {}) {
  laneIndex += 1;
  const value = {
    targetId: "pending-" + laneIndex,
    optimisticPendingInboxCount: 2,
    optimisticSubmittedInboxKeys: new Set(["inbox-a", "inbox-b"]),
    optimisticPendingInboxFloor: 2,
    pendingSubmissionCount: 0,
    sendAwaitingBackendCount: 0,
    knownMessages: [],
    ...overrides,
  };
  laneStore.registerLane(value);
  return value;
}

function pendingChrome(subject, version, count, keys) {
  return {
    targetId: subject.targetId,
    pendingInbox: {
      authority: "inbox",
      order: { epoch: "", revision: version },
      value: { count, label: String(count), keys },
    },
  };
}

const drained = lane();
laneStore.applyLaneChrome(pendingChrome(drained, 10, 0, []));
assert(
  context.lanePendingDisplayCount(drained) === 0,
  "drained canonical pending facet clears stale optimistic count",
);
assert(
  drained.optimisticSubmittedInboxKeys.size === 0,
  "drained canonical keys clear unobserved submitted keys",
);

const submissionInFlight = lane({ pendingSubmissionCount: 1 });
laneStore.applyLaneChrome(pendingChrome(submissionInFlight, 11, 0, []));
assert(
  context.lanePendingDisplayCount(submissionInFlight) === 2,
  "in-flight submission keeps optimistic count through canonical zero",
);
assert(
  submissionInFlight.optimisticSubmittedInboxKeys.size === 2,
  "in-flight submission keeps submitted keys",
);

const sameCountDifferentKeys = lane({
  optimisticPendingInboxCount: 1,
  optimisticSubmittedInboxKeys: new Set(["submitted-key"]),
  optimisticPendingInboxFloor: 1,
});
laneStore.applyLaneChrome(
  pendingChrome(sameCountDifferentKeys, 13, 1, ["other-key"]),
);
assert(
  context.lanePendingDisplayCount(sameCountDifferentKeys) === 1,
  "same-count canonical key replacement remains visible",
);
assert(
  sameCountDifferentKeys.optimisticSubmittedInboxKeys.size === 0,
  "canonical keys clear stale submitted key at the same count",
);
assert(
  laneStore.laneChrome(sameCountDifferentKeys.targetId).pendingInbox.keys[0] ===
    "other-key",
  "canonical record, not the lane, retains pending keys",
);

const versioned = lane({
  optimisticPendingInboxCount: 2,
  optimisticSubmittedInboxKeys: new Set(),
  optimisticPendingInboxFloor: 0,
});
laneStore.applyLaneChrome(
  pendingChrome(versioned, 20, 2, ["new-a", "new-b"]),
);
const acceptedRecord = laneStore.laneChrome(versioned.targetId);
const rendersBeforeStale = badgeRenders;
const stale = laneStore.applyLaneChrome(pendingChrome(versioned, 10, 0, []));
assert(stale.disposition === "stale", "older pending facet is rejected");
assert(
  laneStore.laneChrome(versioned.targetId) === acceptedRecord,
  "older pending facet preserves record identity",
);
assert(
  context.lanePendingDisplayCount(versioned) === 2,
  "older pending facet cannot lower compose count",
);
assert(
  badgeRenders === rendersBeforeStale,
  "older pending facet rerenders no compose badge",
);

laneStore.applyLaneChrome(pendingChrome(versioned, 30, 0, []));
assert(
  context.lanePendingDisplayCount(versioned) === 0,
  "newer pending facet can clear compose count",
);
assert(
  !("backendPendingInboxCount" in versioned),
  "lane retains no backend pending copy",
);

const optimisticSubmission = lane({
  optimisticPendingInboxCount: 1,
  optimisticSubmittedInboxKeys: new Set(),
  optimisticPendingInboxFloor: 0,
});
laneStore.applyLaneChrome(pendingChrome(optimisticSubmission, 1, 1, ["queued"]));
const placeholderBeforeBegin = placeholderSyncs;
context.beginLanePendingSubmission(optimisticSubmission);
assert(
  context.lanePendingDisplayCount(optimisticSubmission) === 2,
  "local pending submission increments the canonical count presentation",
);
assert(
  placeholderSyncs === placeholderBeforeBegin + 1,
  "local submission immediately syncs the composer placeholder",
);
