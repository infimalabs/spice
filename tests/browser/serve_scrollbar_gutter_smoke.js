const { withServePage } = require("./serve_playwright_harness");

// Mosaic §9 scrollbar rule: the pack host must reserve its scrollbar gutter
// from the first frame, so growing content past one viewport of height never
// changes clientWidth (which would otherwise shrink colW and slide every
// card horizontally on the next geometry pass).
//
// The scrollbarGutter assertion is the one that actually discriminates here
// (see OOPS-1k9xxgf8): this sandbox's headless Chromium renders zero-width
// overlay scrollbars, so clientWidth/markerLeft stay put across the overflow
// boundary regardless of the fix. They're kept anyway because the CSS spec
// guarantees they follow from scrollbar-gutter: stable, and a classic-
// scrollbar platform (Linux/Windows Chromium) would fail them without it.
async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-scrollbar-gutter-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 500 } },
    },
    async ({ page, server }) => {
      await page.waitForSelector(".lane", { timeout: 10000 });
      await page.waitForFunction(
        () => Array.isArray(targets) && targets.length > 0,
        { timeout: 10000 },
      );
      const result = await page.evaluate(() => {
        let lane = Array.from(laneStates.values()).find(
          (item) => !item.emptyTeam,
        );
        if (!lane && targets.length) {
          addLane(targets[0].id);
          lane = laneStates.get(targets[0].id);
        }
        if (!lane) throw new Error("no lane available for scrollbar-gutter smoke");
        const host = lane.messagesEl;
        host.innerHTML = "";

        const marker = document.createElement("article");
        marker.dataset.messageKey = "scrollbar-gutter-smoke-marker";
        marker.style.gridColumn = "1 / span 2";
        marker.textContent = "marker";
        host.appendChild(marker);

        const before = {
          scrollbarGutter: getComputedStyle(host).scrollbarGutter,
          clientWidth: host.clientWidth,
          overflowing: host.scrollHeight > host.clientHeight,
          markerLeft: marker.getBoundingClientRect().left,
        };

        const filler = document.createElement("div");
        filler.style.gridColumn = "1 / -1";
        filler.style.height = host.clientHeight * 4 + 200 + "px";
        host.appendChild(filler);

        const after = {
          clientWidth: host.clientWidth,
          overflowing: host.scrollHeight > host.clientHeight,
          markerLeft: marker.getBoundingClientRect().left,
        };

        return { before, after };
      });

      if (result.before.scrollbarGutter !== "stable")
        throw new Error(
          "message host is missing scrollbar-gutter: stable: " +
            JSON.stringify(result),
        );
      if (result.before.overflowing)
        throw new Error(
          "fixture setup was already overflowing before the filler landed: " +
            JSON.stringify(result),
        );
      if (!result.after.overflowing)
        throw new Error(
          "filler did not push the host into overflow: " + JSON.stringify(result),
        );
      if (result.before.clientWidth !== result.after.clientWidth)
        throw new Error(
          "pack host clientWidth changed at the overflow boundary: " +
            JSON.stringify(result),
        );
      if (result.before.markerLeft !== result.after.markerLeft)
        throw new Error(
          "a card slid horizontally at the overflow boundary: " +
            JSON.stringify(result),
        );

      return { ...result, url: server.url };
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
