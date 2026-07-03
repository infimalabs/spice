const path = require("path");
const { withServePage } = require("./serve_playwright_harness");

// Deterministic fixture clock: three hour buckets so time-rule work can build
// on the same fixture, one compaction divider mid-stream as a pack barrier.
// The injected helpers are serialized into the page, so every fixture constant
// travels through the evaluate config rather than lexical capture.
const masonryColumnFloorByWidth = { 560: 1, 1280: 2, 1920: 3 };
const masonryMeasuredWidths = Object.keys(masonryColumnFloorByWidth).map(Number);
const masonryScreenshotWidths = [900, 1280, 1920];
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

async function run(screenshotDir) {
  return withServePage(
    {
      path: "/?smoke=serve-masonry-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 1400 } },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof renderMessage === "function" &&
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
      const screenshots = [];
      if (screenshotDir) {
        for (const width of masonryScreenshotWidths) {
          await measureMasonry(page, width);
          const file = path.join(screenshotDir, "masonry-" + width + ".png");
          await page.screenshot({ path: file });
          screenshots.push(file);
        }
      }
      return { measurements, screenshots, url: server.url };
    },
  );
}

async function installMasonrySmokeHelpers(page) {
  await page.addScriptTag({
    content: [
      renderMasonryFixture,
      masonrySmokeLane,
      masonrySmokeUseSingleLane,
      masonrySmokeItems,
      masonrySmokeBodyHtml,
      masonrySmokeMeasurement,
      masonrySmokeBarrierBounds,
      masonrySmokeColumnAudit,
      masonrySmokeSegmentFanOut,
      masonrySmokeBackfillAudit,
      masonrySmokeColumnRecovery,
      masonrySmokeRepackAudit,
      masonrySmokeImageHtml,
      masonrySmokeImagesReady,
      masonrySmokeRect,
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

async function renderMasonryFixture(config) {
  const lane = masonrySmokeLane();
  masonrySmokeUseSingleLane(lane);
  const host = lane.messagesEl || document.querySelector(".messages");
  if (!host) throw new Error("message host unavailable");
  host
    .querySelectorAll("article[data-message-key], .compaction-divider, .time-rule")
    .forEach((node) => node.remove());
  const nodes = [];
  let previousItem = null;
  for (const item of masonrySmokeItems(config)) {
    const node = renderMessage(lane, item);
    if (!node) continue;
    const rule = timeRuleBetween(previousItem, item);
    if (rule) {
      rule.dataset.masonrySmoke = "rule";
      rule.dataset.masonrySmokeIndex = String(item.index - 0.5);
      nodes.push(rule);
    }
    previousItem = item;
    node.dataset.masonrySmoke = item.kind === "compaction" ? "divider" : "card";
    node.dataset.masonrySmokeIndex = String(item.index);
    nodes.push(node);
  }
  host.append(...nodes);
  await masonrySmokeImagesReady(host);
  packMessageStream(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  await new Promise((resolve) => requestAnimationFrame(resolve));
  return masonrySmokeMeasurement(lane, host, config);
}

function masonrySmokeLane() {
  let lane = Array.from(laneStates.values()).find((item) => !item.emptyTeam);
  if (!lane && targets.length) {
    addLane(targets[0].id);
    lane = laneStates.get(targets[0].id);
  }
  if (!lane) throw new Error("no lane available for masonry smoke");
  return lane;
}

function masonrySmokeUseSingleLane(activeLane) {
  for (const lane of laneStates.values()) {
    lane.element.style.display = lane === activeLane ? "" : "none";
  }
}

// 24 cards over three hour buckets (10:xx, 11:xx, 12:xx) with a compaction
// divider between buckets one and two. Heights cycle through short, medium,
// tall, and code-block bodies so the packer sees realistic spread.
function masonrySmokeItems(config) {
  const plan = config.plan;
  const base = Date.parse(plan.baseIso);
  const shapes = ["short", "medium", "tall", "short", "code", "medium"];
  const items = [];
  let index = 0;
  const push = (kind, minutesFromBase, extras) => {
    const timestamp = new Date(base + minutesFromBase * 60000).toISOString();
    index += 1;
    items.push(
      Object.assign(
        {
          ack_count: 0,
          ack_keys: [],
          index,
          key: "masonry-" + index + "-" + config.width,
          kind,
          timestamp,
        },
        extras,
      ),
    );
  };
  for (let step = 0; step < plan.segmentCardCount; step += 1) {
    const shape = shapes[step % shapes.length];
    push("assistant", step * 4, {
      display_html: masonrySmokeBodyHtml(shape, step),
      display_text: "masonry card " + step,
      text: "masonry card " + step,
    });
  }
  push("compaction", 52, { text: "Context compacted" });
  for (
    let step = plan.segmentCardCount;
    step < plan.segmentCardCount * 2;
    step += 1
  ) {
    const shape = shapes[step % shapes.length];
    push(step === plan.finalCardStep ? "final" : "assistant", 56 + (step - plan.segmentCardCount) * 12, {
      ack_count: step === plan.ackCardStep ? 1 : 0,
      display_html: masonrySmokeBodyHtml(shape, step),
      display_text: "masonry card " + step,
      text: "masonry card " + step,
    });
  }
  push("assistant", plan.epilogueMinute, {
    display_html: masonrySmokeBodyHtml("short", plan.segmentCardCount * 2),
    display_text: "masonry epilogue card",
    text: "masonry epilogue card",
  });
  push("assistant", plan.epilogueMinute + plan.imageEpilogueOffsetMinutes, {
    display_html: masonrySmokeImageHtml(),
    display_text: "masonry image card",
    image_only: true,
    text: "masonry image card",
  });
  return items;
}

function masonrySmokeImageHtml() {
  const svg = encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" width="156" height="96">' +
      '<rect width="156" height="96" fill="#4f8abf"/>' +
      "</svg>",
  );
  return (
    '<p class="message-image-stack"><a class="message-image" href="#">' +
    '<img alt="masonry image control" src="data:image/svg+xml,' +
    svg +
    '"></a></p>'
  );
}

function masonrySmokeImagesReady(host) {
  const images = Array.from(host.querySelectorAll("article img"));
  return Promise.all(
    images.map((image) => {
      if (image.complete) return Promise.resolve();
      return new Promise((resolve, reject) => {
        image.addEventListener("load", resolve, { once: true });
        image.addEventListener("error", reject, { once: true });
      });
    }),
  );
}

function masonrySmokeBodyHtml(shape, step) {
  const sentence =
    "<p>Card " +
    step +
    " keeps the packer honest with realistic prose length.</p>";
  if (shape === "short") return sentence;
  if (shape === "medium") return sentence + sentence;
  if (shape === "code")
    return (
      sentence +
      "<pre><code>packMessageStream(lane)\ncolumnRows[" +
      step +
      "] += span</code></pre>"
    );
  return sentence + sentence + sentence + sentence + sentence;
}

async function masonrySmokeMeasurement(lane, host, config) {
  const cards = Array.from(
    host.querySelectorAll('[data-masonry-smoke="card"]'),
  );
  const divider = host.querySelector('[data-masonry-smoke="divider"]');
  if (!divider) throw new Error("masonry fixture divider missing");
  const repackAudit = await masonrySmokeRepackAudit(lane, host);
  const dividerRect = masonrySmokeRect(divider);
  const dividerIndex = Number(divider.dataset.masonrySmokeIndex);
  const cardRects = cards.map((card) => ({
    alignSelf: getComputedStyle(card).alignSelf,
    column: card.style.gridColumnStart,
    imageOnly: card.classList.contains("image-only"),
    index: Number(card.dataset.masonrySmokeIndex),
    left: masonrySmokeRect(card).left,
    rect: masonrySmokeRect(card),
    row: Number.parseInt(card.style.gridRowStart || "0", 10),
    span: Number.parseInt(
      card.style.getPropertyValue("--message-pack-row-span") || "0",
      10,
    ),
  }));
  const hostStyle = getComputedStyle(host);
  const hostRect = masonrySmokeRect(host);
  const hostInnerWidth =
    hostRect.width -
    Number.parseFloat(hostStyle.paddingLeft) -
    Number.parseFloat(hostStyle.paddingRight);
  const barrierIndexes = masonrySmokeBarrierBounds(host);
  const columnAudit = masonrySmokeColumnAudit(cardRects, barrierIndexes);
  const segmentFanOut = masonrySmokeSegmentFanOut(
    cardRects,
    barrierIndexes,
    config.expectedColumns,
  );
  // Rule rects are read live, so audit them before the backfill and recovery
  // probes below mutate the layout.
  const ruleAudits = Array.from(
    host.querySelectorAll('[data-masonry-smoke="rule"]'),
  ).map((rule) => {
    const rect = masonrySmokeRect(rule);
    const ruleIndex = Number(rule.dataset.masonrySmokeIndex);
    return {
      fillRatio: rect.width / hostInnerWidth,
      iso: rule.querySelector("time")?.dateTime || "",
      maxCardBottomAbove: Math.max(
        ...cardRects
          .filter((card) => card.index < ruleIndex)
          .map((card) => card.rect.top + card.rect.height),
      ),
      minCardTopBelow: Math.min(
        ...cardRects
          .filter((card) => card.index > ruleIndex)
          .map((card) => card.rect.top),
      ),
      bottom: rect.top + rect.height,
      top: rect.top,
    };
  });
  const backfillAudit = await masonrySmokeBackfillAudit(lane, host, config);
  const columnRecovery = await masonrySmokeColumnRecovery(lane, cards);
  return {
    backfillAudit,
    bandAudit: {
      bandRows: Number.parseInt(
        hostStyle.getPropertyValue("--message-pack-band"),
        10,
      ),
      cards: cardRects.map((card) => ({
        alignSelf: card.alignSelf,
        height: card.rect.height,
        imageOnly: card.imageOnly,
        span: card.span,
      })),
      rowGapPx: Number.parseFloat(hostStyle.rowGap),
      rowStridePx:
        Number.parseFloat(hostStyle.gridAutoRows) +
        Number.parseFloat(hostStyle.rowGap),
    },
    columnAudit,
    columnRecovery,
    repackAudit,
    segmentFanOut,
    dividerIndex,
    dividerRect,
    dividerFillRatio:
      dividerRect.width /
      (hostRect.width -
        Number.parseFloat(hostStyle.paddingLeft) -
        Number.parseFloat(hostStyle.paddingRight)),
    cardsAboveDivider: cardRects
      .filter((card) => card.index < dividerIndex)
      .map((card) => card.rect.top + card.rect.height),
    cardsBelowDivider: cardRects
      .filter((card) => card.index > dividerIndex)
      .map((card) => card.rect.top),
    distinctColumnLefts: Array.from(
      new Set(cardRects.map((card) => Math.round(card.left))),
    ).length,
    expectedColumns: config.expectedColumns,
    expectedRuleIsos: config.expectedRuleIsos,
    hostDisplay: hostStyle.display,
    missingTimestampStamps: Array.from(
      host.querySelectorAll(
        "article[data-message-key], .compaction-divider, .time-rule",
      ),
    ).filter((node) => !node.dataset.messageTs).length,
    ruleAudits,
    spans: cardRects.map((card) => card.span),
    viewportWidth: config.width,
  };
}

function masonrySmokeBarrierBounds(host) {
  return Array.from(
    host.querySelectorAll(
      '[data-masonry-smoke="divider"], [data-masonry-smoke="rule"]',
    ),
  )
    .map((node) => Number(node.dataset.masonrySmokeIndex))
    .sort((a, b) => a - b);
}

// Per column and barrier segment: cards sorted by top must appear in
// chronological fixture order, with no interior hole beyond one row stride
// (stretch fill leaves only the row gap; an unstretched image card may add
// sub-stride slack). Cards sharing a grid row must also read left-to-right in
// chronological order — that is the visible band.
function masonrySmokeColumnAudit(cardRects, barrierIndexes) {
  const bounds = [-Infinity, ...barrierIndexes, Infinity];
  const byColumnSegment = new Map();
  for (const card of cardRects) {
    let segment = 0;
    while (card.index > bounds[segment + 1]) segment += 1;
    const key = Math.round(card.left) + ":" + segment;
    const column = byColumnSegment.get(key) || [];
    column.push(card);
    byColumnSegment.set(key, column);
  }
  const audit = {
    chronological: true,
    maxInnerGapPx: 0,
    perColumnCounts: [],
    sameRowChronological: true,
  };
  for (const column of byColumnSegment.values()) {
    column.sort((a, b) => a.rect.top - b.rect.top);
    audit.perColumnCounts.push(column.length);
    for (let i = 1; i < column.length; i += 1) {
      if (column[i].index < column[i - 1].index) audit.chronological = false;
      const previous = column[i - 1].rect;
      const gap = column[i].rect.top - (previous.top + previous.height);
      if (gap > audit.maxInnerGapPx) audit.maxInnerGapPx = gap;
    }
  }
  const byRow = new Map();
  for (const card of cardRects) {
    const row = byRow.get(card.row) || [];
    row.push(card);
    byRow.set(card.row, row);
  }
  for (const row of byRow.values()) {
    row.sort((a, b) => a.index - b.index);
    for (let i = 1; i < row.length; i += 1) {
      if (Number(row[i].column) <= Number(row[i - 1].column))
        audit.sameRowChronological = false;
    }
  }
  return audit;
}

// Leftmost-feasible placement must open every barrier segment as a left-to-
// right fan: the first cards of a segment land in columns 1, 2, 3, ... in
// chronological order.
function masonrySmokeSegmentFanOut(cardRects, barrierIndexes, expectedColumns) {
  const bounds = [-Infinity, ...barrierIndexes, Infinity];
  const fans = [];
  for (let i = 0; i + 1 < bounds.length; i += 1) {
    const cards = cardRects
      .filter((card) => card.index > bounds[i] && card.index < bounds[i + 1])
      .sort((a, b) => a.index - b.index);
    if (!cards.length) continue;
    fans.push(
      cards
        .slice(0, Math.min(expectedColumns, cards.length))
        .map((card) => card.column),
    );
  }
  return fans;
}

// Prepending history must not re-pin a single existing card: segment keys are
// barrier identities, so untouched downstream segments keep their pins.
async function masonrySmokeBackfillAudit(lane, host, config) {
  const cards = Array.from(
    host.querySelectorAll('[data-masonry-smoke="card"]'),
  );
  const capturePins = () =>
    cards.map((card) => [
      card.dataset.messageKey,
      card.style.gridColumnStart,
      card.dataset.messagePackSegment,
    ]);
  const pinsBefore = capturePins();
  const base = Date.parse(config.plan.baseIso);
  const nodes = config.plan.backfillMinutes.map((minute, position) => {
    const node = renderMessage(lane, {
      ack_count: 0,
      ack_keys: [],
      display_html: "<p>Backfill card " + position + " arrives late.</p>",
      display_text: "backfill card " + position,
      index: minute,
      key: "masonry-backfill-" + position + "-" + config.width,
      kind: "assistant",
      text: "backfill card " + position,
      timestamp: new Date(base + minute * 60000).toISOString(),
    });
    node.dataset.masonrySmoke = "card";
    node.dataset.masonrySmokeIndex = String(minute);
    return node;
  });
  host.prepend(...nodes);
  packMessageStream(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  return {
    backfillColumns: nodes.map((node) => node.style.gridColumnStart),
    stable: JSON.stringify(pinsBefore) === JSON.stringify(capturePins()),
  };
}

// Packing the same content twice must be a fixed point: spans, columns, and
// rows all identical, or stretch-fill and measurement are feeding back.
async function masonrySmokeRepackAudit(lane, host) {
  const items = Array.from(host.querySelectorAll("[data-masonry-smoke]"));
  const capture = () =>
    items.map((node) => [
      node.style.getPropertyValue("--message-pack-row-span"),
      node.style.gridColumnStart,
      node.style.gridRowStart,
    ]);
  const before = capture();
  packMessageStream(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  return { stable: JSON.stringify(before) === JSON.stringify(capture()) };
}

// Regression guard for the auto-fit collapse (f63d310): force every card into
// column 1, repack, and demand redistribution from geometry-derived counting.
async function masonrySmokeColumnRecovery(lane, cards) {
  for (const card of cards) {
    delete card.dataset.messagePackColumn;
    delete card.dataset.messagePackSegment;
    card.style.gridColumnStart = "1";
  }
  await new Promise((resolve) => requestAnimationFrame(resolve));
  packMessageStream(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const lefts = new Set(
    cards.map((card) => Math.round(card.getBoundingClientRect().left)),
  );
  return { distinctColumns: lefts.size };
}

function masonrySmokeRect(element) {
  const rect = element.getBoundingClientRect();
  return {
    height: rect.height,
    left: rect.left,
    top: rect.top,
    width: rect.width,
  };
}

function assertMasonryMeasurement(measurement) {
  const label = " at " + measurement.viewportWidth + "px: ";
  const fail = (reason) => {
    throw new Error(reason + label + JSON.stringify(measurement));
  };
  assertMasonryBaseLayout(measurement, fail);
  assertMasonryBands(measurement, fail);
  assertMasonryDividerBarrier(measurement, fail);
  assertMasonryTimeRules(measurement, fail);
  assertMasonryDistribution(measurement, fail);
}

function assertMasonryBaseLayout(measurement, fail) {
  if (measurement.hostDisplay !== "grid") fail("message host is not grid");
  if (!measurement.spans.every((span) => span > 0))
    fail("cards missing row spans");
  if (measurement.missingTimestampStamps > 0)
    fail("pack items missing data-message-ts");
  if (!measurement.repackAudit.stable) fail("pack is not idempotent");
  if (!measurement.columnAudit.chronological)
    fail("column order is not chronological");
  if (!measurement.columnAudit.sameRowChronological)
    fail("cards sharing a row are not chronological left-to-right");
  const gapBound =
    measurement.bandAudit.rowStridePx + measurement.bandAudit.rowGapPx;
  if (measurement.columnAudit.maxInnerGapPx > gapBound)
    fail(
      "column contains an interior hole of " +
        measurement.columnAudit.maxInnerGapPx +
        "px (bound " +
        gapBound +
        "px)",
    );
}

function assertMasonryBands(measurement, fail) {
  const band = measurement.bandAudit;
  for (const card of band.cards) {
    const cellHeight = card.span * band.rowStridePx - band.rowGapPx;
    if (card.imageOnly) {
      if (card.alignSelf !== "start")
        fail("image-only card lost start alignment");
      if (cellHeight - card.height >= band.rowStridePx)
        fail("image-only card span was band-inflated");
      continue;
    }
    if (card.alignSelf !== "stretch") fail("card is not stretch-aligned");
    if (measurement.expectedColumns <= 1) continue;
    if (card.span % band.bandRows !== 0)
      fail("card span is not a whole band multiple");
    if (Math.abs(cellHeight - card.height) > 1)
      fail("card box does not fill its band cell");
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
  if (expectedColumns > 1) {
    for (const fan of measurement.segmentFanOut) {
      const orderedColumns = fan.map((column, position) => String(position + 1));
      if (JSON.stringify(fan) !== JSON.stringify(orderedColumns))
        fail("segment does not fan out left-to-right: " + JSON.stringify(fan));
    }
  }
  if (!measurement.backfillAudit.stable)
    fail("history backfill re-pinned downstream cards");
  if (measurement.distinctColumnLefts < expectedColumns)
    fail(
      "cards under-distributed (" +
        measurement.distinctColumnLefts +
        " columns, expected at least " +
        expectedColumns +
        ")",
    );
  if (expectedColumns === 1 && measurement.distinctColumnLefts !== 1)
    fail("narrow viewport did not collapse to one column");
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
