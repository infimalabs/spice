const fs = require("fs");
const vm = require("vm");

const lanesPath = process.argv[2];
const lane = {
  targetId: "lane-a",
  lastRenderedStatusLine: { agentProcessStatus: "running" },
};
const context = {
  console,
  laneStates: new Map([["lane-a", lane]]),
  targetById: new Map([["lane-a", { agentProcessStatus: "running" }]]),
  uniqueStringList(items) {
    return Array.from(new Set((items || []).filter(Boolean)));
  },
  isLaneOpen() {
    return true;
  },
  isShadowLane() {
    return false;
  },
  laneEffectiveLifetime() {
    return "Drain";
  },
  agentLifetimeDissolvesTaskBoundary() {
    return true;
  },
  agentLifetimeUsesStoredTaskFilters() {
    return false;
  },
  laneGroupMemberLanes() {
    return [lane];
  },
  laneAssignedTaskFilters() {
    return [];
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(lanesPath, "utf8"), context, {
  filename: "app.lanes.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function stem(counts) {
  return {
    name: "serve",
    filters: ["serve.ui"],
    ...counts,
  };
}

const mixed = context.taskFilterStemPillModel(
  stem({
    openTaskCount: 5,
    readyTaskCount: 2,
    inFlightTaskCount: 1,
    blockedTaskCount: 1,
    deferredTaskCount: 1,
  }),
);
assert(context.taskFilterStemPillCountText(mixed) === "2/5", "mixed badge is ready/open");
assert(context.taskFilterStemPillIsLit(mixed) === true, "mixed ready work lights the pill");
assert(
  mixed.title ===
    "2 ready / 5 open across serve.*; 1 in flight, 1 blocked, 1 deferred; drained by 1",
  "mixed hover exposes the complete state breakdown",
);

const allDeferred = context.taskFilterStemPillModel(
  stem({
    openTaskCount: 2,
    readyTaskCount: 0,
    inFlightTaskCount: 0,
    blockedTaskCount: 0,
    deferredTaskCount: 2,
  }),
);
assert(
  context.taskFilterStemPillCountText(allDeferred) === "0/2",
  "all-deferred badge preserves the open denominator",
);
assert(
  allDeferred.readyTaskCount === 0,
  "all-deferred work exposes zero ready tasks for the dim treatment",
);

const readyOnly = context.taskFilterStemPillModel(
  stem({
    openTaskCount: 3,
    readyTaskCount: 3,
    inFlightTaskCount: 0,
    blockedTaskCount: 0,
    deferredTaskCount: 0,
  }),
);
assert(
  context.taskFilterStemPillCountText(readyOnly) === "3/3",
  "ready-only badge retains equal ready and open counts",
);
assert(context.taskFilterStemPillIsLit(readyOnly) === true, "ready-only work lights the pill");

const empty = context.taskFilterStemPillsFromInventory({
  catalog: { approvedStems: ["serve"] },
  primaryStems: [],
});
assert(empty.length === 0, "empty inventory has no pill model");

const waitingPayload = context.taskFilterStemPillsFromInventory({
  catalog: { approvedStems: ["serve"] },
  primaryStems: [
    stem({
      openTaskCount: 2,
      readyTaskCount: 0,
      inFlightTaskCount: 0,
      blockedTaskCount: 0,
      deferredTaskCount: 2,
    }),
    {
      name: "waiting",
      filters: [],
      openTaskCount: 2,
      readyTaskCount: 0,
      inFlightTaskCount: 0,
      blockedTaskCount: 0,
      deferredTaskCount: 2,
    },
  ],
});
assert(
  waitingPayload.map((item) => item.name).join(",") === "serve",
  "project ready/open pills fully represent deferred inventory",
);
