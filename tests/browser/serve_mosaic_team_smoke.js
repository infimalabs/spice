const { withServePage } = require("./serve_playwright_harness");
const { targetPayload, teamPayload, installScript } = require("./payload_factory");

// Mosaic team/fusion surface. A fused team host must run exactly one lattice over the
// merged (epoch, index, key) creation order -- never one lattice per
// member, and never DOM/arrival-observation order standing in for true
// creation order. Also covers the companion width-borrowing deletion: a
// lone visible lane's own flex width already answers "should this lane use
// the full swimlanes width", with no packing-time CSS override needed.

// Expected soloWidthDeltaPx is just .swimlanes' own padding (20px) plus the
// lane's border (2px) -- real budget for chrome, not a bound near zero. A
// regression that stopped the lone lane from claiming full width would
// leave a delta on the order of a whole sibling lane's worth of pixels, far
// past this bound.
const MOSAIC_TEAM_SMOKE_WIDTH_DELTA_BOUND_PX = 30;

function team(memberIds, revision) {
  return teamPayload({ teamId: "team-main", memberIds, revision });
}

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

// Solo alpha, one message: exercises the width-borrowing deletion (no
// sibling lane, so nothing to "borrow" width from/for -- CSS alone must
// already give this lane the full swimlanes width) and captures the
// pre-fuse plane count.
async function measureSoloAlpha(page, { alpha, soloTeam, soloMessage, alphaMessage }) {
  return page.evaluate(
    ({ alpha, soloTeam, soloMessage, alphaMessage }) => {
      applyTargetsPayload({ workTrees: [alpha] });
      applyTeamSnapshotPayload(
        window.spicePayloads.teamSnapshot({ revision: 1, teams: [soloTeam] }),
        { force: true },
      );
      const solo = laneStore.laneForId("alpha");
      if (!solo) throw new Error("missing solo alpha lane");

      solo.knownMessages = [soloMessage];
      solo.retainedMessageLimit = 50;
      renderMessagesIfChanged(solo);
      const swimlanesEl = document.querySelector(".swimlanes");
      const soloWidthDeltaPx = swimlanesEl.clientWidth - solo.messagesEl.clientWidth;

      solo.knownMessages = [alphaMessage];
      renderMessagesIfChanged(solo);
      const planesBeforeFuse = document.querySelectorAll(".mosaic-plane").length;

      return { soloWidthDeltaPx, planesBeforeFuse };
    },
    { alpha, soloTeam, soloMessage, alphaMessage },
  );
}

// Fuses beta in with an OLDER backlog than alpha's already-rendered message,
// then reads back the merged lattice: one plane, both members' keys inside
// it, and creationIndex following true age rather than fuse/observation
// order (the bug this task fixes).
async function fuseBetaAndMeasureLattice(page, { alpha, beta, fusedTeam, betaMessages }) {
  return page.evaluate(
    ({ alpha, beta, fusedTeam, betaMessages }) => {
      applyTargetsPayload({ workTrees: [alpha, beta] });
      applyTeamSnapshotPayload(
        window.spicePayloads.teamSnapshot({ revision: 2, teams: [fusedTeam] }),
        { force: true },
      );
      const host = laneGroupHost(laneStore.laneForId("alpha"));
      const betaLane = laneStore.laneForId("beta");
      betaLane.knownMessages = betaMessages;
      betaLane.retainedMessageLimit = 50;
      host.renderedMessageFingerprint = "";
      renderMessagesIfChanged(host);

      const planesAfterFuse = document.querySelectorAll(".mosaic-plane").length;
      const plane = host.mosaicPlaneEl;
      const keysInPlane = Array.from(
        plane.querySelectorAll("[data-message-key]"),
      )
        .map((node) => node.dataset.messageKey)
        .sort();

      const byKey = new Map(host.mosaicCards.map((card) => [card.key, card]));
      const creationIndexByKey = Object.fromEntries(
        ["alpha-1", "beta-1", "beta-2"].map((key) => [
          key,
          byKey.get(key) ? byKey.get(key).creationIndex : null,
        ]),
      );

      return { planesAfterFuse, keysInPlane, creationIndexByKey };
    },
    { alpha, beta, fusedTeam, betaMessages },
  );
}

