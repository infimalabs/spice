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
            checked: button.getAttribute("aria-checked"),
          })),
        );
      const fastModeAction = actions.find((action) => action.label === "Fast mode");
      if (!fastModeAction)
        throw new Error("Fast mode action missing: " + JSON.stringify(actions));
      if (fastModeAction.detail !== "off")
        throw new Error("Unexpected Fast mode detail: " + fastModeAction.detail);
      if (fastModeAction.checked !== "false")
        throw new Error(
          "Unexpected Fast mode checked state: " + fastModeAction.checked,
        );
      return {
        actionCount: actions.length,
        fastModeDetail: fastModeAction.detail,
        fastModeChecked: fastModeAction.checked,
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
