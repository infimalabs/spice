const { withServePage } = require("./serve_playwright_harness");

// Mosaic §9/§11(a)(c) render plane: card positions are transforms derived
// from (t, b, n, span) anchored to the fixed constant K, so a card whose
// position did not change must receive zero style writes, and the uniform
// plane component must never share a transition (or an element) with the
// differential per-card component.
//
// Instrumentation counts actual style-property setter invocations (via a
// shadowing accessor installed per element/property) rather than diffing
// the serialized `style` attribute via MutationObserver: this headless
// Chromium dedupes attribute-mutation records when a style property is
// re-set to its existing serialized value, so a MutationObserver-based
// check cannot tell "no write attempted" apart from "wrote the same value
// again" here -- only counting the setter calls themselves discriminates
// the two. Blink implements el.style.transform (etc.) as a plain OWN data
// property intercepted at the binding layer, not a JS accessor, so the
// shadowing accessor is backed by the standard CSSOM get/setProperty
// methods rather than by wrapping a preexisting descriptor.
//
// Measurement is split across several page.evaluate calls (state threaded
// via a window-global fixture) instead of one large callback, so each step
// stays small and independently readable.

const EDGES = Array.from({ length: 13 }, (_, t) => t * 108);
const GAP = 8;
const M = 40;
const STYLE_PROPS = ["transform", "width", "height", "transition"];
const CARDS = [
  { t: 0, b: 0, n: 3, span: 4 },
  { t: 4, b: -2, n: 2, span: 4 },
];

async function installMosaicTestHelpers(page) {
  await page.addScriptTag({
    content: `
      window.__mosaicInstrumentStyle = function (el, props) {
        const counts = Object.fromEntries(props.map((p) => [p, 0]));
        for (const prop of props) {
          Object.defineProperty(el.style, prop, {
            configurable: true,
            get() { return el.style.getPropertyValue(prop); },
            set(value) { counts[prop] += 1; el.style.setProperty(prop, value); },
          });
        }
        return counts;
      };
      window.__mosaicWriteCount = function (counts) {
        return Object.values(counts).reduce((sum, n) => sum + n, 0);
      };
      // Ordered [prop, value] log instead of counts: proves *sequence*, e.g.
      // that a transform write lands strictly between a transition:'none'
      // write and the transition re-enable that follows it (the §9
      // first-paint rule), not merely that both happened somewhere.
      window.__mosaicInstrumentStyleLog = function (el, props) {
        const log = [];
        for (const prop of props) {
          Object.defineProperty(el.style, prop, {
            configurable: true,
            get() { return el.style.getPropertyValue(prop); },
            set(value) { log.push([prop, value]); el.style.setProperty(prop, value); },
          });
        }
        return log;
      };
    `,
  });
}

async function setupMosaicFixture(page, edges, gap) {
  await page.evaluate(
    ({ edges, gap }) => {
      const host = document.createElement("div");
      const plane = document.createElement("div");
      plane.className = "mosaic-plane";
      host.appendChild(plane);
      const cardA = document.createElement("div");
      cardA.dataset.mosaicCard = "a";
      const cardB = document.createElement("div");
      cardB.dataset.mosaicCard = "b";
      plane.appendChild(cardA);
      plane.appendChild(cardB);
      document.body.appendChild(host);
      window.__mosaicFixture = { host, plane, cardA, cardB, edges, gap };
    },
    { edges, gap },
  );
}

// Initial placement (first apply always writes, must not touch the plane)
// plus the first plane/host sync. Stashes prevA/prevB/prevMaxRow/prevExtent
// on the fixture for later steps. Also covers the two §9 first-paint sites:
// the plane's pre-anchor (mosaicAnchorPlane, run before any card renders)
// and the first card's suppress/write/reenable sequence, captured as
// ordered logs so the assertions can check sequence, not just occurrence.
async function measureInitialPlacement(page, cards, m, styleProps) {
  return page.evaluate(
    ({ cards, m, styleProps }) => {
      const f = window.__mosaicFixture;
      const instr = window.__mosaicInstrumentStyle;
      const log = window.__mosaicInstrumentStyleLog;
      const count = window.__mosaicWriteCount;

      const anchorLog = log(f.plane, ["transition", "transform"]);
      f.prevMaxRow = mosaicAnchorPlane(f.plane, m, {});
      const anchorTransform = f.plane.style.transform;
      const anchorTransitionAfter = f.plane.style.transition;
      // Independent oracle (spec §9: plane translated by -(K-maxRow)*M; at
      // maxRow=0 that's -K*M) rather than re-deriving via mosaicPlaneTransform,
      // so this cannot pass merely by the module being self-consistent.
      const expectedAnchorTransform = `translateY(${-MOSAIC_PLANE_K * m}px)`;

      const planeCounts = instr(f.plane, ["transform", "transition"]);
      const cardALog = log(f.cardA, ["transition", "transform", "width", "height", "opacity"]);
      f.prevA = mosaicApplyCardPosition(f.cardA, cards[0], null, f.edges, m, f.gap, {});
      f.prevB = mosaicApplyCardPosition(f.cardB, cards[1], null, f.edges, m, f.gap, {});
      const planeTouchedByCardApply = count(planeCounts);

      f.extent = mosaicCardsExtent(cards);
      const cardACounts = instr(f.cardA, styleProps);
      const cardBCounts = instr(f.cardB, styleProps);
      f.prevMaxRow = mosaicSyncPlane(f.plane, f.extent.maxRow, f.prevMaxRow, m, {});
      const cardsTouchedByPlaneSync = count(cardACounts) + count(cardBCounts);

      f.prevExtent = mosaicSyncHostHeight(f.host, f.extent.maxRow, f.extent.minB, m, null);
      return {
        planeTouchedByCardApply,
        cardsTouchedByPlaneSync,
        anchorLog,
        anchorTransform,
        anchorTransitionAfter,
        expectedAnchorTransform,
        cardALog,
      };
    },
    { cards, m, styleProps },
  );
}

