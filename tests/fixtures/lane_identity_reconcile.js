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
  // Current backend snapshots send present-but-unbound identity fields this way;
  // the frontend must not retain old lane identity when the snapshot is present.
  targetBranch: "main-2",
  targetAgentName: "",
  targetThreadId: "",
  taskFilters: [],
  laneFilterVersion: "",
  teamId: "",
  teamRevision: 0,
  configRevision: 0,
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
