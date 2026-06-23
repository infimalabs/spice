const fs = require("fs");
const vm = require("vm");

const lanesPath = process.argv[2];
const context = {
  console,
  taskFilterInventoryRevision: "",
  taskFilterStemPills: [],
  uniqueStringList(items) {
    return Array.from(new Set((items || []).filter(Boolean)));
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

assert(
  context.applyTaskFilterInventory(inventory("90071992547409931234", 1)) === true,
  "initial inventory applies",
);
assert(context.taskFilterStemPills.length === 1, "initial pill is visible");

assert(
  context.applyTaskFilterInventory(inventory("90071992547409931235", 0)) === true,
  "newer empty inventory applies",
);
assert(context.taskFilterStemPills.length === 0, "newer inventory removes pill");

assert(
  context.applyTaskFilterInventory(inventory("90071992547409931234", 1)) === false,
  "older inventory is rejected",
);
assert(
  context.taskFilterInventoryRevision === "90071992547409931235",
  "older inventory does not rewind revision",
);
assert(
  context.taskFilterStemPills.length === 0,
  "older inventory cannot resurrect removed pill",
);
