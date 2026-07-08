const fs = require("fs");
const vm = require("vm");

const lanesPath = process.argv[2];
const storageKey = "spice.serve.laneConfigs";
const storage = {
  values: new Map([
    [
      storageKey,
      JSON.stringify([
        { targetId: "wt-a", open: true, speechMode: "speak", selectedView: "compose" },
        { targetId: "wt-b", open: true, speechMode: "speak", selectedView: "metrics" },
      ]),
    ],
  ]),
  getItem(key) {
    return this.values.has(key) ? this.values.get(key) : null;
  },
  setItem(key, value) {
    this.values.set(key, String(value));
  },
};

const context = {
  console,
  laneStorageKey: storageKey,
  speechModes: ["speak", "quiet"],
  defaultSpeechMode: "speak",
  defaultLaneViewMode: "compose",
  defaultAgentLifetime: "Drive",
  targets: [],
  targetById: new Map(),
  laneStates: new Map(),
  teamSnapshotRevision: 0,
  targetsLoaded: false,
  targetsLoading: false,
  targetsLoadPromise: null,
  taskFilterInventoryRevision: "",
  spiceMenuEl: null,
  filterStripEl: null,
  uniqueStringList(values) {
    return Array.from(new Set(values || []));
  },
  laneViewMode(value) {
    const modes = ["compose", "filters", "metrics", "info"];
    return modes.includes(value || "") ? value : "compose";
  },
  browserStorage() {
    return storage;
  },
  setGlobalActivityStatus() {},
  clearGlobalActivityStatus() {},
  setGlobalTransientError(message) {
    throw new Error(message);
  },
  renderFilterPills() {},
  applyGlobalSettingsPayload(settings) {
    if (!settings || settings.fastMode !== false)
      throw new Error("fresh startup fixture expected fast mode false");
  },
  reconcileLaneGroups(runs) {
    context.reconciledGroupRuns = runs;
  },
  isLaneOpen(lane) {
    return !lane.closed;
  },
  targetTeamAgentId(target) {
    return "target:" + target.id;
  },
  defaultTeamConfig() {
    return {
      lifetime: "Drive",
      speechMode: "speak",
      selectedView: "compose",
      taskFilters: [],
    };
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(lanesPath, "utf8"), context, {
  filename: "app.lanes.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function target(id) {
  return {
    id,
    targetIdentity: {
      targetId: id,
      worktreeName: id,
      branch: id,
      driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
      agent: { state: "unconfigured" },
      thread: { state: "unbound" },
    },
    teamIdentity: { state: "none" },
    statusLine: { agentProcessStatus: "idle" },
  };
}

context.ensureEmptyTeamLane = (team) => {
  const targetId = context.emptyTeamTargetId(team.teamId);
  context.laneStates.set(targetId, {
    targetId,
    emptyTeam: true,
    closed: false,
    teamId: team.teamId,
    speechMode: "speak",
    selectedView: "compose",
  });
};

context.closeLaneCore = (lane) => {
  lane.closed = true;
  context.laneStates.delete(lane.targetId);
};

context.liveBusRequest = async (type, request = {}) => {
  if (type === "targets.refresh")
    return {
      payload: {
        workTrees: [target("wt-a"), target("wt-b")],
        taskFilterInventory: {},
      },
    };
  if (type === "teams.refresh")
    return {
      payload: {
        revision: 1,
        changed: true,
        snapshot: {
          globalRevision: 1,
          globalSettings: { fastMode: false },
          teams: [
            {
              teamId: "team-shell",
              revision: 1,
              config: {},
              splitBack: {},
              members: [],
            },
          ],
        },
      },
    };
  if (type === "teams.command") {
    const member = String(((request.payload || {}).members || [])[0] || "");
    const targetId = member.startsWith("target:") ? member.slice(7) : member;
    context.laneStates.set(targetId, {
      targetId,
      emptyTeam: false,
      closed: false,
      speechMode: "speak",
      selectedView: "compose",
    });
    return { result: { ok: true } };
  }
  throw new Error("unexpected live bus request: " + type);
};

(async () => {
  await context.refreshServerTopology();
  const topology = Array.from(context.laneStates.values()).map((lane) => ({
    targetId: lane.targetId,
    emptyTeam: lane.emptyTeam === true,
  }));
  assert(
    JSON.stringify(topology) ===
      JSON.stringify([{ targetId: "empty-team:team-shell", emptyTeam: true }]),
    "fresh startup topology must be exactly the import shell",
  );
  assert(
    storage.getItem(storageKey) === "[]",
    "fresh startup persisted lane config must be the exact empty list",
  );
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
