const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const groupsPath = process.argv[3];
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

// syncLaneLights reuses the lights already mounted when the member order is
// unchanged, so the stub has to answer querySelectorAll from what it holds
// rather than pretend the container is always empty.
function fakeLaneLights() {
  return {
    hidden: false,
    style: fakeStyle(),
    children: [],
    querySelectorAll(selector) {
      const wanted = selector.replace(".", "");
      return this.children.filter((child) => {
        return String(child.className || "")
          .split(" ")
          .includes(wanted);
      });
    },
    replaceChildren(...children) {
      this.children = children;
    },
  };
}

function fakeLane(targetId, agentName, branchName, statusLine) {
  return {
    targetId,
    agentName,
    branchName,
    renderedFusedStatusLine: false,
    renderedStatusFingerprint: "",
    lastRenderedStatusLine: statusLine,
    element: {
      classList: fakeClassList(),
      nextElementSibling: null,
      style: fakeStyle(),
    },
    pipEl: {
      hidden: false,
      dataset: {
        agentStatus: statusLine.agentVisualStatus || "unknown",
        agentActivity: statusLine.activityStatus || "unknown",
      },
      title: "",
    },
    laneLightsEl: fakeLaneLights(),
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
  activityStatus: "active",
});
const member = fakeLane("member", "Member", "feature", {
  preview: "member retained",
  lastAssistantAt: "2026-06-12T05:01:00Z",
  agentVisualStatus: "idle",
  activityStatus: "inactive",
});

const context = {
  console,
  host,
  member,
  statusWrites,
  composerWrites,
  lanesEl: { insertBefore() {} },
  document: {
    createElement() {
      return { className: "", dataset: {}, style: fakeStyle(), title: "" };
    },
  },
  isLaneOpen: () => true,
  renderMessagesIfChanged: () => {},
  scheduleLiveBusLaneActivitySync: () => {},
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

vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});
const laneStore = vm.runInContext("laneStore", context);
laneStore.registerLane(host);
laneStore.registerLane(member);
vm.runInContext(
  source +
    `
  const lightIds = () => {
    return host.laneLightsEl.children
      .map((light) => { return light.dataset.laneLightTargetId; })
      .join(",");
  };
  const lightActivity = () => {
    return host.laneLightsEl.children
      .map((light) => { return light.dataset.agentActivity; })
      .join(",");
  };

  reconcileLaneGroups([["host", "member"]]);
  if (!host.renderedFusedStatusLine)
    throw new Error("fused host did not record aggregate status ownership");
  if (host.laneLightsEl.hidden || !host.pipEl.hidden)
    throw new Error("fused host did not swap its single pip for the lane lights");
  if (lightIds() !== "host,member")
    throw new Error("fused host lights did not mirror member order: " + lightIds());
  if (lightActivity() !== "active,inactive")
    throw new Error("fused host lights did not carry per-member activity: " + lightActivity());
  const fusedWrite = statusWrites.at(-1);
  if (fusedWrite.preview !== "member retained" ||
      fusedWrite.lastAssistantAt !== "2026-06-12T05:01:00Z")
    throw new Error("fused status did not use latest compact member status: " + JSON.stringify(fusedWrite));

  reconcileLaneGroups([["member", "host"]]);
  if (laneStore.laneGroupTopology("host").role !== "host")
    throw new Error("reorder changed visible host role");
  if (laneStore.laneGroupTopology("member").role !== "member")
    throw new Error("reorder did not keep lead composer as shadow member");
  if (laneStore.laneGroupTopology("host").memberTargetIds.join(",") !== "member,host")
    throw new Error("reorder lost logical composer order: " + laneStore.laneGroupTopology("host").memberTargetIds.join(","));
  const reorderedComposerWrite = composerWrites.at(-1);
  if (reorderedComposerWrite.targetId !== "host" ||
      reorderedComposerWrite.memberTargetIds.join(",") !== "member,host")
    throw new Error("reorder did not render new composer order on stable host: " + JSON.stringify(reorderedComposerWrite));
  if (lightIds() !== "member,host")
    throw new Error("reorder did not restack the host lights: " + lightIds());
  if (lightActivity() !== "inactive,active")
    throw new Error("reorder did not restack per-member activity: " + lightActivity());

  reconcileLaneGroups([]);
  const restoredWrite = statusWrites.at(-1);
  if (host.renderedFusedStatusLine)
    throw new Error("split host kept fused status marker");
  if (restoredWrite.targetId !== "host" || restoredWrite.preview !== "host retained")
    throw new Error("split host did not restore retained status: " + JSON.stringify(restoredWrite));
  if (!host.laneLightsEl.hidden || host.pipEl.hidden)
    throw new Error("split host did not restore its single pip in place of the lane lights");
  if (host.laneLightsEl.children.length !== 0)
    throw new Error("split host kept stale lane lights: " + lightIds());
`,
  context,
  { filename: "app.groups.js" },
);
