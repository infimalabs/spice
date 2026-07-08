const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

// Reload persistence rides the SERVER, not the browser: the scratch task
// backend's ensure-open-team affordance persists one member-less shell team
// in sqlite, and applyTeamSnapshotPayload rehydrates it as an empty-team
// lane on every boot. This smoke stamps that shell with a non-default
// speechMode (createTeam with no members reuses the oldest shell instead of
// minting a sibling), records its server-assigned teamId, reloads, and
// asserts the SAME teamId and config come back from the team snapshot.
// Team ids are random hex per creation, so identity across the reload
// proves the lane came from the store -- no real targets, no localStorage.

async function laneReloadSetupPage() {
  const speechMode = speechModes.find((mode) => mode !== defaultSpeechMode);
  if (!speechMode) throw new Error("no non-default speech mode available");
  await requestTeamCommand(
    teamCommandPayload("createTeam", {
      members: [],
      config: { ...defaultTeamConfig(), speechMode },
    }),
  );
  const shells = Array.from(laneStates.values()).filter(
    (lane) => lane.emptyTeam && lane.teamId,
  );
  if (shells.length !== 1)
    throw new Error(
      "expected exactly one empty-team shell lane, got " + shells.length,
    );
  const lane = shells[0];
  return {
    laneCount: laneStates.size,
    requestedSpeechMode: speechMode,
    speechMode: lane.speechMode,
    targetId: lane.targetId,
    teamId: lane.teamId,
  };
}

function laneReloadVerifyPage(before) {
  const lane = laneStates.get(before.targetId);
  return {
    emptyTeam: Boolean(lane && lane.emptyTeam),
    laneCount: laneStates.size,
    present: Boolean(lane),
    speechMode: lane ? lane.speechMode : "",
    teamId: lane ? lane.teamId : "",
  };
}

const laneReloadFixtureOptions = {
  globals: [
    "requestTeamCommand",
    "teamCommandPayload",
    "defaultTeamConfig",
    "speechModes",
    "defaultSpeechMode",
  ],
};

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-lane-reload-" + Date.now(),
      contextOptions: { viewport: { width: 1440, height: 900 } },
    },
    async ({ page, server }) => {
      await installIsolatedLaneFixture(page, laneReloadFixtureOptions);
      const beforeReload = await page.evaluate(laneReloadSetupPage);
      if (!beforeReload.teamId)
        throw new Error(
          "shell team has no server teamId: " + JSON.stringify(beforeReload),
        );
      if (beforeReload.speechMode !== beforeReload.requestedSpeechMode)
        throw new Error(
          "shell lane did not take the stamped speechMode: " +
            JSON.stringify(beforeReload),
        );
      await page.reload({ waitUntil: "domcontentloaded" });
      await installIsolatedLaneFixture(page, laneReloadFixtureOptions);
      await page.waitForFunction(
        (before) => {
          const lane = laneStates.get(before.targetId);
          return Boolean(lane && lane.emptyTeam && lane.teamId === before.teamId);
        },
        beforeReload,
        { timeout: 10000 },
      );
      const afterReload = await page.evaluate(laneReloadVerifyPage, beforeReload);
      if (!afterReload.present || !afterReload.emptyTeam)
        throw new Error(
          "shell lane did not rehydrate after reload: " +
            JSON.stringify(afterReload),
        );
      if (afterReload.teamId !== beforeReload.teamId)
        throw new Error(
          "rehydrated lane changed teamId: " +
            JSON.stringify({ afterReload, beforeReload }),
        );
      if (afterReload.speechMode !== beforeReload.speechMode)
        throw new Error(
          "stamped speechMode did not survive the reload: " +
            JSON.stringify({ afterReload, beforeReload }),
        );
      return { afterReload, beforeReload, url: server.url };
    },
  );
}

if (require.main === module) {
  run()
    .then((result) => {
      console.log(JSON.stringify(result, null, 2));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run };
