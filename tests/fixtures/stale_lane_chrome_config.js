const fs = require("fs");
const vm = require("vm");

const renderPath = process.argv[2];
const currentConfigRevision = 11;
const freshConfigRevision = 12;
const lifetimeCalls = [];
const statusWrites = [];
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

context.applyServerLaneLifetime = (lane, lifetime, options) => {
  lifetimeCalls.push({ lifetime, configRevision: options.configRevision });
  lane.lifetime = lifetime;
  lane.serverLifetime = lifetime;
  return true;
};
context.renderLaneViewShell = () => {};
context.renderFilterPills = () => {};
context.syncFusedLaneChrome = () => {};
context.syncComposerPlaceholders = () => {};
context.setLaneStatus = (_lane, statusLine) => {
  statusWrites.push(statusLine);
};

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function lane() {
  return {
    teamId: "team-a",
    teamRevision: 4,
    configRevision: currentConfigRevision,
    taskFilters: ["serve.ui"],
    laneFilterVersion: "v11",
    lifetime: "Drain",
    serverLifetime: "Drain",
    backendPendingInboxCount: 0,
    backendPendingInboxKeys: new Set(),
    backendPendingInboxRevision: "",
    backendPendingInboxVersion: 0,
    backendPendingInboxKeysAuthoritative: false,
    optimisticPendingInboxCount: 0,
    optimisticSubmittedInboxKeys: new Set(),
    optimisticPendingInboxFloor: 0,
    pendingSubmissionCount: 0,
    knownMessages: [],
    pipEl: { dataset: {}, title: "" },
  };
}

const guarded = lane();
const currentGuardedMetrics = { completed: 3 };
guarded.laneMetrics = currentGuardedMetrics;
context.renderLaneChrome(guarded, {
  teamIdentity: {
    state: "member",
    teamId: "team-a",
    teamRevision: 3,
    configRevision: 10,
  },
  taskFilters: ["old.filter"],
  laneFilterVersion: "v10",
  lifetime: "Drive",
  laneMetrics: { completed: 2 },
  statusLine: {
    agentProcessStatus: "idle",
    pendingInboxCount: 1,
    pendingInboxKeys: ["queued"],
    pendingInboxRevision: "rev-queued",
    pendingInboxVersion: 20,
  },
});

assert(
  guarded.configRevision === currentConfigRevision,
  "stale config revision is not applied",
);
assert(guarded.teamRevision === 4, "stale team revision is not applied");
assert(
  guarded.taskFilters.join(",") === "serve.ui",
  "stale task filters do not rewind filter badge state",
);
assert(guarded.laneFilterVersion === "v11", "stale filter version is ignored");
assert(guarded.lifetime === "Drain", "stale lifetime does not rewind slider");
assert(
  guarded.laneMetrics === currentGuardedMetrics,
  "stale chrome preserves the on-demand lane metrics snapshot",
);
assert(lifetimeCalls.length === 0, "stale lifetime is not handed to controls");
assert(
  guarded.backendPendingInboxCount === 1,
  "non-config pending chrome still applies",
);
assert(statusWrites.at(-1).pendingInboxCount === 1, "status still renders");

context.renderLaneChrome(guarded, {
  teamIdentity: { state: "none" },
  taskFilters: [],
  laneFilterVersion: "",
  lifetime: "Steer",
  statusLine: {
    agentProcessStatus: "idle",
    pendingInboxCount: 0,
    pendingInboxKeys: [],
    pendingInboxRevision: "rev-clear",
    pendingInboxVersion: 21,
  },
});