// Removes beta from the team: the "their cards leave with them" full
// replay must drop the departed member's cards from the survivor lattice.
async function removeBetaAndMeasureSurvivors(page, { alpha, survivorTeam }) {
  return page.evaluate(
    ({ alpha, survivorTeam }) => {
      applyTargetsPayload({ workTrees: [alpha] });
      applyTeamSnapshotPayload(
        window.spicePayloads.teamSnapshot({ revision: 3, teams: [survivorTeam] }),
        { force: true },
      );
      const survivorHost = laneGroupHost(laneStore.laneForId("alpha"));
      survivorHost.renderedMessageFingerprint = "";
      renderMessagesIfChanged(survivorHost);
      const survivorKeys = survivorHost.mosaicCards
        .map((card) => card.key)
        .sort();
      return { survivorKeys };
    },
    { alpha, survivorTeam },
  );
}

async function installPayloadFactory(page) {
  await page.addScriptTag({ content: installScript });
}

function mosaicTeamSmokeFixtureArgs() {
  return {
    alpha: targetPayload({ id: "alpha", threadId: "alpha-thread", teamId: "team-main" }),
    beta: targetPayload({ id: "beta", threadId: "beta-thread", teamId: "team-main" }),
    soloTeam: team(["alpha"], 1),
    soloMessage: message("solo-1", 5, "alpha-thread"),
    alphaMessage: message("alpha-1", 5, "alpha-thread"),
    fusedTeam: team(["alpha", "beta"], 2),
    betaMessages: [
      message("beta-1", 30, "beta-thread"),
      message("beta-2", 20, "beta-thread"),
    ],
    survivorTeam: team(["alpha"], 3),
  };
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-team-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 800 } },
    },
    async ({ page }) => {
      await page.waitForFunction(
        // Braced body: lizard (the complexity gate) misparses a braceless
        // arrow followed by an object-literal argument, misattributing
        // function spans to the end of the file.
        () => {
          return typeof applyTeamSnapshotPayload === "function";
        },
        { timeout: 10000 },
      );
      await installPayloadFactory(page);
      const args = mosaicTeamSmokeFixtureArgs();
      const solo = await measureSoloAlpha(page, args);
      const fused = await fuseBetaAndMeasureLattice(page, args);
      const survivors = await removeBetaAndMeasureSurvivors(page, args);
      return { ...solo, ...fused, ...survivors };
    },
  );
}

function assertResult(result) {
  if (result.soloWidthDeltaPx > MOSAIC_TEAM_SMOKE_WIDTH_DELTA_BOUND_PX)
    throw new Error(
      "lone lane did not reach full swimlanes width without root-width borrowing: " +
        JSON.stringify(result),
    );
  if (result.planesBeforeFuse !== 1)
    throw new Error("standalone lane did not render its own plane: " + JSON.stringify(result));
  if (result.planesAfterFuse !== 1)
    throw new Error(
      "fused host used more than one lattice/plane: " + JSON.stringify(result),
    );
  if (result.keysInPlane.join(",") !== "alpha-1,beta-1,beta-2")
    throw new Error(
      "fused host's plane did not contain every member's messages: " +
        JSON.stringify(result),
    );
  const { "alpha-1": alphaIndex, "beta-1": beta1Index, "beta-2": beta2Index } =
    result.creationIndexByKey;
  if (alphaIndex === null || beta1Index === null || beta2Index === null)
    throw new Error("fused cards missing from lattice: " + JSON.stringify(result));
  // beta's backlog (30/20 minutes old) is OLDER than alpha's message (5
  // minutes old), even though beta was only just merged in -- creation
  // order must reflect true age, not observation/arrival order.
  if (!(beta1Index < beta2Index && beta2Index < alphaIndex))
    throw new Error(
      "creation order did not follow true (epoch, index, key) order across the fuse: " +
        JSON.stringify(result),
    );
  if (result.survivorKeys.join(",") !== "alpha-1")
    throw new Error(
      "member removal did not leave with their cards: " + JSON.stringify(result),
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
