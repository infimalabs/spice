const { withServePage } = require("./serve_playwright_harness");

async function waitForImportShell(page) {
  await page.waitForFunction(
    () => {
      const lanes = Array.from(laneStates.values());
      return lanes.length === 1 && lanes[0].emptyTeam && lanes[0].teamId;
    },
    {},
    { timeout: 10000 },
  );
}

function pageTopology() {
  const lanes = Array.from(laneStates.values()).map((lane) => ({
    targetId: lane.targetId,
    emptyTeam: Boolean(lane.emptyTeam),
    teamId: lane.teamId || "",
  }));
  return {
    laneCount: laneStates.size,
    lanes,
    storedConfig: localStorage.getItem(laneStorageKey),
    snapshotRevision: teamSnapshotRevision,
    targetIds: laneStore.targetsSnapshot().map((target) => target.id),
  };
}

function installStaleOpenLaneHints() {
  const hints = laneStore.targetsSnapshot().map((target, index) => ({
    targetId: target.id,
    open: true,
    speechMode: defaultSpeechMode,
    selectedView: index % 2 ? "metrics" : "compose",
  }));
  localStorage.setItem(laneStorageKey, JSON.stringify(hints));
  return hints.length;
}

function assertEqual(actual, expected, message, details = null) {
  if (actual === expected) return;
  throw new Error(message + ": " + JSON.stringify(details || { actual, expected }));
}

function assertJsonEqual(actual, expected, message, details = null) {
  assertEqual(JSON.stringify(actual), JSON.stringify(expected), message, details);
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-fresh-startup-import-shell-" + Date.now(),
      contextOptions: { viewport: { width: 1440, height: 900 } },
    },
    async ({ page, server }) => {
      await waitForImportShell(page);
      const beforeReload = await page.evaluate(pageTopology);
      const hintedCount = await page.evaluate(installStaleOpenLaneHints);
      if (hintedCount < 1)
        throw new Error("fresh startup smoke needs at least one target hint");
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForImportShell(page);
      const afterReload = await page.evaluate(pageTopology);
      const expectedLanes = [
        {
          targetId: afterReload.lanes[0].targetId,
          emptyTeam: true,
          teamId: afterReload.lanes[0].teamId,
        },
      ];
      assertJsonEqual(
        afterReload.lanes,
        expectedLanes,
        "fresh startup topology must settle on the import shell",
        afterReload,
      );
      assertEqual(
        afterReload.storedConfig,
        "[]",
        "fresh startup must rewrite stale lane config",
        afterReload,
      );
      assertEqual(
        afterReload.snapshotRevision,
        beforeReload.snapshotRevision,
        "stale open-lane hints must not mutate the team store revision",
        { beforeReload, afterReload },
      );
      return { afterReload, beforeReload, hintedCount, url: server.url };
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
