const { withServePage } = require("./serve_playwright_harness");

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
  await new Promise((resolve) => requestAnimationFrame(resolve));
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const stack = taskArticle.querySelector(".task-directive-stack");
  const imageStack = imageArticle.querySelector(".message-image-stack");
  const image = imageArticle.querySelector(".message-image img");
  const imageArticleRect = taskStackSmokeRect(imageArticle);
  const cards = Array.from(
    taskArticle.querySelectorAll(".task-directive-stack .task-directive-quote"),
  ).map((card) => taskStackSmokeRect(card));
  const messageCards = Array.from(
    host.querySelectorAll('[data-task-stack-smoke="message-card"]'),
  ).map((card) => taskStackSmokeRect(card));
  const firstRowMessageCards = messageCards.filter(
    (card) => Math.abs(card.top - messageCards[0].top) < 2,
  );
  const hostStyle = getComputedStyle(host);
  const hostRect = taskStackSmokeRect(host);
  const hostInlinePadding =
    Number.parseFloat(hostStyle.paddingLeft) +
    Number.parseFloat(hostStyle.paddingRight);
  const firstRowLeft = Math.min(...firstRowMessageCards.map((card) => card.left));
  const firstRowRight = Math.max(
    ...firstRowMessageCards.map((card) => card.left + card.width),
  );
  const rootFontSize =
    Number.parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
  return {
    cardWidths: cards.map((card) => card.width),
    firstRowCount: cards.filter(
      (card) => Math.abs(card.top - cards[0].top) < 2,
    ).length,
    imageStackDisplay: getComputedStyle(imageStack).display,
    imageArticleWidth: imageArticleRect.width,
    imageWidth: taskStackSmokeRect(image).width,
    messageCardDisplays: Array.from(
      host.querySelectorAll('[data-task-stack-smoke="message-card"]'),
    ).map((card) => getComputedStyle(card).display),
    messageCardFirstRowCount: firstRowMessageCards.length,
    messageCardFirstRowFillWidth: firstRowRight - firstRowLeft,
    messageCardFirstRowHeights: firstRowMessageCards.map((card) => card.height),
    messageCardFloor: rootFontSize * 20,
    messageCardHostInnerWidth: hostRect.width - hostInlinePadding,
    messageCardLimit: rootFontSize * 30,
    messageCardRowCount: new Set(
      messageCards.map((card) => Math.round(card.top)),
    ).size,
    messageCardWidths: messageCards.map((card) => card.width),
    rowCount: new Set(cards.map((card) => Math.round(card.top))).size,
    stackDisplay: getComputedStyle(stack).display,
    viewportWidth: config.width,
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
      display_html: "<p>Short card.</p>",
      display_text: "Short card.",
      key: "message-card-short-" + width,
      kind: "assistant",
      text: "Short card.",
    },
    {
      display_html:
        "<p>Longer card body with enough text to define the row height.</p>" +
        "<p>It should make neighboring cards stretch to the same row height.</p>",
      display_text:
        "Longer card body with enough text to define the row height. " +
        "It should make neighboring cards stretch to the same row height.",
      key: "message-card-final-" + width,
      kind: "final",
      text: "Longer card body.",
    },
    {
      ack_count: 1,
      display_html: "<p>Acked card.</p>",
      display_text: "Acked card.",
      key: "message-card-acked-" + width,
      kind: "assistant",
      text: "Acked card.",
    },
  ].map((item, index) =>
    renderMessage(lane, {
      ack_count: item.ack_count || 0,
      ack_keys: [],
      display_html: item.display_html,
      display_text: item.display_text,
      index: 10 + index,
      key: item.key,
      kind: item.kind,
      text: item.text,
      timestamp,
    }),
  );
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
  if (measurement.stackDisplay !== "grid")
    throw new Error("task stack is not grid: " + JSON.stringify(measurement));
  if (measurement.imageStackDisplay !== "flex")
    throw new Error("image stack control is not flex: " + JSON.stringify(measurement));
  if (measurement.imageArticleWidth < measurement.messageCardHostInnerWidth * 0.92)
    throw new Error("image card did not fill row: " + JSON.stringify(measurement));
  const imageTileWidth = Math.max(measurement.imageWidth, 156);
  const minCardWidth = Math.max(measurement.messageCardFloor, imageTileWidth * 2);
  const maxCardWidth = measurement.messageCardLimit;
  for (const width of measurement.cardWidths) {
    if (width + 2 < minCardWidth)
      throw new Error("task card below card min width: " + JSON.stringify(measurement));
    if (width - 2 > maxCardWidth)
      throw new Error("task card above card max width: " + JSON.stringify(measurement));
  }
  if (!measurement.messageCardDisplays.every((display) => display === "flex"))
    throw new Error("message cards are not flex: " + JSON.stringify(measurement));
  if (!options.wraps && measurement.firstRowCount < 2)
    throw new Error("wide task cards did not share a row: " + JSON.stringify(measurement));
  if (options.wraps && measurement.rowCount < 2)
    throw new Error("narrow task cards did not wrap: " + JSON.stringify(measurement));
  if (!options.wraps && measurement.messageCardFirstRowCount < 2)
    throw new Error("wide message cards did not share a row: " + JSON.stringify(measurement));
  if (options.wraps && measurement.messageCardRowCount < 2)
    throw new Error("narrow message cards did not wrap: " + JSON.stringify(measurement));
  if (!options.wraps) {
    const fillRatio =
      measurement.messageCardFirstRowFillWidth /
      measurement.messageCardHostInnerWidth;
    if (fillRatio < 0.92)
      throw new Error("wide message cards did not fill the row: " + JSON.stringify(measurement));
  }
  if (options.wraps) {
    for (const width of measurement.messageCardWidths) {
      if (width < measurement.messageCardHostInnerWidth * 0.92)
        throw new Error("wrapped message card did not fill row: " + JSON.stringify(measurement));
    }
  }
  if (!options.wraps) {
    const heights = measurement.messageCardFirstRowHeights;
    const spread = Math.max(...heights) - Math.min(...heights);
    if (spread > 2)
      throw new Error("message cards did not stretch to row height: " + JSON.stringify(measurement));
  }
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
