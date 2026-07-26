const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const context = { console };
vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});

const ServeLaneStore = vm.runInContext("ServeLaneStore", context);
const authorities = vm.runInContext("LANE_CHROME_FACET_AUTHORITIES", context);

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const EPOCH = "serve-epoch-a";
const LATER_EPOCH = "serve-epoch-b";

function facet(name, value, revision, epoch = EPOCH) {
  return { authority: authorities[name], order: { epoch, revision }, value };
}

function chrome(targetId, facets) {
  return { targetId, ...facets };
}

function summary(transition) {
  return {
    targetId: transition.targetId,
    disposition: transition.disposition,
    changed: transition.changedFacets,
    stale: transition.staleFacets,
  };
}

const store = new ServeLaneStore();
const observed = [];
store.subscribe((change) => {
  if (change.kind === "laneChrome") observed.push(change.transition);
});

// Every well-formed channel payload below is replayed against a second store in
// reverse at the end; per-facet (epoch, revision) ordering is what makes the
// merge commutative, so arrival order cannot change where the target lands.
const wellFormed = [];

function apply(payload) {
  wellFormed.push(payload);
  return store.applyLaneChrome(payload);
}

const identityValue = {
  displayName: "spice-f",
  branch: "main-f",
  targetIdentity: { state: "resolved", threadId: "thread-f" },
  serveAgentIdentity: { state: "resolved" },
};
const renewalValue = {
  lifetime: "day",
  renewalIntent: { state: "idle", requestId: 0 },
};

// Renewal before discovery: the team store publishes a lifetime for a target
// the registry has not described yet, so the record exists holding one facet and
// the rest stay explicitly empty rather than absent.
const renewalFirst = apply(
  chrome("target-f", { renewal: facet("renewal", renewalValue, 4) }),
);
assert(observed[0] === renewalFirst, "subscriber receives the transition identity");
assert(Object.isFrozen(renewalFirst), "published chrome transition is immutable");
assert(
  JSON.stringify({
    ...summary(renewalFirst),
    record: renewalFirst.record,
    stored: store.laneChrome("target-f") === renewalFirst.record,
  }) ===
    JSON.stringify({
      targetId: "target-f",
      disposition: "applied",
      changed: ["renewal"],
      stale: [],
      record: {
        targetId: "target-f",
        identity: null,
        teamConfig: null,
        pendingInbox: null,
        taskBoard: null,
        lifecycle: null,
        renewal: renewalValue,
        activity: null,
      },
      stored: true,
    }),
  "a renewal that precedes discovery creates the canonical record on its own",
);

// Discovery lands afterwards at its own authority's lower revision: freshness is
// per facet, so there is no global revision for a late channel to lose against.
const discovery = apply(
  chrome("target-f", { identity: facet("identity", identityValue, 1) }),
);
assert(
  JSON.stringify({
    ...summary(discovery),
    identity: discovery.record.identity,
    renewal: discovery.record.renewal,
  }) ===
    JSON.stringify({
      targetId: "target-f",
      disposition: "applied",
      changed: ["identity"],
      stale: [],
      identity: identityValue,
      renewal: renewalValue,
    }),
  "a lower-revision facet still applies beside a higher-revision one",
);

// Redelivery of an identical payload advances nothing and keeps the record the
// consumers already hold: the facet is reported stale because its order did not
// move, which is exactly what a duplicate is.
const duplicate = apply(
  chrome("target-f", { identity: facet("identity", identityValue, 1) }),
);
assert(
  JSON.stringify(summary(duplicate)) ===
    JSON.stringify({
      targetId: "target-f",
      disposition: "stale",
      changed: [],
      stale: ["identity"],
    }),
  "a duplicate payload advances no facet",
);
assert(
  duplicate.record === discovery.record,
  "a duplicate payload leaves the record identity untouched",
);

// Re-announcing the same value at a newer order is not a change to render, but
// it must still advance freshness.
const reannounce = apply(
  chrome("target-f", { identity: facet("identity", { ...identityValue }, 2) }),
);
assert(
  JSON.stringify(summary(reannounce)) ===
    JSON.stringify({
      targetId: "target-f",
      disposition: "unchanged",
      changed: [],
      stale: [],
    }),
  "an equal value at a newer order changes no facet",
);
assert(
  reannounce.record === discovery.record,
  "an unchanged facet does not replace the whole record",
);

// The high-water mark moved even though the record did not: a payload carrying a
// different value at the order just consumed is a redelivery, not an update.
// This one is deliberately ill-formed for its order, so it stays out of the
// commutativity replay.
const conflicting = store.applyLaneChrome(
  chrome("target-f", {
    identity: facet("identity", { ...identityValue, branch: "stale-branch" }, 2),
  }),
);
assert(
  JSON.stringify({
    ...summary(conflicting),
    branch: store.laneChrome("target-f").identity.branch,
  }) ===
    JSON.stringify({
      targetId: "target-f",
      disposition: "stale",
      changed: [],
      stale: ["identity"],
      branch: "main-f",
    }),
  "an equal-value advance still raises the high-water mark it reported",
);

