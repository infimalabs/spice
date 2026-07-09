const fs = require("fs");
const vm = require("vm");

const renderPath = process.argv[2];
const composerPath = process.argv[3];

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const context = {
  Date,
  Map,
  Number,
  Set,
  console,
  lanePendingDisplayCount(member) {
    return member.pendingInboxCount || 0;
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(renderPath, "utf8"), context, {
  filename: "app.render.js",
});
vm.runInContext(fs.readFileSync(composerPath, "utf8"), context, {
  filename: "app.composer.js",
});

const terminalStatus = {
  agentProcessStatus: "running",
  latestActivityPreview: "Drain complete. Completed `PERF-1kC57CKH`",
  lastAssistantAt: new Date().toISOString(),
};
const terminalVisual = context.liveAgentVisualStatus(terminalStatus);
assert(terminalVisual === "idle", "drain-complete running status becomes idle");
assert(
  context.laneComposePlaceholderStatus({
    pendingInboxCount: 0,
    lastRenderedStatusLine: {
      ...terminalStatus,
      agentVisualStatus: terminalVisual,
    },
  }) === "0 pending, idle",
  "composer placeholder uses terminal visual status",
);

const activeStatus = {
  agentProcessStatus: "running",
  latestActivityPreview: "Bash: pytest",
  lastAssistantAt: new Date().toISOString(),
};
const activeVisual = context.liveAgentVisualStatus(activeStatus);
assert(activeVisual === "running", "active running status remains running");
assert(
  context.laneComposePlaceholderStatus({
    pendingInboxCount: 0,
    lastRenderedStatusLine: {
      ...activeStatus,
      agentVisualStatus: activeVisual,
    },
  }) === "0 pending, running",
  "composer placeholder preserves active running status",
);
