const fs = require("fs");
const vm = require("vm");

const shellPath = process.argv[2];
const target = {
  targetIdentity: {
    targetId: "target-a",
    worktreeName: "tree-a",
    branch: "main-a",
    driver: { name: "codex", model: "gpt", effort: "high" },
    agent: { state: "configured", name: "agent-a" },
    thread: { state: "bound", threadId: "thread-a" },
  },
  serveAgentIdentity: { actorId: "agent-a" },
  teamIdentity: { state: "member", teamId: "team-a" },
  statusLine: { agentProcessStatus: "running" },
};
const rendered = [];
const identityCalls = [];
const context = {
  console,
  targetById: new Map([["target-a", target]]),
  observerModeEnabled: false,
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(shellPath, "utf8"), context, {
  filename: "app.shell.js",
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
context.laneTeamState = () => ({});
context.laneBackendState = () => ({});
context.laneOverlayState = () => ({});
context.laneDraftState = () => ({});
context.lanePaneState = () => ({});
context.laneElementRefs = () => ({ historySentinelEl: { dataset: {} } });
context.wireLaneShell = () => {};
context.syncComposerShards = () => {};
context.syncLaneEffectiveControls = () => {};
context.renderLaneChrome = (lane, payload) => rendered.push({ lane, payload });

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
