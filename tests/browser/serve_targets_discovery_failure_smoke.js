const { withServePage } = require("./serve_playwright_harness");
const { targetPayload, teamPayload, installScript } = require("./payload_factory");

// Lane survival across a failed worktree discovery.
//
// `git worktree list` fails routinely once many agent worktrees run concurrent
// git operations. Serve used to swallow that failure into an empty record list,
// so the targets payload shipped a single work tree. The client reads a missing
// target as proof the worktree is gone and closes the lane -- removing the DOM
// element, unsubscribing the live bus, disconnecting observers and deleting the
// lane plus its knownMessages. The surviving run then falls under two members
// and the fusion dissolves. Unlike a one-frame render glitch this destroys
// state, and the server caches the short list, so it persists.
//
// A payload that never enumerated the worktrees is not evidence about which
// lanes exist. It must leave every lane, its messages and its fusion intact --
// while a payload that DID enumerate them still retires a removed worktree.

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

function discoveryFixtureArgs() {
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
    // The payload a failed discovery produces: only the anchor root survived,
    // so beta and gamma look deleted to a client that trusts the list.
    shrunkenWorkTrees: [
      targetPayload({ id: "alpha", threadId: "alpha-thread", teamId: TEAM_ID }),
    ],
    discoveryError: "could not list git worktrees from /repo",
    messagesByTargetId: {
      alpha: [message("alpha-1", 4, "alpha-thread")],
      beta: [message("beta-1", 12, "beta-thread"), message("beta-2", 8, "beta-thread")],
      gamma: [message("gamma-1", 20, "gamma-thread")],
    },
  };
}

// One reading of everything the operator can see change: which cards the fused
// host owns, the lattice extent, the group's composer order, and whether the
// lanes still exist in the store at all.
function readBoardStateSource() {
  return (hostTargetId) => {
    const hostLane = laneStore.laneForId(hostTargetId);
    const host = hostLane ? laneGroupHost(hostLane) : null;
    if (!host) {
      return {
        hostTargetId: "",
        planeKeys: [],
        cardCount: 0,
        memberTargetIds: [],
        extentHeight: 0,
        laneIds: laneStore
          .lanesSnapshot()
          .map((lane) => {
            return lane.targetId;
          })
          .sort(),
      };
    }
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
      laneIds: laneStore
        .lanesSnapshot()
        .map((lane) => {
          return lane.targetId;
        })
        .sort(),
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

// The failed discovery: a short workTrees list that reports why it is short.
async function applyFailedDiscovery(page, args) {
  return page.evaluate(
    ({ shrunkenWorkTrees, discoveryError }) => {
      applyTargetsPayload({
        workTrees: shrunkenWorkTrees,
        defaultTargetId: "alpha",
        taskFilterInventory: {},
        targetsDiscoveryErrors: [discoveryError],
      });
      return window.__readBoardState("alpha");
    },
    { shrunkenWorkTrees: args.shrunkenWorkTrees, discoveryError: args.discoveryError },
  );
}

// The same short list from a discovery that actually ran: those worktrees are
// genuinely gone, so their lanes must still be retired.
async function applySuccessfulShrink(page, args) {
  return page.evaluate(
    ({ shrunkenWorkTrees }) => {
      applyTargetsPayload({
        workTrees: shrunkenWorkTrees,
        defaultTargetId: "alpha",
        taskFilterInventory: {},
      });
      return window.__readBoardState("alpha");
    },
    { shrunkenWorkTrees: args.shrunkenWorkTrees },
  );
}

async function installPayloadFactory(page) {
  await page.addScriptTag({ content: installScript });
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-targets-discovery-failure-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 800 } },
    },
    async ({ page }) => {
      await page.waitForFunction(
        // Braced body: lizard (the complexity gate) misparses a braceless
        // arrow followed by an object-literal argument.
        () => {
          return (
            typeof applyTeamSnapshotPayload === "function" &&
            typeof applyTargetsPayload === "function"
          );
        },
        { timeout: 10000 },
      );
      await installPayloadFactory(page);
      const args = discoveryFixtureArgs();
      const settled = await buildFusedBoard(page, args);
      const failed = await applyFailedDiscovery(page, args);
      const enumerated = await applySuccessfulShrink(page, args);
      return { settled, failed, enumerated };
    },
  );
}

function assertResult(result) {
  const { settled, failed, enumerated } = result;
  if (settled.memberTargetIds.join(",") !== "alpha,beta,gamma")
    throw new Error(
      "fixture did not fuse all three members before the failure: " +
        JSON.stringify(settled),
    );
  if (settled.planeKeys.join(",") !== "alpha-1,beta-1,beta-2,gamma-1")
    throw new Error(
      "fixture did not merge every member's messages before the failure: " +
        JSON.stringify(settled),
    );
  if (failed.laneIds.join(",") !== settled.laneIds.join(","))
    throw new Error(
      "a failed discovery closed lanes and deleted their messages: " +
        JSON.stringify(result),
    );
  if (failed.memberTargetIds.join(",") !== settled.memberTargetIds.join(","))
    throw new Error(
      "a failed discovery dissolved the group run: " + JSON.stringify(result),
    );
  if (failed.planeKeys.join(",") !== settled.planeKeys.join(","))
    throw new Error(
      "a failed discovery dropped merged cards from the fused host: " +
        JSON.stringify(result),
    );
  if (failed.cardCount !== settled.cardCount)
    throw new Error(
      "a failed discovery changed the lattice card count: " + JSON.stringify(result),
    );
  if (failed.hostTargetId !== settled.hostTargetId)
    throw new Error("a failed discovery moved the visible host: " + JSON.stringify(result));
  if (failed.extentHeight !== settled.extentHeight)
    throw new Error(
      "a failed discovery collapsed the mosaic extent: " + JSON.stringify(result),
    );
  // Differentiation: the guard keys off the reported failure, not off refusing
  // to ever close a lane. The same short list from a discovery that ran retires
  // the removed worktrees.
  if (enumerated.laneIds.join(",") !== "alpha")
    throw new Error(
      "an enumerated shrink failed to retire removed worktrees, so the guard is " +
        "suppressing every closure rather than only untrustworthy ones: " +
        JSON.stringify(result),
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
