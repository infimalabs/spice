const { withServePage } = require("./serve_playwright_harness");

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-menu-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      const menuButton = page.locator(".spice-menu-button").first();
      await menuButton.waitFor({ state: "visible", timeout: 10000 });
      await menuButton.click();
      await page.waitForSelector(".spice-context-menu .spice-menu-action", {
        timeout: 5000,
      });
      const actions = await page
        .locator(".spice-context-menu .spice-menu-action")
        .evaluateAll((buttons) =>
          buttons.map((button) => ({
            label: button.querySelector(".spice-menu-action-label").textContent,
            detail: button.querySelector(".spice-menu-action-detail").textContent,
          })),
        );
      const newTeamAction = actions.find((action) => action.label === "New team");
      if (!newTeamAction)
        throw new Error("New team action missing: " + JSON.stringify(actions));
      if (newTeamAction.detail !== "no agents")
        throw new Error("Unexpected New team detail: " + newTeamAction.detail);
      return {
        actionCount: actions.length,
        newTeamDetail: newTeamAction.detail,
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
