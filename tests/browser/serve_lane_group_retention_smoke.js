const { withServePage } = require("./serve_playwright_harness");
const { targetPayload, teamPayload, installScript } = require("./payload_factory");

// Group retention across a transient team refresh.
//
// Starting an idle agent renews its thread: the team snapshot names the member
// under its NEW thread actor before the lane and its target carry that thread.
// The member is unresolvable for that one refresh. The store already refuses to
// close such a lane (it stays in the desired set), but it used to drop the lane
// from the team's group run anyway -- so the fused host's merged stream lost
// every card that lane owned, and with enough members unresolved the run fell
// under two and the whole fusion dissolved. On the board that reads as message
// cards vanishing and the mosaic collapsing, then everything snapping back one
// refresh later.
//
// A lane retained BECAUSE it still belongs to the team must stay in that team's
// run, in the position it already held: membership and retention are one
// decision, not two that can disagree for a frame.

const TEAM_ID = "team-main";

function message(key, minutesAgo, threadId) {
  return {
    key,
    kind: "final",
    text: "message " + key,
    timestamp: new Date(Date.now() - minutesAgo * 60000).toISOString(),
    index: 1,
    threadId,
  };
}

function retentionFixtureArgs() {
  return {
    targets: [
      targetPayload({ id: "alpha", threadId: "alpha-thread", teamId: TEAM_ID }),
      targetPayload({ id: "beta", threadId: "beta-thread", teamId: TEAM_ID }),
      targetPayload({ id: "gamma", threadId: "gamma-thread", teamId: TEAM_ID }),
    ],
    settledTeam: teamPayload({
      teamId: TEAM_ID,
      memberIds: ["alpha", "beta", "gamma"],
      revision: 1,
    }),
    // Two members renewing at once: the browser's single-stale-slot fallback
    // (renewedTeamSlotTargetId) deliberately refuses to guess between two
    // candidates, so both stay unresolved -- the exact state a drain queue
    // starting agents produces.
    renewingTeam: teamPayload(
      { teamId: TEAM_ID, memberIds: ["alpha"], revision: 2 },
      {
        members: [
          { agentId: "target:alpha" },
          { agentId: "thread:beta-next" },
          { agentId: "thread:gamma-next" },
        ],
      },
    ),
    messagesByTargetId: {
      alpha: [message("alpha-1", 4, "alpha-thread")],
      beta: [message("beta-1", 12, "beta-thread"), message("beta-2", 8, "beta-thread")],
      gamma: [message("gamma-1", 20, "gamma-thread")],
    },
  };
}

// One reading of everything the operator can see change: which cards the fused
// host actually owns, the lattice extent (mosaic height), and the group's
// composer order.
function readBoardStateSource() {
  return (hostTargetId) => {
    const host = laneGroupHost(laneStore.laneForId(hostTargetId));
    const planeKeys = Array.from(
      host.mosaicPlaneEl ? host.mosaicPlaneEl.querySelectorAll("[data-message-key]") : [],
    )
      .map((node) => {
        return node.dataset.messageKey;
      })
      .sort();
    return {
      hostTargetId: host.targetId,
      planeKeys,
      cardCount: host.mosaicCards.length,
      memberTargetIds: laneGroupMemberTargetIds(host).slice(),
      extentHeight: host.mosaicExtentEl
        ? Math.round(host.mosaicExtentEl.getBoundingClientRect().height)
        : 0,
    };
  };
}

async function buildFusedBoard(page, args) {
  return page.evaluate(
    ({ targets, settledTeam, messagesByTargetId, readBoardStateText }) => {
      const readBoardState = eval("(" + readBoardStateText + ")");
      window.__readBoardState = readBoardState;
      laneStore.replaceTargets(targets);
      applyTeamSnapshotPayload(
        window.spicePayloads.teamSnapshot({ revision: 1, teams: [settledTeam] }),
        { force: true },
      );
      for (const targetId of Object.keys(messagesByTargetId)) {
        const lane = laneStore.laneForId(targetId);
        if (!lane) throw new Error("missing lane for " + targetId);
        lane.knownMessages = messagesByTargetId[targetId];
        lane.retainedMessageLimit = 50;
      }
      const host = laneGroupHost(laneStore.laneForId("alpha"));
      host.renderedMessageFingerprint = "";
      renderMessagesIfChanged(host);
      return readBoardState(host.targetId);
    },
    { ...args, readBoardStateText: readBoardStateSource().toString() },
  );
}

// The one transient refresh under test: the renewing snapshot lands, every
// consumer repaints exactly as production wires them, and the board is read
// back before anything can settle.
async function applyRenewingRefresh(page, args) {
  return page.evaluate(
    ({ renewingTeam }) => {
      applyTeamSnapshotPayload(
        window.spicePayloads.teamSnapshot({ revision: 2, teams: [renewingTeam] }),
        { force: true },
      );
      return window.__readBoardState("alpha");
    },
    { renewingTeam: args.renewingTeam },
  );
}

async function installPayloadFactory(page) {
  await page.addScriptTag({ content: installScript });
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-lane-group-retention-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 800 } },
    },
    async ({ page }) => {
      await page.waitForFunction(
        // Braced body: lizard (the complexity gate) misparses a braceless
        // arrow followed by an object-literal argument.
        () => {
          return typeof applyTeamSnapshotPayload === "function";
        },
        { timeout: 10000 },
      );
      await installPayloadFactory(page);
      const args = retentionFixtureArgs();
      const settled = await buildFusedBoard(page, args);
      const renewing = await applyRenewingRefresh(page, args);
      return { settled, renewing };
    },
  );
}

function assertResult(result) {
  const { settled, renewing } = result;
  if (settled.memberTargetIds.join(",") !== "alpha,beta,gamma")
    throw new Error(
      "fixture did not fuse all three members before the refresh: " +
        JSON.stringify(settled),
    );
  if (settled.planeKeys.join(",") !== "alpha-1,beta-1,beta-2,gamma-1")
    throw new Error(
      "fixture did not merge every member's messages before the refresh: " +
        JSON.stringify(settled),
    );
  if (renewing.memberTargetIds.join(",") !== settled.memberTargetIds.join(","))
    throw new Error(
      "transient refresh changed the group's composer order: " +
        JSON.stringify(result),
    );
  if (renewing.planeKeys.join(",") !== settled.planeKeys.join(","))
    throw new Error(
      "transient refresh dropped merged cards from the fused host: " +
        JSON.stringify(result),
    );
  if (renewing.cardCount !== settled.cardCount)
    throw new Error(
      "transient refresh changed the lattice card count: " + JSON.stringify(result),
    );
  if (renewing.hostTargetId !== settled.hostTargetId)
    throw new Error(
      "transient refresh moved the visible host: " + JSON.stringify(result),
    );
  if (renewing.extentHeight !== settled.extentHeight)
    throw new Error(
      "transient refresh collapsed the mosaic extent: " + JSON.stringify(result),
    );
}

if (require.main === module) {
  run()
    .then((result) => {
      assertResult(result);
      console.log(JSON.stringify(result, null, 2));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run, assertResult };
