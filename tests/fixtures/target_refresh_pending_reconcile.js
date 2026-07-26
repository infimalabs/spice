const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const renderPath = process.argv[3];
const lanesPath = process.argv[4];
const statusWrites = [];
const lifetimeCalls = [];
const errors = [];
const context = {
  console,
  Map,
  Set,
  observerModeEnabled: false,
  targetsLoaded: true,
  taskFilterInventoryRevision: "",
  taskFilterStemPills: [],
  spiceMenuEl: null,
  filterStripEl: null,
  uniqueStringList(values) {
    return Array.from(new Set(values || []));
  },
  laneGroupHost(lane) {
    return lane;
  },
  laneIsFusedHost() {
    return false;
  },
  laneGroupMemberLanes(lane) {
    return [lane];
  },
  relativeAgeSeconds() {
    return null;
  },
  clearGlobalActivityStatus() {},
  setGlobalTransientError(message) {
    errors.push(message);
  },
  renderFilterPills() {},
  renderLaneFiltersPane() {},
  renderLaneInfoPane() {},
  renderLaneViewBadge() {},
  syncFusedLaneLights() {},
  syncFusedLaneStatusLine() {},
  syncLaneTeamMenuButton() {},
  syncComposerShards() {},
  syncComposerPlaceholders() {},
  applyServerLaneLifetime(lane, lifetime, options) {
    lifetimeCalls.push({ lifetime, configRevision: options.configRevision });
    lane.lifetime = lifetime;
    return true;
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});
vm.runInContext(fs.readFileSync(renderPath, "utf8"), context, {
  filename: "app.render.js",
});
vm.runInContext(fs.readFileSync(lanesPath, "utf8"), context, {
  filename: "app.lanes.js",
});
const laneStore = vm.runInContext("laneStore", context);
context.setLaneStatus = (_lane, statusLine) => statusWrites.push(statusLine);
context.setGlobalTransientError = (message) => errors.push(message);

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const transcriptMessages = [{ key: "live-message", display_text: "live text" }];
const knownMessages = [...transcriptMessages];
const composerDraft = { text: "operator draft" };
const focusToken = { name: "focused textarea" };
const cachedPayload = {
  messages: transcriptMessages,
  statusLine: { agentProcessStatus: "running", preview: "cached preview" },
};
const currentMetrics = { completed: 1 };
const lane = {
  targetId: "lane-a",
  emptyTeam: false,
  latestPayload: cachedPayload,
  knownMessages,
  composerDraft,
  focusToken,
  branchName: "stale-branch",
  agentName: "stale-agent",
  targetThreadId: "thread-stale",
  activeThreadId: "thread-stale",
  teamId: "team-stale",
  lifetime: "Drain",
  laneMetrics: currentMetrics,
  laneInfo: { summaryRows: [], members: [] },
  optimisticPendingInboxCount: 2,
  optimisticSubmittedInboxKeys: new Set(["live-a", "live-b"]),
  optimisticPendingInboxFloor: 2,
  pendingSubmissionCount: 0,
  lastRenderedStatusLine: cachedPayload.statusLine,
  pipEl: { dataset: {}, title: "" },
};
laneStore.replaceTargets([
  {
    id: lane.targetId,
    targetIdentity: {
      branch: "stale-branch",
      driver: { name: "codex", model: "old", effort: "low" },
      agent: { state: "configured", name: "stale-agent" },
      thread: { state: "bound", threadId: "thread-stale" },
    },
  },
]);
laneStore.registerLane(lane);

const refreshedInfo = {
  summaryRows: [{ key: "thread", value: "thread-fresh" }],
  members: [{ actorId: "fresh-actor" }],
};
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
  laneInfo: refreshedInfo,
  statusLine: {
    agentProcessStatus: "idle",
    preview: "refreshed target status",
  },
  chrome: {
    targetId: lane.targetId,
    teamConfig: {
      authority: "team-store",
      order: { epoch: "", revision: 2 },
      value: {
        teamIdentity: {
          state: "member",
          teamId: "team-fresh",
          teamRevision: 2,
          configRevision: 2,
        },
      },
    },
    taskBoard: {
      authority: "task-board",
      order: { epoch: "2", revision: 2 },
      value: {
        taskFilters: ["serve.ui"],
        taskFilterEntries: [{ project: "serve.ui", sources: ["manual"] }],
        effectiveTaskFilters: ["serve"],
        taskFilterInventory: {
          revision: "2",
          catalog: { approvedStems: [] },
          primaryStems: [],
        },
        privateTaskCount: 7,
      },
    },
    renewal: {
      authority: "team-store",
      order: { epoch: "", revision: 2 },
      value: { lifetime: "Drive", renewalIntent: { requested: true } },
    },
    pendingInbox: {
      authority: "inbox",
      order: { epoch: "", revision: 41 },
      value: { count: 0, label: "0", keys: [] },
    },
    activity: {
      authority: "transcript",
      order: { epoch: "2026-07-26T05:00:00Z", revision: 0 },
      value: { lastAssistantAt: "2026-07-26T05:00:00Z" },
    },
  },
};

context.applyTargetsPayload({
  workTrees: [refreshedTarget],
  observerErrors: [],
});

const chrome = laneStore.laneChrome(lane.targetId);
const storedTarget = laneStore.targetForId(lane.targetId);
assert(laneStore.laneForId(lane.targetId) === lane, "refresh preserves lane object");
assert(lane.branchName === "fresh-branch", "refresh updates non-faceted identity");
assert(lane.agentName === "fresh-agent", "refresh updates agent identity");
assert(lane.targetThreadId === "thread-fresh", "refresh updates thread identity");
assert(chrome.teamConfig.teamIdentity.teamId === "team-fresh", "team is canonical");
assert(chrome.taskBoard.effectiveTaskFilters[0] === "serve", "board is canonical");
assert(chrome.renewal.lifetime === "Drive", "renewal is canonical");
assert(chrome.pendingInbox.count === 0, "pending identity is canonical");
assert(
  lifetimeCalls.length === 1 && lifetimeCalls[0].configRevision === 2,
  "renewal transition applies lifetime at canonical config revision",
);
assert(
  !("taskFilters" in lane) &&
    !("renewalIntent" in lane) &&
    !("backendPendingInboxCount" in lane),
  "lane retains no server chrome copies",
);
assert(
  !("chrome" in storedTarget) &&
    !("taskFilters" in storedTarget) &&
    !("teamIdentity" in storedTarget),
  "target inventory retains only descriptor and non-faceted identity state",
);
assert(lane.laneMetrics === currentMetrics, "on-demand metrics survive refresh");
assert(lane.laneInfo === refreshedInfo, "non-faceted lane info updates");
assert(
  lane.latestPayload === cachedPayload &&
    lane.knownMessages === knownMessages &&
    lane.composerDraft === composerDraft &&
    lane.focusToken === focusToken,
  "refresh preserves transcript, draft, and focus-local state",
);
assert(
  statusWrites.at(-1).pendingInboxCount === 0 &&
    statusWrites.at(-1).lastAssistantAt === "2026-07-26T05:00:00Z",
  "status presentation overlays canonical pending and activity",
);

const recordBeforeFailure = laneStore.laneChrome(lane.targetId);
context.applyTargetsPayload({
  workTrees: [],
  targetsDiscoveryErrors: ["discovery offline"],
});
assert(errors.at(-1) === "discovery offline", "discovery failure is surfaced");
assert(laneStore.laneForId(lane.targetId) === lane, "failure retains open lane");
assert(
  laneStore.laneChrome(lane.targetId) === recordBeforeFailure,
  "failure retains canonical chrome record",
);
