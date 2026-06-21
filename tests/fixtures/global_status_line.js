const fs = require("fs");
const vm = require("vm");

const renderPath = process.argv[2];
const source = fs.readFileSync(renderPath, "utf8");
let nextTimerId = 1;
const timers = new Map();

function fakeNode() {
  return {
    dataset: {},
    hidden: true,
    textContent: "",
  };
}

function fakeLane(preview) {
  const statusLine = {
    preview,
    lastAssistantAt: new Date(Date.now() - 2000).toISOString(),
  };
  return {
    renderedStatusFingerprint: "",
    lastRenderedStatusLine: statusLine,
    statusErrorEl: fakeNode(),
    statusPreviewEl: fakeNode(),
    statusSeparatorEl: fakeNode(),
    statusTimeEl: fakeNode(),
  };
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const lane = fakeLane("latest activity");
const context = {
  Date,
  Map,
  Number,
  console,
  laneStates: new Map([["lane", lane]]),
  clearTimeout(id) {
    timers.delete(id);
  },
  setTimeout(callback) {
    const id = nextTimerId++;
    timers.set(id, callback);
    return id;
  },
};

vm.createContext(context);
vm.runInContext(source, context, { filename: renderPath });

context.setLaneStatus(lane, lane.lastRenderedStatusLine);
assert(lane.statusPreviewEl.textContent === "latest activity", "lane preview renders");
assert(lane.statusErrorEl.hidden, "lane error starts hidden");

context.setGlobalActivityStatus("loading teams");
assert(lane.statusPreviewEl.textContent === "loading teams", "global activity renders");
assert(lane.statusErrorEl.hidden, "global activity is not an error");

context.setGlobalTransientError("team refresh failed");
assert(lane.statusErrorEl.textContent === "team refresh failed", "global error renders");
assert(!lane.statusErrorEl.hidden, "global error is visible");
assert(lane.statusPreviewEl.hidden, "global error blocks status preview");

lane.lastRenderedStatusLine = {
  preview: "new activity arrived",
  lastAssistantAt: new Date(Date.now() - 1000).toISOString(),
};
context.setLaneStatus(lane, lane.lastRenderedStatusLine);
assert(
  lane.statusErrorEl.textContent === "team refresh failed",
  "global error survives incoming status",
);

context.clearGlobalActivityStatus("loading teams");
for (const callback of [...timers.values()]) callback();
timers.clear();
assert(lane.statusErrorEl.hidden, "global error clears after linger");
assert(
  lane.statusPreviewEl.textContent === "new activity arrived",
  "latest lane activity restores",
);
