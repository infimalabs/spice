const fs = require("fs");
const vm = require("vm");

const lanesPath = process.argv[2];
const context = { console };

vm.createContext(context);
vm.runInContext(fs.readFileSync(lanesPath, "utf8"), context, {
  filename: "app.lanes.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const lane = {
  latestPayload: {
    pendingInboxCount: 1,
    pendingInboxLabel: "1",
    pendingInboxKeys: ["stale-key"],
    pendingInboxRevision: "stale-rev",
    statusLine: {
      pendingInboxCount: 1,
      pendingInboxLabel: "1",
      pendingInboxKeys: ["stale-key"],
      pendingInboxRevision: "stale-rev",
      preview: "previous assistant response",
    },
  },
};
const drainedTarget = {
  id: "lane",
  pendingCount: 0,
  statusLine: {
    pendingInboxCount: 0,
    pendingInboxLabel: "0",
    pendingInboxKeys: [],
    pendingInboxRevision: "drained-rev",
  },
};

const reconciled = context.lanePayloadWithTargetPending(lane, drainedTarget);
assert(reconciled === lane.latestPayload, "open lane keeps cached payload object");
assert(
  lane.latestPayload.statusLine.pendingInboxCount === 0,
  "target refresh clears stale latest-payload pending count",
);
assert(
  lane.latestPayload.pendingInboxCount === 0,
  "target refresh clears stale top-level pending count",
);
assert(
  lane.latestPayload.statusLine.pendingInboxKeys.length === 0,
  "target refresh clears stale latest-payload pending keys",
);
assert(
  lane.latestPayload.pendingInboxRevision === "drained-rev",
  "target refresh carries authoritative pending revision",
);
assert(
  lane.latestPayload.statusLine.preview === "previous assistant response",
  "target refresh preserves retained status summary",
);

context.laneStates = new Map([
  ["lane", { lastRenderedStatusLine: lane.latestPayload.statusLine }],
]);
assert(
  context.targetChoicePendingCount(drainedTarget) === 0,
  "target choice does not resurrect stale lane pending after reconciliation",
);

const queuedLane = {
  latestPayload: {
    pendingInboxCount: 0,
    statusLine: { pendingInboxCount: 0 },
  },
};
const queuedTarget = {
  pendingCount: 2,
  statusLine: {
    pendingInboxCount: 2,
    pendingInboxLabel: "2",
    pendingInboxKeys: ["queued-a", "queued-b"],
    pendingInboxRevision: "queued-rev",
  },
};
context.lanePayloadWithTargetPending(queuedLane, queuedTarget);
assert(
  queuedLane.latestPayload.statusLine.pendingInboxCount === 2,
  "target refresh still surfaces genuinely queued steering",
);
assert(
  queuedLane.latestPayload.statusLine.pendingInboxKeys.join(",") ===
    "queued-a,queued-b",
  "target refresh surfaces authoritative pending keys",
);

const noFreshCountLane = {
  latestPayload: {
    pendingInboxCount: 3,
    statusLine: { pendingInboxCount: 3 },
  },
};
context.lanePayloadWithTargetPending(noFreshCountLane, { statusLine: {} });
assert(
  noFreshCountLane.latestPayload.statusLine.pendingInboxCount === 3,
  "missing target count leaves cached lane payload untouched",
);
