const { withServePage } = require("./serve_playwright_harness");

const laneReloadTargetCount = 3;

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-lane-reload-" + Date.now(),
      contextOptions: { viewport: { width: 1440, height: 900 } },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof addLane === "function" &&
          typeof laneHintsByTargetId === "function" &&
          Array.isArray(targets) &&
          targets.length >= 3,
        { timeout: 10000 },
      );
      const beforeReload = await page.evaluate((count) => {
        const choices = targets
          .filter((target) => target.targetIdentity?.thread?.state === "bound")
          .slice(0, count);
        if (choices.length < count)
          throw new Error("not enough bound targets for lane reload smoke");
        for (const target of choices) addLane(target.id);
        const targetIds = choices.map((target) => target.id);
        const storedHints = targetIds.map((targetId) =>
          laneHintsByTargetId().get(targetId),
        );
        return {
          laneCount: laneStates.size,
          storedHints,
          targetIds,
        };
      }, laneReloadTargetCount);
      if (beforeReload.laneCount < laneReloadTargetCount)
        throw new Error(
          "lanes were not added before reload: " + JSON.stringify(beforeReload),
        );
      if (
        beforeReload.storedHints.some(
          (hint) => !hint || hint.open !== true || !hint.targetId,
        )
      )
        throw new Error(
          "open lanes were not stored before reload: " +
            JSON.stringify(beforeReload),
        );
      await page.reload({ waitUntil: "domcontentloaded" });
      await page.waitForFunction(
        (targetIds) =>
          targetIds.every((targetId) => laneStates.has(targetId)) &&
          targetIds.every((targetId) =>
            laneHintsByTargetId().get(targetId)?.open === true,
          ),
        beforeReload.targetIds,
        { timeout: 20000 },
      );
      const afterReload = await page.evaluate((targetIds) => ({
        laneCount: laneStates.size,
        restoredTargetIds: targetIds.filter((targetId) => laneStates.has(targetId)),
        storedHints: targetIds.map((targetId) =>
          laneHintsByTargetId().get(targetId),
        ),
      }), beforeReload.targetIds);
      return {
        afterReload,
        beforeReload,
        url: server.url,
      };
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
