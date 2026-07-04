const path = require("path");
const { withServePage } = require("./serve_playwright_harness");
const {
  masonrySmokeFixtureHelpers,
  renderMasonryFixture,
  renderMasonryTeamFixture,
} = require("./serve_masonry_smoke_fixtures");
const {
  masonrySmokeAuditHelpers,
  masonrySmokeResizeAudit,
  masonrySmokeRootWidthStaleRecoveryAudit,
} = require("./serve_masonry_smoke_audits");

// Deterministic fixture clock: three hour buckets so time-rule work can build
// on the same fixture, one compaction divider mid-stream as a pack barrier.
// The injected helpers are serialized into the page, so every fixture constant
// travels through the evaluate config rather than lexical capture.
const masonryColumnFloorByWidth = { 560: 1, 1280: 3, 1920: 4, 2560: 6 };
const masonryForcedTallSpan = 120;
const masonryExpectedGridTrackCount = 12;
const masonryLegalColumnSpans = [2, 3, 4, 6, 12];
const masonryMobileBreakpointWidth = 720;
const masonryMeasuredWidths = Object.keys(masonryColumnFloorByWidth).map(Number);
const masonryScreenshotWidths = [900, 1280, 1920];
const masonryTeamColumnFloorByWidth = { 700: 1, 1280: 3, 1920: 4, 2560: 6 };
const masonryTeamMeasuredWidths = Object.keys(masonryTeamColumnFloorByWidth).map(
  Number,
);
const masonryFixturePlan = {
  ackCardStep: 20,
  backfillMinutes: [-4, -3, -2],
  baseIso: "2026-07-03T10:04:00.000Z",
  epilogueMinute: 320,
  finalCardStep: 17,
  imageEpilogueOffsetMinutes: 2,
  segmentCardCount: 12,
};
// The fixture crosses 11:00 right after the compaction divider (rule
// suppressed), 12:00 and 13:00 mid-stream (one rule each), then jumps
// multiple hours to the epilogue card (one collapsed rule at 15:00).
const masonryExpectedRuleIsos = [
  "2026-07-03T12:00:00.000Z",
  "2026-07-03T13:00:00.000Z",
  "2026-07-03T15:00:00.000Z",
];
const masonryTransparentPng = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9sAAAAASUVORK5CYII=",
  "base64",
);

async function run(screenshotDir) {
  return withServePage(
    {
      path: "/?smoke=serve-masonry-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 1400 } },
    },
    async ({ page, server }) => {
      await installMasonrySmokeArtifactImageFallback(page);
      await page.waitForFunction(
        () =>
          typeof renderMessage === "function" &&
          typeof applyTeamSnapshotPayload === "function" &&
          typeof laneGroupMemberTargetIds === "function" &&
          Array.isArray(targets) &&
          targets.length > 0,
        { timeout: 10000 },
      );
      await installMasonrySmokeHelpers(page);
      const measurements = {};
      for (const width of masonryMeasuredWidths) {
        measurements[width] = await measureMasonry(page, width);
        assertMasonryMeasurement(measurements[width]);
      }
      const resizeAudit = await measureMasonryResize(page);
      assertMasonryResizeAudit(resizeAudit);
      const rootWidthMeasurement = await measureMasonryRootWidth(page);
      assertMasonryMeasurement(rootWidthMeasurement);
      const rootWidthRecovery = await measureMasonryRootWidthRecovery(page);
      assertMasonryRootWidthRecovery(rootWidthRecovery);
      const screenshots = [];
      if (screenshotDir) {
        for (const width of masonryScreenshotWidths) {
          await renderMasonryScreenshot(page, width);
          const file = path.join(screenshotDir, "masonry-" + width + ".png");
          await page.screenshot({ path: file });
          screenshots.push(file);
        }
      }
      const teamMeasurements = {};
      for (const width of masonryTeamMeasuredWidths) {
        teamMeasurements[width] = await measureMasonryTeam(page, width);
        assertMasonryTeamMeasurement(teamMeasurements[width]);
      }
      return {
        measurements,
        resizeAudit,
        rootWidthMeasurement,
        rootWidthRecovery,
        screenshots,
        teamMeasurements,
        url: server.url,
      };
    },
  );
}

async function installMasonrySmokeArtifactImageFallback(page) {
  await page.route("**/api/work/trees/*/files/image**", (route) => {
    const url = new URL(route.request().url());
    const imagePath = url.searchParams.get("path") || "";
    if (!imagePath.startsWith("/tmp/spice-masonry-lag/"))
      return route.continue();
    return route.fulfill({
      body: masonryTransparentPng,
      contentType: "image/png",
      status: 200,
    });
  });
}

