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
  taskFilters: ["serve.ui"],
  laneFilterVersion: "stale",
  pipEl: { dataset: {}, title: "" },
};

context.renderLaneChrome(lane, {
  targetIdentity: {
    targetId: "main-2",
    worktreeName: "main-2",
    branch: "main-2",
    driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
    agent: { state: "unconfigured" },
    thread: { state: "unbound" },
  },
  serveAgentIdentity: {
    actorId: "target:main-2",
    driver: { desired: "codex", actual: "", transcriptOwner: "" },
    launch: {
      desired: { model: "gpt-5.5", effort: "xhigh" },
      actual: { model: "", effort: "", serviceTier: "", source: "" },
    },
    renewal: { state: "none" },
    target: {},
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
assert(lane.teamId === "team-open", "stale none does not clear accepted team id");
assert(lane.teamRevision === 2, "stale none does not clear accepted team revision");
assert(lane.configRevision === 2, "stale none does not clear accepted config revision");
assert(lane.taskFilters.join(",") === "serve.ui", "stale none keeps accepted filters");
assert(lane.laneFilterVersion === "stale", "stale none keeps accepted filter version");
assert(lane.pipEl.dataset.agentStatus === "idle", "idle status renders on lane");
assert(lane.driverName === "codex", "unbound driver stays compact");
assert(lane.driverModel === "gpt-5.5", "unbound model stays compact");
assert(lane.driverEffort === "xhigh", "unbound effort stays compact");
assert(lane.driverIconName === "codex", "unbound driver icon uses desired driver");

context.renderLaneChrome(lane, {
  targetIdentity: {
    targetId: "main-2",
    worktreeName: "main-2",
    branch: "main-2",
    driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
    agent: { state: "unconfigured" },
    thread: { state: "bound", threadId: "thread-b" },
  },
  serveAgentIdentity: {
    actorId: "thread:thread-b",
    driver: {
      desired: "codex",
      actual: "claude",
      transcriptOwner: "claude",
    },
    launch: {
      desired: { model: "gpt-5.5", effort: "xhigh" },
      actual: { model: "claude-opus", effort: "low", serviceTier: "", source: "agent state" },
    },
    renewal: { state: "requested" },
    target: {},
    thread: { state: "bound", threadId: "thread-b" },
  },
  teamIdentity: { state: "none" },
  statusLine: { agentProcessStatus: "running" },
});

assert(lane.driverName === "claude -> codex", "driver mismatch renders actual to desired");
assert(lane.driverModel === "claude-opus -> gpt-5.5", "model mismatch renders actual to desired");
assert(lane.driverEffort === "low -> xhigh", "effort mismatch renders actual to desired");
assert(lane.driverIconName === "claude", "driver icon uses actual driver");
assert(lane.driverTranscriptOwner === "claude", "transcript owner is retained");
assert(lane.targetThreadId === "thread-b", "serve identity thread updates target thread");

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
        driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
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
        driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
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
