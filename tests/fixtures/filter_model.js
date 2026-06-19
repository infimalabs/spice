const fs = require("fs");
const vm = require("vm");

const source = fs.readFileSync(process.argv[2], "utf8");
const context = { console };
vm.createContext(context);
vm.runInContext(source, context, { filename: "app.filter-model.js" });

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function names(values) {
  return [...values].sort().join(",");
}

const inventory = {
  filters: [
    { name: "serve.ui", primaryStem: "serve", openTaskCount: 2 },
    { name: "task.review", primaryStem: "task", openTaskCount: 1 },
    { name: "task.config", primaryStem: "task", openTaskCount: 3 },
  ],
  primaryStems: [
    { name: "serve", openTaskCount: 2, filters: ["serve.ui"] },
    { name: "task", openTaskCount: 4, filters: ["task.review", "task.config"] },
  ],
};

assert(
  names(context.taskFilterEffectiveAssignedNames(inventory, ["serve"])) ===
    "serve,serve.ui",
  "stem assignment should cover concrete filters under that stem",
);
assert(
  names(context.availableTaskFilterNames(inventory, ["serve"])) ===
    "task.config,task.review",
  "available filters should exclude filters covered by assigned stems",
);
assert(
  context.availableTaskFilterOpenTaskCount(inventory, ["serve"]) === 4,
  "available open count should sum only uncovered concrete filters",
);
assert(
  context.taskFilterOpenCount(inventory, "task") === 4,
  "stem count should use primary stem counts",
);
assert(
  context.taskFilterOpenCount(inventory, "task.config") === 3,
  "concrete filter count should use exact filter rows",
);
assert(
  context.taskFilterOpenCount(inventory, "missing") === 0,
  "missing filters should count as zero",
);
assert(
  names(context.availableTaskFilterNames(inventory, ["task.review"])) ===
    "serve.ui,task.config",
  "exact assignments should not cover sibling filters",
);
