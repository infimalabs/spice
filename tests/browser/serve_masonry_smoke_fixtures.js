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
  const items = masonrySmokeItems(config);
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
  packMessageStream(lane);
  lane.knownMessages = items.slice();
  lane.knownMessageKeys = new Set(items.map((item) => item.key));
  lane.newestMessageKey = items.length ? items[0].key : "";
  lane.oldestMessageKey = items.length ? items[items.length - 1].key : "";
  lane.renderedMessageFingerprint = messageRenderFingerprint(lane, items);
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
  const noopRenderAudit = config.captureOnly
    ? { historySyncCount: 0, observerSyncCount: 0, packCount: 0, teamSyncCount: 0 }
    : await masonrySmokeNoopRenderAudit(lane);
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
  const segmentSlotPressure = masonrySmokeSegmentSlotPressure(segmentFanOut);
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
    noopRenderAudit,
    repackAudit,
    keepLanesVisible: Boolean(config.keepLanesVisible),
    rootWidthActive: lane.element.classList.contains("lane--message-pack-root"),
    segmentFanOut,
    segmentOpeningSlots: segmentFanOut
      .map((fan) => fan[0]?.slot || 0)
      .filter(Boolean),
    segmentSlotPressure,
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

function masonrySmokeSegmentSlotPressure(segmentFanOut) {
  const occupiedTrackCounts = new Array(masonryExpectedGridTrackCount).fill(0);
  for (const fan of segmentFanOut) {
    for (const entry of fan) {
      for (
        let slot = entry.slot;
        slot < entry.slot + entry.slotSpan &&
        slot <= masonryExpectedGridTrackCount;
        slot += 1
      ) {
        occupiedTrackCounts[slot - 1] += 1;
      }
    }
  }
  const maxOccupiedTrackCount = Math.max(...occupiedTrackCounts);
  const minOccupiedTrackCount = Math.min(...occupiedTrackCounts);
  return {
    maxOccupiedTrackCount,
    minOccupiedTrackCount,
    occupiedTrackCounts,
    spread: maxOccupiedTrackCount - minOccupiedTrackCount,
  };
}

// Newer live arrivals enter ahead of the retained DOM in the same shape as the
// app's newest-first render path. Existing cards must keep their columns while
// the operator is reading; only the new cards need fresh placement.

const masonrySmokeFixtureHelpers = [
  renderMasonryFixture,
  renderMasonryTeamFixture,
  masonryTeamTargetPayload,
  masonryTeamItems,
  masonrySmokeLane,
  masonrySmokeUseSingleLane,
  masonrySmokeShowAllLanes,
  masonrySmokeItems,
  masonrySmokeImageHtml,
  masonrySmokeImagesReady,
  masonrySmokeBodyHtml,
  masonrySmokeMeasurement,
  masonrySmokeVisibleLaneCount,
  masonrySmokeRowAudit,
  masonrySmokeColumnSpan,
  masonrySmokeGridTrackCount,
  masonrySmokeRuleAudits,
  masonryTeamMeasurement,
  masonryTeamForcedSpanRecovery,
  masonrySmokeBarrierBounds,
  masonrySmokeColumnAudit,
  masonrySmokeSegmentFanOut,
  masonrySmokeSegmentSlotPressure,
];

module.exports = {
  renderMasonryFixture,
  renderMasonryTeamFixture,
  masonryTeamTargetPayload,
  masonryTeamItems,
  masonrySmokeLane,
  masonrySmokeUseSingleLane,
  masonrySmokeShowAllLanes,
  masonrySmokeItems,
  masonrySmokeImageHtml,
  masonrySmokeImagesReady,
  masonrySmokeBodyHtml,
  masonrySmokeMeasurement,
  masonrySmokeVisibleLaneCount,
  masonrySmokeRowAudit,
  masonrySmokeColumnSpan,
  masonrySmokeGridTrackCount,
  masonrySmokeRuleAudits,
  masonryTeamMeasurement,
  masonryTeamForcedSpanRecovery,
  masonrySmokeBarrierBounds,
  masonrySmokeColumnAudit,
  masonrySmokeSegmentFanOut,
  masonrySmokeSegmentSlotPressure,
  masonrySmokeFixtureHelpers,
};
