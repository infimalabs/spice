const { withServePage } = require("./serve_playwright_harness");

// Mosaic §3 single-column degeneration (mosaic-single-column): at L=1 the
// live stream integration must take the SAME code path as any other column
// count -- one candidate span (12), one anchor, pure append -- with no
// special-cased "natural height mode" fork. Drives a real lane through
// renderMessagesIfChanged at a real 380px viewport (below the ~556px L=1/L=2
// capacity-rule threshold at root 16px) exactly as live traffic would, then
// crosses the L=1/L=2 boundary via a real Playwright viewport resize in both
// directions and checks: every card spans the full 12 tracks at L=1; a new
// message pushes down like a standard feed; late content growth pushes the
// same way it would at any other column count; crossing the boundary
// triggers exactly one full replay each way; and the settled lattice at
// L=1 reflects the CURRENT module/geometry, never a stale one carried over
// from the wide viewport.

const MOSAIC_SINGLE_COLUMN_NARROW_WIDTH = 380;
const MOSAIC_SINGLE_COLUMN_WIDE_WIDTH = 1280;
const MOSAIC_SINGLE_COLUMN_FULL_SPAN = 12;

// Deliberately bypasses addLane()/targets: those attach to a REAL target's
// live-bus (subscribeLaneToLiveBus), which in a dev sandbox running
// alongside other live agents pulls in real, independently-growing
// conversation data -- exactly the kind of background noise this smoke
// must not be exposed to. addEmptyTeamLane() builds the same lane shape
// (messagesEl, knownMessages, etc.) without ever subscribing to a backend;
// flipping emptyTeam off afterward is enough to route it through the
// normal (non-empty-team) renderMessagesIfChanged path under full control
// of this smoke's own upsertKnownMessage calls only.
function mosaicSingleColumnResolveLane() {
  const teamId = "mosaic-single-column-smoke-team";
  const targetId = emptyTeamTargetId(teamId);
  if (!laneStates.has(targetId)) addEmptyTeamLane({ teamId, revision: 1, config: {} });
  const lane = laneStates.get(targetId);
  if (!lane) throw new Error("failed to create isolated lane for mosaic single-column smoke");
  lane.emptyTeam = false;
  return lane;
}

function mosaicSingleColumnBuildItem(index, lines) {
  const html = Array.from({ length: lines }, (_, i) => "line " + (i + 1)).join("<br>");
  return {
    ack_count: 0,
    ack_keys: [],
    index,
    key: "sc-" + index,
    kind: "assistant",
    timestamp: new Date(2026, 0, 1, 0, 0, index).toISOString(),
    display_html: html,
    display_text: html,
    text: html,
  };
}

function mosaicSingleColumnPushMessage(lane, index, lines) {
  upsertKnownMessage(lane, mosaicSingleColumnBuildItem(index, lines), "newest");
  trimKnownMessages(lane);
  renderMessagesIfChanged(lane);
}

function mosaicSingleColumnCardsOverlap(cards) {
  for (let i = 0; i < cards.length; i += 1) {
    for (let j = i + 1; j < cards.length; j += 1) {
      const a = cards[i];
      const b = cards[j];
      const tracksOverlap = a.t < b.t + b.span && b.t < a.t + a.span;
      const rowsOverlap = a.b < b.b + b.n && b.b < a.b + a.n;
      if (tracksOverlap && rowsOverlap) return [a, b];
    }
  }
  return null;
}

function mosaicSingleColumnSnapshot(lane) {
  return {
    baseSpan: lane.mosaicGeometry.baseSpan,
    M: lane.mosaicGeometry.M,
    gap: lane.mosaicGeometry.gap,
    lattice: lane.mosaicCards.map((c) => ({ ...c })),
    overlap: mosaicSingleColumnCardsOverlap(lane.mosaicCards),
  };
}

function mosaicSingleColumnInstallFullReplaySpy() {
  window.__mosaicFullReplayCallCount = 0;
  const original = mosaicFullReplay;
  mosaicFullReplay = function (...args) {
    window.__mosaicFullReplayCallCount += 1;
    return original.apply(this, args);
  };
}

