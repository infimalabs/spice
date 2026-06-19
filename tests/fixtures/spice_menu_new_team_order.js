const fs = require("fs");
const vm = require("vm");

const menuPath = process.argv[2];
const context = {
  console,
  spiceMenuEl: null,
  spiceMenuNewTeamPlacementHints: [],
  targetById: new Map(),
  targets: [],
  compareTargetChoices(left, right) {
    return context.targetChoiceName(left).localeCompare(
      context.targetChoiceName(right),
    );
  },
  targetChoiceName(target) {
    return String(target.branch || "");
  },
  teamIdentityTeamId(identity) {
    return String((identity || {}).teamId || "");
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(menuPath, "utf8"), context, {
  filename: "app.menu.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function assertOrder(actual, expected, message) {
  const actualText = actual.join(",");
  const expectedText = expected.join(",");
  assert(
    actualText === expectedText,
    message + "\nexpected: " + expectedText + "\nactual:   " + actualText,
  );
}

function target(id, branch, teamId = "") {
  return {
    id,
    branch,
    targetIdentity: { branch },
    teamIdentity: { teamId },
  };
}

function setTargets(items) {
  context.targets = items;
  context.targetById = new Map(items.map((item) => [item.id, item]));
}

function orderedMenuTeamIds() {
  const choices = context.targets
    .slice()
    .sort(context.compareSpiceMenuTargetChoices);
  return context.spiceMenuTeamGroups(choices).map((group) => {
    if (group.newTeam) return "new-team-drop";
    if (group.unassigned) return "unassigned";
    return group.teamId + ":" + group.targets.map((item) => item.id).join("+");
  });
}

setTargets([
  target("created", "aaa-created", "team-old"),
  target("existing", "zzz-existing", "team-existing"),
  target("loose", "mmm-loose"),
]);

context.moveTargetToMenuTeamOptimisticUi("__new_team_drop__", "created");
assertOrder(
  orderedMenuTeamIds(),
  [
    "team-existing:existing",
    "new-team:created:created",
    "new-team-drop",
    "unassigned",
  ],
  "optimistic new-team drop stays next to the drop zone",
);

setTargets([
  target("created", "aaa-created", "durable-created"),
  target("existing", "zzz-existing", "team-existing"),
  target("loose", "mmm-loose"),
]);
assertOrder(
  orderedMenuTeamIds(),
  [
    "team-existing:existing",
    "durable-created:created",
    "new-team-drop",
    "unassigned",
  ],
  "server-refreshed durable team stays next to the drop zone",
);
assert(
  context.spiceMenuNewTeamPlacementHints[0].teamId === "durable-created",
  "server-refreshed topology binds the placement hint to the durable team id",
);
