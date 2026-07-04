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
// on the fixture for later steps.
async function measureInitialPlacement(page, cards, m, styleProps) {
  return page.evaluate(
    ({ cards, m, styleProps }) => {
      const f = window.__mosaicFixture;
      const instr = window.__mosaicInstrumentStyle;
      const count = window.__mosaicWriteCount;
      const planeCounts = instr(f.plane, ["transform", "transition"]);
      f.prevA = mosaicApplyCardPosition(f.cardA, cards[0], null, f.edges, m, f.gap, {});
      f.prevB = mosaicApplyCardPosition(f.cardB, cards[1], null, f.edges, m, f.gap, {});
      const planeTouchedByCardApply = count(planeCounts);

      f.extent = mosaicCardsExtent(cards);
      const cardACounts = instr(f.cardA, styleProps);
      const cardBCounts = instr(f.cardB, styleProps);
      f.prevMaxRow = mosaicSyncPlane(f.plane, f.extent.maxRow, 0, m, {});
      const cardsTouchedByPlaneSync = count(cardACounts) + count(cardBCounts);

      f.prevExtent = mosaicSyncHostHeight(f.host, f.extent.maxRow, f.extent.minB, m, null);
      return { planeTouchedByCardApply, cardsTouchedByPlaneSync };
    },
    { cards, m, styleProps },
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
}

async function run() {
  return withServePage(
    { path: "/?smoke=serve-mosaic-render-" + Date.now() },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof mosaicApplyCardPosition === "function" &&
          typeof mosaicSyncPlane === "function" &&
          typeof mosaicSyncHostHeight === "function" &&
          typeof mosaicCardsExtent === "function" &&
          typeof mosaicCardY === "function" &&
          typeof MOSAIC_PLANE_K === "number",
        { timeout: 10000 },
      );

      await installMosaicTestHelpers(page);
      await setupMosaicFixture(page, EDGES, GAP);
      const placement = await measureInitialPlacement(page, CARDS, M, STYLE_PROPS);
      const unchanged = await measureUnchangedWrites(page, CARDS, M, STYLE_PROPS);
      const extras = await measureMovedCardAndExtras(page, CARDS, M, STYLE_PROPS);
      const result = { ...placement, ...unchanged, ...extras };

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
