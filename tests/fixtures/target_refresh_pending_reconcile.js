const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const renderPath = process.argv[3];
const lanesPath = process.argv[4];
const statusWrites = [];
const lifetimeCalls = [];
const renderedPayloads = [];
const context = {
  console,
  Map,
  Set,
  observerModeEnabled: false,
  targetsLoaded: false,
  taskFilterInventoryRevision: "99",
  taskFilterStemPills: [],
  spiceMenuEl: null,
  uniqueStringList(values) {
    return Array.from(new Set(values || []));
  },
  laneGroupHost(lane) {
    return lane;
  },
  relativeAgeSeconds() {
    return null;
  },
  clearGlobalActivityStatus() {},
  renderFilterPills() {},
  syncFusedLaneChrome() {},
  syncComposerPlaceholders() {},
  renderLaneViewShell() {},
  applyServerLaneLifetime(lane, lifetime, options) {
    lifetimeCalls.push({ lifetime, configRevision: options.configRevision });
    lane.lifetime = lifetime;
    lane.serverLifetime = lifetime;
    return true;
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});
const laneStore = vm.runInContext("laneStore", context);
vm.runInContext(fs.readFileSync(renderPath, "utf8"), context, {
  filename: "app.render.js",
});
vm.runInContext(fs.readFileSync(lanesPath, "utf8"), context, {
  filename: "app.lanes.js",
});

context.setLaneStatus = (_lane, statusLine) => statusWrites.push(statusLine);
context.renderFilterPills = () => {};
context.renderLaneViewShell = () => {};
context.syncFusedLaneChrome = () => {};
context.syncComposerPlaceholders = () => {};
const canonicalRenderLaneChrome = context.renderLaneChrome;
context.renderLaneChrome = (lane, payload) => {
  renderedPayloads.push(payload);
  return canonicalRenderLaneChrome(lane, payload);
};

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const transcriptMessages = [{ key: "live-message", display_text: "live text" }];
const knownMessages = [...transcriptMessages];
const cachedPayload = {
  messages: transcriptMessages,
  statusLine: {
    agentProcessStatus: "running",
    preview: "cached live preview",
    pendingInboxCount: 2,
    pendingInboxKeys: ["live-a", "live-b"],
    pendingInboxRevision: "live-revision",
    pendingInboxVersion: 40,
  },
};
const staleMetrics = { completed: 1 };
const staleInfo = {
  summaryRows: [{ key: "thread", value: "thread-stale" }],
  members: [],
};
const lane = {
  targetId: "lane-a",
  emptyTeam: false,
  latestPayload: cachedPayload,
  knownMessages,
  branchName: "stale-branch",
  agentName: "stale-agent",
  driverName: "stale-driver",
  driverModel: "stale-model",
  driverEffort: "low",
  serveAgentIdentity: { actorId: "stale-actor" },
  targetThreadId: "thread-stale",
  activeThreadId: "thread-stale",
  teamId: "team-stale",
  teamRevision: 1,
  configRevision: 1,
  taskFilters: ["stale.filter"],
  effectiveTaskFilters: ["stale"],
  laneFilterVersion: "stale-filter-version",
  lifetime: "Drain",
  serverLifetime: "Drain",
  renewalIntent: { requested: false },
  laneMetrics: staleMetrics,
  laneInfo: staleInfo,
  privateTaskCount: 1,
  backendPendingInboxCount: 2,
  backendPendingInboxKeys: new Set(["live-a", "live-b"]),
  backendPendingInboxRevision: "live-revision",
  backendPendingInboxVersion: 40,
  backendPendingInboxKeysAuthoritative: true,
  optimisticPendingInboxCount: 2,
  optimisticSubmittedInboxKeys: new Set(),
  optimisticPendingInboxFloor: 0,
  pendingSubmissionCount: 0,
  lastRenderedStatusLine: cachedPayload.statusLine,
  pipEl: { dataset: {}, title: "" },
};
laneStore.registerLane(lane);

