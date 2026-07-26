const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const context = {};
vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});

const compareEpoch = vm.runInContext("compareLaneChromeEpoch", context);
const isNewerOrder = vm.runInContext("isNewerLaneChromeOrder", context);
const cases = JSON.parse(fs.readFileSync(0, "utf8"));
const comparisons = cases.map(
  ([epoch, previous, revision, previousRevision]) => [
    compareEpoch(epoch, previous),
    isNewerOrder(
      { epoch, revision },
      { epoch: previous, revision: previousRevision },
    ),
  ],
);

process.stdout.write(JSON.stringify(comparisons));
