const assert = require("assert/strict");
const { withServePage } = require("./serve_playwright_harness");

const expectedLabels = [
  "Renew this agent",
  "Create new team",
  "Leave all teams",
];

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-composer-menu-order-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof composerPrimaryMenuActions === "function" &&
          typeof composerBandMenuTrigger === "function" &&
          typeof laneStore !== "undefined",
        { timeout: 10000 },
      );

      const result = await page.evaluate(() => {
        const targetId = "composer-menu-order-agent";
        const lane = { targetId, closed: false };
        const member = { targetId };
        laneStore.registerLane(lane);

        const band = document.createElement("div");
        band.className = "composer-band";
        const trigger = composerBandMenuTrigger(
          "Composer actions",
          "Composer actions",
          () => composerPrimaryMenuActions(lane, member, "test agent"),
        );
        const textarea = document.createElement("textarea");
        band.append(trigger, textarea);
        document.body.append(band);
        trigger.click();

        const buttons = Array.from(
          band.querySelectorAll(".composer-band-menu-action"),
        );
        const snapshot = {
          labels: buttons.map(
            (button) =>
              button.querySelector(".spice-menu-action-label").textContent,
          ),
          roles: buttons.map((button) => button.getAttribute("role")),
        };

        band.remove();
        laneStore.removeLane(targetId);
        return snapshot;
      });

      assert.deepStrictEqual(result.labels, expectedLabels);
      assert.deepStrictEqual(result.roles, [
        "menuitemcheckbox",
        "menuitem",
        "menuitem",
      ]);
      return { ...result, url: server.url };
    },
  );
}

if (require.main === module) {
  run()
    .then((result) => console.log(JSON.stringify(result, null, 2)))
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run };
