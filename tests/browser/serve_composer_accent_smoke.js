const { withServePage } = require("./serve_playwright_harness");
const { installScript } = require("./payload_factory");

// Composer reorder is a color-only remap: the accent slot (member order ->
// border color) is NOT part of message identity, so reordering composers
// recolors cards in place with ZERO mosaic movement -- no re-measure, no
// lattice event, no transform/size write. Guards the jitter the operator
// reported when composer shuffles rippled through the message board.

const ACCENT_MEMBER_IDS = ["a1", "a2", "a3"];
const ACCENT_REORDERED_IDS = ["a2", "a1", "a3"];
const ACCENT_MESSAGES_PER_MEMBER = 3;

function accentMeasureReorder(config) {
  const accentSnapshotHost = (host) =>
    Array.from(
      host.mosaicPlaneEl.querySelectorAll("article[data-message-key]"),
    ).map((el) => ({
      key: el.dataset.messageKey,
      transform: el.style.transform,
      width: el.style.width,
      height: el.style.height,
      accent: el.dataset.accentSlot,
  }));
  const ids = config.memberIds;
  applyTargetsPayload({
    workTrees: ids.map((id) =>
      window.spicePayloads.targetPayload({
        id,
        threadId: id + "-th",
        teamId: "team-main",
      }),
    ),
  });
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
  const host = laneGroupHost(laneStore.laneForId(ids[0]));
  for (const member of laneGroupMemberLanes(host)) {
    const base = member.targetId.charCodeAt(1) * 10;
    member.knownMessages = Array.from(
      { length: config.perMember },
      (_, n) => ({
        ack_count: 0,
        ack_keys: [],
        index: base + n,
        key: "m-" + base + "-" + n,
        kind: "assistant",
        threadId: member.targetThreadId,
        timestamp: new Date(2027, 0, 1, 0, base + n, 0).toISOString(),
        display_html: "line<br>line",
        display_text: "x",
        text: "x",
      }),
    );
  }
  renderMessagesIfChanged(host);
  const before = accentSnapshotHost(host);
  const eventsBefore = host.mosaicEventLog.events.length;
  const fingerprintBefore = host.renderedMessageFingerprint;
  // Tag every current node: a fingerprint-driven re-render replaces nodes
  // (renderOrReuseMessageNode builds fresh elements on any fingerprint
  // change), so a surviving tag proves the reorder did NOT rebuild the
  // message stream -- the real coupling signal, independent of whether
  // uniform card heights happen to re-measure to the same row.
  let tag = 0;
  for (const node of host.mosaicPlaneEl.querySelectorAll(
    "article[data-message-key]",
  ))
    node.dataset.accentProbeTag = String((tag += 1));
  const taggedCount = tag;
  applyTeamSnapshotPayload(
    window.spicePayloads.teamSnapshot({
      revision: 8,
      teams: [
        window.spicePayloads.teamPayload({
          teamId: "team-main",
          revision: 8,
          memberIds: config.reorderedIds,
        }),
      ],
    }),
    { force: true },
  );
  renderMessagesIfChanged(host);
  const after = accentSnapshotHost(host);
  const byKey = new Map(after.map((card) => [card.key, card]));
  let moved = 0;
  let recolored = 0;
  for (const card of before) {
    const next = byKey.get(card.key);
    if (!next) {
      moved += 1;
      continue;
    }
    if (
      next.transform !== card.transform ||
      next.width !== card.width ||
      next.height !== card.height
    )
      moved += 1;
    if (next.accent !== card.accent) recolored += 1;
  }
  const survivingTags = host.mosaicPlaneEl.querySelectorAll(
    "article[data-accent-probe-tag]",
  ).length;
  return {
    cards: before.length,
    moved,
    recolored,
    eventLogGrew: host.mosaicEventLog.events.length - eventsBefore,
    taggedCount,
    survivingTags,
    fingerprintStable: host.renderedMessageFingerprint === fingerprintBefore,
  };
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=composer-accent-" + Date.now(),
      contextOptions: { viewport: { width: 1900, height: 1000 } },
    },
    async ({ page }) => {
      await page.waitForFunction(
        () =>
          typeof applyTeamSnapshotPayload === "function" &&
          typeof renderMessagesIfChanged === "function",
        { timeout: 10000 },
      );
      await page.addScriptTag({ content: installScript });
      const result = await page.evaluate(accentMeasureReorder, {
        memberIds: ACCENT_MEMBER_IDS,
        reorderedIds: ACCENT_REORDERED_IDS,
        perMember: ACCENT_MESSAGES_PER_MEMBER,
      });
      const expectedRecolor = ACCENT_MESSAGES_PER_MEMBER * 2;
      if (result.cards !== ACCENT_MEMBER_IDS.length * ACCENT_MESSAGES_PER_MEMBER)
        throw new Error("unexpected card count: " + JSON.stringify(result));
      if (result.moved !== 0)
        throw new Error("composer reorder moved cards: " + JSON.stringify(result));
      if (result.eventLogGrew !== 0)
        throw new Error("composer reorder ran mosaic events: " + JSON.stringify(result));
      if (!result.fingerprintStable)
        throw new Error(
          "composer reorder changed the message fingerprint (re-render): " +
            JSON.stringify(result),
        );
      if (result.survivingTags !== result.taggedCount)
        throw new Error(
          "composer reorder rebuilt message nodes: " + JSON.stringify(result),
        );
      if (result.recolored !== expectedRecolor)
        throw new Error(
          "composer reorder recolor count wrong (want " +
            expectedRecolor +
            "): " +
            JSON.stringify(result),
        );
      return result;
    },
  );
}

run().then(
  (result) => {
    console.log("serve_composer_accent_smoke ok " + JSON.stringify(result));
  },
  (error) => {
    console.error(error);
    process.exit(1);
  },
);
