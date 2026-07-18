const { withServePage } = require("./serve_playwright_harness");

// Agent join must not rebuild the board. When a new member fuses into a
// team, the merged stream gains that member's keys -- but every EXISTING
// card must keep its exact DOM node (no fingerprint-driven rebuild from the
// member count) and must not be detached from the plane (no replaceChildren
// churn). Guards the one-by-one flash the operator saw on load, where each
// joining agent read as the whole board being deleted and reinstated.

const JOIN_INITIAL_IDS = ["j1", "j2"];
const JOIN_ADDED_ID = "j3";
const JOIN_MESSAGES_PER_MEMBER = 3;
const JOIN_OBSERVER_FLUSH_MS = 60;

function joinTargetPayload(id, threadId) {
  return {
    id,
    name: id,
    branch: id,
    targetIdentity: {
      targetId: id,
      worktreeName: id,
      branch: id,
      driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
      agent: { state: "unconfigured" },
      thread: { state: "bound", threadId },
    },
    serveAgentIdentity: {
      actorId: "thread:" + threadId,
      target: { id },
      thread: { state: "bound", threadId },
    },
    teamIdentity: {
      state: "member",
      teamId: "team-main",
      teamRevision: 1,
      configRevision: 1,
    },
    taskFilters: [],
    laneFilterVersion: "",
    lifetime: "Drive",
    pendingInboxCount: 0,
    pendingInboxKeys: [],
    pendingInboxRevision: "p-" + id,
    pendingInboxVersion: 1,
    statusLine: {
      pendingInboxCount: 0,
      pendingInboxKeys: [],
      pendingInboxRevision: "p-" + id,
      pendingInboxVersion: 1,
    },
  };
}

function joinTeam(ids, revision) {
  return {
    teamId: "team-main",
    revision,
    config: {
      revision,
      lifetime: "Drive",
      taskFilters: [],
      taskFilterEntries: [],
    },
    splitBack: {},
    members: ids.map((id) => ({ agentId: "target:" + id })),
  };
}

function joinMeasure(config) {
  const seed = (ids) => {
    laneStore.replaceTargets(
      ids.map((id) => window.__joinTarget(id, id + "-th")),
    );
    applyTeamSnapshotPayload(
      {
        revision: 7,
        changed: true,
        snapshot: {
          globalSettings: { fastMode: false },
          teams: [window.__joinTeam(ids, 7)],
        },
      },
      { force: true },
    );
  };
  const fillMessages = (lane) => {
    if (lane.knownMessages && lane.knownMessages.length) return;
    const base = lane.targetId.charCodeAt(1) * 10;
    lane.knownMessages = Array.from({ length: config.perMember }, (_, n) => ({
      ack_count: 0,
      ack_keys: [],
      index: base + n,
      key: "m-" + base + "-" + n,
      kind: "assistant",
      threadId: lane.targetThreadId,
      timestamp: new Date(2027, 0, 1, 0, base + n, 0).toISOString(),
      display_html: "line<br>line<br>line",
      display_text: "x",
      text: "x",
    }));
  };

  seed(config.initialIds);
  let host = laneGroupHost(laneStates.get(config.initialIds[0]));
  for (const member of laneGroupMemberLanes(host)) fillMessages(member);
  renderMessagesIfChanged(host);

  const nodeByKey = new Map();
  for (const node of host.mosaicPlaneEl.querySelectorAll("article[data-message-key]"))
    nodeByKey.set(node.dataset.messageKey, node);
  const planeBefore = host.mosaicPlaneEl;

  let detached = 0;
  let attached = 0;
  new MutationObserver((records) => {
    for (const record of records) {
      detached += record.removedNodes.length;
      attached += record.addedNodes.length;
    }
  }).observe(host.mosaicPlaneEl, { childList: true });

  seed([...config.initialIds, config.addedId]);
  host = laneGroupHost(laneStates.get(config.initialIds[0]));
  for (const member of laneGroupMemberLanes(host)) fillMessages(member);
  renderMessagesIfChanged(host);

  return new Promise((resolve) => {
    setTimeout(() => {
      let reusedSame = 0;
      let rebuilt = 0;
      const after = new Map();
      for (const node of host.mosaicPlaneEl.querySelectorAll("article[data-message-key]"))
        after.set(node.dataset.messageKey, node);
      for (const [key, node] of nodeByKey) {
        const now = after.get(key);
        if (now === node) reusedSame += 1;
        else rebuilt += 1;
      }
      resolve({
        planeSame: host.mosaicPlaneEl === planeBefore,
        originalCount: nodeByKey.size,
        reusedSame,
        rebuilt,
        detachedNodes: detached,
        attachedNodes: attached,
        addedMemberMessages: config.perMember,
      });
    }, config.flushMs);
  });
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=mosaic-join-" + Date.now(),
      contextOptions: { viewport: { width: 1900, height: 1000 } },
    },
    async ({ page }) => {
      await page.waitForFunction(
        () =>
          typeof applyTeamSnapshotPayload === "function" &&
          typeof renderMessagesIfChanged === "function" &&
          typeof laneGroupHost === "function",
        { timeout: 10000 },
      );
      await page.addScriptTag({
        content:
          "window.__joinTarget = " +
          joinTargetPayload.toString() +
          "; window.__joinTeam = " +
          joinTeam.toString() +
          ";",
      });
      const result = await page.evaluate(joinMeasure, {
        initialIds: JOIN_INITIAL_IDS,
        addedId: JOIN_ADDED_ID,
        perMember: JOIN_MESSAGES_PER_MEMBER,
        flushMs: JOIN_OBSERVER_FLUSH_MS,
      });
      const expectedOriginal = JOIN_INITIAL_IDS.length * JOIN_MESSAGES_PER_MEMBER;
      if (result.originalCount !== expectedOriginal)
        throw new Error("unexpected pre-join card count: " + JSON.stringify(result));
      if (result.rebuilt !== 0)
        throw new Error(
          "agent join rebuilt existing card nodes (fingerprint churn): " +
            JSON.stringify(result),
        );
      if (result.reusedSame !== expectedOriginal)
        throw new Error("existing cards were not reused on join: " + JSON.stringify(result));
      if (result.detachedNodes !== 0)
        throw new Error(
          "agent join detached existing cards from the plane: " +
            JSON.stringify(result),
        );
      if (result.attachedNodes !== JOIN_MESSAGES_PER_MEMBER)
        throw new Error(
          "join attached more than the new member's cards: " + JSON.stringify(result),
        );
      return result;
    },
  );
}

run().then(
  (result) => {
    console.log("serve_mosaic_join_smoke ok " + JSON.stringify(result));
  },
  (error) => {
    console.error(error);
    process.exit(1);
  },
);