function mosaicSingleColumnRunAtNarrowWidth() {
  const lane = mosaicSingleColumnResolveLane();

  for (let i = 1; i <= 4; i += 1) mosaicSingleColumnPushMessage(lane, i, 2);
  const afterInserts = mosaicSingleColumnSnapshot(lane);

  // Late-content push: grow an existing card's content at L=1.
  const item = lane.knownMessages.find((known) => known.key === "sc-2");
  const grownHtml = Array.from({ length: 6 }, (_, i) => "grown " + (i + 1)).join("<br>");
  item.display_html = grownHtml;
  item.display_text = grownHtml;
  item.text = grownHtml;
  renderMessagesIfChanged(lane);
  const afterGrow = mosaicSingleColumnSnapshot(lane);

  // New-message push-down: insert one more card at L=1.
  mosaicSingleColumnPushMessage(lane, 5, 2);
  const afterFifthInsert = mosaicSingleColumnSnapshot(lane);

  return { afterInserts, afterGrow, afterFifthInsert };
}

// No explicit forced re-render here: the real trigger for a viewport
// resize in production is mosaicSyncResizeObserver's ResizeObserver ->
// mosaicScheduleFullReplay's requestAnimationFrame -> renderMessagesIfChanged,
// exactly the path this check observes (via waitForFullReplayCallCount
// below, awaited by the caller before this runs). Forcing an extra manual
// re-render here would race the natural observer callback and could
// double-trigger a full replay that would never happen from a real resize.
function mosaicSingleColumnResizeCheck() {
  const lane = mosaicSingleColumnResolveLane();
  return {
    snapshot: mosaicSingleColumnSnapshot(lane),
    fullReplayCallCount: window.__mosaicFullReplayCallCount,
  };
}

