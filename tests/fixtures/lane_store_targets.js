const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const context = { console };
vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});

const ServeLaneStore = vm.runInContext("ServeLaneStore", context);
const productionStore = vm.runInContext("laneStore", context);

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

assert(
  productionStore instanceof ServeLaneStore,
  "production lane store uses the constructible store",
);

const store = new ServeLaneStore();
const notifications = [];
const unsubscribeFirst = store.subscribe((change) => {
  notifications.push("first:" + change.targets.map((target) => target.id).join(","));
  assert(
    store.targetForId(change.targets[0]?.id) === change.targets[0],
    "listeners observe the updated index",
  );
});
const unsubscribeSecond = store.subscribe((change) => {
  notifications.push("second:" + change.targets.map((target) => target.id).join(","));
});
assert(notifications.length === 0, "subscription does not notify immediately");

const alpha = { id: "alpha", team: "one" };
const beta = { id: "beta", team: "two" };
store.replaceTargets([beta, alpha]);
assert(
  store.targetsSnapshot().map((target) => target.id).join(",") === "beta,alpha",
  "replacement preserves target order",
);
assert(store.targetForId("alpha") === alpha, "replacement rebuilds lookup index");
assert(
  notifications.join("|") === "first:beta,alpha|second:beta,alpha",
  "replacement notifies once in registration order",
);

const callerSnapshot = store.targetsSnapshot();
callerSnapshot.reverse();
callerSnapshot.push({ id: "intruder" });
assert(
  store.targetsSnapshot().map((target) => target.id).join(",") === "beta,alpha",
  "caller mutation cannot change store ordering",
);
assert(!store.targetForId("intruder"), "caller mutation cannot change lookup index");

notifications.length = 0;
assert(
  store.updateTarget("alpha", (target) => ({ ...target, team: "three" })),
  "optimistic update reports success",
);
assert(
  store.targetsSnapshot().map((target) => target.id).join(",") === "beta,alpha",
  "optimistic update preserves ordering",
);
assert(store.targetForId("alpha").team === "three", "optimistic update refreshes lookup");
assert(
  notifications.join("|") === "first:beta,alpha|second:beta,alpha",
  "optimistic update notifies exactly once in registration order",
);

notifications.length = 0;
assert(
  store.updateTarget("missing", (target) => target) === false,
  "missing optimistic target is a no-op",
);
assert(notifications.length === 0, "missing optimistic target does not notify");

unsubscribeFirst();
unsubscribeFirst();
notifications.length = 0;
store.updateTarget("beta", (target) => ({ ...target, team: "four" }));
assert(
  notifications.join("|") === "second:beta,alpha",
  "unsubscribe is idempotent and removes only its registration",
);
unsubscribeSecond();

const laneNotifications = [];
store.subscribe((change) => {
  if (change.kind !== "lanes") return;
  laneNotifications.push(
    change.transition +
      ":" +
      change.lane.targetId +
      ":" +
      change.lanes.map((lane) => lane.targetId).join(","),
  );
});
const laneAlpha = { targetId: "lane-alpha", closed: false };
const laneBeta = { targetId: "lane-beta", closed: false };
assert(store.registerLane(laneAlpha) === laneAlpha, "registration returns lane identity");
assert(store.registerLane(laneBeta) === laneBeta, "second registration returns identity");
assert(
  store.lanesSnapshot().map((lane) => lane.targetId).join(",") ===
    "lane-alpha,lane-beta",
  "lane registration preserves insertion order",
);
assert(store.laneForId("lane-alpha") === laneAlpha, "lane lookup preserves identity");
assert(store.hasLane("lane-beta") === true, "lane membership reports registration");
const callerLanes = store.lanesSnapshot();
callerLanes.reverse();
assert(
  store.lanesSnapshot().map((lane) => lane.targetId).join(",") ===
    "lane-alpha,lane-beta",
  "caller lane snapshot changes preserve store ordering",
);
assert(
  store.removeLane("lane-alpha") === laneAlpha,
  "lane removal returns removed identity",
);
assert(
  store.lanesSnapshot().map((lane) => lane.targetId).join(",") === "lane-beta",
  "lane removal preserves remaining order",
);
assert(
  laneNotifications.join("|") ===
    "registered:lane-alpha:lane-alpha|" +
      "registered:lane-beta:lane-alpha,lane-beta|" +
      "removed:lane-alpha:lane-beta",
  "lane transitions notify once after ordered state changes",
);
