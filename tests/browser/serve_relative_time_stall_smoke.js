const {
  assertNoBrowserErrors,
  collectBrowserErrors,
  serveBrowserContextOptions,
  startServe,
} = require("./serve_playwright_harness");
const { chromium } = require("playwright");

// The relative-time clock is browser-local UI. This smoke holds the initial
// targets.refresh request open, matching the observed overloaded-server stall,
// and verifies that a visible timestamp keeps advancing while Serve remains in
// its honest `booting` lifecycle state. No payload or unrelated UI update is
// allowed to provide the changing text.

async function run() {
  const options = {
    contextOptions: { viewport: { width: 1280, height: 800 } },
  };
  const server = await startServe(options);
  let browser = null;
  try {
    browser = await chromium.launch();
    const context = await browser.newContext(
      await serveBrowserContextOptions(options),
    );
    const page = await context.newPage();
    const browserErrors = collectBrowserErrors(page);
    const requests = [];
    await page.routeWebSocket("**/api/live/bus", (socket) => {
      socket.onMessage((raw) => requests.push(JSON.parse(String(raw))));
    });
    await page.goto(
      new URL("/?smoke=serve-relative-time-stall-" + Date.now(), server.url).toString(),
      { waitUntil: "domcontentloaded" },
    );
    await page.waitForFunction(() => {
      return (
        window.__spiceRelativeTimeDiagnostics?.tickCount >= 1 &&
        typeof setRelativeTimeText === "function"
      );
    });
    await page.evaluate(() => {
      const time = document.createElement("time");
      time.id = "relative-time-stall-probe";
      time.dataset.relativeTimestamp = new Date(Date.now() - 100).toISOString();
      time.dataset.relativeFallback = "waiting";
      window.__relativeTimeStallTransitions = [];
      new MutationObserver(() => {
        window.__relativeTimeStallTransitions.push({
          at: Date.now(),
          text: time.textContent,
        });
      }).observe(time, { childList: true, characterData: true, subtree: true });
      document.body.append(time);
    });
    await page.waitForFunction(() => {
      return window.__spiceRelativeTimeDiagnostics?.tickCount >= 4;
    });
    const result = await page.evaluate(() => {
      const transitions = window.__relativeTimeStallTransitions || [];
      return {
        diagnostics: { ...window.__spiceRelativeTimeDiagnostics },
        lifecycle: { ...window.__spiceServeLifecycle },
        pendingRequests: liveBusPendingRequests.size,
        renderedText: document.querySelector("#relative-time-stall-probe")?.textContent,
        transitions,
        uniqueTexts: [...new Set(transitions.map((entry) => entry.text))],
      };
    });
    result.requests = requests;
    assertResult(result);
    assertNoBrowserErrors(browserErrors);
    return { ...result, url: server.url };
  } finally {
    if (browser) await browser.close().catch(() => {});
    await server.stop();
  }
}

function assertResult(result) {
  if (result.lifecycle.state !== "booting")
    throw new Error("held topology must preserve booting lifecycle: " + JSON.stringify(result));
  if (result.pendingRequests !== 1)
    throw new Error("held topology must leave one pending request: " + JSON.stringify(result));
  if (result.requests[0]?.type !== "targets.refresh")
    throw new Error("fixture must hold targets.refresh: " + JSON.stringify(result));
  if (result.diagnostics.tickCount < 4)
    throw new Error("browser-local ticker cadence was incomplete: " + JSON.stringify(result));
  if (result.uniqueTexts.length < 3)
    throw new Error("relative text must advance each second: " + JSON.stringify(result));
  if (!/\d+s$/.test(result.renderedText || ""))
    throw new Error("relative text must render a seconds age: " + JSON.stringify(result));
}

if (require.main === module) {
  run()
    .then((result) => console.log(JSON.stringify(result, null, 2)))
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { assertResult, run };
