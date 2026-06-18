const fs = require("fs");
const vm = require("vm");

const renderPath = process.argv[2];
const context = {
  console,
  uniqueStringList(values) {
    return Array.from(new Set(values || []));
  },
  laneGroupHost(lane) {
    return lane;
  },
  relativeAgeSeconds() {
    return null;
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(renderPath, "utf8"), context, {
  filename: "app.render.js",
});

context.setLaneStatus = (lane, statusLine) => {
  lane.renderedStatusLine = statusLine;
};
context.syncLaneBackendPending = () => {};
context.renderLaneViewShell = () => {};
context.renderFilterPills = () => {};
context.syncFusedLaneChrome = () => {};
context.syncComposerPlaceholders = () => {};
context.updateLiveTargetChoiceMetadata = () => {};

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const lane = {
  branchName: "main-2",
  agentName: "main",
  targetThreadId: "main-thread",
  activeThreadId: "main-thread",
  teamId: "team-open",
  teamRevision: 2,
  configRevision: 2,
  taskFilters: ["serve"],
  laneFilterVersion: "stale",
  pipEl: { dataset: {}, title: "" },
};

context.renderLaneChrome(lane, {
  targetIdentity: {
    targetId: "main-2",
    worktreeName: "main-2",
    branch: "main-2",
    agent: { state: "unconfigured" },
    thread: { state: "unbound" },
  },
  taskFilters: [],
  laneFilterVersion: "",
  teamIdentity: { state: "none" },
  statusLine: { agentProcessStatus: "idle" },
});

assert(lane.branchName === "main-2", "branch remains bound to main-2");
assert(lane.agentName === "", "present target agent replaces stale main label");
assert(lane.targetThreadId === "", "present target thread replaces stale thread");
assert(lane.activeThreadId === "", "present target thread replaces active thread");
assert(lane.teamId === "", "present team id replaces stale team binding");
assert(lane.teamRevision === 0, "present team revision replaces stale revision");
assert(lane.configRevision === 0, "present config revision replaces stale revision");
assert(lane.taskFilters.length === 0, "present task filters replace stale filters");
assert(lane.laneFilterVersion === "", "present filter version replaces stale version");
assert(lane.pipEl.dataset.agentStatus === "idle", "idle status renders on lane");

function assertThrows(fn, expectedMessage) {
  try {
    fn();
  } catch (error) {
    assert(
      String(error && error.message).includes(expectedMessage),
      "unexpected error: " + error,
    );
    return;
  }
  throw new Error("expected error containing: " + expectedMessage);
}

assertThrows(
  () =>
    context.renderLaneChrome({ ...lane }, {
      targetIdentity: {
        targetId: "main-2",
        worktreeName: "main-2",
        branch: "main-2",
        agent: { state: "unconfigured" },
        thread: { state: "bound", threadId: "" },
      },
      statusLine: {},
    }),
  "thread id must be non-empty",
);

assertThrows(
  () =>
    context.renderLaneChrome({ ...lane }, {
      targetIdentity: {
        targetId: "main-2",
        worktreeName: "main-2",
        branch: "main-2",
        agent: { state: "unconfigured" },
        thread: { state: "unbound" },
      },
      teamIdentity: {
        state: "member",
        teamId: "",
        teamRevision: 1,
        configRevision: 1,
      },
      statusLine: {},
    }),
  "team id must be non-empty",
);
