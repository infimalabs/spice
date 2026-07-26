const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const renderPath = process.argv[3];
const freshRevision = 12;
const renders = { pending: 0, filters: 0, lifetime: 0, team: 0 };
const context = {
  console,
  laneGroupHost(lane) {
    return lane;
  },
  laneIsFusedHost() {
    return false;
  },
  laneGroupMemberLanes(lane) {
    return [lane];
  },
  applyTaskFilterInventory() {
    return false;
  },
  renderLaneFiltersPane() {
    renders.filters += 1;
  },
  renderLaneViewBadge(_lane, view) {
    if (view === "compose") renders.pending += 1;
  },
  renderFilterPills() {},
  syncComposerPlaceholders() {},
  syncComposerShards() {},
  syncFusedLaneLights() {},
  syncFusedLaneStatusLine() {},
  syncLaneTeamMenuButton() {
    renders.team += 1;
  },
  applyServerLaneLifetime(lane, lifetime) {
    renders.lifetime += 1;
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
const laneStore = vm.runInContext("laneStore", context);

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const fusion = { hostTargetId: "lane-a", members: ["lane-a", "lane-b"] };
const draft = { value: "do not replace" };
const focus = { id: "composer-a" };
const lane = {
  targetId: "lane-a",
  lifetime: "Drive",
  fusion,
  draft,
  focus,
  optimisticPendingInboxCount: 0,
  optimisticSubmittedInboxKeys: new Set(),
  optimisticPendingInboxFloor: 0,
  pendingSubmissionCount: 0,
};
laneStore.registerLane(lane);

function chrome({ teamRevision, boardEpoch, renewalRevision, pendingRevision, count }) {
  return {
    targetId: lane.targetId,
    teamConfig: {
      authority: "team-store",
      order: { epoch: "", revision: teamRevision },
      value: {
        teamIdentity: {
          state: "member",
          teamId: "team-a",
          teamRevision,
          configRevision: teamRevision,
        },
      },
    },
    taskBoard: {
      authority: "task-board",
      order: { epoch: boardEpoch, revision: 0 },
      value: {
        taskFilters: ["serve." + boardEpoch],
        taskFilterEntries: [],
        effectiveTaskFilters: ["serve"],
        taskFilterInventory: {
          revision: boardEpoch,
          catalog: { approvedStems: [] },
          primaryStems: [],
        },
        privateTaskCount: 0,
      },
    },
    renewal: {
      authority: "team-store",
      order: { epoch: "", revision: renewalRevision },
      value: {
        lifetime: renewalRevision >= freshRevision ? "Drain" : "Drive",
        renewalIntent: { revision: renewalRevision, requested: false },
      },
    },
    pendingInbox: {
      authority: "inbox",
      order: { epoch: "", revision: pendingRevision },
      value: { count, label: String(count), keys: count ? ["queued"] : [] },
    },
  };
}

laneStore.applyLaneChrome(
  chrome({
    teamRevision: 11,
    boardEpoch: "11",
    renewalRevision: 11,
    pendingRevision: 20,
    count: 1,
  }),
);
const accepted = laneStore.laneChrome(lane.targetId);
const rendersAfterInitial = { ...renders };

const mixed = laneStore.applyLaneChrome(
  chrome({
    teamRevision: 10,
    boardEpoch: "10",
    renewalRevision: 10,
    pendingRevision: 21,
    count: 0,
  }),
);
assert(mixed.disposition === "applied", "fresh pending can accompany stale config");
assert(
  mixed.changedFacets.join(",") === "pendingInbox",
  "only the fresh facet is reported changed",
);
assert(
  mixed.staleFacets.join(",") === "teamConfig,taskBoard,renewal",
  "reordered config facets are reported stale",
);
const afterMixed = laneStore.laneChrome(lane.targetId);
assert(
  afterMixed.teamConfig === accepted.teamConfig &&
    afterMixed.taskBoard === accepted.taskBoard &&
    afterMixed.renewal === accepted.renewal,
  "reordered facets cannot rewind canonical chrome",
);
assert(afterMixed.pendingInbox.count === 0, "fresh pending facet still applies");
assert(
  renders.filters === rendersAfterInitial.filters &&
    renders.lifetime === rendersAfterInitial.lifetime &&
    renders.team === rendersAfterInitial.team,
  "pending-only change rerenders no unrelated surface",
);
assert(renders.pending === rendersAfterInitial.pending + 1, "pending badge rerenders");

const mixedRecord = laneStore.laneChrome(lane.targetId);
const renderSignature = JSON.stringify(renders);
const duplicate = laneStore.applyLaneChrome(
  chrome({
    teamRevision: 10,
    boardEpoch: "10",
    renewalRevision: 10,
    pendingRevision: 21,
    count: 0,
  }),
);
assert(duplicate.disposition === "stale", "duplicate envelope is stale");
assert(
  laneStore.laneChrome(lane.targetId) === mixedRecord,
  "duplicate envelope preserves canonical record identity",
);
assert(JSON.stringify(renders) === renderSignature, "duplicate rerenders nothing");

laneStore.applyLaneChrome(
  chrome({
    teamRevision: freshRevision,
    boardEpoch: String(freshRevision),
    renewalRevision: freshRevision,
    pendingRevision: 22,
    count: 2,
  }),
);
const fresh = laneStore.laneChrome(lane.targetId);
assert(
  fresh.teamConfig.teamIdentity.configRevision === freshRevision,
  "fresh team applies",
);
assert(
  fresh.taskBoard.taskFilters[0] === "serve." + freshRevision,
  "fresh board applies",
);
assert(fresh.renewal.lifetime === "Drain", "fresh renewal applies");
assert(fresh.pendingInbox.count === 2, "fresh pending applies");
assert(
  laneStore.laneForId(lane.targetId) === lane &&
    lane.fusion === fusion &&
    lane.draft === draft &&
    lane.focus === focus,
  "facet reconciliation preserves lane, fusion, draft, and focus continuity",
);
assert(
  !("taskFilters" in lane) &&
    !("renewalIntent" in lane) &&
    !("backendPendingInboxCount" in lane),
  "lane contains no canonical field copies",
);
