const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const renderPath = process.argv[3];
const lanesPath = process.argv[4];
const context = { console };

vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});
vm.runInContext(fs.readFileSync(renderPath, "utf8"), context, {
  filename: "app.render.js",
});
vm.runInContext(fs.readFileSync(lanesPath, "utf8"), context, {
  filename: "app.lanes.js",
});
const laneStore = vm.runInContext("laneStore", context);

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function canonicalThreadActorId(threadId) {
  return String(threadId || "")
    .trim()
    .replaceAll("-", "")
    .toLowerCase();
}

function targetTeamActorId(targetId) {
  const id = String(targetId || "").trim();
  return id ? "target:" + id : "";
}

function threadTeamActorId(threadId) {
  const actor = canonicalThreadActorId(threadId);
  return actor ? "thread:" + actor : "";
}

function teamActorKind(actorId) {
  const actor = String(actorId || "").trim();
  if (actor.startsWith("target:")) return "target";
  if (actor.startsWith("thread:")) return "thread";
  return "";
}

function teamActorValue(actorId) {
  const actor = String(actorId || "").trim();
  return teamActorKind(actor) ? actor.slice(actor.indexOf(":") + 1) : actor;
}

function teamActorThreadId(actorId) {
  const actor = String(actorId || "").trim();
  if (actor.startsWith("thread:")) return canonicalThreadActorId(actor.slice(7));
  if (actor.startsWith("target:")) return "";
  return canonicalThreadActorId(actor);
}

function teamActorMatchesThread(actorId, threadId) {
  const actorThreadId = teamActorThreadId(actorId);
  return Boolean(actorThreadId && actorThreadId === canonicalThreadActorId(threadId));
}

function normalizeTeamActorId(actorId) {
  const actor = String(actorId || "").trim();
  if (!actor) return "";
  if (actor.startsWith("target:")) return targetTeamActorId(actor.slice(7));
  if (actor.startsWith("thread:")) return threadTeamActorId(actor.slice(7));
  if (laneStore.targetForId(actor)) return targetTeamActorId(actor);
  return threadTeamActorId(actor);
}

function resetGlobals() {
  laneStore.replaceTargets([]);
  for (const lane of laneStore.lanesSnapshot())
    laneStore.removeLane(lane.targetId);
  context.browserStorage = () => null;
  context.canonicalThreadActorId = canonicalThreadActorId;
  context.targetTeamActorId = targetTeamActorId;
  context.threadTeamActorId = threadTeamActorId;
  context.teamActorKind = teamActorKind;
  context.teamActorValue = teamActorValue;
  context.teamActorThreadId = teamActorThreadId;
  context.teamActorMatchesThread = teamActorMatchesThread;
  context.normalizeTeamActorId = normalizeTeamActorId;
  context.applyGlobalSettingsPayload = (settings) => {
    if (!settings || typeof settings.fastMode !== "boolean")
      throw new Error("missing fixture global fast mode");
  };
  context.reconcileLaneGroups = (runs) => {
    context.reconciledGroupRuns = runs;
  };
  context.syncLaneEffectiveControls = () => {};
  context.ensureLaneOccupant = (lane, threadId) => {
    lane.occupantThreadId = threadId;
  };
  context.ensureEmptyTeamLane = (team) => {
    context.emptyTeamCalls.push(team.teamId);
  };
  context.unsubscribeLaneFromLiveBus = () => {};
  context.abortLaneSpeech = () => {};
  context.syncNarrationMediaSession = () => {};
  context.renderSpiceMenu = () => {
    context.menuRenderCount += 1;
  };
  context.isLaneOpen = (lane) => !lane.closed;
  context.laneComposerDraftText = () => "";
  context.targetsLoaded = true;
  context.filterStripEl = null;
  context.spiceMenuEl = null;
  context.emptyTeamCalls = [];
  context.reconciledGroupRuns = null;
  context.menuRenderCount = 0;
}

function renewalTeam(memberThreadId) {
  return {
    teamId: "team-1",
    revision: 2,
    config: {},
    splitBack: {},
    members: [{ agentId: memberThreadId, renewalIntent: { state: "started" } }],
  };
}

function targetIdentity(threadId) {
  return {
    targetId: "target-1",
    worktreeName: "target-1",
    branch: "target-1",
    driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
    agent: { state: "unconfigured" },
    thread: { state: "bound", threadId },
  };
}

function teamIdentity() {
  return {
    state: "member",
    teamId: "team-1",
    teamRevision: 1,
    configRevision: 1,
  };
}

function applySnapshot(team) {
  context.applyTeamSnapshotPayload(
    {
      revision: 2,
      changed: true,
      snapshot: { globalSettings: { fastMode: false }, teams: [team] },
    },
    { force: true },
  );
}

resetGlobals();
const staleTarget = {
  id: "target-1",
  targetIdentity: targetIdentity("predecessorthread"),
  teamIdentity: teamIdentity(),
};
const renamedLane = {
  targetId: "target-1",
  targetThreadId: "successorthread",
  activeThreadId: "successorthread",
  teamId: "team-1",
  sendAwaitingBackendCount: 0,
  element: { remove() {} },
};
laneStore.replaceTargets([staleTarget]);
laneStore.registerLane(renamedLane);

applySnapshot(renewalTeam("thread:successorthread"));

assert(
  laneStore.laneForId("target-1") === renamedLane,
  "renewed successor stays attached to the existing worktree lane",
);
assert(
  laneStore.targetForId("target-1").targetIdentity.thread.threadId ===
    "successorthread",
  "stale target inventory thread id is replaced with the successor",
);
assert(
  renamedLane.occupantThreadId === "successorthread",
  "renewed lane occupant follows the successor thread",
);
assert(
  context.emptyTeamCalls.length === 0,
  "mapped renewal snapshot does not create an empty-team placeholder",
);
assert(
  context.menuRenderCount === 0,
  "mapped renewal updates lane and target state without repainting the membership menu",
);

resetGlobals();
const pendingTarget = {
  id: "target-1",
  targetIdentity: targetIdentity("predecessorthread"),
  teamIdentity: teamIdentity(),
};
const pendingLane = {
  targetId: "target-1",
  targetThreadId: "predecessorthread",
  activeThreadId: "predecessorthread",
  teamId: "team-1",
  sendAwaitingBackendCount: 1,
  element: { remove() {} },
};
laneStore.replaceTargets([pendingTarget]);
laneStore.registerLane(pendingLane);

applySnapshot(renewalTeam("thread:successorthread"));

assert(
  laneStore.laneForId("target-1") === pendingLane,
  "early renewal snapshot keeps the existing lane instead of closing it",
);
assert(
  pendingLane.targetThreadId === "successorthread",
  "early renewal snapshot renames the lane thread id in place",
);
assert(
  laneStore.targetForId("target-1").targetIdentity.thread.threadId ===
    "successorthread",
  "early renewal snapshot replaces stale target inventory",
);
assert(
  context.emptyTeamCalls.length === 0,
  "early renewal snapshot does not present the non-empty team as empty",
);
assert(
  context.menuRenderCount === 0,
  "early renewal renames the lane thread in place without repainting the membership menu",
);