const teamConfigValue = { teamIdentity: { state: "resolved", teamId: "team-1" } };
const teamSnapshot = apply(
  chrome("target-f", { teamConfig: facet("teamConfig", teamConfigValue, 5) }),
);
assert(
  JSON.stringify(summary(teamSnapshot)) ===
    JSON.stringify({
      targetId: "target-f",
      disposition: "applied",
      changed: ["teamConfig"],
      stale: [],
    }),
  "a team snapshot applies the team-config facet",
);

// An older lane push arriving after a newer team config: the lane's stale copy
// of the config is refused while the activity it genuinely owns applies, so a
// fresher unrelated facet never drags a newer one backwards.
const activityValue = { lastAssistantAt: "2026-07-25T21:00:00Z", preview: "…" };
const lanePush = apply(
  chrome("target-f", {
    teamConfig: facet("teamConfig", { teamIdentity: { state: "none" } }, 4),
    activity: facet("activity", activityValue, 3),
  }),
);
assert(
  JSON.stringify({
    ...summary(lanePush),
    teamConfig: lanePush.record.teamConfig,
    activity: lanePush.record.activity,
  }) ===
    JSON.stringify({
      targetId: "target-f",
      disposition: "applied",
      changed: ["activity"],
      stale: ["teamConfig"],
      teamConfig: teamConfigValue,
      activity: activityValue,
    }),
  "an older facet is refused while the same payload's fresher facet applies",
);

// A pending-only live-bus event touches its own facet and leaves the rest of the
// record exactly as it stands.
const pendingValue = { count: 2, label: "2 pending", keys: ["k1", "k2"] };
const pendingOnly = apply(
  chrome("target-f", { pendingInbox: facet("pendingInbox", pendingValue, 7) }),
);
assert(
  JSON.stringify({ ...summary(pendingOnly), record: pendingOnly.record }) ===
    JSON.stringify({
      targetId: "target-f",
      disposition: "applied",
      changed: ["pendingInbox"],
      stale: [],
      record: {
        targetId: "target-f",
        identity: identityValue,
        teamConfig: teamConfigValue,
        pendingInbox: pendingValue,
        taskBoard: null,
        lifecycle: null,
        renewal: renewalValue,
        activity: activityValue,
      },
    }),
  "a partial payload merges into one record and leaves every other facet alone",
);

// An explicit null is the authority saying the facet has no value now; the same
// clear redelivered at a newer order is no longer a change.
const cleared = apply(
  chrome("target-f", { pendingInbox: facet("pendingInbox", null, 8) }),
);
const reCleared = apply(
  chrome("target-f", { pendingInbox: facet("pendingInbox", null, 9) }),
);
assert(
  JSON.stringify({
    cleared: summary(cleared),
    value: cleared.record.pendingInbox,
    reCleared: summary(reCleared),
    stable: reCleared.record === cleared.record,
  }) ===
    JSON.stringify({
      cleared: {
        targetId: "target-f",
        disposition: "applied",
        changed: ["pendingInbox"],
        stale: [],
      },
      value: null,
      reCleared: {
        targetId: "target-f",
        disposition: "unchanged",
        changed: [],
        stale: [],
      },
      stable: true,
    }),
  "an explicit clear applies once and stays settled",
);

// An authority that restarted resumes its revisions from the bottom, so the
// epoch is what orders the two generations.
const restartedActivity = { lastAssistantAt: "2026-07-25T21:30:00Z" };
const restarted = apply(
  chrome("target-f", {
    activity: facet("activity", restartedActivity, 0, LATER_EPOCH),
  }),
);
assert(
  JSON.stringify({
    ...summary(restarted),
    activity: restarted.record.activity,
  }) ===
    JSON.stringify({
      targetId: "target-f",
      disposition: "applied",
      changed: ["activity"],
      stale: [],
      activity: restartedActivity,
    }),
  "a newer epoch supersedes a higher revision from the epoch before it",
);

// An authority is free to name its generations with a plain decimal counter,
// which is where JS string collation would betray the reducer: "10" sorts below
// "9", so the generation after the carry would be classified stale forever and
// the facet would hold the pre-restart value for the life of the session.
const carriedActivity = { lastAssistantAt: "2026-07-25T22:00:00Z" };
const beforeCarry = apply(
  chrome("target-i", {
    activity: facet("activity", { lastAssistantAt: "2026-07-25T21:00:00Z" }, 7, "9"),
  }),
);
const afterCarry = apply(
  chrome("target-i", { activity: facet("activity", carriedActivity, 0, "10") }),
);
assert(
  JSON.stringify({
    before: summary(beforeCarry).disposition,
    ...summary(afterCarry),
    activity: afterCarry.record.activity,
  }) ===
    JSON.stringify({
      before: "applied",
      targetId: "target-i",
      disposition: "applied",
      changed: ["activity"],
      stale: [],
      activity: carriedActivity,
    }),
  "a decimal generation counter supersedes across the carry from 9 to 10",
);