const refreshedMetrics = { completed: 9 };
const refreshedInfo = {
  summaryRows: [{ key: "thread", value: "thread-fresh" }],
  members: [{ actorId: "fresh-actor" }],
};
const refreshedRenewal = { requested: true };
const refreshedTarget = {
  id: lane.targetId,
  targetIdentity: {
    targetId: lane.targetId,
    worktreeName: "fresh-tree",
    branch: "fresh-branch",
    driver: { name: "target-driver", model: "target-model", effort: "high" },
    agent: { state: "configured", name: "fresh-agent" },
    thread: { state: "bound", threadId: "thread-fresh" },
  },
  serveAgentIdentity: {
    actorId: "fresh-actor",
    driver: {
      desired: "desired-driver",
      actual: "actual-driver",
      transcriptOwner: "transcript-driver",
    },
    thread: { state: "bound", threadId: "thread-fresh" },
    launch: {
      desired: { model: "desired-model", effort: "high" },
      actual: { model: "actual-model", effort: "medium" },
    },
  },
  taskFilters: ["serve.ui"],
  effectiveTaskFilters: ["serve"],
  laneFilterVersion: "fresh-filter-version",
  teamIdentity: {
    state: "member",
    teamId: "team-fresh",
    teamRevision: 2,
    configRevision: 2,
  },
  lifetime: "Drive",
  renewalIntent: refreshedRenewal,
  laneMetrics: refreshedMetrics,
  laneInfo: refreshedInfo,
  privateTaskCount: 7,
  statusLine: {
    agentProcessStatus: "idle",
    preview: "refreshed target status",
    pendingInboxCount: 0,
    pendingInboxKeys: [],
    pendingInboxRevision: "drained-revision",
    pendingInboxVersion: 41,
  },
};

context.applyTargetsPayload({
  workTrees: [refreshedTarget],
  observerErrors: [],
  taskFilterInventory: { revision: "1" },
});

assert(
  renderedPayloads.length === 1 && renderedPayloads[0] === refreshedTarget,
  "target refresh renders the canonical target payload directly",
);
assert(lane.branchName === "fresh-branch", "refresh updates target identity");
assert(lane.agentName === "fresh-agent", "refresh updates agent identity");
assert(
  lane.serveAgentIdentity === refreshedTarget.serveAgentIdentity,
  "refresh updates serve-agent identity",
);
assert(
  lane.driverName === "actual-driver -> desired-driver",
  "refresh updates live driver identity",
);
assert(lane.targetThreadId === "thread-fresh", "refresh updates thread identity");
assert(lane.teamId === "team-fresh", "refresh updates team identity");
assert(lane.teamRevision === 2, "refresh updates team revision");
assert(lane.configRevision === 2, "refresh updates team config revision");
assert(lane.taskFilters.join(",") === "serve.ui", "refresh updates task filters");
assert(
  lane.effectiveTaskFilters.join(",") === "serve",
  "refresh updates effective task filters",
);
assert(
  lane.laneFilterVersion === "fresh-filter-version",
  "refresh updates filter version",
);
assert(lane.lifetime === "Drive", "refresh updates lifetime");
assert(
  lifetimeCalls.length === 1 && lifetimeCalls[0].configRevision === 2,
  "refresh applies lifetime at the refreshed config revision",
);
assert(lane.renewalIntent === refreshedRenewal, "refresh updates renewal intent");
assert(lane.laneMetrics === refreshedMetrics, "refresh updates lane metrics");
assert(lane.laneInfo === refreshedInfo, "refresh updates lane info");
assert(lane.privateTaskCount === 7, "refresh updates private task count");
assert(
  lane.backendPendingInboxCount === 0 &&
    lane.backendPendingInboxRevision === "drained-revision",
  "refresh applies pending identity through canonical chrome rendering",
);
assert(
  statusWrites.at(-1).agentProcessStatus === "idle" &&
    statusWrites.at(-1).preview === "refreshed target status",
  "refresh updates non-pending lane status",
);
assert(
  lane.latestPayload === cachedPayload &&
    lane.latestPayload.messages === transcriptMessages,
  "refresh preserves the authoritative cached transcript payload",
);
assert(
  lane.knownMessages === knownMessages && lane.knownMessages[0].key === "live-message",
  "refresh preserves materialized live transcript state",
);
