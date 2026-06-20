const fs = require("fs");
const vm = require("vm");

const renderPath = process.argv[2];
const lanesPath = process.argv[3];
const context = { console };

vm.createContext(context);
vm.runInContext(fs.readFileSync(renderPath, "utf8"), context, {
  filename: "app.render.js",
});
vm.runInContext(fs.readFileSync(lanesPath, "utf8"), context, {
  filename: "app.lanes.js",
});

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
  if (context.targetById && context.targetById.has(actor))
    return targetTeamActorId(actor);
  return threadTeamActorId(actor);
}

function resetGlobals() {
  context.teamSnapshotRevision = 0;
  context.browserStorage = () => null;
  context.canonicalThreadActorId = canonicalThreadActorId;
  context.targetTeamActorId = targetTeamActorId;
  context.threadTeamActorId = threadTeamActorId;
  context.teamActorKind = teamActorKind;
  context.teamActorValue = teamActorValue;
  context.teamActorThreadId = teamActorThreadId;
  context.teamActorMatchesThread = teamActorMatchesThread;
  context.normalizeTeamActorId = normalizeTeamActorId;
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
  context.isLaneOpen = (lane) => !lane.closed;
  context.laneComposerDraftText = () => "";
  context.filterStripEl = null;
  context.spiceMenuEl = null;
  context.emptyTeamCalls = [];
  context.reconciledGroupRuns = null;
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
      snapshot: { teams: [team] },
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
context.targets = [staleTarget];
context.targetById = new Map([[staleTarget.id, staleTarget]]);
context.laneStates = new Map([[renamedLane.targetId, renamedLane]]);

applySnapshot(renewalTeam("thread:successorthread"));

assert(
  context.laneStates.get("target-1") === renamedLane,
  "renewed successor stays attached to the existing worktree lane",
);
assert(
  staleTarget.targetIdentity.thread.threadId === "successorthread",
  "stale target inventory thread id is renamed in place to the successor",
);
assert(
  renamedLane.occupantThreadId === "successorthread",
  "renewed lane occupant follows the successor thread",
);
assert(
  context.emptyTeamCalls.length === 0,
  "mapped renewal snapshot does not create an empty-team placeholder",
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
context.targets = [pendingTarget];
context.targetById = new Map([[pendingTarget.id, pendingTarget]]);
context.laneStates = new Map([[pendingLane.targetId, pendingLane]]);

applySnapshot(renewalTeam("thread:successorthread"));

assert(
  context.laneStates.get("target-1") === pendingLane,
  "early renewal snapshot keeps the existing lane instead of closing it",
);
assert(
  pendingLane.targetThreadId === "successorthread",
  "early renewal snapshot renames the lane thread id in place",
);
assert(
  pendingTarget.targetIdentity.thread.threadId === "successorthread",
  "early renewal snapshot renames stale target inventory in place",
);
assert(
  context.emptyTeamCalls.length === 0,
  "early renewal snapshot does not present the non-empty team as empty",
);