// §9: from the first painted frame through entrance-animation end (300ms,
// §12 entrance fade), a fresh card's position must be pixel-stable -- the
// only thing allowed to animate on entrance is opacity, never position.
// Checked via the style values this module itself controls (transform
// unchanged, zero further writes) rather than a live getBoundingClientRect()
// diff: the served page has its own unrelated timers/reflows (message-stream
// ticks, etc.) that can shift real layout geometry within the window for
// reasons that have nothing to do with this module, which would make a
// DOM-geometry check flaky for a cause this module does not own.
async function measureEntranceStability(page) {
  return page.evaluate(
    () =>
      new Promise((resolve) => {
        const f = window.__mosaicFixture;
        const transformBefore = f.cardA.style.transform;
        const counts = window.__mosaicInstrumentStyle(f.cardA, [
          "transform",
          "width",
          "height",
          "opacity",
        ]);
        setTimeout(() => {
          resolve({
            transformStable: transformBefore === f.cardA.style.transform,
            writesDuringEntranceWindow: window.__mosaicWriteCount(counts),
          });
        }, 320);
      }),
  );
}

// §11c: re-applying the same card values, and re-syncing plane/host at the
// same extent, must invoke zero setters.
async function measureUnchangedWrites(page, cards, m, styleProps) {
  return page.evaluate(
    ({ cards, m, styleProps }) => {
      const f = window.__mosaicFixture;
      const instr = window.__mosaicInstrumentStyle;
      const count = window.__mosaicWriteCount;
      const cardACounts = instr(f.cardA, styleProps);
      const cardBCounts = instr(f.cardB, styleProps);
      const nextPrevA = mosaicApplyCardPosition(f.cardA, cards[0], f.prevA, f.edges, m, f.gap, {});
      const nextPrevB = mosaicApplyCardPosition(f.cardB, cards[1], f.prevB, f.edges, m, f.gap, {});
      const unchangedCardWrites = count(cardACounts) + count(cardBCounts);
      const recordsIdentical = nextPrevA === f.prevA && nextPrevB === f.prevB;

      const planeCounts = instr(f.plane, ["transform", "transition"]);
      const hostCounts = instr(f.host, ["height"]);
      const prevMaxRowAgain = mosaicSyncPlane(f.plane, f.extent.maxRow, f.prevMaxRow, m, {});
      const extentAgain = mosaicSyncHostHeight(f.host, f.extent.maxRow, f.extent.minB, m, f.prevExtent);
      return {
        unchangedCardWrites,
        recordsIdentical,
        unchangedPlaneWrites: count(planeCounts),
        unchangedHostWrites: count(hostCounts),
        prevMaxRowUnchanged: prevMaxRowAgain === f.prevMaxRow,
        extentUnchangedRef: extentAgain === f.prevExtent,
      };
    },
    { cards, m, styleProps },
  );
}

// Moving a card must write (discriminates the skip logic from "never
// writes"), and every card's plane-space top must be 0 mod M (§11a).
async function measureMovedCardAndExtras(page, cards, m, styleProps) {
  return page.evaluate(
    ({ cards, m, styleProps }) => {
      const f = window.__mosaicFixture;
      const instr = window.__mosaicInstrumentStyle;
      const count = window.__mosaicWriteCount;
      const movedCard = { ...cards[0], b: 1 };
      const cardACounts = instr(f.cardA, styleProps);
      mosaicApplyCardPosition(f.cardA, movedCard, f.prevA, f.edges, m, f.gap, {});
      const modResults = [cards[0], cards[1], movedCard].map(
        (card) => mosaicCardY(card, m) % m,
      );
      return {
        movedCardWrites: count(cardACounts),
        modResults,
        hostHeight: f.host.style.height,
        expectedHostHeight: `${(f.extent.maxRow - f.extent.minB) * m}px`,
      };
    },
    { cards, m, styleProps },
  );
}

