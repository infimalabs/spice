const { withServePage } = require("./serve_playwright_harness");
const { installScript } = require("./payload_factory");

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

function joinMeasure(config) {
  const seed = (ids) => {
    laneStore.replaceTargets(
      ids.map((id) =>
        window.spicePayloads.targetPayload({
          id,
          threadId: id + "-th",
          teamId: "team-main",
          pendingPrefix: "p-",
        }),
      ),
    );
    applyTeamSnapshotPayload(
      window.spicePayloads.teamSnapshot({
        revision: 7,
        teams: [
          window.spicePayloads.teamPayload({
            teamId: "team-main",
            revision: 7,
            memberIds: ids,
          }),
        ],
      }),
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
  let host = laneGroupHost(laneStore.laneForId(config.initialIds[0]));
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
  host = laneGroupHost(laneStore.laneForId(config.initialIds[0]));
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
      await page.addScriptTag({ content: installScript });
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
