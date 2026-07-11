const path = require("path");
const {
  installIsolatedLaneFixture,
} = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const MOBILE_VIEWPORT = { width: 390, height: 844 };
const MINIMUM_MODE_BUTTON_HEIGHT_PX = 30;
const SCREENSHOT_PATH = path.join(
  "/tmp",
  "spice-serve-mobile-layout.png",
);
const MENU_SCREENSHOT_PATH = path.join(
  "/tmp",
  "spice-serve-mobile-menu.png",
);

function assertInside(inner, outer, label) {
  const contained =
    inner.left >= outer.left &&
    inner.right <= outer.right &&
    inner.top >= outer.top &&
    inner.bottom <= outer.bottom;
  if (!contained)
    throw new Error(
      label +
        " must fit inside its container: " +
        JSON.stringify({ inner, outer }),
    );
}

function rectSnapshot(element) {
  const rect = element.getBoundingClientRect();
  return {
    bottom: rect.bottom,
    height: rect.height,
    left: rect.left,
    right: rect.right,
    top: rect.top,
    width: rect.width,
  };
}

function prepareMobileLane() {
  const lane = resolveIsolatedLane("mobile-layout-smoke-team");
  lane.emptyTeam = false;
  lane.element.classList.remove("lane--empty-team");
  syncComposerShards(lane, [lane]);
  setLaneSelectedView(lane, "compose");
  return lane.targetId;
}

function mobileLayoutSnapshot(targetId) {
  const viewport = {
    bottom: window.innerHeight,
    left: 0,
    right: window.innerWidth,
    top: 0,
  };
  const lane = laneStates.get(targetId);
  const panel = lane.element.querySelector(
    '[data-lane-view-panel="compose"]',
  );
  const composerActionElements = {
    speech: lane.element.querySelector("[data-speech]"),
    lifetime: lane.element.querySelector("[data-lifetime]"),
    submit: lane.element.querySelector("[data-submit]"),
  };
  for (const [name, element] of Object.entries(composerActionElements)) {
    if (!element) throw new Error("missing composer action: " + name);
  }
  return {
    composerActions: Object.fromEntries(
      Object.entries(composerActionElements).map(([name, element]) => [
        name,
        rectSnapshot(element),
      ]),
    ),
    composerControls: rectSnapshot(
      lane.element.querySelector(".composer-controls"),
    ),
    composerInput: rectSnapshot(lane.element.querySelector("textarea")),
    documentWidth: document.documentElement.scrollWidth,
    header: rectSnapshot(document.querySelector(".app-header")),
    lane: rectSnapshot(lane.element),
    menuButton: rectSnapshot(document.querySelector("#open-lane")),
    modeButtons: [...lane.element.querySelectorAll(".lane-mode-button")].map(
      rectSnapshot,
    ),
    panel: rectSnapshot(panel),
    targetId,
    viewport,
  };
}

function activePanelSnapshot(targetId) {
  const lane = laneStates.get(targetId);
  const panel = lane.element.querySelector(
    ".lane-view-panel--active[data-lane-view-panel]",
  );
  return {
    lane: rectSnapshot(lane.element),
    panel: rectSnapshot(panel),
    selectedView: lane.selectedView,
  };
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mobile-layout-" + Date.now(),
      contextOptions: { viewport: MOBILE_VIEWPORT },
    },
    async ({ page, server }) => {
      await installIsolatedLaneFixture(page, {
        globals: ["setLaneSelectedView", "syncComposerShards"],
      });
      await page.addScriptTag({
        content: [
          rectSnapshot,
          prepareMobileLane,
          mobileLayoutSnapshot,
          activePanelSnapshot,
        ]
          .map((helper) => helper.toString())
          .join("\n"),
      });

      const targetId = await page.evaluate(prepareMobileLane);
      await page
        .locator('[data-target-id="' + targetId + '"]')
        .scrollIntoViewIfNeeded();
      await page.waitForTimeout(250);
      const layout = await page.evaluate(mobileLayoutSnapshot, targetId);
      if (layout.documentWidth !== MOBILE_VIEWPORT.width)
        throw new Error(
          "document width must equal the phone viewport: " +
            layout.documentWidth,
        );
      assertInside(layout.header, layout.viewport, "header");
      assertInside(layout.menuButton, layout.header, "spice menu button");
      assertInside(layout.lane, layout.viewport, "active lane");
      assertInside(layout.panel, layout.lane, "compose panel");
      assertInside(layout.composerInput, layout.panel, "composer input");
      assertInside(
        layout.composerControls,
        layout.panel,
        "composer control rail",
      );
      for (const [name, action] of Object.entries(layout.composerActions))
        assertInside(action, layout.panel, "composer action " + name);
      for (const [index, button] of layout.modeButtons.entries()) {
        assertInside(button, layout.lane, "lane mode button " + index);
        if (button.height < MINIMUM_MODE_BUTTON_HEIGHT_PX)
          throw new Error("lane mode button must remain at least 30px high");
      }
      const composerInput = page.locator(
        '[data-target-id="' + layout.targetId + '"] textarea',
      );
      await composerInput.fill("phone viewport input");
      if ((await composerInput.inputValue()) !== "phone viewport input")
        throw new Error("composer input must accept phone-width typing");

      for (const view of ["filters", "metrics", "info", "compose"]) {
        await page
          .locator(
            '[data-target-id="' +
              layout.targetId +
              '"] [data-lane-view-button="' +
              view +
              '"]',
          )
          .click();
        await page.waitForTimeout(200);
        const active = await page.evaluate(
          activePanelSnapshot,
          layout.targetId,
        );
        if (active.selectedView !== view)
          throw new Error("lane view must select " + view);
        assertInside(active.panel, active.lane, view + " panel");
      }

      await page.screenshot({ path: SCREENSHOT_PATH });
      await page.locator("#open-lane").click();
      const menu = page.locator(".spice-context-menu");
      await menu.waitFor({ state: "visible" });
      const menuRect = await menu.evaluate(rectSnapshot);
      assertInside(menuRect, layout.viewport, "spice menu");
      await page.screenshot({ path: MENU_SCREENSHOT_PATH });

      return {
        layout,
        menu: menuRect,
        screenshots: [SCREENSHOT_PATH, MENU_SCREENSHOT_PATH],
        url: server.url,
      };
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