assert(
  guarded.teamId === "team-a",
  "revisionless none team identity does not clear accepted team id",
);
assert(
  guarded.configRevision === currentConfigRevision,
  "revisionless none team identity does not clear accepted config revision",
);
assert(
  guarded.teamRevision === 4,
  "revisionless none team identity does not clear accepted team revision",
);
assert(
  guarded.taskFilters.join(",") === "serve.ui",
  "revisionless none payload does not clear accepted task filters",
);
assert(
  guarded.laneFilterVersion === "v11",
  "revisionless none payload does not clear accepted filter version",
);
assert(
  guarded.lifetime === "Drain",
  "revisionless none payload does not clear accepted lifetime",
);
assert(lifetimeCalls.length === 0, "revisionless none lifetime is ignored");
assert(
  guarded.backendPendingInboxCount === 0,
  "revisionless stale chrome still applies non-config pending state",
);
assert(statusWrites.at(-1).pendingInboxCount === 0, "clear status still renders");

context.renderLaneChrome(guarded, {
  teamIdentity: {
    state: "member",
    teamId: "team-a",
    teamRevision: 5,
    configRevision: freshConfigRevision,
  },
  taskFilters: ["serve.api"],
  laneFilterVersion: "v12",
  lifetime: "Drive",
  statusLine: {
    agentProcessStatus: "idle",
    pendingInboxCount: 1,
    pendingInboxKeys: ["queued"],
    pendingInboxRevision: "rev-queued",
    pendingInboxVersion: 20,
  },
});

assert(guarded.configRevision === freshConfigRevision, "fresh config revision applies");
assert(guarded.teamRevision === 5, "fresh team revision applies");
assert(guarded.taskFilters.join(",") === "serve.api", "fresh filters apply");
assert(guarded.laneFilterVersion === "v12", "fresh filter version applies");
assert(guarded.lifetime === "Drive", "fresh lifetime applies");
assert(
  lifetimeCalls.at(-1).configRevision === freshConfigRevision,
  "fresh revision reaches controls",
);

const direct = lane();
const currentDirectMetrics = { completed: 6 };
direct.laneMetrics = currentDirectMetrics;
const eagerDirectMetrics = { completed: 7 };
const directInfo = {
  summaryRows: [{ key: "thread", value: "thread-direct" }],
  members: [],
};
const directRenewal = { requested: true };
context.renderLaneChrome(direct, {
  targetIdentity: {
    targetId: "target-direct",
    worktreeName: "direct-tree",
    branch: "direct-branch",
    driver: { name: "codex", model: "gpt-direct", effort: "high" },
    agent: { state: "configured", name: "agent-direct" },
    thread: { state: "bound", threadId: "thread-direct" },
  },
  taskFilters: ["serve.direct"],
  effectiveTaskFilters: ["serve"],
  laneFilterVersion: "direct-filter-version",
  teamIdentity: {
    state: "member",
    teamId: "team-direct",
    teamRevision: 8,
    configRevision: 20,
  },
  lifetime: "Drive",
  renewalIntent: directRenewal,
  laneMetrics: eagerDirectMetrics,
  laneInfo: directInfo,
  privateTaskCount: 4,
  statusLine: {
    agentProcessStatus: "running",
    pendingInboxCount: 2,
    pendingInboxKeys: ["direct-a", "direct-b"],
    pendingInboxRevision: "direct-revision",
    pendingInboxVersion: 30,
  },
});

assert(direct.branchName === "direct-branch", "direct identity reaches chrome");
assert(direct.agentName === "agent-direct", "direct agent identity reaches chrome");
assert(direct.teamId === "team-direct", "direct team identity reaches chrome");
assert(
  direct.taskFilters.join(",") === "serve.direct",
  "direct filters reach chrome",
);
assert(
  direct.effectiveTaskFilters.join(",") === "serve",
  "direct effective filters reach chrome",
);
assert(direct.lifetime === "Drive", "direct lifetime reaches chrome");
assert(direct.renewalIntent === directRenewal, "direct renewal intent reaches chrome");
assert(
  direct.laneMetrics === currentDirectMetrics,
  "direct chrome preserves the on-demand lane metrics snapshot",
);
assert(direct.laneInfo === directInfo, "direct info reaches chrome");
assert(direct.privateTaskCount === 4, "direct private count reaches chrome");
assert(
  statusWrites.at(-1).pendingInboxCount === 2,
  "direct status line reaches chrome",
);
