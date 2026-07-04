const { withServePage } = require("./serve_playwright_harness");

// Mosaic §9 plane scroll behavior: scrolled down-page, a frontier rise must
// produce zero visual viewport movement (plane jump + scroll compensation
// land in the same frame, transitions disabled); at top, the same rise
// animates the plane gently with no compensation; the compensation must
// not be misread as user scroll intent by the pane's own scroll-intent
// suppression machinery.

const THRESHOLD = 80;
const M = 40;
const GAP = 8;
const EDGES = Array.from({ length: 13 }, (_, t) => t * 100);
// A card whose own (t, b, n, span) never changes across these scenarios --
// exactly the case the compensation exists for: the frontier rises because
// of activity elsewhere, and this card must not visually move.
const CARD_RECORD = { t: 0, b: 0, n: 1, span: 12 };

async function installFixture(page, edges, gap, cardRecord) {
  await page.evaluate(
    ({ edges, gap, cardRecord }) => {
      const host = document.createElement("div");
      host.style.height = "200px";
      host.style.overflow = "auto";
      host.style.position = "relative";
      // A plain, non-absolutely-positioned spacer gives host a reliable
      // scrollable extent on its own terms, decoupled from whatever the
      // plane/card transforms resolve to -- keeping "is host scrollable"
      // and "does the K-anchored cancellation land the card correctly" as
      // two independently verifiable concerns instead of one fragile one.
      const spacer = document.createElement("div");
      spacer.style.height = "2000px";
      const plane = document.createElement("div");
      plane.className = "mosaic-plane";
      const card = document.createElement("div");
      card.dataset.mosaicCard = "a";
      plane.appendChild(card);
      host.appendChild(spacer);
      host.appendChild(plane);
      document.body.appendChild(host);
      // Sync the plane to maxRow=0 BEFORE placing any card. Order matters:
      // mosaicApplyCardPosition's first-paint rule forces a synchronous
      // reflow (void el.offsetWidth) while the card already carries its
      // full K-anchored transform; if the plane's own cancelling transform
      // is not already in place at that reflow, this headless Chromium
      // freezes a stale, enormous scrollable-overflow region on the host
      // (observed: ~163800px instead of ~200px) that a later plane
      // transform update does not fix. Confirmed by reproducing both
      // orderings directly. mosaicSyncPlane(plane, 0, -1, ...) forces a
      // real write despite maxRow already being 0 (prevMaxRow=-1 can never
      // equal it), matching what mosaic-stream-integration must also do:
      // land the plane at the correct transform before the first card
      // reflow, not after.
      mosaicSyncPlane(plane, 0, -1, 40, {});
      mosaicApplyCardPosition(card, cardRecord, null, edges, 40, gap, {});
      window.__mosaicScrollFixture = {
        lane: { messagesEl: host },
        host,
        plane,
        card,
      };
    },
    { edges, gap, cardRecord },
  );
}

async function measureScrolledCompensation(page, threshold, m) {
  return page.evaluate(
    ({ threshold, m }) => {
      const f = window.__mosaicScrollFixture;
      f.host.scrollTop = threshold + 200;
      const beforeScrollTop = f.host.scrollTop;
      const beforeCardTop = f.card.getBoundingClientRect().top;
      const options = mosaicPlaneScrollOptions(f.lane, threshold);
      const prevSuppressed = f.lane.paneScrollSuppressed;
      const nextMaxRow = mosaicSyncPlane(f.plane, 3, 0, m, options);
      return {
        scrolled: options.scrolled,
        transition: f.plane.style.transition,
        nextMaxRow,
        scrollTopChanged: f.host.scrollTop !== beforeScrollTop,
        cardTopUnchanged:
          f.card.getBoundingClientRect().top === beforeCardTop,
        prevSuppressed,
        suppressedAfter: Boolean(f.lane.paneScrollSuppressed),
      };
    },
    { threshold, m },
  );
}

async function measureAtTopNoCompensation(page, threshold, m) {
  return page.evaluate(
    ({ threshold, m }) => {
      const f = window.__mosaicScrollFixture;
      f.host.scrollTop = 0;
      const beforeScrollTop = f.host.scrollTop;
      const options = mosaicPlaneScrollOptions(f.lane, threshold);
      const nextMaxRow = mosaicSyncPlane(f.plane, 6, 3, m, options);
      return {
        scrolled: options.scrolled,
        transition: f.plane.style.transition,
        nextMaxRow,
        scrollTopUnchanged: f.host.scrollTop === beforeScrollTop,
      };
    },
    { threshold, m },
  );
}

// Composability: a caller merges {replaying: true} over
// mosaicPlaneScrollOptions' own fields -- even while scrolled, replay must
// still animate the plane rather than jump-and-compensate.
async function measureReplayOverridesScrolled(page, threshold, m) {
  return page.evaluate(
    ({ threshold, m }) => {
      const f = window.__mosaicScrollFixture;
      f.host.scrollTop = threshold + 200;
      const beforeScrollTop = f.host.scrollTop;
      const options = {
        ...mosaicPlaneScrollOptions(f.lane, threshold),
        replaying: true,
      };
      mosaicSyncPlane(f.plane, 9, 6, m, options);
      return {
        scrolled: options.scrolled,
        transition: f.plane.style.transition,
        scrollTopUnchanged: f.host.scrollTop === beforeScrollTop,
      };
    },
    { threshold, m },
  );
}

function assertScrollInvariants(scrolled, atTop, replay) {
  const fail = (message, payload) => {
    throw new Error(message + ": " + JSON.stringify(payload));
  };
  if (!scrolled.scrolled) fail("fixture did not read as scrolled", scrolled);
  if (scrolled.transition !== "none")
    fail("scrolled jump must disable transitions", scrolled);
  if (!scrolled.scrollTopChanged)
    fail("scrolled jump must compensate scrollTop", scrolled);
  if (!scrolled.cardTopUnchanged)
    fail("viewport moved despite same-frame compensation", scrolled);
  if (!scrolled.suppressedAfter)
    fail("compensation was not routed through scroll-intent suppression", scrolled);

  if (atTop.scrolled) fail("fixture unexpectedly read as scrolled", atTop);
  if (atTop.transition === "none")
    fail("at-top rise must animate, not jump", atTop);
  if (!atTop.scrollTopUnchanged)
    fail("at-top rise must not compensate scroll", atTop);

  if (!replay.scrolled) fail("replay fixture did not read as scrolled", replay);
  if (replay.transition === "none")
    fail("full replay must animate the plane even while scrolled", replay);
  if (!replay.scrollTopUnchanged)
    fail("full replay must not jump-and-compensate", replay);
}

async function run() {
  return withServePage(
    { path: "/?smoke=serve-mosaic-scroll-" + Date.now() },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof mosaicPlaneScrollOptions === "function" &&
          typeof mosaicPaneIsScrolled === "function" &&
          typeof mosaicSyncPlane === "function" &&
          typeof mosaicApplyCardPosition === "function" &&
          typeof setLaneScrollTopWithoutPaneIntent === "function",
        { timeout: 10000 },
      );

      await installFixture(page, EDGES, GAP, CARD_RECORD);
      const scrolled = await measureScrolledCompensation(page, THRESHOLD, M);
      const atTop = await measureAtTopNoCompensation(page, THRESHOLD, M);
      const replay = await measureReplayOverridesScrolled(page, THRESHOLD, M);

      assertScrollInvariants(scrolled, atTop, replay);

      return { scrolled, atTop, replay, url: server.url };
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
