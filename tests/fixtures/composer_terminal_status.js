const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const renderPath = process.argv[3];
const submissionsPath = process.argv[4];
const composerPath = process.argv[5];

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
    return member.pending || 0;
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});
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
    pending: 0,
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
    pending: 0,
    submissionLifecycleByKey: new Map(),
    lastRenderedStatusLine: {
      ...activeStatus,
      agentVisualStatus: activeVisual,
    },
  }) === "0 pending, running",
  "composer placeholder preserves active running status",
);

const claimedMember = {
  agentName: "spice-b",
  branchName: "main-b",
  lastRenderedStatusLine: {
    agentVisualStatus: "running",
    claimedTask: {
      handle: "UI-1kF5xdSM",
      phase: "todo",
      title:
        "Show the claimed task even when its deliberately long title must wrap inside the agent card",
    },
  },
};
assert(
  context.laneComposePlaceholder(claimedMember) ===
    "spice-b, main-b\n0 pending, running\nUI-1kF5xdSM, todo",
  "claimed composer placeholder is three high-value comma lines with no Next line or dot join",
);
assert(
  context.laneComposeTaskTooltip(claimedMember) ===
    "spice-b, main-b\n0 pending, running\nUI-1kF5xdSM, todo\n" +
      "Show the claimed task even when its deliberately long title must wrap inside the agent card",
  "claimed composer hover carries agent, status, task label, and the full untruncated title",
);

const unclaimedMember = {
  agentName: "",
  branchName: "main-b",
  lastRenderedStatusLine: { agentVisualStatus: "idle" },
};
assert(
  context.laneComposePlaceholder(unclaimedMember) === "main-b\n0 pending, idle",
  "unclaimed composer placeholder collapses to branch and status with no task line",
);
assert(
  context.laneComposeTaskTooltip(unclaimedMember) === "",
  "unclaimed composer hover stays empty when no task is claimed",
);

const retainedStatus = context.statusLineWithRetainedSummary(
  { lastRenderedStatusLine: terminalStatus },
  { agentProcessStatus: "running" },
);
assert(
  retainedStatus.latestActivityKind === "final",
  "partial status updates retain the structural activity kind",
);
