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
  context.taskFilterStemPillCountText(mixed) === "2·1·2",
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
  context.taskFilterStemPillCountText(readyOnly) === "3·0·0",
  "ready-only badge still renders the full triple with zero slots",
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
  context.taskFilterStemPillCountText(activeOnly) === "0·2·0",
  "active-only badge renders the full triple with claimed work in the middle slot",
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
  context.taskFilterStemPillCountText(blockedOnly) === "0·0·2",
  "blocked-only badge renders the full triple with unavailable work in the last slot",
);
assert(
  context.taskFilterStemPillTone(blockedOnly) === "dormant",
  "blocked-only work (0/0/N) receives the fully desaturated dormant treatment",
);

const readyTone = context.taskFilterStemPillTone(readyOnly);
const activeTone = context.taskFilterStemPillTone(activeOnly);
const dormantTone = context.taskFilterStemPillTone(blockedOnly);
assert(
  readyTone !== activeTone &&
    activeTone !== dormantTone &&
    readyTone !== dormantTone,
  "each populated slot of the triple selects a distinct saturation tone",
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
  context.taskFilterStemPillCountText(deferredOnly) === "0·0·2",
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
  context.taskFilterStemPillCountText(unresolvedBlocker) === "0·0·1",
  "unresolved dependency is unavailable",
);
assert(
  context.taskFilterStemPillCountText(resolvedBlocker) === "1·0·0",
  "resolved dependency moves automatically into the ready slot",
);
assert(
  context.taskFilterStemPillTone(unresolvedBlocker) === "dormant",
  "unresolved dependency starts dormant",
);
assert(
  context.taskFilterStemPillTone(resolvedBlocker) === "ready",
  "resolved dependency receives the ready treatment",
);

// Hidden stems (marked from catalog.hiddenStems) are never handed out, so their
// badge collapses to a single open count and they ride the saturation ramp. The
// private agent channel IS handed out (moves through phases), so it keeps the
// full public triple. Project-defined hidden stems behave exactly like oops.
const oopsPill = context.taskFilterStemPillModel({
  name: "oops",
  filters: [],
  hidden: true,
  openTaskCount: 3,
  readyTaskCount: 0,
  inFlightTaskCount: 0,
  blockedTaskCount: 0,
  deferredTaskCount: 3,
});
assert(
  context.taskFilterStemPillCountText(oopsPill) === "3",
  "the oops hidden stem collapses its badge to the single open count",
);
assert(
  context.taskFilterStemPillTone(oopsPill) === "dormant",
  "an idle hidden stem desaturates to dormant instead of a pinned accent",
);
assert(
  oopsPill.classes.includes("filter-pill--system"),
  "hidden stems carry the dashed system marker",
);

const maximPill = context.taskFilterStemPillModel({
  name: "maxim_proposal",
  filters: [],
  hidden: true,
  openTaskCount: 4,
  readyTaskCount: 0,
  inFlightTaskCount: 0,
  blockedTaskCount: 0,
  deferredTaskCount: 4,
});
assert(
  context.taskFilterStemPillCountText(maximPill) === "4",
  "a project-defined hidden stem collapses exactly like oops",
);
assert(
  maximPill.classes.includes("filter-pill--system"),
  "project-defined hidden stems also carry the dashed system marker",
);

const agentPill = context.taskFilterStemPillModel({
  name: "agent",
  filters: [],
  hidden: false,
  openTaskCount: 3,
  readyTaskCount: 2,
  inFlightTaskCount: 1,
  blockedTaskCount: 0,
  deferredTaskCount: 0,
});
assert(
  context.taskFilterStemPillCountText(agentPill) === "2·1·0",
  "the private agent channel keeps the public triple because its tasks move through phases",
);
assert(
  context.taskFilterStemPillTone(agentPill) === "ready",
  "the agent channel reacts to the saturation ramp like a public stem",
);
assert(
  agentPill.classes.includes("filter-pill--private"),
  "the agent channel is marked private, not system",
);
assert(
  context.taskFilterStemPillCountText(maximPill) !==
    context.taskFilterStemPillCountText(agentPill),
  "hidden stems collapse to one number while the moving agent channel stays split",
);

// taskFilterStemPillsFromInventory marks stems hidden from catalog.hiddenStems
// and includes public stems, the private agent channel, then every hidden stem.
const inventoryPills = context.taskFilterStemPillsFromInventory({
  catalog: {
    approvedStems: ["serve"],
    hiddenStems: ["oops", "maxim_proposal"],
  },
  primaryStems: [
    stem({
      openTaskCount: 1,
      readyTaskCount: 1,
      inFlightTaskCount: 0,
      blockedTaskCount: 0,
      deferredTaskCount: 0,
    }),
    {
      name: "agent",
      filters: [],
      openTaskCount: 1,
      readyTaskCount: 1,
      inFlightTaskCount: 0,
      blockedTaskCount: 0,
      deferredTaskCount: 0,
    },
    {
      name: "oops",
      filters: [],
      openTaskCount: 2,
      readyTaskCount: 0,
      inFlightTaskCount: 0,
      blockedTaskCount: 0,
      deferredTaskCount: 2,
    },
    {
      name: "maxim_proposal",
      filters: [],
      openTaskCount: 3,
      readyTaskCount: 0,
      inFlightTaskCount: 0,
      blockedTaskCount: 0,
      deferredTaskCount: 3,
    },
  ],
});
const hiddenByName = new Map(
  inventoryPills.map((item) => [item.name, Boolean(item.hidden)]),
);
assert(
  hiddenByName.get("serve") === false && hiddenByName.get("agent") === false,
  "public stems and the private agent channel are not marked hidden",
);
assert(
  hiddenByName.get("oops") === true &&
    hiddenByName.get("maxim_proposal") === true,
  "catalog hidden stems (built-in and project-defined) are marked hidden",
);
assert(
  inventoryPills.map((item) => item.name).join(",") ===
    "serve,agent,oops,maxim_proposal",
  "pill order is public stems, then the private agent channel, then hidden stems",
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
