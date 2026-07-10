const fs = require("fs");
const vm = require("vm");

const renderPath = process.argv[2];
const submissionsPath = process.argv[3];
const composerPath = process.argv[4];

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
vm.runInContext(fs.readFileSync(submissionsPath, "utf8"), context, {
  filename: "app.submissions.js",
});
vm.runInContext(fs.readFileSync(composerPath, "utf8"), context, {
  filename: "app.composer.js",
});

const terminalStatus = {
  agentProcessStatus: "running",
  latestActivityKind: "final",
  latestActivityPreview: "Confirmed fixed.",
  lastAssistantAt: new Date(Date.now() - 20 * 60 * 1000).toISOString(),
};
const terminalVisual = context.liveAgentVisualStatus(terminalStatus);
assert(terminalVisual === "idle", "structural final running status becomes idle");
assert(
  context.relativeTime(terminalStatus.lastAssistantAt) === "20m",
  "relative age remains independent from structural activity state",
);
assert(
  context.laneComposePlaceholderStatus({
    pendingInboxCount: 0,
    submissionLifecycleByKey: new Map(),
    lastRenderedStatusLine: {
      ...terminalStatus,
      agentVisualStatus: terminalVisual,
    },
  }) === "0 pending, idle",
  "composer placeholder uses terminal visual status",
);

const activeStatus = {
  agentProcessStatus: "running",
  latestActivityKind: "presence:function_call",
  latestActivityPreview: "Bash: pytest",
  lastAssistantAt: new Date().toISOString(),
};
const activeVisual = context.liveAgentVisualStatus(activeStatus);
assert(activeVisual === "running", "active running status remains running");
assert(
  context.laneComposePlaceholderStatus({
    pendingInboxCount: 0,
    submissionLifecycleByKey: new Map(),
    lastRenderedStatusLine: {
      ...activeStatus,
      agentVisualStatus: activeVisual,
    },
  }) === "0 pending, running",
  "composer placeholder preserves active running status",
);

const retainedStatus = context.statusLineWithRetainedSummary(
  { lastRenderedStatusLine: terminalStatus },
  { agentProcessStatus: "running" },
);
assert(
  retainedStatus.latestActivityKind === "final",
  "partial status updates retain the structural activity kind",
);
