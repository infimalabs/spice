const path = require("path");
const { withServePage } = require("./serve_playwright_harness");

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

async function installMasonrySmokeHelpers(page) {
  await page.addScriptTag({
    content: [
      "const masonryExpectedGridTrackCount = " +
        masonryExpectedGridTrackCount +
        ";",
      renderMasonryFixture,
      masonrySmokeLane,
      masonrySmokeShowAllLanes,
      masonrySmokeUseSingleLane,
      masonrySmokeItems,
      masonrySmokeBodyHtml,
      masonrySmokeMeasurement,
      masonrySmokeVisibleLaneCount,
      masonrySmokeRowAudit,
      masonrySmokeColumnSpan,
      masonrySmokeGridTrackCount,
      masonrySmokeRuleAudits,
      masonrySmokeBarrierBounds,
      masonrySmokeColumnAudit,
      masonrySmokeSegmentFanOut,
      masonrySmokeAppendStabilityAudit,
      masonrySmokeHydrationGrowthAudit,
      masonrySmokeMiddleRemovalReflowAudit,
      masonrySmokePackSnapshot,
      masonrySmokeFirstPackDiff,
      masonrySmokeSpannedCardCount,
      masonrySmokeColumnRecovery,
      masonrySmokeRootWidthStaleRecoveryAudit,
      masonrySmokeRepackAudit,
      masonrySmokeResizeAudit,
      masonrySmokeStructuredFinalAudit,
      renderMasonryTeamFixture,
      masonryTeamTargetPayload,
      masonryTeamItems,
      masonryTeamMeasurement,
      masonryTeamForcedSpanRecovery,
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

async function renderMasonryFixture(config) {
  const lane = masonrySmokeLane();
  if (config.keepLanesVisible) masonrySmokeShowAllLanes();
  else masonrySmokeUseSingleLane(lane);
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

async function renderMasonryTeamFixture(config) {
  const teamTargets = ["alpha", "beta", "gamma"].map((id, index) =>
    masonryTeamTargetPayload(id, index),
  );
  targets = teamTargets;
  targetById = new Map(targets.map((target) => [target.id, target]));
  laneStates.clear();
  lanesEl.replaceChildren();
  teamSnapshotRevision = 0;
  applyTeamSnapshotPayload(
    {
      revision: 1,
      changed: true,
      snapshot: {
        globalSettings: { fastMode: false },
        teams: [
          {
            teamId: "masonry-team",
            revision: 1,
            config: {
              revision: 1,
              lifetime: "Drive",
              speechMode: "speak",
              selectedView: "compose",
              taskFilters: [],
              taskFilterEntries: [],
            },
            splitBack: {},
            members: teamTargets.map((target) => ({
              agentId: "target:" + target.id,
            })),
          },
        ],
      },
    },
    { force: true },
  );
  const host = Array.from(laneStates.values()).find(
    (lane) =>
      !isShadowLane(lane) &&
      laneGroupMemberTargetIds(lane).length === teamTargets.length,
  );
  if (!host) throw new Error("missing masonry team host");
  const messageHost = host.messagesEl || document.querySelector(".messages");
  if (!messageHost) throw new Error("team message host unavailable");
  messageHost
    .querySelectorAll("article[data-message-key], .compaction-divider, .time-rule")
    .forEach((node) => node.remove());
  const nodes = masonryTeamItems(config, teamTargets).map((item) => {
    const node = renderMessage(host, item);
    if (!node) throw new Error("team masonry item did not render");
    node.dataset.masonryTeamSmoke = "card";
    node.dataset.masonryTeamMember = item.producerTargetId;
    return node;
  });
  messageHost.append(...nodes);
  await masonrySmokeImagesReady(messageHost);
  packMessageStream(host);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  await new Promise((resolve) => requestAnimationFrame(resolve));
  return masonryTeamMeasurement(host, messageHost, config);
}

function masonryTeamTargetPayload(id, index) {
  const threadId = id + "-thread";
  return {
    id,
    name: id,
    branch: id,
    targetIdentity: {
      targetId: id,
      worktreeName: id,
      branch: id,
      driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
      agent: { state: "configured", name: id },
      thread: { state: "bound", threadId },
    },
    serveAgentIdentity: {
      actorId: "thread:" + threadId,
      target: { id },
      thread: { state: "bound", threadId },
    },
    teamIdentity: {
      state: "member",
      teamId: "masonry-team",
      teamRevision: 1,
      configRevision: 1,
    },
    taskFilters: [],
    laneFilterVersion: "",
    lifetime: "Drive",
    statusLine: {
      agentVisualStatus: index === 0 ? "running" : "idle",
      pendingInboxCount: 0,
      pendingInboxKeys: [],
      pendingInboxRevision: "pending-" + id,
      pendingInboxVersion: 1,
    },
  };
}

function masonryTeamItems(config, teamTargets) {
  const plan = config.plan;
  const base = Date.parse(plan.baseIso);
  const shapes = ["short", "medium", "tall", "code", "short"];
  const producerPattern = [0, 1, 0, 2, 1, 2, 0, 2, 1, 0, 1, 2, 2, 0, 1];
  return Array.from({ length: 15 }, (_, index) => {
    const target = teamTargets[producerPattern[index] % teamTargets.length];
    const threadId = target.targetIdentity.thread.threadId;
    const timestamp = new Date(base + index * 60000).toISOString();
    return {
      ack_count: 0,
      ack_keys: [],
      display_html: masonrySmokeBodyHtml(shapes[index % shapes.length], index),
      display_text: "team masonry card " + index,
      index,
      key: "team-masonry-" + index + "-" + config.width,
      kind: index % 7 === 0 ? "final" : "assistant",
      producerTargetId: target.id,
      text: "team masonry card " + index,
      threadId,
      timestamp,
    };
  });
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

function masonrySmokeShowAllLanes() {
  for (const lane of laneStates.values()) {
    lane.element.style.display = "";
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
    const kind = step === plan.finalCardStep ? "final" : "assistant";
    const shape =
      kind === "final" ? "structured-final" : shapes[step % shapes.length];
    push(kind, 56 + (step - plan.segmentCardCount) * 12, {
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
  if (shape === "structured-final")
    return (
      sentence +
      sentence +
      "<p>Final response section one carries the decision and validation.</p>" +
      "<ul><li>Implemented deterministic placement.</li>" +
      "<li>Verified wide screen refresh.</li>" +
      "<li>Checked responsive resize behavior.</li></ul>" +
      "<pre><code>spice task done UI-1k9vBqzn --validation \"...\"</code></pre>" +
      "<p>Final response section two stays long enough to exercise a tall card.</p>"
    );
  return sentence + sentence + sentence + sentence + sentence;
}

async function masonrySmokeMeasurement(lane, host, config) {
  const cards = Array.from(
    host.querySelectorAll('[data-masonry-smoke="card"]'),
  );
  const divider = host.querySelector('[data-masonry-smoke="divider"]');
  if (!divider) throw new Error("masonry fixture divider missing");
  const repackAudit = config.captureOnly
    ? { firstDiff: null, spannedCards: 0, stable: true }
    : await masonrySmokeRepackAudit(lane, host);
  const dividerRect = masonrySmokeRect(divider);
  const dividerIndex = Number(divider.dataset.masonrySmokeIndex);
  const cardRects = cards.map((card) => {
    const columnSpan = masonrySmokeColumnSpan(card);
    return {
      alignSelf: getComputedStyle(card).alignSelf,
      column: card.style.gridColumnStart,
      columnSpan,
      final: card.classList.contains("final"),
      imageOnly: card.classList.contains("image-only"),
      index: Number(card.dataset.masonrySmokeIndex),
      key: card.dataset.messageKey,
      left: masonrySmokeRect(card).left,
      rect: masonrySmokeRect(card),
      row: Number.parseInt(card.style.gridRowStart || "0", 10),
      scrollHeight: card.scrollHeight,
      slot: Number.parseInt(card.dataset.messagePackColumn || "0", 10) + 1,
      slotSpan: columnSpan,
      span: Number.parseInt(
        card.style.getPropertyValue("--message-pack-row-span") || "0",
        10,
      ),
    };
  });
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
  const ruleAudits = masonrySmokeRuleAudits(host, cardRects, hostInnerWidth);
  const appendStabilityAudit = config.captureOnly
    ? { stable: true }
    : await masonrySmokeAppendStabilityAudit(lane, host, config);
  const hydrationGrowthAudit = config.captureOnly
    ? { stable: true }
    : await masonrySmokeHydrationGrowthAudit(lane, host, config);
  const columnRecovery = config.captureOnly
    ? { distinctColumns: config.expectedColumns }
    : await masonrySmokeColumnRecovery(lane, cards);
  const middleRemovalReflowAudit = config.captureOnly
    ? { reflowed: true }
    : await masonrySmokeMiddleRemovalReflowAudit(lane, host, config);
  return {
    appendStabilityAudit,
    rowAudit: masonrySmokeRowAudit(cardRects, hostStyle),
    columnAudit,
    columnRecovery,
    hydrationGrowthAudit,
    middleRemovalReflowAudit,
    repackAudit,
    keepLanesVisible: Boolean(config.keepLanesVisible),
    rootWidthActive: lane.element.classList.contains("lane--message-pack-root"),
    segmentFanOut,
    structuredFinalAudit: masonrySmokeStructuredFinalAudit(cardRects),
    dividerIndex,
    dividerRect,
    dividerFillRatio:
      dividerRect.width /
      (hostRect.width -
        Number.parseFloat(hostStyle.paddingLeft) -
        Number.parseFloat(hostStyle.paddingRight)),
    gridTrackCount: masonrySmokeGridTrackCount(hostStyle),
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
    columnSpans: cardRects.map((card) => card.columnSpan),
    spans: cardRects.map((card) => card.span),
    viewportWidth: config.width,
    visibleLaneCount: masonrySmokeVisibleLaneCount(),
  };
}

function masonrySmokeVisibleLaneCount() {
  return Array.from(document.querySelectorAll(".swimlanes > .lane")).filter(
    (lane) => getComputedStyle(lane).display !== "none",
  ).length;
}

function masonrySmokeRowAudit(cardRects, hostStyle) {
  return {
    cards: cardRects.map((card) => ({
      alignSelf: card.alignSelf,
      height: card.rect.height,
      imageOnly: card.imageOnly,
      scrollHeight: card.scrollHeight,
      span: card.span,
    })),
    rowGapPx: Number.parseFloat(hostStyle.rowGap),
    rowStridePx:
      Number.parseFloat(hostStyle.gridAutoRows) +
      Number.parseFloat(hostStyle.rowGap),
  };
}

function masonrySmokeColumnSpan(card) {
  const span = Number.parseInt(
    String(card.style.gridColumnEnd || "").replace("span", ""),
    10,
  );
  return Number.isFinite(span) && span > 0 ? span : 1;
}

function masonrySmokeGridTrackCount(style) {
  return String(style.gridTemplateColumns || "")
    .split(/\s+/)
    .filter(Boolean).length;
}

function masonrySmokeRuleAudits(host, cardRects, hostInnerWidth) {
  return Array.from(host.querySelectorAll('[data-masonry-smoke="rule"]')).map(
    (rule) => {
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
    },
  );
}

function masonryTeamMeasurement(lane, host, config) {
  const cards = Array.from(
    host.querySelectorAll('[data-masonry-team-smoke="card"]'),
  );
  if (!cards.length) throw new Error("team masonry fixture missing cards");
  const hostStyle = getComputedStyle(host);
  const hostRect = masonrySmokeRect(host);
  const columnGap = Number.parseFloat(hostStyle.columnGap) || 0;
  const rowGap = Number.parseFloat(hostStyle.rowGap) || 0;
  const rowStride =
    (Number.parseFloat(hostStyle.gridAutoRows) || 0) + rowGap;
  const hostInnerWidth =
    hostRect.width -
    Number.parseFloat(hostStyle.paddingLeft) -
    Number.parseFloat(hostStyle.paddingRight);
  const expectedColumnSpan =
    masonryExpectedGridTrackCount / config.expectedColumns;
  const gridTrackWidth =
    (hostInnerWidth - columnGap * (masonryExpectedGridTrackCount - 1)) /
    masonryExpectedGridTrackCount;
  const expectedTrackWidth =
    gridTrackWidth * expectedColumnSpan + columnGap * (expectedColumnSpan - 1);
  const memberColumns = {};
  const cardAudits = cards.map((card) => {
    const rect = masonrySmokeRect(card);
    const member = card.dataset.masonryTeamMember || "";
    const left = Math.round(rect.left);
    const columns = memberColumns[member] || new Set();
    columns.add(left);
    memberColumns[member] = columns;
    const span = Number.parseInt(
      card.style.getPropertyValue("--message-pack-row-span") || "0",
      10,
    );
    const columnSpan = masonrySmokeColumnSpan(card);
    const cellHeight = span * rowStride - rowGap;
    return {
      cellHeight,
      columnSpan,
      left,
      member,
      scrollHeight: card.scrollHeight,
      span,
      width: rect.width,
    };
  });
  const forcedSpanRecovery = masonryTeamForcedSpanRecovery(
    lane,
    cards[0],
    rowStride,
    rowGap,
    config.forcedTallSpan,
  );
  const memberColumnCounts = Object.fromEntries(
    Object.entries(memberColumns).map(([member, columns]) => [
      member,
      columns.size,
    ]),
  );
  return {
    cardWidths: cardAudits.map((card) => card.width),
    distinctColumnLefts: new Set(cardAudits.map((card) => card.left)).size,
    expectedColumns: config.expectedColumns,
    expectedColumnSpan,
    expectedTrackWidth,
    baseMaxCardWidthDelta: Math.max(
      ...cardAudits
        .filter(
          (card) =>
            card.columnSpan ===
            masonryExpectedGridTrackCount / config.expectedColumns,
        )
        .map((card) => Math.abs(card.width - expectedTrackWidth)),
    ),
    gridTrackCount: masonrySmokeGridTrackCount(hostStyle),
    hostDisplay: hostStyle.display,
    hostInnerWidth,
    maxCardWidthDelta: Math.max(
      ...cardAudits.map((card) => Math.abs(card.width - expectedTrackWidth)),
    ),
    maxColumnSpan: Math.max(...cardAudits.map((card) => card.columnSpan)),
    minColumnSpan: Math.min(...cardAudits.map((card) => card.columnSpan)),
    maxStretchSlackPx: Math.max(
      ...cardAudits.map((card) =>
        Math.max(0, card.cellHeight - card.scrollHeight - 2),
      ),
    ),
    memberColumnCounts,
    forcedSpanRecovery,
    rowStridePx: rowStride,
    viewportWidth: config.width,
  };
}

function masonryTeamForcedSpanRecovery(
  lane,
  card,
  rowStride,
  rowGap,
  forcedTallSpan,
) {
  const naturalHeight = card.scrollHeight + 2;
  card.style.setProperty("--message-pack-row-span", String(forcedTallSpan));
  card.getBoundingClientRect();
  packMessageStream(lane);
  const span = Number.parseInt(
    card.style.getPropertyValue("--message-pack-row-span") || "0",
    10,
  );
  const cellHeight = span * rowStride - rowGap;
  return {
    cellHeight,
    naturalHeight,
    slackPx: Math.max(0, cellHeight - naturalHeight),
    span,
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
// sub-stride slack). Cards sharing a grid row may read left-to-right,
// right-to-left, or from independent best-fit columns, but their occupied slot
// ranges must not overlap.
function masonrySmokeColumnAudit(cardRects, barrierIndexes) {
  const bounds = [-Infinity, ...barrierIndexes, Infinity];
  const byColumnSegment = new Map();
  for (const card of cardRects) {
    let segment = 0;
    while (card.index > bounds[segment + 1]) segment += 1;
    for (let slot = card.slot; slot < card.slot + card.slotSpan; slot += 1) {
      const key = slot + ":" + segment;
      const column = byColumnSegment.get(key) || [];
      column.push({ ...card, slot });
      byColumnSegment.set(key, column);
    }
  }
  const audit = {
    chronological: true,
    maxInnerGapPair: null,
    maxInnerGapPx: 0,
    perColumnCounts: [],
    sameRowNonOverlapping: true,
  };
  for (const column of byColumnSegment.values()) {
    column.sort((a, b) => a.rect.top - b.rect.top);
    audit.perColumnCounts.push(column.length);
    for (let i = 1; i < column.length; i += 1) {
      if (column[i].index < column[i - 1].index) audit.chronological = false;
      const previous = column[i - 1].rect;
      const gap = column[i].rect.top - (previous.top + previous.height);
      if (gap > audit.maxInnerGapPx) {
        audit.maxInnerGapPx = gap;
        audit.maxInnerGapPair = {
          gap,
          previousIndex: column[i - 1].index,
          previousSlot: column[i - 1].slot,
          nextIndex: column[i].index,
          nextSlot: column[i].slot,
        };
      }
    }
  }
  const byRow = new Map();
  for (const card of cardRects) {
    const row = byRow.get(card.row) || [];
    row.push(card);
    byRow.set(card.row, row);
  }
  for (const row of byRow.values()) {
    row.sort((a, b) => a.slot - b.slot);
    for (let i = 1; i < row.length; i += 1) {
      if (row[i].slot < row[i - 1].slot + row[i - 1].slotSpan)
        audit.sameRowNonOverlapping = false;
    }
  }
  return audit;
}

// Best-slot placement must open every barrier segment without overflowing the
// 12-track grid. Multi-track cards may consume several adjacent tracks, so
// coverage is checked by occupied range rather than a single-column march.
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
        .map((card) => ({ slot: card.slot, slotSpan: card.slotSpan })),
    );
  }
  return fans;
}

// Newer live arrivals enter ahead of the retained DOM in the same shape as the
// app's newest-first render path. Existing cards must keep their columns while
// the operator is reading; only the new cards need fresh placement.
async function masonrySmokeAppendStabilityAudit(lane, host, config) {
  const cards = Array.from(
    host.querySelectorAll('[data-masonry-smoke="card"]'),
  );
  const capturePins = () =>
    Object.fromEntries(
      cards.map((card) => [
        card.dataset.messageKey,
        card.style.gridColumnStart,
      ]),
    );
  const pinsBefore = capturePins();
  const base = Date.parse(config.plan.baseIso);
  const nodes = [0, 1, 2].map((offset, position) => {
    const minute = config.plan.epilogueMinute + 20 + offset;
    const node = renderMessage(lane, {
      ack_count: 0,
      ack_keys: [],
      display_html: "<p>Live card " + position + " arrives while reading.</p>",
      display_text: "live card " + position,
      index: 1000 + position,
      key: "masonry-live-" + position + "-" + config.width,
      kind: "assistant",
      text: "live card " + position,
      timestamp: new Date(base + minute * 60000).toISOString(),
    });
    node.dataset.masonrySmoke = "card";
    node.dataset.masonrySmokeIndex = String(1000 + position);
    return node;
  });
  host.prepend(...nodes);
  packMessageStream(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  return {
    existingColumnsBefore: pinsBefore,
    existingColumnsAfter: capturePins(),
    newColumns: nodes.map((node) => node.style.gridColumnStart),
    stable:
      JSON.stringify(pinsBefore) === JSON.stringify(capturePins()) &&
      (config.expectedColumns <= 1 ||
        new Set(nodes.map((node) => node.style.gridColumnStart)).size > 1),
  };
}

// Older history hydration grows the retained transcript past the old tail.
// When the width is unchanged, existing cards should keep their columns and
// the newly hydrated cards should grow below them.
async function masonrySmokeHydrationGrowthAudit(lane, host, config) {
  const cards = Array.from(
    host.querySelectorAll('[data-masonry-smoke="card"]'),
  );
  const capturePins = () =>
    Object.fromEntries(
      cards.map((card) => [
        card.dataset.messageKey,
        card.style.gridColumnStart,
      ]),
    );
  const pinsBefore = capturePins();
  const base = Date.parse(config.plan.baseIso);
  const nodes = config.plan.backfillMinutes.map((minute, position) => {
    const node = renderMessage(lane, {
      ack_count: 0,
      ack_keys: [],
      display_html: "<p>Hydrated card " + position + " arrives late.</p>",
      display_text: "hydrated card " + position,
      index: minute,
      key: "masonry-hydrated-" + position + "-" + config.width,
      kind: "assistant",
      text: "hydrated card " + position,
      timestamp: new Date(base + minute * 60000).toISOString(),
    });
    node.dataset.masonrySmoke = "card";
    node.dataset.masonrySmokeIndex = String(minute);
    return node;
  });
  host.append(...nodes);
  packMessageStream(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const hydratedColumns = nodes.map((node) => node.style.gridColumnStart);
  return {
    existingColumnsBefore: pinsBefore,
    existingColumnsAfter: capturePins(),
    hydratedColumns,
    stable:
      JSON.stringify(pinsBefore) === JSON.stringify(capturePins()) &&
      (config.expectedColumns <= 1 ||
        hydratedColumns.every((column) => Boolean(column))),
  };
}

// Packing the same content twice must be a fixed point: spans, columns, and
// rows all identical, or stretch-fill and measurement are feeding back.
async function masonrySmokeRepackAudit(lane, host) {
  const items = Array.from(host.querySelectorAll("[data-masonry-smoke]"));
  const before = masonrySmokePackSnapshot(items);
  packMessageStream(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const after = masonrySmokePackSnapshot(items);
  const firstDiff = masonrySmokeFirstPackDiff(before, after);
  return {
    firstDiff,
    spannedCards: masonrySmokeSpannedCardCount(items),
    stable: firstDiff === null,
  };
}

function masonrySmokePackSnapshot(items) {
  return items.map((node) => [
    node.style.getPropertyValue("--message-pack-row-span"),
    node.style.gridColumnStart,
    node.style.gridRowStart,
  ]);
}

function masonrySmokeFirstPackDiff(before, after) {
  const index = before.findIndex(
    (entry, position) => JSON.stringify(entry) !== JSON.stringify(after[position]),
  );
  if (index < 0) return null;
  return { after: after[index], before: before[index] };
}

function masonrySmokeSpannedCardCount(items) {
  return items.filter(
    (node) =>
      node.dataset.masonrySmoke === "card" && masonrySmokeColumnSpan(node) > 1,
  ).length;
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

async function masonrySmokeRootWidthStaleRecoveryAudit(config) {
  const lane = masonrySmokeLane();
  masonrySmokeUseSingleLane(lane);
  const host = lane.messagesEl || document.querySelector(".messages");
  if (!host) throw new Error("message host unavailable");
  host
    .querySelectorAll("article[data-message-key], .compaction-divider, .time-rule")
    .forEach((node) => node.remove());
  const items = masonrySmokeItems(config);
  const nodes = [];
  let previousItem = null;
  for (const item of items) {
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
  lane.knownMessages = items.slice();
  lane.knownMessageKeys = new Set(items.map((item) => item.key));
  lane.newestMessageKey = items.length ? items[0].key : "";
  lane.oldestMessageKey = items.length ? items[items.length - 1].key : "";
  lane.renderedMessageFingerprint = messageRenderFingerprint(lane, items);
  packMessageStream(lane);
  const root = messagePackRootElement(host);
  lane.element.classList.add("lane--message-pack-root");
  host.style.setProperty(
    "--message-pack-root-width",
    Math.max(lane.element.getBoundingClientRect().width, root.clientWidth) + "px",
  );
  lane.messagePackLayoutState = {
    columnCount: config.expectedColumns,
    itemKeys: messagePackItemKeys(host),
    rootWidthActive: true,
    version: messagePackLayoutVersion,
  };
  const beforeRootWidthActive =
    lane.element.classList.contains("lane--message-pack-root") &&
    Boolean(host.style.getPropertyValue("--message-pack-root-width"));
  masonrySmokeShowAllLanes();
  renderMessagesIfChanged(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const cards = Array.from(host.querySelectorAll('[data-masonry-smoke="card"]'));
  return {
    afterRootWidthActive:
      lane.element.classList.contains("lane--message-pack-root") &&
      Boolean(host.style.getPropertyValue("--message-pack-root-width")),
    beforeRootWidthActive,
    distinctColumnLefts: new Set(
      cards.map((card) => Math.round(card.getBoundingClientRect().left)),
    ).size,
    expectedColumns: config.expectedColumns,
    gridTrackCount: masonrySmokeGridTrackCount(getComputedStyle(host)),
    visibleLaneCount: masonrySmokeVisibleLaneCount(),
  };
}

async function masonrySmokeMiddleRemovalReflowAudit(lane, host, config) {
  if (config.expectedColumns <= 1) return { reflowed: true };
  const cards = Array.from(
    host.querySelectorAll('[data-masonry-smoke="card"]'),
  );
  if (cards.length < 5) return { reflowed: true };
  cards[2].remove();
  const remaining = cards.filter((card) => card.isConnected);
  for (const card of remaining) {
    card.dataset.messagePackColumn = "0";
    card.style.gridColumnStart = "1";
  }
  packMessageStream(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const distinctColumns = new Set(
    remaining.map((card) => card.style.gridColumnStart),
  ).size;
  return {
    distinctColumns,
    reflowed:
      distinctColumns >= Math.min(config.expectedColumns, remaining.length),
  };
}

function masonrySmokeResizeAudit(config) {
  const lane = masonrySmokeLane();
  const host = lane.messagesEl || document.querySelector(".messages");
  packMessageStream(lane);
  const cards = Array.from(
    host.querySelectorAll('[data-masonry-smoke="card"]'),
  );
  const lefts = new Set(
    cards.map((card) => Math.round(card.getBoundingClientRect().left)),
  );
  return {
    distinctColumns: lefts.size,
    expectedColumns: config.expectedColumns,
    gridTrackCount: masonrySmokeGridTrackCount(getComputedStyle(host)),
    viewportWidth: config.width,
  };
}

function masonrySmokeStructuredFinalAudit(cardRects) {
  const regularHeights = cardRects
    .filter((card) => !card.final && !card.imageOnly)
    .map((card) => card.rect.height)
    .sort((a, b) => a - b);
  const finalHeights = cardRects
    .filter((card) => card.final)
    .map((card) => card.rect.height);
  const medianRegular =
    regularHeights[Math.floor(regularHeights.length / 2)] || 0;
  const maxFinalHeight = Math.max(...finalHeights, 0);
  return {
    finalCount: finalHeights.length,
    maxFinalHeight,
    medianRegular,
    ratio: medianRegular > 0 ? maxFinalHeight / medianRegular : 0,
  };
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
