const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const lanesPath = process.argv[3];
const lane = {
  targetId: "lane-a",
  lastRenderedStatusLine: { agentProcessStatus: "running" },
};
const context = {
  console,
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
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});
vm.runInContext(fs.readFileSync(lanesPath, "utf8"), context, {
  filename: "app.lanes.js",
});
const laneStore = vm.runInContext("laneStore", context);
laneStore.replaceTargets([
  { id: "lane-a", agentProcessStatus: "running" },
]);
laneStore.registerLane(lane);

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
assert(
  context.taskFilterStemPillCountText(mixed) === "2/1/2",
  "mixed badge labels ready, active, and unavailable work",
);
assert(
  context.taskFilterStemPillTone(mixed) === "ready",
  "mixed work receives the ready treatment",
);
assert(mixed.unavailableTaskCount === 2, "blocked and deferred work combine as unavailable");
assert(
  mixed.title ===
    "2 ready, 1 active/in flight, 1 blocked, 1 deferred; 5 open across serve.*; ready work drained by 1",
  "mixed hover exposes the complete state breakdown",
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
  context.taskFilterStemPillCountText(readyOnly) === "3",
  "ready-only badge omits both zero suffixes",
);
assert(
  context.taskFilterStemPillTone(readyOnly) === "ready",
  "ready-only work receives the ready treatment",
);

const activeOnly = context.taskFilterStemPillModel(
  stem({
    openTaskCount: 2,
    readyTaskCount: 0,
    inFlightTaskCount: 2,
    blockedTaskCount: 0,
    deferredTaskCount: 0,
  }),
);
assert(
  context.taskFilterStemPillCountText(activeOnly) === "0/2",
  "active-only badge keeps claimed work explicit",
);
assert(
  context.taskFilterStemPillTone(activeOnly) === "active",
  "active-only work receives a distinct live treatment",
);

const blockedOnly = context.taskFilterStemPillModel(
  stem({
    openTaskCount: 2,
    readyTaskCount: 0,
    inFlightTaskCount: 0,
    blockedTaskCount: 2,
    deferredTaskCount: 0,
  }),
);
assert(
  context.taskFilterStemPillCountText(blockedOnly) === "0/0/2",
  "blocked-only badge uses the conditional unavailable count",
);
assert(
  context.taskFilterStemPillTone(blockedOnly) === "dormant",
  "blocked-only work receives the dormant treatment",
);

const deferredOnly = context.taskFilterStemPillModel(
  stem({
    openTaskCount: 2,
    readyTaskCount: 0,
    inFlightTaskCount: 0,
    blockedTaskCount: 0,
    deferredTaskCount: 2,
  }),
);
assert(
  context.taskFilterStemPillCountText(deferredOnly) === "0/0/2",
  "deferred-only badge uses the same glance-level unavailable count",
);
assert(
  context.taskFilterStemPillTone(deferredOnly) === "dormant",
  "deferred-only work receives the dormant treatment",
);

const unresolvedBlocker = context.taskFilterStemPillModel(
  stem({
    openTaskCount: 1,
    readyTaskCount: 0,
    inFlightTaskCount: 0,
    blockedTaskCount: 1,
    deferredTaskCount: 0,
  }),
);
const resolvedBlocker = context.taskFilterStemPillModel(
  stem({
    openTaskCount: 1,
    readyTaskCount: 1,
    inFlightTaskCount: 0,
    blockedTaskCount: 0,
    deferredTaskCount: 0,
  }),
);
assert(
  context.taskFilterStemPillCountText(unresolvedBlocker) === "0/0/1",
  "unresolved dependency is unavailable",
);
assert(
  context.taskFilterStemPillCountText(resolvedBlocker) === "1",
  "resolved dependency moves automatically into ready",
);
assert(
  context.taskFilterStemPillTone(unresolvedBlocker) === "dormant",
  "unresolved dependency starts dormant",
);
assert(
  context.taskFilterStemPillTone(resolvedBlocker) === "ready",
  "resolved dependency receives the ready treatment",
);

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
  "project state pills fully represent deferred inventory",
);