// The rule is an order, not "any different epoch wins": the generation left
// behind stays behind, however high its revisions climbed.
const afterLapsed = apply(
  chrome("target-i", {
    activity: facet("activity", { lastAssistantAt: "2026-07-25T21:59:00Z" }, 99, "9"),
  }),
);
assert(
  JSON.stringify({
    ...summary(afterLapsed),
    activity: afterLapsed.record.activity,
  }) ===
    JSON.stringify({
      targetId: "target-i",
      disposition: "stale",
      changed: [],
      stale: ["activity"],
      activity: carriedActivity,
    }),
  "a superseded generation cannot reclaim the facet with a higher revision",
);

// The wire contract lets an authority spell its generations however it likes
// and asks only that they advance, so every encoding a producer might plausibly
// reach for gets swept here: each generation in turn must supersede the one
// before it while restarting its revisions, and no generation left behind may
// reclaim the facet however high its revisions later climb.
const EPOCH_ENCODINGS = [
  ["decimal counter", ["2", "9", "10", "100"]],
  [
    "ISO instant",
    ["2026-07-25T21:00:00Z", "2026-07-25T21:30:00Z", "2026-07-26T01:00:00Z"],
  ],
  ["prefixed label", ["gen-2", "gen-9", "gen-10"]],
];
const SWEEP_REVISION = 0;
const SWEEP_LAPSED_REVISION = 99;
const SWEEP_TARGET = "target-sweep";

for (const [encoding, epochs] of EPOCH_ENCODINGS) {
  const sweepStore = new ServeLaneStore();
  const generation = (index) => {
    return { lastAssistantAt: encoding + " generation " + index };
  };
  const applied = epochs.map((epoch, index) => {
    return sweepStore.applyLaneChrome(
      chrome(SWEEP_TARGET, {
        activity: facet("activity", generation(index), SWEEP_REVISION, epoch),
      }),
    ).disposition;
  });
  const lapsed = epochs.slice(0, -1).map((epoch, index) => {
    return sweepStore.applyLaneChrome(
      chrome(SWEEP_TARGET, {
        activity: facet(
          "activity",
          generation(index),
          SWEEP_LAPSED_REVISION,
          epoch,
        ),
      }),
    ).disposition;
  });
  assert(
    JSON.stringify({
      applied,
      lapsed,
      activity: sweepStore.laneChrome(SWEEP_TARGET).activity,
    }) ===
      JSON.stringify({
        applied: epochs.map(() => "applied"),
        lapsed: epochs.slice(0, -1).map(() => "stale"),
        activity: generation(epochs.length - 1),
      }),
    encoding + " epochs advance in order and no lapsed generation reclaims",
  );
}

// Records are per target: another target's chrome neither rewrites this one nor
// publishes a transition naming it.
const settled = store.laneChrome("target-f");
const neighbour = apply(
  chrome("target-g", { lifecycle: facet("lifecycle", { processStatus: "running" }, 1) }),
);
assert(
  JSON.stringify({
    ...summary(neighbour),
    lifecycle: neighbour.record.lifecycle,
    untouched: store.laneChrome("target-f") === settled,
    missing: store.laneChrome("target-h"),
  }) ===
    JSON.stringify({
      targetId: "target-g",
      disposition: "applied",
      changed: ["lifecycle"],
      stale: [],
      lifecycle: { processStatus: "running" },
      untouched: true,
      missing: null,
    }),
  "each target owns its own record and an unknown target has none",
);

assert(
  observed.length === wellFormed.length + 1,
  "every apply publishes exactly one chrome transition",
);

// Commutativity: the same well-formed channel payloads replayed in reverse land
// on the same record, because each facet keeps only its highest-ordered value.
const replayStore = new ServeLaneStore();
for (const payload of wellFormed.slice().reverse())
  replayStore.applyLaneChrome(payload);
assert(
  JSON.stringify(replayStore.laneChrome("target-f")) ===
    JSON.stringify(store.laneChrome("target-f")),
  "reversed arrival order reaches the identical canonical record",
);

let crossAuthority = "";
try {
  store.applyLaneChrome(
    chrome("target-f", {
      pendingInbox: {
        authority: authorities.renewal,
        order: { epoch: EPOCH, revision: 12 },
        value: pendingValue,
      },
    }),
  );
} catch (error) {
  crossAuthority = error.message;
}
let missingTarget = "";
try {
  store.applyLaneChrome({ identity: facet("identity", identityValue, 12) });
} catch (error) {
  missingTarget = error.message;
}
assert(
  JSON.stringify({ crossAuthority, missingTarget }) ===
    JSON.stringify({
      crossAuthority: "lane chrome facet authority mismatch: pendingInbox",
      missingTarget: "lane chrome target id is required",
    }),
  "the reducer refuses cross-authority facets and untargeted payloads",
);

assert(
  store.laneChrome("target-f") === settled,
  "a refused payload leaves the canonical record exactly as it stood",
);