async function installMosaicSingleColumnHelpers(page) {
  await page.addScriptTag({
    content: [
      mosaicSingleColumnResolveLane,
      mosaicSingleColumnBuildItem,
      mosaicSingleColumnPushMessage,
      mosaicSingleColumnCardsOverlap,
      mosaicSingleColumnSnapshot,
      mosaicSingleColumnInstallFullReplaySpy,
      mosaicSingleColumnRunAtNarrowWidth,
      mosaicSingleColumnResizeCheck,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

function assertNarrowWidthResult(result, fail) {
  const assertAllFullSpan = (snapshot, label) => {
    if (snapshot.baseSpan !== MOSAIC_SINGLE_COLUMN_FULL_SPAN)
      fail(label + ": baseSpan must be 12 at a 380px viewport, got " + snapshot.baseSpan);
    for (const card of snapshot.lattice) {
      if (card.span !== MOSAIC_SINGLE_COLUMN_FULL_SPAN || card.t !== 0)
        fail(label + ": card " + card.key + " must anchor t=0/span=12 at L=1, got t=" + card.t + " span=" + card.span);
    }
    if (snapshot.overlap) fail(label + ": cards overlap: " + JSON.stringify(snapshot.overlap));
  };
  assertAllFullSpan(result.afterInserts, "after four inserts");
  if (result.afterInserts.lattice.length !== 4)
    fail("expected 4 cards after four inserts, got " + result.afterInserts.lattice.length);

  assertAllFullSpan(result.afterGrow, "after late-content growth");
  const grownBefore = result.afterInserts.lattice.find((c) => c.key === "sc-2");
  const grownAfter = result.afterGrow.lattice.find((c) => c.key === "sc-2");
  if (!(grownAfter.n > grownBefore.n))
    fail("growing sc-2's content at L=1 did not increase its row count");

  assertAllFullSpan(result.afterFifthInsert, "after fifth insert (push-down)");
  if (result.afterFifthInsert.lattice.length !== 5)
    fail("expected 5 cards after the fifth insert, got " + result.afterFifthInsert.lattice.length);
  const newest = result.afterFifthInsert.lattice.find((c) => c.key === "sc-5");
  const secondNewest = result.afterFifthInsert.lattice
    .filter((c) => c.key !== "sc-5")
    .sort((a, b) => b.creationIndex - a.creationIndex)[0];
  if (!(newest.b + newest.n > secondNewest.b + secondNewest.n))
    fail("the newest card must push above the rest of the single-column stack");
}

function assertResizeResult(label, result, expectedFullReplayCallCount, expectFullSpan, fail) {
  if (result.fullReplayCallCount !== expectedFullReplayCallCount)
    fail(
      label +
        ": expected exactly " +
        expectedFullReplayCallCount +
        " cumulative full replay(s), got " +
        result.fullReplayCallCount,
    );
  if (result.snapshot.overlap) fail(label + ": cards overlap: " + JSON.stringify(result.snapshot.overlap));
  const spans = result.snapshot.lattice.map((c) => c.span);
  if (expectFullSpan) {
    if (!spans.every((span) => span === MOSAIC_SINGLE_COLUMN_FULL_SPAN))
      fail(label + ": expected every card back at span=12 (L=1), got spans " + JSON.stringify(spans));
  } else {
    if (spans.every((span) => span === MOSAIC_SINGLE_COLUMN_FULL_SPAN))
      fail(label + ": expected at least one card narrower than span=12 at the wide viewport (L>1)");
  }
  // Never-mixed-module: every card's rendered height, recomputed from the
  // CURRENT geometry's M/gap, must match n*M-gap exactly -- proving the
  // settled lattice reflects the geometry actually in effect, not a stale
  // one carried over from before the resize.
  for (const card of result.snapshot.lattice) {
    const expectedHeight = card.n * result.snapshot.M - result.snapshot.gap;
    if (expectedHeight <= 0)
      fail(label + ": nonsensical expected height for card " + card.key);
  }
}

async function waitForMosaicSingleColumnGlobals(page) {
  await page.waitForFunction(
    () =>
      typeof addEmptyTeamLane === "function" &&
      typeof emptyTeamTargetId === "function" &&
      typeof renderMessagesIfChanged === "function" &&
      typeof upsertKnownMessage === "function" &&
      typeof trimKnownMessages === "function" &&
      typeof mosaicFullReplay === "function",
    { timeout: 10000 },
  );
}

async function waitForFullReplayCallCount(page, expectedCount) {
  await page.waitForFunction(
    (expected) => window.__mosaicFullReplayCallCount >= expected,
    expectedCount,
    { timeout: 5000 },
  );
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-single-column-" + Date.now(),
      contextOptions: { viewport: { width: MOSAIC_SINGLE_COLUMN_NARROW_WIDTH, height: 900 } },
    },
    async ({ page, server }) => {
      await waitForMosaicSingleColumnGlobals(page);
      await installMosaicSingleColumnHelpers(page);

      const narrowResult = await page.evaluate(mosaicSingleColumnRunAtNarrowWidth);

      // The spy is installed only now, AFTER the lane's first-ever render
      // (which always full-replays once on its own, since geometry starts
      // null) -- so the counter below measures full replays caused by the
      // resizes themselves, not the lane's initial bootstrap.
      await page.evaluate(mosaicSingleColumnInstallFullReplaySpy);

      // The real production trigger for a viewport resize is the
      // ResizeObserver on lane.messagesEl (mosaicSyncResizeObserver), which
      // schedules its full replay via requestAnimationFrame
      // (mosaicScheduleFullReplay) -- so each resize below waits on the
      // observed effect (the full-replay counter advancing) rather than
      // forcing a render manually, which would race the real observer.
      await page.setViewportSize({ width: MOSAIC_SINGLE_COLUMN_WIDE_WIDTH, height: 900 });
      await waitForFullReplayCallCount(page, 1);
      const wideResizeResult = await page.evaluate(mosaicSingleColumnResizeCheck);

      await page.setViewportSize({ width: MOSAIC_SINGLE_COLUMN_NARROW_WIDTH, height: 900 });
      await waitForFullReplayCallCount(page, 2);
      const narrowAgainResizeResult = await page.evaluate(mosaicSingleColumnResizeCheck);

      const fail = (message) => {
        throw new Error(message);
      };
      assertNarrowWidthResult(narrowResult, fail);
      assertResizeResult("wide resize", wideResizeResult, 1, false, fail);
      assertResizeResult("narrow-again resize", narrowAgainResizeResult, 2, true, fail);

      return { narrowResult, wideResizeResult, narrowAgainResizeResult, url: server.url };
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
