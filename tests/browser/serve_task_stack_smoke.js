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
  const host = document.querySelector(".messages");
  if (!host) throw new Error("message host unavailable");
  host.querySelectorAll("[data-task-stack-smoke]").forEach((node) => node.remove());
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
  taskArticle.dataset.taskStackSmoke = "task";
  imageArticle.dataset.taskStackSmoke = "image";
  host.prepend(imageArticle);
  host.prepend(taskArticle);
  await taskStackSmokeImagesReady(imageArticle);
  await new Promise((resolve) => requestAnimationFrame(resolve));
  await new Promise((resolve) => requestAnimationFrame(resolve));
  const stack = taskArticle.querySelector(".task-directive-stack");
  const imageStack = imageArticle.querySelector(".message-image-stack");
  const image = imageArticle.querySelector(".message-image img");
  const cards = Array.from(
    taskArticle.querySelectorAll(".task-directive-stack .task-directive-quote"),
  ).map((card) => taskStackSmokeRect(card));
  return {
    cardWidths: cards.map((card) => card.width),
    firstRowCount: cards.filter(
      (card) => Math.abs(card.top - cards[0].top) < 2,
    ).length,
    imageStackDisplay: getComputedStyle(imageStack).display,
    imageWidth: taskStackSmokeRect(image).width,
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
  if (measurement.stackDisplay !== "flex")
    throw new Error("task stack is not flex: " + JSON.stringify(measurement));
  if (measurement.imageStackDisplay !== "flex")
    throw new Error("image stack control is not flex: " + JSON.stringify(measurement));
  const imageTileWidth = Math.max(measurement.imageWidth, 156);
  for (const width of measurement.cardWidths) {
    if (width + 2 < imageTileWidth * 2)
      throw new Error("task card below 2x image width: " + JSON.stringify(measurement));
    if (width - 2 > imageTileWidth * 3)
      throw new Error("task card above 3x image width: " + JSON.stringify(measurement));
  }
  if (!options.wraps && measurement.firstRowCount < 2)
    throw new Error("wide task cards did not share a row: " + JSON.stringify(measurement));
  if (options.wraps && measurement.rowCount < 2)
    throw new Error("narrow task cards did not wrap: " + JSON.stringify(measurement));
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
