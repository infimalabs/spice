const { withServePage } = require("./serve_playwright_harness");

// Mosaic prefers-reduced-motion: transform transitions and entrance
// animation are disabled for insertion, settling/ripple, and full replay;
// the plane never animates. Scrolled-reader compensation is a correctness
// behavior, not a motion effect, and must still land under reduced motion.

const M = 40;
const GAP = 8;
const EDGES = Array.from({ length: 13 }, (_, t) => t * 100);
const CARD_A = { t: 0, b: 0, n: 1, span: 12 };
const CARD_A_MOVED = { t: 0, b: 1, n: 1, span: 12 };

async function installFixture(page, edges, gap, cardRecord) {
  await page.evaluate(
    ({ edges, gap, cardRecord }) => {
      const host = document.createElement("div");
      host.style.height = "200px";
      host.style.overflow = "auto";
      host.style.position = "relative";
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
      // Same ordering contract as the plane-scroll fixture: sync the plane
      // before the card's first-paint reflow (see the call-order comment
      // on mosaicApplyCardPosition).
      mosaicSyncPlane(plane, 0, -1, 40, {});
      window.__mosaicMotionFixture = {
        lane: { messagesEl: host },
        host,
        plane,
        card,
        cardRecord,
      };
    },
    { edges, gap, cardRecord },
  );
}

async function measureFreshCardInsertion(page, edges, gap, cardRecord) {
  return page.evaluate(
    ({ edges, gap, cardRecord }) => {
      const f = window.__mosaicMotionFixture;
      mosaicApplyCardPosition(f.card, cardRecord, null, edges, 40, gap, {
        reducedMotion: mosaicPrefersReducedMotion(),
      });
      return { transition: f.card.style.transition };
    },
    { edges, gap, cardRecord },
  );
}

async function measureSettlingUnderReducedMotion(
  page,
  edges,
  gap,
  cardRecord,
  movedRecord,
) {
  return page.evaluate(
    ({ edges, gap, cardRecord, movedRecord }) => {
      const f = window.__mosaicMotionFixture;
      mosaicApplyCardPosition(f.card, cardRecord, null, edges, 40, gap, {});
      f.card.style.transition = "";
      mosaicApplyCardPosition(f.card, movedRecord, cardRecord, edges, 40, gap, {
        reducedMotion: mosaicPrefersReducedMotion(),
      });
      return { transition: f.card.style.transition };
    },
    { edges, gap, cardRecord, movedRecord },
  );
}

async function measurePlaneAtTop(page, m) {
  return page.evaluate(
    ({ m }) => {
      const f = window.__mosaicMotionFixture;
      f.host.scrollTop = 0;
      mosaicSyncPlane(f.plane, 3, 0, m, {
        reducedMotion: mosaicPrefersReducedMotion(),
      });
      return { transition: f.plane.style.transition };
    },
    { m },
  );
}

async function measurePlaneScrolledCompensates(page, m) {
  return page.evaluate(
    ({ m }) => {
      const f = window.__mosaicMotionFixture;
      f.host.scrollTop = 300;
      const beforeScrollTop = f.host.scrollTop;
      const options = {
        ...mosaicPlaneScrollOptions(f.lane, 80),
        reducedMotion: mosaicPrefersReducedMotion(),
      };
      mosaicSyncPlane(f.plane, 6, 3, m, options);
      return {
        scrolled: options.scrolled,
        transition: f.plane.style.transition,
        scrollTopChanged: f.host.scrollTop !== beforeScrollTop,
      };
    },
    { m },
  );
}

async function measurePlaneReplayIsInstant(page, m) {
  return page.evaluate(
    ({ m }) => {
      const f = window.__mosaicMotionFixture;
      f.host.scrollTop = 300;
      mosaicSyncPlane(f.plane, 9, 6, m, {
        replaying: true,
        reducedMotion: mosaicPrefersReducedMotion(),
      });
      return { transition: f.plane.style.transition };
    },
    { m },
  );
}

function assertReducedMotionInvariants(result) {
  const fail = (message) => {
    throw new Error(message + ": " + JSON.stringify(result));
  };
  if (!result.prefersReducedMotion)
    fail("browser context did not report prefers-reduced-motion: reduce");
  if (result.freshInsertion.transition !== "none")
    fail("fresh card insertion must not transition, reduced motion or not");
  if (result.settling.transition !== "none")
    fail("settling/ripple must not transition under reduced motion");
  if (result.atTop.transition !== "none")
    fail("plane sync at top must not animate under reduced motion");
  if (!result.scrolledCompensates.scrolled)
    fail("fixture did not read as scrolled");
  if (result.scrolledCompensates.transition !== "none")
    fail("scrolled plane jump must not animate under reduced motion");
  if (!result.scrolledCompensates.scrollTopChanged)
    fail(
      "scroll compensation must still land under reduced motion -- it is a correctness behavior, not a motion effect",
    );
  if (result.replay.transition !== "none")
    fail("full replay must degrade to instant repositioning under reduced motion");
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-reduced-motion-" + Date.now(),
      contextOptions: { reducedMotion: "reduce" },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof mosaicPrefersReducedMotion === "function" &&
          typeof mosaicApplyCardPosition === "function" &&
          typeof mosaicSyncPlane === "function" &&
          typeof mosaicPlaneScrollOptions === "function",
        { timeout: 10000 },
      );

      const prefersReducedMotion = await page.evaluate(() =>
        mosaicPrefersReducedMotion(),
      );

      await installFixture(page, EDGES, GAP, CARD_A);
      const freshInsertion = await measureFreshCardInsertion(
        page,
        EDGES,
        GAP,
        CARD_A,
      );
      const settling = await measureSettlingUnderReducedMotion(
        page,
        EDGES,
        GAP,
        CARD_A,
        CARD_A_MOVED,
      );
      const atTop = await measurePlaneAtTop(page, M);
      const scrolledCompensates = await measurePlaneScrolledCompensates(page, M);
      const replay = await measurePlaneReplayIsInstant(page, M);

      const result = {
        prefersReducedMotion,
        freshInsertion,
        settling,
        atTop,
        scrolledCompensates,
        replay,
      };
      assertReducedMotionInvariants(result);
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
