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

function lane(targetId) {
  return { targetId, closed: false, pendingLifetimeCommit: "" };
}

const store = new ServeLaneStore();
for (const targetId of ["a", "b", "c", "d"]) store.registerLane(lane(targetId));
const isLaneOpen = (candidate) => !candidate.closed;
const captureLaneState = (candidate) => ({
  pending: candidate.pendingLifetimeCommit,
});

const observed = [];
store.subscribe((change) => {
  if (change.kind === "laneGroups") observed.push(change.transition);
});

// Fusion selects the first member as host and roles every member; unfused lanes
// stay standalone (no topology entry).
store.laneForId("b").pendingLifetimeCommit = "day";
const fuse = store.applyLaneGroups([["a", "b"]], {
  isLaneOpen,
  captureLaneState,
});
assert(observed[0] === fuse, "subscriber receives the fusion transition identity");
assert(Object.isFrozen(fuse), "published lane-group transition is immutable");
assert(
  JSON.stringify({
    runs: fuse.runs,
    prior: fuse.priorLaneStateByTargetId.get("b"),
    a: store.laneGroupTopology("a"),
    b: store.laneGroupTopology("b"),
    c: store.laneGroupTopology("c"),
  }) ===
    JSON.stringify({
      runs: [{ hostTargetId: "a", memberTargetIds: ["a", "b"] }],
      prior: { pending: "day" },
      a: { role: "host", hostTargetId: "a", memberTargetIds: ["a", "b"] },
      b: { role: "member", hostTargetId: "a", memberTargetIds: ["a", "b"] },
      c: null,
    }),
  "fusion owns host/member roles, captures pre-transition state, leaves the rest standalone",
);

// Reordering the run keeps the previously chosen host stable while adopting the
// new member ordering.
const reorder = store.applyLaneGroups([["b", "a"]], {
  isLaneOpen,
  captureLaneState,
});
assert(
  JSON.stringify({
    runs: reorder.runs,
    role: store.laneGroupTopology("a").role,
    order: store.laneGroupTopology("b").memberTargetIds,
  }) ===
    JSON.stringify({
      runs: [{ hostTargetId: "a", memberTargetIds: ["b", "a"] }],
      role: "host",
      order: ["b", "a"],
    }),
  "reorder preserves the stable host and adopts the new member order",
);

// Growth keeps the stable host and appends the joining member.
const grow = store.applyLaneGroups([["b", "a", "c"]], {
  isLaneOpen,
  captureLaneState,
});
assert(
  JSON.stringify({
    runs: grow.runs,
    c: store.laneGroupTopology("c"),
  }) ===
    JSON.stringify({
      runs: [{ hostTargetId: "a", memberTargetIds: ["b", "a", "c"] }],
      c: { role: "member", hostTargetId: "a", memberTargetIds: ["b", "a", "c"] },
    }),
  "growth keeps the stable host and rolls the new member into the group",
);

// A closed lane is filtered out; a run that falls below two live members is not
// a group, while an independent valid run still forms.
store.laneForId("a").closed = true;
const filtered = store.applyLaneGroups([["a", "b"], ["c", "d"]], {
  isLaneOpen,
  captureLaneState,
});
assert(
  JSON.stringify({
    runs: filtered.runs,
    c: store.laneGroupTopology("c"),
    d: store.laneGroupTopology("d").role,
  }) ===
    JSON.stringify({
      runs: [{ hostTargetId: "c", memberTargetIds: ["c", "d"] }],
      c: { role: "host", hostTargetId: "c", memberTargetIds: ["c", "d"] },
      d: "member",
    }),
  "closed members drop out, sub-pair runs dissolve, independent runs still form",
);

// Splitting returns an empty run set and standalone topology; a later fusion
// re-forms cleanly from a fresh host.
store.laneForId("a").closed = false;
const split = store.applyLaneGroups([], { isLaneOpen, captureLaneState });
const refuse = store.applyLaneGroups([["d", "c"]], {
  isLaneOpen,
  captureLaneState,
});
assert(
  JSON.stringify({
    split: split.runs,
    refuse: refuse.runs,
    d: store.laneGroupTopology("d").role,
  }) ===
    JSON.stringify({
      split: [],
      refuse: [{ hostTargetId: "d", memberTargetIds: ["d", "c"] }],
      d: "host",
    }),
  "split empties the run set and a later fusion re-forms from a fresh host",
);
