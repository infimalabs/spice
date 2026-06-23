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
  context.applyTaskFilterInventory(inventory("90071992547409931235", 0)) === true,
  "newer empty inventory applies",
);
assert(pillSignature() === "empty", "newer inventory removes pill");

const acceptedEmptyState = acceptedStateSignature();
context.applyTaskFilterInventory(inventory("90071992547409931234", 1));
assert(
  acceptedStateSignature() === acceptedEmptyState,
  "older inventory preserves accepted empty state",
);
