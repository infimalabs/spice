const { withServePage } = require("./serve_playwright_harness");

const headerRightTolerancePx = 16;
const pillLeftTolerancePx = 24;

async function fastModeActionState(page) {
  const actions = await page
    .locator(".spice-context-menu .spice-menu-action")
    .evaluateAll((buttons) =>
      buttons.map((button) => ({
        label: button.querySelector(".spice-menu-action-label").textContent,
        detail: button.querySelector(".spice-menu-action-detail").textContent,
        checked: button.getAttribute("aria-checked"),
      })),
    );
  const action = actions.find((item) => item.label === "Fast mode");
  if (!action)
    throw new Error("Fast mode action missing: " + JSON.stringify(actions));
  return action;
}

async function setFastModeFromMenu(page, enabled) {
  await page.evaluate(async (next) => {
    const action = [...document.querySelectorAll(".spice-menu-action")].find(
      (button) =>
        button.querySelector(".spice-menu-action-label").textContent ===
        "Fast mode",
    );
    if (!action) throw new Error("Fast mode action missing");
    await setFastModeEnabled(next);
  }, enabled);
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-menu-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      const menuButton = page.locator(".spice-menu-button").first();
      await menuButton.waitFor({ state: "visible", timeout: 10000 });
      const headerLayout = await page.evaluate(() => {
        const header = document.querySelector(".app-header");
        const strip = document.querySelector("#filter-strip");
        const button = document.querySelector("#open-lane");
        strip.setAttribute("aria-hidden", "false");
        strip.replaceChildren();
        const pill = document.createElement("span");
        pill.className = "filter-pill";
        pill.textContent = "task";
        strip.append(pill);
        const headerRect = header.getBoundingClientRect();
        const pillRect = pill.getBoundingClientRect();
        const buttonRect = button.getBoundingClientRect();
        return {
          buttonLeft: buttonRect.left,
          buttonRight: buttonRect.right,
          headerLeft: headerRect.left,
          headerRight: headerRect.right,
          pillLeft: pillRect.left,
          pillRight: pillRect.right,
        };
      });
      if (
        Math.abs(headerLayout.headerRight - headerLayout.buttonRight) >
        headerRightTolerancePx
      )
        throw new Error("spice button is not pinned right");
      if (headerLayout.pillLeft - headerLayout.headerLeft > pillLeftTolerancePx)
        throw new Error("filter pills are not anchored left");
      if (headerLayout.pillRight >= headerLayout.buttonLeft)
        throw new Error("filter pills overlap the spice button");
      await menuButton.click();
      await page.waitForSelector(".spice-context-menu .spice-menu-action", {
        timeout: 5000,
      });
      const actionCount = await page
        .locator(".spice-context-menu .spice-menu-action")
        .count();
      const fastModeAction = await fastModeActionState(page);
      if (fastModeAction.detail !== "off")
        throw new Error("Unexpected Fast mode detail: " + fastModeAction.detail);
      if (fastModeAction.checked !== "false")
        throw new Error(
          "Unexpected Fast mode checked state: " + fastModeAction.checked,
        );
      await setFastModeFromMenu(page, true);
      const enabledFastModeAction = await fastModeActionState(page);
      if (enabledFastModeAction.detail !== "on")
        throw new Error(
          "Fast mode did not toggle on: " + enabledFastModeAction.detail,
        );
      if (enabledFastModeAction.checked !== "true")
        throw new Error(
          "Fast mode checked state did not toggle on: " +
            enabledFastModeAction.checked,
        );
      await page.reload({ waitUntil: "domcontentloaded" });
      await menuButton.waitFor({ state: "visible", timeout: 10000 });
      await menuButton.click();
      await page.waitForSelector(".spice-context-menu .spice-menu-action", {
        timeout: 5000,
      });
      const reloadedFastModeAction = await fastModeActionState(page);
      if (reloadedFastModeAction.detail !== "on")
        throw new Error(
          "Fast mode did not survive reload: " + reloadedFastModeAction.detail,
        );
      if (reloadedFastModeAction.checked !== "true")
        throw new Error(
          "Fast mode checked state did not survive reload: " +
            reloadedFastModeAction.checked,
        );
      return {
        actionCount,
        fastModeDetail: fastModeAction.detail,
        fastModeChecked: fastModeAction.checked,
        fastModeReloadedDetail: reloadedFastModeAction.detail,
        fastModeReloadedChecked: reloadedFastModeAction.checked,
        headerLayout,
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