function assertMosaicRenderInvariants(result) {
  const fail = (message) => {
    throw new Error(message + ": " + JSON.stringify(result));
  };
  if (result.planeTouchedByCardApply !== 0)
    fail("applying card positions wrote to the plane element");
  if (result.cardsTouchedByPlaneSync !== 0)
    fail("syncing the plane wrote to a card element");
  if (result.unchangedCardWrites !== 0)
    fail("re-applying an unchanged card position wrote styles");
  if (!result.recordsIdentical)
    fail("unchanged card apply did not return the same record");
  if (result.unchangedPlaneWrites !== 0)
    fail("re-syncing the plane at the same maxRow wrote styles");
  if (result.unchangedHostWrites !== 0)
    fail("re-syncing host height at the same extent wrote styles");
  if (result.movedCardWrites === 0)
    fail("moving a card did not write any style (skip logic over-suppresses)");
  if (result.modResults.some((m) => m !== 0))
    fail("a card's plane-space top was not 0 mod M");
  if (result.hostHeight !== result.expectedHostHeight)
    fail("host height did not match (maxRow-minB)*M");
  if (!result.prevMaxRowUnchanged || !result.extentUnchangedRef)
    fail("no-op sync did not return the prior reference");

  // §9 first-paint rule, plane side: pre-anchored to the K constant before
  // the first card renders, with the anchor write itself transition-free.
  if (result.anchorTransform !== result.expectedAnchorTransform)
    fail("plane was not pre-anchored to the K constant before the first card");
  if (result.anchorTransitionAfter !== "")
    fail("plane transition was not re-enabled after the pre-anchor forced flush");
  const anchorLog = result.anchorLog;
  const anchorOrdered =
    anchorLog.length === 3 &&
    anchorLog[0][0] === "transition" && anchorLog[0][1] === "none" &&
    anchorLog[1][0] === "transform" && anchorLog[1][1] === result.expectedAnchorTransform &&
    anchorLog[2][0] === "transition" && anchorLog[2][1] === "";
  if (!anchorOrdered)
    fail("plane pre-anchor did not suppress/write/re-enable transition in that order");

  // §9 first-paint rule, card side: the fresh transform write must land
  // strictly between the transition:'none' write and the re-enable, so no
  // transform-transition is ever in effect while the transform changes --
  // the deterministic equivalent of "no transform transition event fires".
  const cardALog = result.cardALog;
  if (cardALog[0][0] !== "transition" || cardALog[0][1] !== "none")
    fail("fresh card did not suppress transitions before its first transform write");
  const transformIndex = cardALog.findIndex(([prop]) => prop === "transform");
  const reenableIndex = cardALog.findIndex(
    ([prop, value], i) => prop === "transition" && i > 0 && value !== "none",
  );
  if (transformIndex <= 0 || reenableIndex === -1 || transformIndex >= reenableIndex)
    fail("fresh card transform write did not land between transition suppress and re-enable");
  if (cardALog.some(([prop]) => prop === "opacity"))
    fail("mosaicApplyCardPosition wrote opacity -- entrance fade is a CSS concern, not this module's");

  // §9 first-paint rule: pixel-stable from first paint through entrance-fade end.
  if (!result.transformStable)
    fail("fresh card's transform changed between first paint and entrance-animation end");
  if (result.writesDuringEntranceWindow !== 0)
    fail("fresh card received additional style writes during the entrance-fade window");
}

async function run() {
  return withServePage(
    { path: "/?smoke=serve-mosaic-render-" + Date.now() },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof mosaicApplyCardPosition === "function" &&
          typeof mosaicSyncPlane === "function" &&
          typeof mosaicAnchorPlane === "function" &&
          typeof mosaicSyncHostHeight === "function" &&
          typeof mosaicCardsExtent === "function" &&
          typeof mosaicCardY === "function" &&
          typeof mosaicPlaneTransform === "function" &&
          typeof MOSAIC_PLANE_K === "number",
        { timeout: 10000 },
      );

      await installMosaicTestHelpers(page);
      await setupMosaicFixture(page, EDGES, GAP);
      const placement = await measureInitialPlacement(page, CARDS, M, STYLE_PROPS);
      const entrance = await measureEntranceStability(page);
      const unchanged = await measureUnchangedWrites(page, CARDS, M, STYLE_PROPS);
      const extras = await measureMovedCardAndExtras(page, CARDS, M, STYLE_PROPS);
      const result = { ...placement, ...entrance, ...unchanged, ...extras };

      assertMosaicRenderInvariants(result);
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
