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

// Subscription is lazy: the notification log opens with the replacement's own
// entries below, so subscribe itself delivered nothing ahead of the first
// mutation.
const alpha = { id: "alpha", team: "one" };
const beta = { id: "beta", team: "two" };
store.replaceTargets([beta, alpha]);
assert(
  store.targetsSnapshot().map((target) => target.id).join(",") === "beta,alpha",
  "replacement preserves target order",
);
assert(store.targetForId("alpha") === alpha, "replacement indexes alpha");
assert(store.targetForId("beta") === beta, "replacement indexes beta");
assert(
  notifications.join("|") === "first:beta,alpha|second:beta,alpha",
  "replacement notifies once per listener in registration order",
);

const callerSnapshot = store.targetsSnapshot();
callerSnapshot.reverse();
callerSnapshot.push({ id: "intruder" });
assert(
  store.targetsSnapshot().map((target) => target.id).join(",") === "beta,alpha",
  "caller mutation of a snapshot cannot change store ordering",
);
assert(store.targetForId("alpha") === alpha, "caller mutation leaves alpha indexed");
assert(store.targetForId("beta") === beta, "caller mutation leaves beta indexed");
assert(
  store.targetsSnapshot().length === 2,
  "the store still holds exactly the two real targets after caller mutation",
);

notifications.length = 0;
assert(
  store.updateTarget("alpha", (target) => ({ ...target, team: "three" })) === true,
  "optimistic update of a present target reports success",
);
assert(
  store.targetsSnapshot().map((target) => target.id).join(",") === "beta,alpha",
  "optimistic update preserves ordering",
);
assert(store.targetForId("alpha").team === "three", "optimistic update refreshes lookup");
assert(
  notifications.join("|") === "first:beta,alpha|second:beta,alpha",
  "optimistic update notifies exactly once per listener in registration order",
);

assert(
  store.updateTarget("missing", (target) => target) === false,
  "optimistic update of an unknown id reports failure",
);
assert(
  store.targetsSnapshot().map((target) => target.id).join(",") === "beta,alpha",
  "an unknown-id update leaves the collection exactly as it was",
);
assert(
  notifications.join("|") === "first:beta,alpha|second:beta,alpha",
  "an unknown-id update adds nothing to the notification log",
);

unsubscribeFirst();
unsubscribeFirst();
notifications.length = 0;
store.updateTarget("beta", (target) => ({ ...target, team: "four" }));
assert(
  notifications.join("|") === "second:beta,alpha",
  "after unsubscribe the surviving listener alone is notified, and repeat unsubscribe changes nothing",
);
assert(
  store.targetForId("beta").team === "four",
  "the surviving update still commits to the store",
);
unsubscribeSecond();
