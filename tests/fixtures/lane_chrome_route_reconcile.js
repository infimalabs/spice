const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const renderPath = process.argv[3];
const streamPath = process.argv[4];
const renders = { filters: 0, pending: 0, lifetime: 0, identity: 0 };
const context = {
  console,
  WebSocket: { OPEN: 1, CONNECTING: 0 },
  window: { location: { protocol: "http:", host: "localhost" } },
  laneGroupHost(lane) {
    return lane;
  },
  laneIsFusedHost() {
    return false;
  },
  laneGroupMemberLanes(lane) {
    return [lane];
  },
  applyTaskFilterInventory() {
    return false;
  },
  renderLaneFiltersPane() {
    renders.filters += 1;
  },
  renderLaneViewBadge(_lane, view) {
    if (view === "compose") renders.pending += 1;
  },
  renderFilterPills() {},
  syncComposerPlaceholders() {},
  syncComposerShards() {},
  syncFusedLaneLights() {},
  syncFusedLaneStatusLine() {},
  syncLaneTeamMenuButton() {},
  syncFusedLaneChrome() {
    renders.identity += 1;
  },
  applyServerLaneLifetime(lane, lifetime) {
    lane.lifetime = lifetime;
    renders.lifetime += 1;
    return true;
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});
vm.runInContext(fs.readFileSync(renderPath, "utf8"), context, {
  filename: "app.render.js",
});
vm.runInContext(fs.readFileSync(streamPath, "utf8"), context, {
  filename: "app.stream.js",
});
const laneStore = vm.runInContext("laneStore", context);

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const oldIdentity = {
  branch: "old",
  driver: { name: "codex", model: "old", effort: "low" },
  agent: { state: "unconfigured" },
  thread: { state: "unbound" },
};
const freshIdentity = {
  branch: "fresh",
  driver: { name: "codex", model: "gpt", effort: "high" },
  agent: { state: "configured", name: "agent-a" },
  thread: { state: "bound", threadId: "thread-a" },
};
const freshServeIdentity = {
  actorId: "thread:thread-a",
  driver: { desired: "codex", actual: "codex", transcriptOwner: "codex" },
  launch: {
    desired: { model: "gpt", effort: "high" },
    actual: { model: "gpt", effort: "high" },
  },
  thread: { state: "bound", threadId: "thread-a" },
};
laneStore.replaceTargets([
  {
    id: "lane-a",
    targetIdentity: oldIdentity,
    serveAgentIdentity: {},
  },
]);
const draft = { text: "operator draft" };
const focus = { id: "composer" };
const lane = {
  targetId: "lane-a",
  branchName: "old",
  targetThreadId: "",
  activeThreadId: "",
  lifetime: "Drive",
  draft,
  focus,
  optimisticPendingInboxCount: 0,
  optimisticSubmittedInboxKeys: new Set(),
  optimisticPendingInboxFloor: 0,
  pendingSubmissionCount: 0,
};
laneStore.registerLane(lane);

function routeResult(order, pendingCount = 1) {
  return {
    ok: true,
    route: {
      targetIdentity: freshIdentity,
      serveAgentIdentity: freshServeIdentity,
      chrome: {
        targetId: lane.targetId,
        teamConfig: {
          authority: "team-store",
          order: { epoch: "", revision: order },
          value: {
            teamIdentity: {
              state: "member",
              teamId: "team-a",
              teamRevision: order,
              configRevision: order,
            },
          },
        },
        taskBoard: {
          authority: "task-board",
          order: { epoch: String(order), revision: order },
          value: {
            taskFilters: ["serve.ui"],
            taskFilterEntries: [],
            effectiveTaskFilters: ["serve"],
            taskFilterInventory: {
              revision: String(order),
              catalog: { approvedStems: [] },
              primaryStems: [],
            },
            privateTaskCount: 0,
          },
        },
      },
    },
    chrome: {
      targetId: lane.targetId,
      pendingInbox: {
        authority: "inbox",
        order: { epoch: "", revision: order + 2 },
        value: {
          count: pendingCount,
          label: String(pendingCount),
          keys: pendingCount ? ["route-key"] : [],
        },
      },
      renewal: {
        authority: "team-store",
        order: { epoch: "", revision: order },
        value: {
          lifetime: order >= 5 ? "Drain" : "Drive",
          renewalIntent: { revision: order, requested: false },
        },
      },
    },
  };
}

const applied = context.applyLaneSendChrome(lane, routeResult(5));
assert(applied.identityChanged === true, "fresh route applies direct identity");
assert(
  applied.routeTransition.changedFacets.join(",") === "teamConfig,taskBoard",
  "route feedback reduces config and board facets",
);
assert(
  applied.resultTransition.changedFacets.join(",") === "pendingInbox,renewal",
  "send feedback reduces pending and renewal facets",
);
const chrome = laneStore.laneChrome(lane.targetId);
assert(chrome.teamConfig.teamIdentity.configRevision === 5, "team is canonical");
assert(chrome.taskBoard.taskFilters[0] === "serve.ui", "board is canonical");
assert(chrome.pendingInbox.count === 1, "pending is canonical");
assert(chrome.renewal.lifetime === "Drain", "renewal is canonical");
assert(
  laneStore.targetForId(lane.targetId).targetIdentity === freshIdentity,
  "non-faceted route identity updates target descriptor",
);
assert(lane.branchName === "fresh", "direct lane identity feedback remains immediate");
assert(
  !("taskFilters" in lane) &&
    !("renewalIntent" in lane) &&
    !("backendPendingInboxCount" in lane),
  "route feedback creates no lane chrome copies",
);
assert(lane.draft === draft && lane.focus === focus, "route preserves draft and focus");

const record = laneStore.laneChrome(lane.targetId);
const renderSignature = JSON.stringify(renders);
const duplicate = context.applyLaneSendChrome(lane, routeResult(5));
assert(duplicate.identityChanged === false, "duplicate identity is unchanged");
assert(
  duplicate.routeTransition.disposition === "stale" &&
    duplicate.resultTransition.disposition === "stale",
  "duplicate route and send envelopes are stale",
);
assert(laneStore.laneChrome(lane.targetId) === record, "duplicate preserves record");
assert(JSON.stringify(renders) === renderSignature, "duplicate rerenders nothing");

const reordered = context.applyLaneSendChrome(lane, routeResult(4, 0));
assert(
  reordered.routeTransition.disposition === "stale" &&
    reordered.resultTransition.disposition === "stale",
  "reordered route and send feedback cannot rewind facets",
);
assert(laneStore.laneChrome(lane.targetId) === record, "reordered record is stable");
assert(JSON.stringify(renders) === renderSignature, "reordered feedback rerenders nothing");
