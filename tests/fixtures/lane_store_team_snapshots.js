const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const context = { console };
vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});

const ServeLaneStore = vm.runInContext("ServeLaneStore", context);

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function target(id) {
  return { id };
}

function lane(targetId) {
  return { targetId };
}

function snapshot(revision, teams, changed = true) {
  return {
    revision,
    changed,
    snapshot: {
      globalRevision: revision,
      globalSettings: { fastMode: false },
      teams,
    },
  };
}

const revisionStore = new ServeLaneStore();
const revisionEvents = [];
revisionStore.subscribe((change) => {
  if (change.kind === "teamSnapshot")
    revisionEvents.push(change.transition.disposition);
});
const initial = revisionStore.applyTeamSnapshot(snapshot(5, []));
const stale = revisionStore.applyTeamSnapshot(snapshot(4, []), { force: true });
assert(
  JSON.stringify([
    initial.disposition,
    stale.disposition,
    stale.incomingRevision,
    stale.revision,
    revisionStore.teamSnapshotRevision(),
  ]) === JSON.stringify(["applied", "stale", 4, 5, 5]),
  "stale snapshots preserve the accepted store revision",
);
assert(
  JSON.stringify(revisionEvents) === JSON.stringify(["applied", "stale"]),
  "snapshot subscribers observe each disposition in order",
);

const forceStore = new ServeLaneStore();
const unchanged = forceStore.applyTeamSnapshot(snapshot(3, [], false));
const expectedCommand = forceStore.teamCommandPayload("mergeTeams", {
  sourceTeamId: "source",
});
const forced = forceStore.applyTeamSnapshot(
  snapshot(3, [{ teamId: "empty", members: [] }], false),
  { force: true },
);
assert(
  JSON.stringify([
    unchanged.disposition,
    unchanged.revision,
    expectedCommand,
    forced.disposition,
    forced.forced,
    forced.adds.map((change) => [change.kind, change.targetId, change.canClose]),
  ]) ===
    JSON.stringify([
      "unchanged",
      3,
      { command: "mergeTeams", expectedRevision: 3, sourceTeamId: "source" },
      "applied",
      true,
      [["emptyTeam", "empty-team:empty", false]],
    ]),
  "unchanged snapshots advance command revision and force materializes topology",
);

const store = new ServeLaneStore();
store.replaceTargets(["a", "b", "c", "r", "u", "x"].map(target));
for (const targetId of ["a", "c", "r", "u", "x"])
  store.registerLane(lane(targetId));
let observedTransition = null;
store.subscribe((change) => {
  if (change.kind === "teamSnapshot") observedTransition = change.transition;
});
const transition = store.applyTeamSnapshot(
  snapshot(9, [
    {
      teamId: "group",
      members: [{ agentId: "target:a" }, { agentId: "target:b" }],
    },
    { teamId: "empty", members: [] },
    { teamId: "unresolved", members: [{ agentId: "thread:missing" }] },
    { teamId: "renewal", members: [{ agentId: "thread:successor" }] },
  ]),
  {
    resolveMember(member) {
      if (member.agentId === "target:a") return { targetId: "a" };
      if (member.agentId === "target:b") return { targetId: "b" };
      if (member.agentId === "thread:successor")
        return {
          targetId: "r",
          actorId: member.agentId,
          threadId: "successor",
        };
      return {};
    },
    unresolvedLaneTargetIds(team) {
      return team.teamId === "unresolved" ? ["c"] : [];
    },
    canRemoveLane(candidate) {
      return candidate.targetId !== "u";
    },
  },
);

assert(observedTransition === transition, "subscriber receives transition identity");
assert(Object.isFrozen(transition), "published transition is immutable");
assert(
  JSON.stringify({
    disposition: transition.disposition,
    revision: transition.revision,
    adds: transition.adds.map((change) => [change.kind, change.targetId]),
    updates: transition.updates.map((change) => [change.kind, change.targetId]),
    removes: transition.removes.map((candidate) => candidate.targetId),
    retained: transition.retained.map((candidate) => candidate.targetId),
    renewals: transition.renewals.map((renewal) => [
      renewal.targetId,
      renewal.actorId,
      renewal.threadId,
    ]),
    groups: transition.groupRuns,
    desired: transition.desiredTargetIds,
  }) ===
    JSON.stringify({
      disposition: "applied",
      revision: 9,
      adds: [
        ["member", "b"],
        ["emptyTeam", "empty-team:empty"],
      ],
      updates: [
        ["member", "a"],
        ["member", "r"],
      ],
      removes: ["x"],
      retained: ["u"],
      renewals: [["r", "thread:successor", "successor"]],
      groups: [["a", "b"]],
      desired: ["a", "b", "empty-team:empty", "c", "r"],
    }),
  "team reconciliation returns the exact materialization change set",
);
