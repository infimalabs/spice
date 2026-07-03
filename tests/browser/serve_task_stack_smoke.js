const { withServePage } = require("./serve_playwright_harness");

const unstretchedMessageHeightSpreadFloorPx = 16;

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-task-stack-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof renderMessage === "function" &&
          Array.isArray(targets) &&
          targets.length > 0,
        { timeout: 10000 },
      );
      await installTaskStackSmokeHelpers(page);
      const wide = await measureTaskStack(page, 1920);
      const narrow = await measureTaskStack(page, 520);
      assertTaskStackMeasurement(wide, { wraps: false });
      assertTaskStackMeasurement(narrow, { wraps: true });
      return { narrow, url: server.url, wide };
    },
  );
}

async function installTaskStackSmokeHelpers(page) {
  await page.addScriptTag({
    content: [
      renderTaskStackFixture,
      taskStackSmokeLane,
      taskStackSmokeUseSingleLane,
      taskStackSmokeMessageArticles,
      taskStackSmokeMeasurement,
      taskStackSmokeHorizontalBounds,
      taskStackSmokeAnchorStability,
      taskStackSmokeColumnStability,
      taskStackSmokeColumnRecovery,
      taskStackSmokeTaskHtml,
      taskStackSmokeImageHtml,
      taskStackSmokeImagesReady,
      taskStackSmokeRect,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

async function measureTaskStack(page, width) {
  await page.setViewportSize({ width, height: 720 });
  return page.evaluate(renderTaskStackFixture, { width });
}

async function renderTaskStackFixture(config) {
  const lane = taskStackSmokeLane();
  taskStackSmokeUseSingleLane(lane);
  const host = lane.messagesEl || document.querySelector(".messages");
  if (!host) throw new Error("message host unavailable");
  host.querySelectorAll("[data-task-stack-smoke]").forEach((node) => node.remove());
  const messageArticles = taskStackSmokeMessageArticles(lane, config.width);
  const taskArticle = renderMessage(lane, {
    ack_count: 0,
    ack_keys: [],
    display_html: taskStackSmokeTaskHtml(),
    display_text: "task stack smoke",
    index: 1,
    key: "task-stack-smoke-" + config.width,
    kind: "assistant",
    text: "task stack smoke",
    timestamp: new Date().toISOString(),
  });
  const imageArticle = renderMessage(lane, {
    ack_count: 0,
    ack_keys: [],
    display_html: taskStackSmokeImageHtml(),
    display_text: "image stack smoke",
    image_only: true,
    index: 2,
    key: "image-stack-smoke-" + config.width,
    kind: "assistant",
    text: "image stack smoke",
    timestamp: new Date().toISOString(),
  });
  for (const article of messageArticles) {
    article.dataset.taskStackSmoke = "message-card";
  }
  taskArticle.dataset.taskStackSmoke = "task";
  imageArticle.dataset.taskStackSmoke = "image";
  host.prepend(...messageArticles, taskArticle, imageArticle);
  await taskStackSmokeImagesReady(imageArticle);
  packMessageStream(lane);
  syncMessagePackObserver(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  await new Promise((resolve) => requestAnimationFrame(resolve));
  return taskStackSmokeMeasurement(lane, host, taskArticle, imageArticle, config);
}

async function taskStackSmokeMeasurement(lane, host, taskArticle, imageArticle, config) {
  const stack = taskArticle.querySelector(".task-directive-stack");
  const imageStack = imageArticle.querySelector(".message-image-stack");
  const image = imageArticle.querySelector(".message-image img");
  const imageArticleRect = taskStackSmokeRect(imageArticle);
  const messageCardNodes = Array.from(
    host.querySelectorAll('[data-task-stack-smoke="message-card"]'),
  );
  const cards = Array.from(
    taskArticle.querySelectorAll(".task-directive-stack .task-directive-quote"),
  ).map((card) => taskStackSmokeRect(card));
  const messageCards = messageCardNodes.map((card) => taskStackSmokeRect(card));
  const firstRowMessageCards = messageCards.filter(
    (card) => Math.abs(card.top - messageCards[0].top) < 2,
  );
  const tallCard = messageCardNodes.find(
    (card) => card.dataset.taskStackSmokeSize === "tall",
  );
  const backfilledCard = messageCardNodes.find(
    (card) => card.dataset.taskStackSmokeSize === "backfill",
  );
  if (!tallCard || !backfilledCard) throw new Error("packing fixture missing cards");
  const tallCardRect = taskStackSmokeRect(tallCard);
  const backfilledCardRect = taskStackSmokeRect(backfilledCard);
  const anchorStability = await taskStackSmokeAnchorStability(
    lane,
    host,
    tallCard,
    backfilledCard,
  );
  const columnStability = await taskStackSmokeColumnStability(
    lane,
    messageCardNodes,
    messageCardNodes[0],
  );
  const columnRecovery = await taskStackSmokeColumnRecovery(
    lane,
    messageCardNodes,
  );
  const hostStyle = getComputedStyle(host);
  const hostRect = taskStackSmokeRect(host);
  const hostInlinePadding =
    Number.parseFloat(hostStyle.paddingLeft) +
    Number.parseFloat(hostStyle.paddingRight);
  const firstRowBounds = taskStackSmokeHorizontalBounds(firstRowMessageCards);
  const rootFontSize =
    Number.parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
  return {
    anchorStability,
    columnStability,
    columnRecovery,
    backfilledCardTop: backfilledCardRect.top,
    cardWidths: cards.map((card) => card.width),
    firstRowCount: cards.filter(
      (card) => Math.abs(card.top - cards[0].top) < 2,
    ).length,
    hostColumnGap: hostStyle.columnGap,
    hostDisplay: hostStyle.display,
    hostGridAutoFlow: hostStyle.gridAutoFlow,
    hostGridAutoRows: hostStyle.gridAutoRows,
    hostRowGap: hostStyle.rowGap,
    imageStackDisplay: getComputedStyle(imageStack).display,
    imageArticleWidth: imageArticleRect.width,
    imageWidth: taskStackSmokeRect(image).width,
    messageCardDisplays: messageCardNodes.map((card) => getComputedStyle(card).display),
    messageCardFirstRowCount: firstRowMessageCards.length,
    messageCardFirstRowFillWidth: firstRowBounds.right - firstRowBounds.left,
    messageCardFirstRowHeights: firstRowMessageCards.map((card) => card.height),
    messageCardFloor: rootFontSize * 20,
    messageCardHostInnerWidth: hostRect.width - hostInlinePadding,
    messageCardLimit: rootFontSize * 30,
    messageCardRowSpans: messageCardNodes.map((card) =>
      Number.parseInt(
        card.style.getPropertyValue("--message-pack-row-span") || "0",
        10,
      ),
    ),
    messageCardRowCount: new Set(
      messageCards.map((card) => Math.round(card.top)),
    ).size,
    messageCardTopSpread:
      Math.max(...messageCards.map((card) => card.top)) -
      Math.min(...messageCards.map((card) => card.top)),
    messageCardWidths: messageCards.map((card) => card.width),
    rowCount: new Set(cards.map((card) => Math.round(card.top))).size,
    stackDisplay: getComputedStyle(stack).display,
    tallCardBottom: tallCardRect.top + tallCardRect.height,
    viewportWidth: config.width,
  };
}

function taskStackSmokeHorizontalBounds(cards) {
  return {
    left: Math.min(...cards.map((card) => card.left)),
    right: Math.max(...cards.map((card) => card.left + card.width)),
  };
}

function taskStackSmokeLane() {
  let lane = Array.from(laneStates.values()).find((item) => !item.emptyTeam);
  if (!lane && targets.length) {
    addLane(targets[0].id);
    lane = laneStates.get(targets[0].id);
  }
  if (!lane) throw new Error("no lane available for task stack smoke");
  return lane;
}

function taskStackSmokeUseSingleLane(activeLane) {
  for (const lane of laneStates.values()) {
    lane.element.style.display = lane === activeLane ? "" : "none";
  }
}

function taskStackSmokeMessageArticles(lane, width) {
  const timestamp = new Date().toISOString();
  return [
    {
      size: "short",
      display_html: "<p>Short card.</p>",
      display_text: "Short card.",
      key: "message-card-short-" + width,
      kind: "assistant",
      text: "Short card.",
    },
    {
      size: "tall",
      display_html:
        "<p>Longer card body with enough text to create a tall packing column.</p>" +
        "<p>It should not stretch neighboring cards to the same height.</p>" +
        "<p>The row grid should let later short cards stack under shorter siblings.</p>" +
        "<p>That keeps the stream from leaving a blank bucket beside this card.</p>",
      display_text:
        "Longer card body with enough text to create a tall packing column. " +
        "It should not stretch neighboring cards to the same height.",
      key: "message-card-final-" + width,
      kind: "final",
      text: "Longer card body.",
    },
    {
      size: "short",
      ack_count: 1,
      display_html: "<p>Acked card.</p>",
      display_text: "Acked card.",
      key: "message-card-acked-" + width,
      kind: "assistant",
      text: "Acked card.",
    },
    {
      size: "short",
      display_html: "<p>Another compact card.</p>",
      display_text: "Another compact card.",
      key: "message-card-compact-" + width,
      kind: "assistant",
      text: "Another compact card.",
    },
    {
      size: "short",
      display_html: "<p>Small follow-up.</p>",
      display_text: "Small follow-up.",
      key: "message-card-follow-up-" + width,
      kind: "assistant",
      text: "Small follow-up.",
    },
    {
      size: "backfill",
      display_html: "<p>Backfill card.</p>",
      display_text: "Backfill card.",
      key: "message-card-backfill-" + width,
      kind: "assistant",
      text: "Backfill card.",
    },
  ].map((item, index) => {
    const article = renderMessage(lane, {
      ack_count: item.ack_count || 0,
      ack_keys: [],
      display_html: item.display_html,
      display_text: item.display_text,
      index: 10 + index,
      key: item.key,
      kind: item.kind,
      text: item.text,
      timestamp,
    });
    article.dataset.taskStackSmokeSize = item.size;
    return article;
  });
}

async function taskStackSmokeAnchorStability(lane, host, tallCard, anchorCard) {
  host.scrollTop = Math.max(0, anchorCard.offsetTop - 24);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const hostTop = taskStackSmokeRect(host).top;
  const before = taskStackSmokeRect(anchorCard).top - hostTop;
  const body = tallCard.querySelector(".message-body");
  const extra = document.createElement("p");
  extra.textContent =
    "Extra measured content forces a repack above the current reading anchor.";
  body.append(extra);
  scheduleMessageStreamPack(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const after = taskStackSmokeRect(anchorCard).top - taskStackSmokeRect(host).top;
  return {
    after,
    before,
    delta: Math.abs(after - before),
    scrollTop: host.scrollTop,
  };
}

async function taskStackSmokeColumnStability(lane, cards, growCard) {
  const columnsBefore = cards.map((card) => card.style.gridColumnStart);
  const leftsBefore = cards.map((card) => taskStackSmokeRect(card).left);
  const body = growCard.querySelector(".message-body");
  const extra = document.createElement("p");
  extra.textContent =
    "Column stability growth: this card grows, and no card may switch columns.";
  body.append(extra);
  scheduleMessageStreamPack(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  await new Promise((resolve) => requestAnimationFrame(resolve));
  return {
    columnsAfter: cards.map((card) => card.style.gridColumnStart),
    columnsBefore,
    leftsAfter: cards.map((card) => taskStackSmokeRect(card).left),
    leftsBefore,
  };
}

async function taskStackSmokeColumnRecovery(lane, cards) {
  // Reproduce the auto-fit death spiral: every card forced into column 1
  // collapses the remaining tracks, so a computed-style track count reads 1
  // and the packer never redistributes. Geometry-derived counting must
  // recover on the next pack.
  for (const card of cards) {
    delete card.dataset.messagePackColumn;
    delete card.dataset.messagePackSegment;
    card.style.gridColumnStart = "1";
  }
  await new Promise((resolve) => requestAnimationFrame(resolve));
  packMessageStream(lane);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const columns = cards.map((card) => card.style.gridColumnStart);
  return { distinctColumns: Array.from(new Set(columns)).length, columns };
}

function taskStackSmokeTaskHtml() {
  const cards = ["First", "Second", "Third"]
    .map(
      (title) =>
        '<blockquote class="task-directive-quote">' +
        '<div class="task-directive-kicker">Task capture</div>' +
        '<dl class="task-directive-properties">' +
        '<div class="task-directive-property">' +
        "<dt>title</dt>" +
        "<dd>" +
        title +
        " task stack smoke</dd>" +
        "</div>" +
        '<div class="task-directive-property">' +
        "<dt>project</dt><dd>serve.ui</dd>" +
        "</div>" +
        '<div class="task-directive-property">' +
        "<dt>acceptance</dt><dd>Stacks at measured card width</dd>" +
        "</div>" +
        "</dl>" +
        "</blockquote>",
    )
    .join("");
  return '<div class="task-directive-stack">' + cards + "</div>";
}

function taskStackSmokeImageHtml() {
  const svg = encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" width="156" height="96">' +
      '<rect width="156" height="96" fill="#8a4fbf"/>' +
      "</svg>",
  );
  const image =
    '<a class="message-image" href="#">' +
    '<img alt="image stack control" src="data:image/svg+xml,' +
    svg +
    '"></a>';
  return '<p class="message-image-stack">' + image + image + image + "</p>";
}

function taskStackSmokeImagesReady(article) {
  const images = Array.from(article.querySelectorAll("img"));
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

function taskStackSmokeRect(element) {
  const rect = element.getBoundingClientRect();
  return {
    height: rect.height,
    left: rect.left,
    top: rect.top,
    width: rect.width,
  };
}

function assertTaskStackMeasurement(measurement, options) {
  assertBaseTaskStackLayout(measurement);
  assertImageArticleSizing(measurement, options);
  assertTaskDirectiveCardWidths(measurement);
  assertMessageCardPacking(measurement, options);
  assertMessageAnchorStability(measurement, options);
  assertMessageColumnStability(measurement, options);
  assertMessageColumnRecovery(measurement, options);
}

function assertMessageColumnRecovery(measurement, options) {
  if (options.wraps) return;
  if (measurement.columnRecovery.distinctColumns < 2)
    throw new Error(
      "packer did not redistribute cards out of a collapsed single column: " +
        JSON.stringify(measurement),
    );
}

function assertMessageColumnStability(measurement, options) {
  const stability = measurement.columnStability;
  if (!options.wraps) {
    const pinned = stability.columnsBefore.every((column) => /^\d+$/.test(column));
    if (!pinned)
      throw new Error("wide message cards are not column-pinned: " + JSON.stringify(measurement));
  }
  for (let index = 0; index < stability.columnsBefore.length; index += 1) {
    if (stability.columnsBefore[index] !== stability.columnsAfter[index])
      throw new Error("card switched columns after mid-stream growth: " + JSON.stringify(measurement));
    if (Math.abs(stability.leftsBefore[index] - stability.leftsAfter[index]) > 1)
      throw new Error("card slid horizontally after mid-stream growth: " + JSON.stringify(measurement));
  }
}

function assertBaseTaskStackLayout(measurement) {
  if (measurement.hostDisplay !== "grid")
    throw new Error("message host is not grid: " + JSON.stringify(measurement));
  if (measurement.hostGridAutoFlow !== "row")
    throw new Error("message host is not stable row-packed: " + JSON.stringify(measurement));
  if (measurement.hostGridAutoRows !== "4px")
    throw new Error("message host row unit changed: " + JSON.stringify(measurement));
  if (measurement.hostRowGap !== "6px")
    throw new Error("message host row gap changed: " + JSON.stringify(measurement));
  if (measurement.hostColumnGap !== "9px")
    throw new Error("message host column gap changed: " + JSON.stringify(measurement));
  if (measurement.stackDisplay !== "grid")
    throw new Error("task stack is not grid: " + JSON.stringify(measurement));
  if (measurement.imageStackDisplay !== "flex")
    throw new Error("image stack control is not flex: " + JSON.stringify(measurement));
  if (!measurement.messageCardDisplays.every((display) => display === "flex"))
    throw new Error("message cards are not flex: " + JSON.stringify(measurement));
  if (!measurement.messageCardRowSpans.every((span) => span > 0))
    throw new Error("message cards were not measured into row spans: " + JSON.stringify(measurement));
}

function assertImageArticleSizing(measurement, options) {
  if (options.wraps) {
    if (measurement.imageArticleWidth < measurement.messageCardHostInnerWidth * 0.92)
      throw new Error("wrapped image card did not fill row: " + JSON.stringify(measurement));
    return;
  }
  if (measurement.imageArticleWidth > measurement.messageCardHostInnerWidth * 0.55)
    throw new Error("wide image card consumed too much row: " + JSON.stringify(measurement));
}

function assertTaskDirectiveCardWidths(measurement) {
  const imageTileWidth = Math.max(measurement.imageWidth, 156);
  const minCardWidth = Math.max(measurement.messageCardFloor, imageTileWidth * 2);
  const maxCardWidth = measurement.messageCardLimit;
  for (const width of measurement.cardWidths) {
    if (width + 2 < minCardWidth)
      throw new Error("task card below card min width: " + JSON.stringify(measurement));
    if (width - 2 > maxCardWidth)
      throw new Error("task card above card max width: " + JSON.stringify(measurement));
  }
}

function assertMessageCardPacking(measurement, options) {
  if (options.wraps) assertNarrowMessagePacking(measurement);
  else assertWideMessagePacking(measurement);
}

function assertWideMessagePacking(measurement) {
  if (measurement.firstRowCount < 2)
    throw new Error("wide task cards did not share a row: " + JSON.stringify(measurement));
  if (measurement.messageCardFirstRowCount < 2)
    throw new Error("wide message cards did not share a row: " + JSON.stringify(measurement));
  const fillRatio =
    measurement.messageCardFirstRowFillWidth / measurement.messageCardHostInnerWidth;
  if (fillRatio < 0.92)
    throw new Error("wide message cards did not fill the row: " + JSON.stringify(measurement));
  if (measurement.backfilledCardTop >= measurement.tallCardBottom - 2)
    throw new Error("stable row packing did not stack below short cards: " + JSON.stringify(measurement));
  if (measurement.messageCardTopSpread < 8)
    throw new Error("wide message cards did not create multiple packed tiers: " + JSON.stringify(measurement));
  const heights = measurement.messageCardFirstRowHeights;
  const spread = Math.max(...heights) - Math.min(...heights);
  if (spread < unstretchedMessageHeightSpreadFloorPx)
    throw new Error("message cards still look stretched to row height: " + JSON.stringify(measurement));
}

function assertNarrowMessagePacking(measurement) {
  if (measurement.rowCount < 2)
    throw new Error("narrow task cards did not wrap: " + JSON.stringify(measurement));
  if (measurement.messageCardRowCount < 2)
    throw new Error("narrow message cards did not wrap: " + JSON.stringify(measurement));
  for (const width of measurement.messageCardWidths) {
    if (width < measurement.messageCardHostInnerWidth * 0.92)
      throw new Error("wrapped message card did not fill row: " + JSON.stringify(measurement));
  }
}

function assertMessageAnchorStability(measurement, options) {
  if (measurement.anchorStability.delta > 2)
    throw new Error("message repack moved the reading anchor: " + JSON.stringify(measurement));
  if (options.wraps && measurement.anchorStability.scrollTop < 1)
    throw new Error("anchor stability fixture did not scroll: " + JSON.stringify(measurement));
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
