const fs = require("fs");
const vm = require("vm");

const controlsPath = process.argv[2];
const context = {
  agentLifetimeLabels: ["Steer", "Drive", "Drain"],
  defaultAgentLifetime: "Drive",
  speechModes: ["quiet", "speak", "narrate"],
  defaultSpeechMode: "speak",
  laneGroupHost(lane) {
    return lane.groupHost || lane;
  },
  persistLaneHints() {},
  syncLaneEffectiveControls(lane) {
    lane.renderedLifetime = context.laneEffectiveLifetime(lane);
  },
  renderFilterPills() {},
  renderLaneViewShell() {},
  syncNarrationMediaSession() {},
  updateTaskDrainForLane(lane) {
    lane.taskDrainCalls.push({
      lifetime: context.laneEffectiveLifetime(lane),
      requestId: lane.lifetimeRequestId,
      pendingRequestId: lane.pendingLifetimeRequestId,
    });
  },
  requestTeamCommand(payload) {
    context.teamCommands.push(payload);
    return Promise.resolve({});
  },
  teamCommandPayload(command, fields = {}) {
    return { command, ...fields };
  },
  setLaneTransientStatus(lane, message) {
    lane.transientStatus = message;
  },
  teamCommands: [],
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(controlsPath, "utf8"), context);
context.syncLaneEffectiveControls = function syncLaneEffectiveControls(lane) {
  lane.renderedLifetime = context.laneEffectiveLifetime(lane);
};

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function lane() {
  return {
    lifetime: "Drive",
    serverLifetime: "Drive",
    speechMode: "speak",
    configRevision: 10,
    pendingLifetimeCommit: "",
    pendingLifetimeConfigRevision: 0,
    pendingLifetimeRequestId: 0,
    lifetimeRequestId: 0,
    taskDrainCalls: [],
  };
}

const staleMismatch = lane();
context.setLaneLifetime(staleMismatch, "Drain");
assert(staleMismatch.lifetime === "Drain", "local choice renders immediately");
assert(
  staleMismatch.pendingLifetimeCommit === "Drain",
  "local choice remains pending",
);
const mismatchApplied = context.applyServerLaneLifetime(staleMismatch, "Drive", {
  configRevision: 11,
});
assert(mismatchApplied === false, "stale mismatch is ignored");
assert(staleMismatch.lifetime === "Drain", "stale mismatch does not rewind");
assert(
  staleMismatch.pendingLifetimeCommit === "Drain",
  "stale mismatch does not clear pending state",
);

const staleSame = lane();
context.setLaneLifetime(staleSame, "Drain");
const staleSameRequest = staleSame.pendingLifetimeRequestId - 1;
context.applyServerLaneLifetime(staleSame, "Drain", {
  requestId: staleSameRequest,
});
assert(
  staleSame.pendingLifetimeCommit === "Drain",
  "same lifetime with stale request id does not settle",
);

const currentMatch = lane();
context.setLaneLifetime(currentMatch, "Steer");
context.applyServerLaneLifetime(currentMatch, "Steer", {
  configRevision: 11,
  requestId: currentMatch.pendingLifetimeRequestId,
  supersedePending: false,
});
assert(currentMatch.lifetime === "Steer", "matching response keeps selection");
assert(currentMatch.pendingLifetimeCommit === "", "matching response settles");

currentMatch.configRevision = 11;
currentMatch.serverLifetime = "Steer";
const settledStaleApplied = context.applyServerLaneLifetime(currentMatch, "Drive", {
  configRevision: 10,
});
assert(settledStaleApplied === false, "settled stale revision is ignored");
assert(
  currentMatch.lifetime === "Steer",
  "settled stale revision does not rewind lifetime",
);
assert(
  currentMatch.serverLifetime === "Steer",
  "settled stale revision does not rewind server lifetime",
);

const rollback = lane();
context.setLaneLifetime(rollback, "Drain");
context.rollbackLaneLifetimeCommit(rollback, "Drain", "Drive", {
  requestId: rollback.pendingLifetimeRequestId,
});
assert(rollback.lifetime === "Drive", "forced rollback can restore server state");
assert(rollback.pendingLifetimeCommit === "", "forced rollback clears pending");

const emptyTeam = lane();
emptyTeam.emptyTeam = true;
emptyTeam.teamId = "team-empty";
context.setLaneLifetime(emptyTeam, "Steer");
assert(emptyTeam.taskDrainCalls.length === 0, "empty team skips task drain");
assert(context.teamCommands.length === 1, "empty team uses team command");
assert(
  context.teamCommands[0].command === "updateTeamConfig",
  "empty team updates config",
);
assert(
  context.teamCommands[0].teamId === "team-empty",
  "empty team targets its team id",
);
assert(
  context.teamCommands[0].configPatch.lifetime === "Steer",
  "empty team sends selected lifetime",
);
assert(
  !Object.prototype.hasOwnProperty.call(
    context.teamCommands[0].configPatch,
    "taskFilters",
  ),
  "lifetime-only empty team update preserves filter provenance",
);
