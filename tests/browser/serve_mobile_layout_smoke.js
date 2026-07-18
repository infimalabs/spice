const path = require("path");
const {
  installIsolatedLaneFixture,
} = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const MOBILE_VIEWPORT = { width: 390, height: 844 };
const DESKTOP_VIEWPORT = { width: 1280, height: 720 };
const MINIMUM_MODE_BUTTON_HEIGHT_PX = 30;
const SCREENSHOT_PATH = path.join(
  "/tmp",
  "spice-serve-mobile-layout.png",
);
const MENU_SCREENSHOT_PATH = path.join(
  "/tmp",
  "spice-serve-mobile-menu.png",
);
const DESKTOP_SCREENSHOT_PATH = path.join(
  "/tmp",
  "spice-serve-message-footer-desktop.png",
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
  const timestamp = "2027-01-01T00:00:00.000000Z";
  lane.knownMessages = [
    {
      ack_count: 1,
      ack_keys: [],
      ack_segments: [],
      display_html: "<p>Responsive message footer probe.</p>",
      display_text: "Responsive message footer probe.",
      index: 0,
      key: timestamp + "#mobile-footer:0",
      kind: "assistant",
      text: "Responsive message footer probe.",
      timestamp,
    },
  ];
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  syncComposerShards(lane, [lane]);
  setLaneSelectedView(lane, "compose");
  return lane.targetId;
}

function messageFooterLayoutSnapshot(targetId) {
  const lane = laneStore.laneForId(targetId);
  const article = lane.messagesEl.querySelector("article[data-mosaic-card]");
  const footer = article.querySelector(".message-footer");
  const left = footer.querySelector(".message-footer-left");
  const right = footer.querySelector(".message-footer-right");
  const controls = [
    ...footer.querySelectorAll(
      "time, .badge, .message-agent-name, .icon-button",
    ),
  ].map((element) => ({
    label:
      element.getAttribute("aria-label") ||
      element.getAttribute("title") ||
      element.className ||
      element.tagName,
    rect: rectSnapshot(element),
  }));
  return {
    article: rectSnapshot(article),
    controls,
    footer: rectSnapshot(footer),
    left: rectSnapshot(left),
    right: rectSnapshot(right),
  };
}

function assertMessageFooterContained(snapshot, label) {
  assertInside(snapshot.footer, snapshot.article, label + " footer");
  assertInside(snapshot.left, snapshot.footer, label + " metadata group");
  assertInside(snapshot.right, snapshot.footer, label + " action group");
  for (const control of snapshot.controls) {
    assertInside(control.rect, snapshot.footer, label + " " + control.label);
    assertInside(
      control.rect,
      snapshot.article,
      label + " card " + control.label,
    );
  }
}

function mobileLayoutSnapshot(targetId) {
  const viewport = {
    bottom: window.innerHeight,
    left: 0,
    right: window.innerWidth,
    top: 0,
  };
  const lane = laneStore.laneForId(targetId);
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
    messageFooter: messageFooterLayoutSnapshot(targetId),
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
  const lane = laneStore.laneForId(targetId);
  const panel = lane.element.querySelector(
    ".lane-view-panel--active[data-lane-view-panel]",
  );
  return {
    lane: rectSnapshot(lane.element),
    panel: rectSnapshot(panel),
    selectedView: lane.selectedView,
  };
}

