const { withServePage } = require("./serve_playwright_harness");
const { targetPayload, teamPayload, installScript } = require("./payload_factory");

// Retention is a stream concern with a mosaic-visible consequence. These
// fixtures drive the production solo and fused paths through cap pressure:
// the oldest retained compaction divider becomes the floor, the whole epoch
// below it leaves together, and the mosaic's vacancy path preserves every
// surviving coordinate without a full replay.

const RETENTION_LIMIT = 6;

function retentionTeam(teamId, memberIds) {
  return teamPayload({ teamId, memberIds, revision: 1 });
}

function retentionItem(prefix, label, second, kind, threadId) {
  const text = prefix + " " + label;
  return {
    ack_count: 0,
    ack_keys: [],
    display_html: text,
    display_text: text,
    index: second,
    key: prefix + "-" + label,
    kind: kind || "assistant",
    text,
    threadId,
    timestamp: new Date(Date.UTC(2026, 0, 1, 0, 0, second)).toISOString(),
  };
}

function retentionEpoch(prefix, threadId) {
  return [
    retentionItem(prefix, "current-new", 50, "assistant", threadId),
    retentionItem(prefix, "current-old", 40, "assistant", threadId),
    retentionItem(prefix, "floor", 30, "compaction", threadId),
    retentionItem(prefix, "expired-new", 20, "assistant", threadId),
    retentionItem(prefix, "expired-old", 10, "assistant", threadId),
  ];
}

function retentionReplayCount(lane) {
  return lane.mosaicEventLog.events.filter(
    (event) => event.type === "full-replay",
  ).length;
}

function retentionSnapshot(lane) {
  const nodes = new Map(
    Array.from(lane.mosaicPlaneEl.querySelectorAll("[data-message-key]")).map(
      (node) => [node.dataset.messageKey, node],
    ),
  );
  const cards = {};
  for (const card of lane.mosaicCards) {
    const node = nodes.get(card.key);
    cards[card.key] = {
      b: card.b,
      height: node ? node.style.height : "",
      n: card.n,
      span: card.span,
      t: card.t,
      transform: node ? node.style.transform : "",
      width: node ? node.style.width : "",
    };
  }
  return {
    cards,
    keys: lane.mosaicCards.map((card) => card.key),
    replayCount: retentionReplayCount(lane),
  };
}

function retentionSurvivorMismatches(before, after) {
  const mismatches = [];
  for (const [key, coordinates] of Object.entries(before.cards)) {
    if (!after.cards[key]) continue;
    if (JSON.stringify(after.cards[key]) !== JSON.stringify(coordinates))
      mismatches.push({ key, before: coordinates, after: after.cards[key] });
  }
  return mismatches;
}

function retentionSeedLane(lane, messages, limit) {
  lane.retainedMessageLimit = limit;
  mergePayloadMessages(lane, { messages });
}

function retentionNoDividerContract() {
  const messages = [
    { key: "plain-4", kind: "assistant" },
    { key: "plain-3", kind: "assistant" },
    { key: "plain-2", kind: "assistant" },
    { key: "plain-1", kind: "assistant" },
  ];
  return retainedVisibleMessagesAtCompactionFloor(messages, 3).map(
    (item) => item.key,
  );
}

function retentionRunFixtures(args) {
  applyTargetsPayload({ workTrees: args.targets });
  applyTeamSnapshotPayload(
    window.spicePayloads.teamSnapshot({
      revision: 1,
      teams: args.teams,
    }),
    { force: true },
  );

  const solo = laneStore.laneForId("retention-solo");
  const alpha = laneStore.laneForId("retention-alpha");
  const beta = laneStore.laneForId("retention-beta");
  const fused = laneGroupHost(alpha);
  if (!solo || !alpha || !beta || !fused)
    throw new Error("retention fixture lanes were not created");

  const soloEpoch = retentionEpoch("solo", "retention-solo-thread");
  const alphaEpoch = retentionEpoch("alpha", "retention-alpha-thread");
  const betaMessages = [
    retentionItem(
      "beta",
      "interleaved-new",
      45,
      "assistant",
      "retention-beta-thread",
    ),
    retentionItem(
      "beta",
      "interleaved-old",
      15,
      "assistant",
      "retention-beta-thread",
    ),
  ];
  retentionSeedLane(solo, soloEpoch, args.limit);
  retentionSeedLane(alpha, alphaEpoch, args.limit);
  retentionSeedLane(beta, betaMessages, args.limit);
  renderMessagesIfChanged(solo);
  renderMessagesIfChanged(fused);
  const soloBefore = retentionSnapshot(solo);
  const fusedBefore = retentionSnapshot(fused);

  mergePayloadMessages(solo, {
    messages: [
      retentionItem(
        "solo",
        "appended",
        60,
        "assistant",
        "retention-solo-thread",
      ),
    ],
  });
  mergePayloadMessages(alpha, {
    messages: [
      retentionItem(
        "alpha",
        "appended",
        60,
        "assistant",
        "retention-alpha-thread",
      ),
    ],
  });
  renderMessagesIfChanged(solo);
  renderMessagesIfChanged(fused);
  const soloAfter = retentionSnapshot(solo);
  const fusedAfter = retentionSnapshot(fused);
  const soloRetainedKeys = solo.knownMessages.map((item) => item.key);
  const fusedOrder = laneGroupMergedMessages(fused)
    .filter((item) => !isPresenceMessage(item))
    .map((item) => item.key);

  const historyAdded = mergeOlderPayloadMessages(solo, {
    messages: soloEpoch.slice(3),
  });
  renderMessagesIfChanged(solo);
  const soloHistory = retentionSnapshot(solo);

  return {
    fused: {
      after: fusedAfter,
      before: fusedBefore,
      mismatches: retentionSurvivorMismatches(fusedBefore, fusedAfter),
      order: fusedOrder,
      retainedAlphaKeys: alpha.knownMessages.map((item) => item.key),
      retainedBetaKeys: beta.knownMessages.map((item) => item.key),
    },
    history: {
      added: historyAdded,
      keys: solo.knownMessages.map((item) => item.key),
      limit: solo.retainedMessageLimit,
      replayCount: soloHistory.replayCount,
    },
    noDividerKeys: retentionNoDividerContract(),
    solo: {
      after: soloAfter,
      before: soloBefore,
      mismatches: retentionSurvivorMismatches(soloBefore, soloAfter),
      retainedKeys: soloRetainedKeys,
    },
  };
}

