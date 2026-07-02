const fs = require("fs");
const vm = require("vm");

const lanesPath = process.argv[2];
const targetA = { id: "a", taskFilterInventory: { revision: "0" } };
const targetB = { id: "b", taskFilterInventory: { revision: "0" } };
const laneA = { targetId: "a", taskFilterInventory: { revision: "0" } };
const laneB = { targetId: "b", taskFilterInventory: { revision: "0" } };
const renderedFilterPaneTargetIds = [];
const context = {
  console,
  targets: [targetA, targetB],
  targetById: new Map([
    ["a", targetA],
    ["b", targetB],
  ]),
  laneStates: new Map([
    ["a", laneA],
    ["b", laneB],
  ]),
  taskFilterInventoryRevision: "",
  taskFilterStemPills: [],
  uniqueStringList(items) {
    return Array.from(new Set((items || []).filter(Boolean)));
  },
  laneGroupHost(lane) {
    return lane;
  },
  renderLaneFiltersPane(lane) {
    renderedFilterPaneTargetIds.push(lane.targetId);
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(lanesPath, "utf8"), context, {
  filename: "app.lanes.js",
});

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

function pillSignature() {
  return (
    context.taskFilterStemPills
      .map((stem) => stem.name + ":" + stem.openTaskCount)
      .join(",") || "empty"
  );
}

function acceptedStateSignature() {
  return context.taskFilterInventoryRevision + "|" + pillSignature();
}

assert(
  context.applyTaskFilterInventory(inventory("90071992547409931234", 1)) === true,
  "initial inventory applies",
);
assert(pillSignature() === "serve:1", "initial pill is visible");
assert(
  laneA.taskFilterInventory.revision === "90071992547409931234" &&
    laneB.taskFilterInventory.revision === "90071992547409931234",
  "initial inventory syncs to every open lane",
);
assert(
  targetA.taskFilterInventory.revision === "90071992547409931234" &&
    targetB.taskFilterInventory.revision === "90071992547409931234",
  "initial inventory syncs to every target cache entry",
);
assert(
  renderedFilterPaneTargetIds.join(",") === "a,b",
  "initial inventory repaints every lane filter pane",
);

assert(
  context.applyTaskFilterInventory(inventory("90071992547409931235", 0)) === true,
  "newer empty inventory applies",
);
assert(pillSignature() === "empty", "newer inventory removes pill");
assert(
  laneA.taskFilterInventory.revision === "90071992547409931235" &&
    laneB.taskFilterInventory.revision === "90071992547409931235",
  "newer inventory replaces every open lane inventory",
);
assert(
  targetA.taskFilterInventory.revision === "90071992547409931235" &&
    targetB.taskFilterInventory.revision === "90071992547409931235",
  "newer inventory replaces every target cache entry",
);
assert(
  renderedFilterPaneTargetIds.join(",") === "a,b,a,b",
  "newer inventory repaints every lane filter pane again",
);

const acceptedEmptyState = acceptedStateSignature();
context.applyTaskFilterInventory(inventory("90071992547409931234", 1));
assert(
  acceptedStateSignature() === acceptedEmptyState,
  "older inventory preserves accepted empty state",
);
assert(
  laneA.taskFilterInventory.revision === "90071992547409931235" &&
    laneB.taskFilterInventory.revision === "90071992547409931235",
  "older inventory does not restore stale lane inventory",
);
assert(
  renderedFilterPaneTargetIds.join(",") === "a,b,a,b",
  "older inventory does not repaint filter panes",
);