async function installMobileLayoutFixture(page) {
  await installIsolatedLaneFixture(page, {
    globals: [
      "renderMessagesIfChanged",
      "setLaneSelectedView",
      "syncComposerShards",
    ],
  });
  await page.addScriptTag({
    content: [
      rectSnapshot,
      prepareMobileLane,
      messageFooterLayoutSnapshot,
      mobileLayoutSnapshot,
      activePanelSnapshot,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

function assertMobileLayout(layout) {
  if (layout.documentWidth !== MOBILE_VIEWPORT.width)
    throw new Error(
      "document width must equal the phone viewport: " + layout.documentWidth,
    );
  assertInside(layout.header, layout.viewport, "header");
  assertInside(layout.menuButton, layout.header, "spice menu button");
  assertInside(layout.lane, layout.viewport, "active lane");
  assertInside(layout.panel, layout.lane, "compose panel");
  assertInside(layout.composerInput, layout.panel, "composer input");
  assertInside(layout.composerControls, layout.panel, "composer control rail");
  assertMessageFooterContained(layout.messageFooter, "mobile message");
  if (layout.messageFooter.right.top < layout.messageFooter.left.bottom)
    throw new Error(
      "mobile message footer rows must stack without overlap: " +
        JSON.stringify(layout.messageFooter),
    );
  for (const [name, action] of Object.entries(layout.composerActions))
    assertInside(action, layout.panel, "composer action " + name);
  for (const [index, button] of layout.modeButtons.entries()) {
    assertInside(button, layout.lane, "lane mode button " + index);
    if (button.height < MINIMUM_MODE_BUTTON_HEIGHT_PX)
      throw new Error("lane mode button must remain at least 30px high");
  }
}

async function exerciseMobileComposer(page, targetId) {
  const composerInput = page.locator(
    '[data-target-id="' + targetId + '"] textarea',
  );
  await composerInput.fill("phone viewport input");
  if ((await composerInput.inputValue()) !== "phone viewport input")
    throw new Error("composer input must accept phone-width typing");

  for (const view of ["filters", "metrics", "info", "compose"]) {
    await page
      .locator(
        '[data-target-id="' +
          targetId +
          '"] [data-lane-view-button="' +
          view +
          '"]',
      )
      .click();
    await page.waitForTimeout(200);
    const active = await page.evaluate(activePanelSnapshot, targetId);
    if (active.selectedView !== view)
      throw new Error("lane view must select " + view);
    assertInside(active.panel, active.lane, view + " panel");
  }
}

async function captureMobileMenu(page, viewport) {
  await page.screenshot({ path: SCREENSHOT_PATH });
  await page.locator("#open-lane").click();
  const menu = page.locator(".spice-context-menu");
  await menu.waitFor({ state: "visible" });
  const menuRect = await menu.evaluate(rectSnapshot);
  assertInside(menuRect, viewport, "spice menu");
  await page.screenshot({ path: MENU_SCREENSHOT_PATH });
  await page.keyboard.press("Escape");
  return menuRect;
}

async function captureDesktopMessageFooter(page, targetId) {
  await page.setViewportSize(DESKTOP_VIEWPORT);
  await page.waitForTimeout(250);
  const snapshot = await page.evaluate(messageFooterLayoutSnapshot, targetId);
  assertMessageFooterContained(snapshot, "desktop message");
  const leftCenter = snapshot.left.top + snapshot.left.height / 2;
  const rightCenter = snapshot.right.top + snapshot.right.height / 2;
  if (Math.abs(leftCenter - rightCenter) > 1)
    throw new Error(
      "desktop message footer must remain a centered single row: " +
        JSON.stringify(snapshot),
    );
  await page.screenshot({ path: DESKTOP_SCREENSHOT_PATH });
  return snapshot;
}

async function runMobileLayoutScenario({ page, server }) {
  await installMobileLayoutFixture(page);
  const targetId = await page.evaluate(prepareMobileLane);
  await page
    .locator('[data-target-id="' + targetId + '"]')
    .scrollIntoViewIfNeeded();
  await page.waitForTimeout(250);
  const layout = await page.evaluate(mobileLayoutSnapshot, targetId);
  assertMobileLayout(layout);
  await exerciseMobileComposer(page, targetId);
  const menu = await captureMobileMenu(page, layout.viewport);
  const desktopMessageFooter = await captureDesktopMessageFooter(page, targetId);
  return {
    desktopMessageFooter,
    layout,
    menu,
    screenshots: [
      SCREENSHOT_PATH,
      MENU_SCREENSHOT_PATH,
      DESKTOP_SCREENSHOT_PATH,
    ],
    url: server.url,
  };
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mobile-layout-" + Date.now(),
      contextOptions: { viewport: MOBILE_VIEWPORT },
    },
    runMobileLayoutScenario,
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