function fixtureArgs() {
  return {
    limit: RETENTION_LIMIT,
    targets: [
      targetPayload({
        id: "retention-solo",
        teamId: "retention-solo-team",
        threadId: "retention-solo-thread",
      }),
      targetPayload({
        id: "retention-alpha",
        teamId: "retention-fused-team",
        threadId: "retention-alpha-thread",
      }),
      targetPayload({
        id: "retention-beta",
        teamId: "retention-fused-team",
        threadId: "retention-beta-thread",
      }),
    ],
    teams: [
      retentionTeam("retention-solo-team", ["retention-solo"]),
      retentionTeam("retention-fused-team", [
        "retention-alpha",
        "retention-beta",
      ]),
    ],
  };
}

const retentionPageHelpers = [
  retentionItem,
  retentionEpoch,
  retentionReplayCount,
  retentionSnapshot,
  retentionSurvivorMismatches,
  retentionSeedLane,
  retentionNoDividerContract,
  retentionRunFixtures,
];

async function installRetentionHelpers(page) {
  await page.addScriptTag({
    content: retentionPageHelpers.map((helper) => helper.toString()).join("\n"),
  });
}

function assertFixture(result) {
  const expectedSolo = "solo-appended,solo-current-new,solo-current-old,solo-floor";
  if (result.solo.retainedKeys.join(",") !== expectedSolo)
    throw new Error("solo retention bisected a compaction epoch: " + JSON.stringify(result));
  if (result.solo.mismatches.length)
    throw new Error("solo retention moved surviving cards: " + JSON.stringify(result));
  if (result.solo.after.replayCount !== result.solo.before.replayCount)
    throw new Error("solo retention entered full replay: " + JSON.stringify(result));

  const expectedAlpha = "alpha-appended,alpha-current-new,alpha-current-old,alpha-floor";
  if (result.fused.retainedAlphaKeys.join(",") !== expectedAlpha)
    throw new Error("fused retention bisected alpha's epoch: " + JSON.stringify(result));
  if (
    result.fused.retainedBetaKeys.join(",") !==
    "beta-interleaved-new,beta-interleaved-old"
  )
    throw new Error("fused retention changed the other member: " + JSON.stringify(result));
  if (
    result.fused.order.join(",") !==
    "alpha-appended,alpha-current-new,beta-interleaved-new,alpha-current-old,alpha-floor,beta-interleaved-old"
  )
    throw new Error("fused retention changed merged ordering: " + JSON.stringify(result));
  if (result.fused.mismatches.length)
    throw new Error("fused retention moved surviving cards: " + JSON.stringify(result));
  if (result.fused.after.replayCount !== result.fused.before.replayCount)
    throw new Error("fused retention entered full replay: " + JSON.stringify(result));

  if (result.noDividerKeys.join(",") !== "plain-4,plain-3,plain-2")
    throw new Error("no-divider retention did not use the hard cap: " + JSON.stringify(result));
  if (result.history.added !== 2 || result.history.limit !== 8)
    throw new Error("history pagination did not expand retention: " + JSON.stringify(result));
  if (
    result.history.keys.join(",") !==
    "solo-appended,solo-current-new,solo-current-old,solo-floor,solo-expired-new,solo-expired-old"
  )
    throw new Error("history pagination lost chronological backfill: " + JSON.stringify(result));
  if (result.history.replayCount !== result.solo.after.replayCount)
    throw new Error("history backfill entered full replay: " + JSON.stringify(result));
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-compaction-retention-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 900 } },
    },
    async ({ page }) => {
      await page.waitForFunction(
        () => {
          return (
            typeof applyTeamSnapshotPayload === "function" &&
            typeof retainedVisibleMessagesAtCompactionFloor === "function"
          );
        },
        { timeout: 10000 },
      );
      await page.addScriptTag({ content: installScript });
      await installRetentionHelpers(page);
      const result = await page.evaluate(
        (args) => retentionRunFixtures(args),
        fixtureArgs(),
      );
      return result;
    },
  );
}

if (require.main === module) {
  run()
    .then((result) => {
      assertFixture(result);
      console.log(JSON.stringify(result, null, 2));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { assertFixture, run };