async function installMasonrySmokeHelpers(page) {
  await page.addScriptTag({
    content: [
      "const masonryExpectedGridTrackCount = " +
        masonryExpectedGridTrackCount +
        ";",
      ...masonrySmokeFixtureHelpers,
      ...masonrySmokeAuditHelpers,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

async function measureMasonry(page, width) {
  await page.setViewportSize({ width, height: 1400 });
  return page.evaluate(renderMasonryFixture, {
    expectedColumns: masonryColumnFloorByWidth[width] || 1,
    expectedRuleIsos: masonryExpectedRuleIsos,
    plan: masonryFixturePlan,
    width,
  });
}

async function renderMasonryScreenshot(page, width) {
  await page.setViewportSize({ width, height: 1400 });
  await page.evaluate(renderMasonryFixture, {
    captureOnly: true,
    expectedColumns: masonryColumnFloorByWidth[width] || 1,
    expectedRuleIsos: masonryExpectedRuleIsos,
    plan: masonryFixturePlan,
    width,
  });
}

async function measureMasonryResize(page) {
  await page.setViewportSize({ width: 1280, height: 1400 });
  await page.evaluate(renderMasonryFixture, {
    captureOnly: true,
    expectedColumns: masonryColumnFloorByWidth[1280],
    expectedRuleIsos: masonryExpectedRuleIsos,
    plan: masonryFixturePlan,
    width: 1280,
  });
  await page.setViewportSize({ width: 1920, height: 1400 });
  return page.evaluate(masonrySmokeResizeAudit, {
    expectedColumns: masonryColumnFloorByWidth[1920],
    width: 1920,
  });
}

async function measureMasonryRootWidth(page) {
  await page.setViewportSize({ width: 1920, height: 1400 });
  return page.evaluate(renderMasonryFixture, {
    expectedColumns: 2,
    expectedRuleIsos: masonryExpectedRuleIsos,
    keepLanesVisible: true,
    plan: masonryFixturePlan,
    width: 1920,
  });
}

async function measureMasonryRootWidthRecovery(page) {
  await page.setViewportSize({ width: 1920, height: 1400 });
  return page.evaluate(masonrySmokeRootWidthStaleRecoveryAudit, {
    expectedColumns: 2,
    expectedRuleIsos: masonryExpectedRuleIsos,
    keepLanesVisible: true,
    plan: masonryFixturePlan,
    width: 1920,
  });
}

async function measureMasonryTeam(page, width) {
  await page.setViewportSize({ width, height: 1400 });
  return page.evaluate(renderMasonryTeamFixture, {
    expectedColumns: masonryTeamColumnFloorByWidth[width] || 1,
    forcedTallSpan: masonryForcedTallSpan,
    plan: masonryFixturePlan,
    width,
  });
}

function assertMasonryMeasurement(measurement) {
  const label = " at " + measurement.viewportWidth + "px: ";
  const fail = (reason) => {
    throw new Error(reason + label + JSON.stringify(measurement));
  };
  assertMasonryBaseLayout(measurement, fail);
  assertMasonryRows(measurement, fail);
  assertMasonryDividerBarrier(measurement, fail);
  assertMasonryTimeRules(measurement, fail);
  assertMasonryDistribution(measurement, fail);
}

function assertMasonryResizeAudit(audit) {
  if (audit.distinctColumns < audit.expectedColumns)
    throw new Error("resize reflow used " + JSON.stringify(audit));
  if (audit.gridTrackCount !== masonryExpectedGridTrackCount)
    throw new Error("resize reflow lost 12 grid tracks " + JSON.stringify(audit));
}

function assertMasonryRootWidthRecovery(audit) {
  if (!audit.beforeRootWidthActive)
    throw new Error("root-width recovery did not exercise stale state");
  if (audit.visibleLaneCount <= 1)
    throw new Error("root-width recovery did not reveal sibling lanes");
  if (audit.afterRootWidthActive)
    throw new Error("same-fingerprint render kept stale root width");
  if (audit.distinctColumnLefts < audit.expectedColumns)
    throw new Error("root-width recovery collapsed columns " + JSON.stringify(audit));
  if (audit.gridTrackCount !== masonryExpectedGridTrackCount)
    throw new Error("root-width recovery lost 12 grid tracks " + JSON.stringify(audit));
}

function assertMasonryTeamMeasurement(measurement) {
  const label = " at " + measurement.viewportWidth + "px: ";
  const fail = (reason) => {
    throw new Error(reason + label + JSON.stringify(measurement));
  };
  if (measurement.hostDisplay !== "grid") fail("team message host is not grid");
  if (measurement.gridTrackCount !== masonryExpectedGridTrackCount)
    fail("team message host did not keep 12 grid tracks");
  if (
    measurement.viewportWidth <= masonryMobileBreakpointWidth &&
    measurement.expectedColumns !== 1
  )
    fail("mobile team expected more than one Mosaic column");
  if (measurement.distinctColumnLefts < measurement.expectedColumns)
    fail(
      "team cards used " +
        measurement.distinctColumnLefts +
        " Mosaic columns below floor " +
        measurement.expectedColumns,
    );
  if (measurement.baseMaxCardWidthDelta > 2)
    fail(
      "base team cards did not fill Mosaic columns; max width delta " +
        measurement.baseMaxCardWidthDelta,
    );
  if (
    measurement.minColumnSpan < measurement.expectedColumnSpan ||
    measurement.maxColumnSpan > masonryExpectedGridTrackCount ||
    !masonryLegalColumnSpans.includes(measurement.minColumnSpan) ||
    !masonryLegalColumnSpans.includes(measurement.maxColumnSpan)
  )
    fail(
      "team cards did not use bounded 12-track spans: " +
        measurement.minColumnSpan +
        ".." +
        measurement.maxColumnSpan,
    );
  if (
    measurement.expectedColumns > 2 &&
    measurement.maxColumnSpan <= measurement.expectedColumnSpan
  )
    fail("team Mosaic did not include a larger piece");
  if (measurement.expectedColumns > 1) {
    for (const [member, count] of Object.entries(
      measurement.memberColumnCounts,
    )) {
      if (count <= 1)
        fail(member + " cards stayed in one producer column");
    }
  }
  const slackBound = measurement.rowStridePx + 2;
  if (measurement.maxStretchSlackPx > slackBound)
    fail(
      "team cards retained excessive row-span slack " +
        measurement.maxStretchSlackPx,
    );
  if (measurement.forcedSpanRecovery.slackPx > slackBound)
    fail(
      "forced span recovery retained excessive row-span slack " +
        measurement.forcedSpanRecovery.slackPx,
    );
}

function assertMasonryBaseLayout(measurement, fail) {
  if (measurement.hostDisplay !== "grid") fail("message host is not grid");
  if (
    measurement.keepLanesVisible &&
    measurement.visibleLaneCount > 1 &&
    measurement.rootWidthActive
  )
    fail("visible multi-lane Mosaic used root-width overflow");
  if (measurement.gridTrackCount !== masonryExpectedGridTrackCount)
    fail("message host did not keep 12 grid tracks");
  if (!measurement.spans.every((span) => span > 0))
    fail("cards missing row spans");
  if (measurement.missingTimestampStamps > 0)
    fail("pack items missing data-message-ts");
  if (!measurement.repackAudit.stable) fail("pack is not idempotent");
  if (
    measurement.noopRenderAudit.packCount ||
    measurement.noopRenderAudit.observerSyncCount ||
    measurement.noopRenderAudit.historySyncCount ||
    measurement.noopRenderAudit.teamSyncCount
  )
    fail("same-fingerprint render did heavy sync work");
  if (!measurement.columnAudit.chronological)
    fail("column order is not chronological");
  if (!measurement.columnAudit.sameRowNonOverlapping)
    fail("cards sharing a row overlap slot ranges");
  const gapBound =
    measurement.rowAudit.rowStridePx * 4 + measurement.rowAudit.rowGapPx + 2;
  if (measurement.columnAudit.maxInnerGapPx > gapBound)
    fail(
      "column contains an interior hole of " +
        measurement.columnAudit.maxInnerGapPx +
        "px (bound " +
        gapBound +
        "px)",
    );
  if (measurement.structuredFinalAudit.finalCount < 1)
    fail("fixture did not include a final message");
  if (measurement.structuredFinalAudit.ratio < 2)
    fail(
      "structured final card was not at least 2x a normal card: " +
        JSON.stringify(measurement.structuredFinalAudit),
    );
}

function assertMasonryRows(measurement, fail) {
  const row = measurement.rowAudit;
  for (const card of row.cards) {
    const cellHeight = card.span * row.rowStridePx - row.rowGapPx;
    if (card.imageOnly) {
      if (card.alignSelf !== "start")
        fail("image-only card lost start alignment");
      if (cellHeight - card.height >= row.rowStridePx)
        fail("image-only card span was over-expanded");
      continue;
    }
    if (card.alignSelf !== "stretch") fail("card is not stretch-aligned");
    if (Math.abs(cellHeight - card.height) > 1)
      fail("card box does not fill its grid cell");
    if (cellHeight - card.scrollHeight > row.rowStridePx + 2)
      fail("card retained excessive row-span slack");
  }
}

function assertMasonryDividerBarrier(measurement, fail) {
  if (measurement.dividerFillRatio < 0.98)
    fail("compaction divider is not full width");
  const dividerTop = measurement.dividerRect.top;
  const dividerBottom = dividerTop + measurement.dividerRect.height;
  if (!measurement.cardsAboveDivider.every((bottom) => bottom <= dividerTop + 1))
    fail("pre-compaction card crossed the barrier");
  if (!measurement.cardsBelowDivider.every((top) => top >= dividerBottom - 1))
    fail("post-compaction card floated above the barrier");
}

function assertMasonryTimeRules(measurement, fail) {
  const ruleIsos = measurement.ruleAudits.map((rule) => rule.iso);
  if (JSON.stringify(ruleIsos) !== JSON.stringify(measurement.expectedRuleIsos))
    fail(
      "time rules mismatch (suppression or collapse broken): " +
        JSON.stringify(ruleIsos),
    );
  for (const rule of measurement.ruleAudits) {
    if (rule.fillRatio < 0.98) fail("time rule is not full width");
    if (rule.maxCardBottomAbove > rule.top + 1)
      fail("card crossed a time-rule barrier from above");
    if (rule.minCardTopBelow < rule.bottom - 1)
      fail("card floated above a time-rule barrier");
  }
}

function assertMasonryDistribution(measurement, fail) {
  const expectedColumns = measurement.expectedColumns;
  const expectedColumnSpan = masonryExpectedGridTrackCount / expectedColumns;
  if (
    !measurement.columnSpans.every(
      (span) =>
        span >= expectedColumnSpan &&
        span <= masonryExpectedGridTrackCount &&
        masonryLegalColumnSpans.includes(span),
    )
  )
    fail(
      "cards did not use legal 12-grid spans from floor " +
        expectedColumnSpan +
        ": " +
        JSON.stringify(measurement.columnSpans),
    );
  if (
    expectedColumns > 2 &&
    !measurement.columnSpans.some((span) => span > expectedColumnSpan)
  )
    fail("Mosaic did not include a larger piece");
  if (expectedColumns > 1) {
    for (const fan of measurement.segmentFanOut) {
      for (const entry of fan) {
        if (
          entry.slot < 1 ||
          entry.slot + entry.slotSpan - 1 > masonryExpectedGridTrackCount
        )
          fail("segment fan overflowed the track grid: " + JSON.stringify(fan));
      }
      if (fan.some((entry) => entry.slotSpan < 1))
        fail("segment fan included an invalid span: " + JSON.stringify(fan));
    }
    if (
      measurement.segmentOpeningSlots.length > 1 &&
      new Set(measurement.segmentOpeningSlots).size <= 1
    )
      fail(
        "barrier segments all reopened on the same slot: " +
          JSON.stringify(measurement.segmentOpeningSlots),
      );
  }
  if (!measurement.appendStabilityAudit.stable)
    fail(
      "newer append changed existing card columns: " +
        JSON.stringify(measurement.appendStabilityAudit),
    );
  if (!measurement.hydrationGrowthAudit.stable)
    fail(
      "history hydration changed existing card columns: " +
        JSON.stringify(measurement.hydrationGrowthAudit),
    );
  if (!measurement.middleRemovalReflowAudit.reflowed)
    fail(
      "middle removal did not trigger reflow: " +
        JSON.stringify(measurement.middleRemovalReflowAudit),
    );
  if (measurement.distinctColumnLefts < expectedColumns)
    fail(
      "cards used " +
        measurement.distinctColumnLefts +
        " columns below floor " +
        expectedColumns +
        "",
    );
  if (
    expectedColumns > 1 &&
    measurement.columnRecovery.distinctColumns < expectedColumns
  )
    fail("packer did not redistribute out of a collapsed single column");
}

if (require.main === module) {
  run(process.argv[2] || "")
    .then((result) => {
      console.log(JSON.stringify(result, null, 2));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run };
