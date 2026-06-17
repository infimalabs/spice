const fs = require("fs");
const vm = require("vm");

const groupsPath = process.argv[2];
const source = fs.readFileSync(groupsPath, "utf8");
const statusWrites = [];
const composerWrites = [];

function fakeStyle() {
  return {
    setProperty() {},
    removeProperty() {},
  };
}

function fakeClassList() {
  const names = new Set();
  return {
    add(...items) {
      for (const item of items) names.add(item);
    },
    remove(...items) {
      for (const item of items) names.delete(item);
    },
    contains(item) {
      return names.has(item);
    },
  };
}

function fakeLane(targetId, agentName, branchName, statusLine) {
  return {
    targetId,
    agentName,
    branchName,
    groupTopology: null,
    renderedFusedStatusLine: false,
    renderedStatusFingerprint: "",
    lastRenderedStatusLine: statusLine,
    element: {
      classList: fakeClassList(),
      nextElementSibling: null,
    },
    pipEl: {
      hidden: false,
      dataset: { agentStatus: statusLine.agentVisualStatus || "unknown" },
      title: "",
    },
    laneLightsEl: {
      hidden: false,
      style: fakeStyle(),
      replaceChildren(...children) {
        this.children = children;
      },
    },
    teamMenuButtonEl: {
      innerHTML: "",
      title: "",
      setAttribute(name, value) {
        this[name] = value;
      },
      removeAttribute(name) {
        delete this[name];
      },
    },
  };
}

const host = fakeLane("host", "Host", "main", {
  preview: "host retained",
  lastAssistantAt: "2026-06-12T05:00:00Z",
  agentVisualStatus: "running",
});
const member = fakeLane("member", "Member", "feature", {
  preview: "member retained",
  lastAssistantAt: "2026-06-12T05:01:00Z",
  agentVisualStatus: "idle",
});

const context = {
  console,
  host,
  member,
  statusWrites,
  composerWrites,
  laneStates: new Map([
    [host.targetId, host],
    [member.targetId, member],
  ]),
  lanesEl: { insertBefore() {} },
  document: {
    createElement() {
      return { className: "", dataset: {}, style: fakeStyle(), title: "" };
    },
  },
  isLaneOpen: () => true,
  renderMessagesIfChanged: () => {},
  syncComposerShards(lane, members) {
    composerWrites.push({
      targetId: lane.targetId,
      memberTargetIds: members.map((candidate) => candidate.targetId),
    });
  },
  syncLaneEffectiveControls: () => {},
  laneLifetimeRuntimeState: () => ({ pendingLifetimeCommit: "" }),
  restoreLaneLifetimeRuntimeState: () => {},
  relativeTime: () => "2m ago",
  setLaneStatus(lane, statusLine) {
    statusWrites.push({
      targetId: lane.targetId,
      lastAssistantAt: statusLine.lastAssistantAt || "",
      preview: statusLine.preview || "",
    });
    lane.renderedStatusFingerprint = statusLine.preview || "";
  },
};

vm.runInNewContext(
  source +
    `
  reconcileLaneGroups([["host", "member"]]);
  if (!host.renderedFusedStatusLine)
    throw new Error("fused host did not record aggregate status ownership");
  const fusedWrite = statusWrites.at(-1);
  if (fusedWrite.preview !== "member retained" ||
      fusedWrite.lastAssistantAt !== "2026-06-12T05:01:00Z")
    throw new Error("fused status did not use latest compact member status: " + JSON.stringify(fusedWrite));

  reconcileLaneGroups([["member", "host"]]);
  if (host.groupTopology.role !== "host")
    throw new Error("reorder changed visible host role");
  if (member.groupTopology.role !== "member")
    throw new Error("reorder did not keep lead composer as shadow member");
  if (host.groupTopology.memberTargetIds.join(",") !== "member,host")
    throw new Error("reorder lost logical composer order: " + host.groupTopology.memberTargetIds.join(","));
  const reorderedComposerWrite = composerWrites.at(-1);
  if (reorderedComposerWrite.targetId !== "host" ||
      reorderedComposerWrite.memberTargetIds.join(",") !== "member,host")
    throw new Error("reorder did not render new composer order on stable host: " + JSON.stringify(reorderedComposerWrite));

  reconcileLaneGroups([]);
  const restoredWrite = statusWrites.at(-1);
  if (host.renderedFusedStatusLine)
    throw new Error("split host kept fused status marker");
  if (restoredWrite.targetId !== "host" || restoredWrite.preview !== "host retained")
    throw new Error("split host did not restore retained status: " + JSON.stringify(restoredWrite));
`,
  context,
  { filename: "app.groups.js" },
);
