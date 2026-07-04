const { withServePage } = require("./serve_playwright_harness");

// Mosaic seam rule: adjacent cards sharing a column boundary
// must have identical boundary pixels in a real browser, at container
// widths where colW is fractional (1281/977 at root 16px, per
// tests/fixtures/mosaic_seam.js), and the boundary must hold mid-transition
// as well as at rest -- a stationary card's width/x are set once from the
// shared edges[] table and never touched again, so a sibling card's
// concurrent transform transition must not perturb it.

const WIDTHS = [1281, 977];
const GAP_ROOT_PX = 16;

async function setupSeamFixture(page) {
  await page.evaluate(() => {
    const host = document.createElement("div");
    const plane = document.createElement("div");
    plane.className = "mosaic-plane";
    host.style.position = "relative";
    host.appendChild(plane);
    const elA = document.createElement("div");
    const elB = document.createElement("div");
    const elC = document.createElement("div");
    for (const el of [elA, elB, elC]) {
      // [data-mosaic-card] is the stylesheet hook (messages.css) that
      // declares the differential transform transition; without it these
      // synthetic elements would jump instantly and the mid-transition
      // sample below would be measuring a no-op.
      el.setAttribute("data-mosaic-card", "");
      plane.appendChild(el);
    }
    document.body.appendChild(host);
    window.__seamFixture = { host, plane, elA, elB, elC };
  });
}

// Places three cards for one width's geometry: A and B sit side by side on
// adjacent tracks (the seam under test); C sits further right and is the
// only one that will move, so A/B's rest state is never touched by it.
async function placeSeamCards(page, containerWidthPx) {
  return page.evaluate(
    ({ containerWidthPx, rootFontSizePx }) => {
      const f = window.__seamFixture;
      const geometry = mosaicGeometry(rootFontSizePx, containerWidthPx);
      const { edges, gap, M } = geometry;
      const cardA = { t: 0, span: 4, b: 0, n: 3 };
      const cardB = { t: 4, span: 4, b: 0, n: 3 };
      const cardC = { t: 8, span: 4, b: 0, n: 3 };
      f.prevA = mosaicApplyCardPosition(f.elA, cardA, null, edges, M, gap, {});
      f.prevB = mosaicApplyCardPosition(f.elB, cardB, null, edges, M, gap, {});
      f.prevC = mosaicApplyCardPosition(f.elC, cardC, null, edges, M, gap, {});
      f.geometry = geometry;
      f.cardA = cardA;
      f.cardB = cardB;
      f.cardC = cardC;
      return {
        edges,
        gap,
        colWIsFractional: geometry.colW % 1 !== 0,
      };
    },
    { containerWidthPx, rootFontSizePx: GAP_ROOT_PX },
  );
}

async function measureRestBoundary(page) {
  return page.evaluate(() => {
    const f = window.__seamFixture;
    const a = f.elA.getBoundingClientRect();
    const b = f.elB.getBoundingClientRect();
    return { aRight: a.right, bLeft: b.left, gap: f.geometry.gap };
  });
}

// Moves C only (A/B untouched -- they receive zero style writes, per the
// render module's skip-if-unchanged invariant already covered by
// serve_mosaic_render_smoke.js) and immediately samples A/B's boundary
// partway through C's 340ms transition.
async function measureMidTransitionBoundary(page) {
  await page.evaluate(() => {
    const f = window.__seamFixture;
    const movedC = { ...f.cardC, b: f.cardC.b + 2 };
    f.prevC = mosaicApplyCardPosition(f.elC, movedC, f.prevC, f.geometry.edges, f.geometry.M, f.geometry.gap, {});
    f.cardC = movedC;
  });
  await page.waitForTimeout(150); // ~mid-way through the 340ms tween
  return page.evaluate(() => {
    const f = window.__seamFixture;
    const a = f.elA.getBoundingClientRect();
    const b = f.elB.getBoundingClientRect();
    return { aRight: a.right, bLeft: b.left, gap: f.geometry.gap };
  });
}

function assertBoundary(label, boundary) {
  const expectedLeft = boundary.aRight + boundary.gap;
  if (Math.abs(expectedLeft - boundary.bLeft) > 0.01)
    throw new Error(
      label +
        ": seam mismatch, right(A)+gap=" +
        expectedLeft +
        " left(B)=" +
        boundary.bLeft,
    );
}

async function run() {
  return withServePage(
    { path: "/?smoke=serve-mosaic-seam-" + Date.now() },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof mosaicGeometry === "function" &&
          typeof mosaicApplyCardPosition === "function",
        { timeout: 10000 },
      );

      const results = {};
      for (const width of WIDTHS) {
        await setupSeamFixture(page);
        const placement = await placeSeamCards(page, width);
        if (!placement.colWIsFractional)
          throw new Error("width " + width + " did not produce a fractional colW as expected");

        const rest = await measureRestBoundary(page);
        assertBoundary("width " + width + " at rest", rest);

        const midTransition = await measureMidTransitionBoundary(page);
        assertBoundary("width " + width + " mid-transition", midTransition);

        results[width] = { rest, midTransition, edges: placement.edges };

        await page.evaluate(() => {
          window.__seamFixture.host.remove();
        });
      }

      return { ...results, url: server.url };
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
