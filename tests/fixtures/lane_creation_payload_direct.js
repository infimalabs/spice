const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const renderPath = process.argv[3];
const shellPath = process.argv[4];
const target = {
  id: "target-a",
  targetIdentity: {
    targetId: "target-a",
    worktreeName: "tree-a",
    branch: "main-a",
    driver: { name: "codex", model: "gpt", effort: "high" },
    agent: { state: "configured", name: "agent-a" },
    thread: { state: "bound", threadId: "thread-a" },
  },
  serveAgentIdentity: { actorId: "agent-a" },
  statusLine: { agentProcessStatus: "running" },
};
const rendered = [];
const identityCalls = [];
const teamCalls = [];
const context = {
  console,
  observerModeEnabled: false,
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});
vm.runInContext(fs.readFileSync(renderPath, "utf8"), context, {
  filename: "app.render.js",
});
vm.runInContext(fs.readFileSync(shellPath, "utf8"), context, {
  filename: "app.shell.js",
});
const laneStore = vm.runInContext("laneStore", context);
laneStore.replaceTargets([target]);
laneStore.applyLaneChrome({
  targetId: target.id,
  teamConfig: {
    authority: "team-store",
    order: { epoch: "", revision: 4 },
    value: {
      teamIdentity: {
        state: "member",
        teamId: "team-a",
        teamRevision: 3,
        configRevision: 4,
      },
    },
  },
});

context.createLaneShellElement = () => ({ classList: { add() {} } });
context.laneTargetIdentityFields = (
  emptyTeam,
  targetIdentity,
  serveAgentIdentity,
) => {
  identityCalls.push({ emptyTeam, targetIdentity, serveAgentIdentity });
  return {};
};
context.laneStreamState = () => ({ latestPayload: null });
context.laneControlState = () => ({});
context.laneTeamState = (_emptyTeam, teamIdentity) => {
  teamCalls.push(teamIdentity);
  return {};
};
context.laneBackendState = () => ({});
context.laneOverlayState = () => ({});
context.laneDraftState = () => ({});
context.lanePaneState = () => ({});
context.laneElementRefs = () => ({ historySentinelEl: { dataset: {} } });
context.wireLaneShell = () => {};
context.syncComposerShards = () => {};
context.syncLaneEffectiveControls = () => {};
context.renderLanePayloadPresentation = (lane, payload) =>
  rendered.push({ lane, payload });
context.renderLaneViewShell = () => {};

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const lane = context.createLaneState("target-a");
assert(lane.targetId === "target-a", "lane creation keeps its target id");
assert(rendered.length === 1, "lane creation renders chrome exactly once");
assert(
  rendered[0].payload === target,
  "lane creation passes the canonical inventory payload directly",
);
assert(identityCalls.length === 1, "lane creation reads identity exactly once");
assert(
  identityCalls[0].targetIdentity === target.targetIdentity,
  "lane creation reads identity from the canonical payload",
);
assert(
  identityCalls[0].serveAgentIdentity === target.serveAgentIdentity,
  "lane creation reads serve identity from the canonical payload",
);
assert(
  teamCalls[0].teamId === "team-a" && teamCalls[0].configRevision === 4,
  "lane creation reads team config from canonical chrome",
);
