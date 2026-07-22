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
laneStore.replaceTargets([{ id: "lane-a", agentProcessStatus: "running" }]);
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

// Toggle whether the running, boundary-dissolving Drain lane covers the public
// stems. A running lane makes every public stem "covered" (an agent could pull
// from it); stopping the lane leaves the stems uncovered, so the same task state
// resolves to the idle/dormant gray floor instead of the saturated band.
function setCoverage(running) {
  const status = running ? "running" : "stopped";
  lane.lastRenderedStatusLine.agentProcessStatus = status;
  laneStore.replaceTargets([{ id: "lane-a", agentProcessStatus: status }]);
}

function model(counts) {
  return context.taskFilterStemPillModel(stem(counts));
}
function tone(counts) {
  return context.taskFilterStemPillTone(model(counts));
}

const READY_MIX = {
  openTaskCount: 5,
  readyTaskCount: 2,
  inFlightTaskCount: 1,
  blockedTaskCount: 1,
  deferredTaskCount: 1,
};
const READY_ONLY = {
  openTaskCount: 3,
  readyTaskCount: 3,
  inFlightTaskCount: 0,
  blockedTaskCount: 0,
  deferredTaskCount: 0,
};
const ACTIVE_ONLY = {
  openTaskCount: 2,
  readyTaskCount: 0,
  inFlightTaskCount: 2,
  blockedTaskCount: 0,
  deferredTaskCount: 0,
};
const BLOCKED_ONLY = {
  openTaskCount: 2,
  readyTaskCount: 0,
  inFlightTaskCount: 0,
  blockedTaskCount: 2,
  deferredTaskCount: 0,
};
const DEFERRED_ONLY = {
  openTaskCount: 2,
  readyTaskCount: 0,
  inFlightTaskCount: 0,
  blockedTaskCount: 0,
  deferredTaskCount: 2,
};

// ---- badge text is independent of coverage: the ready/in-flight/unavailable
// triple always renders with a fixed footprint so the pill never jitters. ----
setCoverage(true);
const mixed = model(READY_MIX);
assert(
  context.taskFilterStemPillCountText(mixed) === "2·1·2",
  "mixed badge labels ready, active, and unavailable work",
);
assert(
  mixed.unavailableTaskCount === 2,
  "blocked and deferred work combine as unavailable",
);
assert(
  mixed.title ===
    "2 ready, 1 active/in flight, 1 blocked, 1 deferred; 5 open across serve.*; ready work drained by 1",
  "mixed hover exposes the complete state breakdown",
);
assert(
  context.taskFilterStemPillCountText(model(READY_ONLY)) === "3·0·0",
  "ready-only badge still renders the full triple with zero slots",
);
assert(
  context.taskFilterStemPillCountText(model(ACTIVE_ONLY)) === "0·2·0",
  "active-only badge renders claimed work in the middle slot",
);
assert(
  context.taskFilterStemPillCountText(model(BLOCKED_ONLY)) === "0·0·2",
  "blocked-only badge renders unavailable work in the last slot",
);
assert(
  context.taskFilterStemPillCountText(model(DEFERRED_ONLY)) === "0·0·2",
  "deferred-only badge uses the same glance-level unavailable count",
);
assert(
  context.taskFilterStemPillCountText(
    model({
      openTaskCount: 1,
      readyTaskCount: 0,
      inFlightTaskCount: 0,
      blockedTaskCount: 1,
      deferredTaskCount: 0,
    }),
  ) === "0·0·1",
  "unresolved dependency is unavailable",
);
assert(
  context.taskFilterStemPillCountText(
    model({
      openTaskCount: 1,
      readyTaskCount: 1,
      inFlightTaskCount: 0,
      blockedTaskCount: 0,
      deferredTaskCount: 0,
    }),
  ) === "1·0·0",
  "resolved dependency moves automatically into the ready slot",
);

// ---- covered: a running agent is assigned to (or can pull from) the stem, so
// the ramp climbs assigned -> active -> saturated as work moves, and even a
// fully-blocked stem stays lit because an agent is camped on it. ----
setCoverage(true);
assert(
  tone(READY_MIX) === "saturated",
  "a covered stem with ready work is fully saturated",
);
assert(
  tone(READY_ONLY) === "saturated",
  "covered ready-only work is fully saturated",
);
assert(
  tone(ACTIVE_ONLY) === "active",
  "covered in-flight work with no ready backlog takes the active step",
);
assert(
  tone(BLOCKED_ONLY) === "assigned",
  "a covered but fully-blocked stem stays lit as legitimately blocked, not dormant",
);
assert(
  tone(DEFERRED_ONLY) === "assigned",
  "a covered stem with only deferred work is still legitimately covered",
);
const saturatedTone = tone(READY_ONLY);
const activeTone = tone(ACTIVE_ONLY);
const assignedTone = tone(BLOCKED_ONLY);
assert(
  saturatedTone !== activeTone &&
    activeTone !== assignedTone &&
    saturatedTone !== assignedTone,
  "each covered work state selects a distinct saturation step",
);

// ---- uncovered: no running agent is assigned to the stem. Ready work reads as
// idle (waiting for an agent) and everything-blocked reads as dormant, so an
// uncovered stem is visibly distinct from a covered one at the same task state.
setCoverage(false);
const idleTone = tone(READY_ONLY);
const dormantTone = tone(BLOCKED_ONLY);
assert(
  idleTone === "idle",
  "uncovered ready work desaturates to the idle waiting step",
);
assert(
  dormantTone === "dormant",
  "uncovered fully-blocked work falls to the neutral dormant floor",
);
assert(idleTone !== dormantTone, "idle and dormant are distinct steps");
assert(
  idleTone !== saturatedTone,
  "the same ready work reads idle when uncovered and saturated when covered",
);
assert(
  assignedTone !== dormantTone,
  "a fully-blocked stem reads assigned when covered and dormant when not",
);

// Restore coverage for the hidden-stem / inventory checks below.
setCoverage(true);

// Hidden stems (marked from catalog.hiddenStems) are never handed out, so their
// coverage never advances and their badge collapses to a single open count. The
// dashed system marker plus the warn accent (applied in CSS) keep a non-empty
// deferred bucket standing out; the tone itself stays dormant. The private agent
// channel IS handed out (moves through phases), so it keeps the full triple and
// rides the coverage ramp. Project-defined hidden stems behave exactly like oops.
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
  "a never-covered hidden stem resolves to the dormant tone (its warn accent is applied in CSS)",
);
assert(
  oopsPill.classes.includes("filter-pill--system"),
  "hidden stems carry the dashed system marker that pins the warn accent",
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
// The private agent channel sits out boundary dissolution, so the public Drain
// lane does not cover it; ready private work with no lane assigned to it reads
// idle (unattended), not saturated -- it would only climb the ramp if a lane
// were specifically assigned to the agent filter.
assert(
  context.taskFilterStemPillTone(agentPill) === "idle",
  "the private agent channel with unattended ready work reads idle, not saturated",
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
