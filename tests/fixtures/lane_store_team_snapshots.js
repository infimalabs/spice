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

// Retention and membership are one decision. Starting an idle agent renews its
// thread, so the team names the member under a thread actor the lane and its
// target do not carry yet; that member is unresolvable for exactly one refresh.
// The store keeps such a lane open -- and must keep it in the run too. Dropping
// it takes every card it owns out of the fused host's merged stream, and a run
// that falls under two dissolves the fusion outright.
const retentionStore = new ServeLaneStore();
retentionStore.replaceTargets(["alpha", "beta", "gamma"].map(target));
for (const targetId of ["alpha", "beta", "gamma"])
  retentionStore.registerLane(lane(targetId));
retentionStore.applyLaneGroups([["alpha", "beta", "gamma"]], {
  isLaneOpen: () => {
    return true;
  },
});
const renewing = retentionStore.applyTeamSnapshot(
  snapshot(2, [
    {
      teamId: "main",
      members: [
        { agentId: "target:alpha" },
        { agentId: "thread:beta-next" },
        { agentId: "thread:gamma-next" },
      ],
    },
  ]),
  {
    resolveMember(member) {
      return member.agentId === "target:alpha" ? { targetId: "alpha" } : {};
    },
    unresolvedLaneTargetIds() {
      return ["beta", "gamma"];
    },
  },
);
assert(
  JSON.stringify(renewing.groupRuns) ===
    JSON.stringify([["alpha", "beta", "gamma"]]),
  "a transiently unresolved member keeps its prior seat in the team's run",
);

// Prior seat, not the tail: member order is composer order and drives accent
// colour, so a retained lane re-entering at the end would reshuffle and recolour
// the composers on every transient refresh.
const seatStore = new ServeLaneStore();
seatStore.replaceTargets(["alpha", "beta", "gamma"].map(target));
for (const targetId of ["alpha", "beta", "gamma"])
  seatStore.registerLane(lane(targetId));
seatStore.applyLaneGroups([["beta", "alpha", "gamma"]], {
  isLaneOpen: () => {
    return true;
  },
});
const seated = seatStore.applyTeamSnapshot(
  snapshot(2, [
    {
      teamId: "main",
      members: [
        { agentId: "thread:beta-next" },
        { agentId: "target:alpha" },
        { agentId: "target:gamma" },
      ],
    },
  ]),
  {
    resolveMember(member) {
      if (member.agentId === "target:alpha") return { targetId: "alpha" };
      if (member.agentId === "target:gamma") return { targetId: "gamma" };
      return {};
    },
    unresolvedLaneTargetIds() {
      return ["beta"];
    },
  },
);
assert(
  JSON.stringify(seated.groupRuns) === JSON.stringify([["beta", "alpha", "gamma"]]),
  "a retained lane re-enters at the seat it held, not at the tail",
);
