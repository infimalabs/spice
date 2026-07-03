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
  finalCardStep: 17,
  segmentCardCount: 12,
};

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
      masonrySmokeColumnAudit,
      masonrySmokeColumnRecovery,
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
    .querySelectorAll("article[data-message-key], .compaction-divider")
    .forEach((node) => node.remove());
  const nodes = [];
  for (const item of masonrySmokeItems(config)) {
    const node = renderMessage(lane, item);
    if (!node) continue;
    node.dataset.masonrySmoke = item.kind === "compaction" ? "divider" : "card";
    node.dataset.masonrySmokeIndex = String(item.index);
    nodes.push(node);
  }
  host.append(...nodes);
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
  const base = Date.parse("2026-07-03T10:04:00.000Z");
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
  return items;
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
  const dividerRect = masonrySmokeRect(divider);
  const dividerIndex = Number(divider.dataset.masonrySmokeIndex);
  const cardRects = cards.map((card) => ({
    column: card.style.gridColumnStart,
    index: Number(card.dataset.masonrySmokeIndex),
    left: masonrySmokeRect(card).left,
    rect: masonrySmokeRect(card),
    span: Number.parseInt(
      card.style.getPropertyValue("--message-pack-row-span") || "0",
      10,
    ),
  }));
  const hostStyle = getComputedStyle(host);
  const hostRect = masonrySmokeRect(host);
  const columnAudit = masonrySmokeColumnAudit(cardRects);
  const columnRecovery = await masonrySmokeColumnRecovery(lane, cards);
  return {
    columnAudit,
    columnRecovery,
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
    hostDisplay: hostStyle.display,
    missingTimestampStamps: Array.from(
      host.querySelectorAll("article[data-message-key], .compaction-divider"),
    ).filter((node) => !node.dataset.messageTs).length,
    spans: cardRects.map((card) => card.span),
    viewportWidth: config.width,
  };
}

// Per column: cards sorted by top must appear in chronological fixture order,
// and the largest vertical hole between consecutive cards is reported so the
// banded-packing work has a measured baseline to tighten.
function masonrySmokeColumnAudit(cardRects) {
  const byColumn = new Map();
  for (const card of cardRects) {
    const key = Math.round(card.left);
    const column = byColumn.get(key) || [];
    column.push(card);
    byColumn.set(key, column);
  }
  const audit = { chronological: true, maxInnerGapPx: 0, perColumnCounts: [] };
  for (const column of byColumn.values()) {
    column.sort((a, b) => a.rect.top - b.rect.top);
    audit.perColumnCounts.push(column.length);
    for (let i = 1; i < column.length; i += 1) {
      if (column[i].index < column[i - 1].index) audit.chronological = false;
      const previous = column[i - 1].rect;
      const gap = column[i].rect.top - (previous.top + previous.height);
      if (gap > audit.maxInnerGapPx) audit.maxInnerGapPx = gap;
    }
  }
  return audit;
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
  if (measurement.hostDisplay !== "grid")
    throw new Error("message host is not grid" + label + JSON.stringify(measurement));
  if (!measurement.spans.every((span) => span > 0))
    throw new Error("cards missing row spans" + label + JSON.stringify(measurement));
  if (measurement.missingTimestampStamps > 0)
    throw new Error("pack items missing data-message-ts" + label + JSON.stringify(measurement));
  if (measurement.dividerFillRatio < 0.98)
    throw new Error("compaction divider is not full width" + label + JSON.stringify(measurement));
  const dividerTop = measurement.dividerRect.top;
  const dividerBottom = dividerTop + measurement.dividerRect.height;
  if (!measurement.cardsAboveDivider.every((bottom) => bottom <= dividerTop + 1))
    throw new Error("pre-compaction card crossed the barrier" + label + JSON.stringify(measurement));
  if (!measurement.cardsBelowDivider.every((top) => top >= dividerBottom - 1))
    throw new Error("post-compaction card floated above the barrier" + label + JSON.stringify(measurement));
  if (!measurement.columnAudit.chronological)
    throw new Error("column order is not chronological" + label + JSON.stringify(measurement));
  const expectedColumns = measurement.expectedColumns;
  if (measurement.distinctColumnLefts < expectedColumns)
    throw new Error(
      "cards under-distributed (" +
        measurement.distinctColumnLefts +
        " columns, expected at least " +
        expectedColumns +
        ")" +
        label +
        JSON.stringify(measurement),
    );
  if (expectedColumns === 1 && measurement.distinctColumnLefts !== 1)
    throw new Error("narrow viewport did not collapse to one column" + label + JSON.stringify(measurement));
  if (
    expectedColumns > 1 &&
    measurement.columnRecovery.distinctColumns < expectedColumns
  )
    throw new Error(
      "packer did not redistribute out of a collapsed single column" +
        label +
        JSON.stringify(measurement),
    );
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
