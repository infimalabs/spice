const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const renderPath = process.argv[3];
const lanesPath = process.argv[4];
const renderedFilterPaneTargetIds = [];
const context = {
  console,
  taskFilterInventoryRevision: "",
  taskFilterStemPills: [],
  filterStripEl: null,
  uniqueStringList(items) {
    return Array.from(new Set((items || []).filter(Boolean)));
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
  renderLaneFiltersPane(lane) {
    renderedFilterPaneTargetIds.push(lane.targetId);
  },
  renderLaneViewBadge() {},
  renderFilterPills() {},
  syncComposerPlaceholders() {},
  syncComposerShards() {},
  syncFusedLaneLights() {},
  syncFusedLaneStatusLine() {},
  syncLaneTeamMenuButton() {},
  applyServerLaneLifetime() {},
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
laneStore.replaceTargets([{ id: "a" }, { id: "b" }]);
laneStore.registerLane({ targetId: "a" });
laneStore.registerLane({ targetId: "b" });

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function inventory(revision, count) {
  return {
    revision,
    catalog: { approvedStems: ["serve"] },
    primaryStems: count
      ? [{ name: "serve", openTaskCount: count, filters: ["serve.ui"] }]
      : [],
  };
}

function taskBoardChrome(targetId, revision, count) {
  return {
    targetId,
    taskBoard: {
      authority: "task-board",
      order: { epoch: revision, revision: 0 },
      value: {
        taskFilters: ["serve.ui"],
        taskFilterEntries: [{ project: "serve.ui", sources: ["manual"] }],
        effectiveTaskFilters: ["serve"],
        taskFilterInventory: inventory(revision, count),
        privateTaskCount: 0,
      },
    },
  };
}

function pillSignature() {
  return (
    context.taskFilterStemPills
      .map((stem) => stem.name + ":" + stem.openTaskCount)
      .join(",") || "empty"
  );
}

const first = laneStore.applyLaneChrome(
  taskBoardChrome("a", "90071992547409931234", 1),
);
assert(first.disposition === "applied", "initial task-board facet applies");
assert(pillSignature() === "serve:1", "initial pill is visible");
assert(
  laneStore.laneChrome("a").taskBoard.taskFilterInventory.revision ===
    "90071992547409931234",
  "canonical target record owns inventory",
);
assert(
  !("taskFilterInventory" in laneStore.laneForId("a")) &&
    !("taskFilterInventory" in laneStore.targetForId("a")),
  "lane and target retain no inventory copies",
);
const firstRecord = laneStore.laneChrome("a");
const rendersAfterFirst = renderedFilterPaneTargetIds.length;

const duplicate = laneStore.applyLaneChrome(
  taskBoardChrome("a", "90071992547409931234", 1),
);
assert(duplicate.disposition === "stale", "duplicate task-board facet is stale");
assert(
  laneStore.laneChrome("a") === firstRecord,
  "duplicate inventory preserves canonical record identity",
);
assert(
  renderedFilterPaneTargetIds.length === rendersAfterFirst,
  "duplicate inventory rerenders no pane",
);

const newer = laneStore.applyLaneChrome(
  taskBoardChrome("a", "90071992547409931235", 0),
);
assert(newer.disposition === "applied", "newer empty inventory applies");
assert(pillSignature() === "empty", "newer inventory removes pill");
const rendersAfterNewer = renderedFilterPaneTargetIds.length;

const reordered = laneStore.applyLaneChrome(
  taskBoardChrome("a", "90071992547409931234", 1),
);
assert(reordered.disposition === "stale", "reordered older inventory is stale");
assert(pillSignature() === "empty", "older inventory cannot resurrect pill");
assert(
  renderedFilterPaneTargetIds.length === rendersAfterNewer,
  "reordered inventory rerenders no pane",
);

laneStore.applyLaneChrome(
  taskBoardChrome("b", "90071992547409931235", 0),
);
assert(
  context.taskFilterInventoryRevision === "90071992547409931235",
  "same global inventory on another target does not rewind freshness",
);
