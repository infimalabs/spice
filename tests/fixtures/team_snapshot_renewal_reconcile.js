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

function canonicalThreadActorId(threadId) {
  return String(threadId || "")
    .trim()
    .replaceAll("-", "")
    .toLowerCase();
}

function resetGlobals() {
  context.teamSnapshotRevision = 0;
  context.browserStorage = () => null;
  context.canonicalThreadActorId = canonicalThreadActorId;
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
  threadId: "predecessor-thread",
  teamId: "team-1",
};
const renamedLane = {
  targetId: "target-1",
  targetThreadId: "successor-thread",
  activeThreadId: "successor-thread",
  teamId: "team-1",
  sendAwaitingBackendCount: 0,
  element: { remove() {} },
};
context.targets = [staleTarget];
context.targetById = new Map([[staleTarget.id, staleTarget]]);
context.laneStates = new Map([[renamedLane.targetId, renamedLane]]);

applySnapshot(renewalTeam("successor-thread"));

assert(
  context.laneStates.get("target-1") === renamedLane,
  "renewed successor stays attached to the existing worktree lane",
);
assert(
  staleTarget.threadId === "successor-thread",
  "stale target inventory thread id is renamed in place to the successor",
);
assert(
  renamedLane.occupantThreadId === "successor-thread",
  "renewed lane occupant follows the successor thread",
);
assert(
  context.emptyTeamCalls.length === 0,
  "mapped renewal snapshot does not create an empty-team placeholder",
);

resetGlobals();
const pendingTarget = {
  id: "target-1",
  threadId: "predecessor-thread",
  teamId: "team-1",
};
const pendingLane = {
  targetId: "target-1",
  targetThreadId: "predecessor-thread",
  activeThreadId: "predecessor-thread",
  teamId: "team-1",
  sendAwaitingBackendCount: 1,
  element: { remove() {} },
};
context.targets = [pendingTarget];
context.targetById = new Map([[pendingTarget.id, pendingTarget]]);
context.laneStates = new Map([[pendingLane.targetId, pendingLane]]);

applySnapshot(renewalTeam("successor-thread"));

assert(
  context.laneStates.get("target-1") === pendingLane,
  "early renewal snapshot keeps the existing lane instead of closing it",
);
assert(
  pendingLane.targetThreadId === "successor-thread",
  "early renewal snapshot renames the lane thread id in place",
);
assert(
  pendingTarget.threadId === "successor-thread",
  "early renewal snapshot renames stale target inventory in place",
);
assert(
  context.emptyTeamCalls.length === 0,
  "early renewal snapshot does not present the non-empty team as empty",
);
