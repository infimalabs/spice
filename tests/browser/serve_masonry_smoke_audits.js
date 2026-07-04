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

async function masonrySmokeNoopRenderAudit(lane) {
  const originalPack = packMessageStream;
  const originalObserverSync = syncMessagePackObserver;
  const originalHistorySync = syncLaneHistoryObserver;
  const originalTeamSync = syncTeamImportOverlay;
  const audit = {
    historySyncCount: 0,
    observerSyncCount: 0,
    packCount: 0,
    teamSyncCount: 0,
  };
  packMessageStream = function (...args) {
    audit.packCount += 1;
    return originalPack.apply(this, args);
  };
  syncMessagePackObserver = function (...args) {
    audit.observerSyncCount += 1;
    return originalObserverSync.apply(this, args);
  };
  syncLaneHistoryObserver = function (...args) {
    audit.historySyncCount += 1;
    return originalHistorySync.apply(this, args);
  };
  syncTeamImportOverlay = function (...args) {
    audit.teamSyncCount += 1;
    return originalTeamSync.apply(this, args);
  };
  try {
    renderMessagesIfChanged(lane);
    renderMessagesIfChanged(lane);
    renderMessagesIfChanged(lane);
    await new Promise((resolve) => requestAnimationFrame(resolve));
  } finally {
    packMessageStream = originalPack;
    syncMessagePackObserver = originalObserverSync;
    syncLaneHistoryObserver = originalHistorySync;
    syncTeamImportOverlay = originalTeamSync;
  }
  return audit;
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

const masonrySmokeAuditHelpers = [
  masonrySmokeAppendStabilityAudit,
  masonrySmokeHydrationGrowthAudit,
  masonrySmokeRepackAudit,
  masonrySmokePackSnapshot,
  masonrySmokeFirstPackDiff,
  masonrySmokeSpannedCardCount,
  masonrySmokeColumnRecovery,
  masonrySmokeNoopRenderAudit,
  masonrySmokeRootWidthStaleRecoveryAudit,
  masonrySmokeMiddleRemovalReflowAudit,
  masonrySmokeResizeAudit,
  masonrySmokeStructuredFinalAudit,
  masonrySmokeRect,
];

module.exports = {
  masonrySmokeAppendStabilityAudit,
  masonrySmokeHydrationGrowthAudit,
  masonrySmokeRepackAudit,
  masonrySmokePackSnapshot,
  masonrySmokeFirstPackDiff,
  masonrySmokeSpannedCardCount,
  masonrySmokeColumnRecovery,
  masonrySmokeNoopRenderAudit,
  masonrySmokeRootWidthStaleRecoveryAudit,
  masonrySmokeMiddleRemovalReflowAudit,
  masonrySmokeResizeAudit,
  masonrySmokeStructuredFinalAudit,
  masonrySmokeRect,
  masonrySmokeAuditHelpers,
};
