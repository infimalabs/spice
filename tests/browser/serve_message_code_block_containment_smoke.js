const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const LONG_COMMAND =
  "spice task add --acceptance=" + "literal-value-".repeat(24) + "unsafe-end";
const FENCE = "`".repeat(3);
const RAW_MESSAGE =
  "Before the fence.\n\n" +
  FENCE +
  "text\n" +
  LONG_COMMAND +
  "\n" +
  FENCE +
  "\n\nAfter the fence.";
const RENDERED_HTML =
  "<p>Before the fence.</p><pre><code>" +
  LONG_COMMAND +
  "</code></pre><p>After the fence.</p>";
const CONTAINMENT_TOLERANCE_PX = 1;
const SUBMISSION_WAIT_ATTEMPTS = 100;

function codeBlockRect(element) {
  const rect = element.getBoundingClientRect();
  return {
    bottom: rect.bottom,
    left: rect.left,
    right: rect.right,
    top: rect.top,
    width: rect.width,
  };
}

function codeBlockSuccessfulSendResult(lane, fields, config) {
  const at = new Date().toISOString();
  return {
    agentEnsure: { ok: true, threadId: lane.targetThreadId || "" },
    key: config.ackKey,
    ok: true,
    requestHtml: config.renderedHtml,
    requestText: fields.payload.text,
    submission: {
      disposition: "",
      durationsMs: {},
      key: config.ackKey,
      stage: "accepted",
      stages: {
        accepted: { at, evidence: config.ackKey, source: "inbox-write" },
      },
    },
  };
}

function codeBlockBuildItems(config) {
  const timestamp = "2027-01-01T00:00:00.000000Z";
  return [
    {
      ack_count: 1,
      ack_keys: [config.ackKey],
      ack_segments: [{ keys: [config.ackKey], html: "<p>Reply body.</p>" }],
      display_html: "",
      display_text: "",
      index: 1,
      key: "code-block-ack-message",
      kind: "assistant",
      text: "",
      timestamp,
    },
    {
      ack_count: 0,
      ack_keys: [],
      ack_segments: [],
      display_html: config.renderedHtml,
      display_text: config.rawMessage,
      index: 2,
      key: "code-block-plain-message",
      kind: "assistant",
      text: config.rawMessage,
      timestamp: "2027-01-01T00:00:01.000000Z",
    },
  ];
}

function codeBlockSnapshot(article, content, pre) {
  return {
    article: codeBlockRect(article),
    articleClientWidth: article.clientWidth,
    articleScrollWidth: article.scrollWidth,
    codeText: pre.querySelector("code").textContent,
    content: codeBlockRect(content),
    overflowX: getComputedStyle(pre).overflowX,
    paragraphs: Array.from(content.querySelectorAll(":scope > p")).map(
      (paragraph) => paragraph.textContent,
    ),
    pre: codeBlockRect(pre),
    preClientWidth: pre.clientWidth,
    preScrollWidth: pre.scrollWidth,
  };
}

function codeBlockNextPaint() {
  return new Promise((resolve) =>
    requestAnimationFrame(() => requestAnimationFrame(resolve)),
  );
}

async function runCodeBlockContainment(config) {
  const lane = resolveIsolatedLane("message-code-block-containment-smoke-team");
  syncComposerShards(lane, [lane]);
  const textarea = lane.shardTextareas.get(lane.targetId);
  if (!textarea) throw new Error("code-block smoke needs a composer textarea");

  const originalLiveBusRequest = liveBusRequest;
  let submittedPayload = null;
  liveBusRequest = async (type, fields = {}) => {
    if (type !== "lane.send") return originalLiveBusRequest(type, fields);
    submittedPayload = fields.payload;
    return {
      result: codeBlockSuccessfulSendResult(lane, fields, config),
    };
  };

  try {
    textarea.value = config.rawMessage;
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    lane.formEl.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );
    for (
      let attempt = 0;
      attempt < SUBMISSION_WAIT_ATTEMPTS && !submittedPayload;
      attempt += 1
    )
      await new Promise((resolve) => setTimeout(resolve, 10));
    if (!submittedPayload) throw new Error("composer did not dispatch lane.send");
    for (
      let attempt = 0;
      attempt < SUBMISSION_WAIT_ATTEMPTS && lane.pendingSubmissionCount;
      attempt += 1
    )
      await new Promise((resolve) => setTimeout(resolve, 10));

    const items = codeBlockBuildItems(config);
    const ackArticle = renderMessage(lane, items[0]);
    const plainArticle = renderMessage(lane, items[1]);
    for (const article of [ackArticle, plainArticle]) {
      article.style.alignSelf = "start";
      article.style.height = "auto";
      article.style.maxWidth = "100%";
      article.style.minHeight = "";
      article.style.width = "32rem";
    }
    lane.messagesEl.replaceChildren(
      ackArticle,
      plainArticle,
      lane.historySentinelEl,
    );
    await codeBlockNextPaint();

    const ackQuote = ackArticle.querySelector(".ack-quote");
    const ackPre = ackQuote && ackQuote.querySelector("pre");
    const plainBody = plainArticle.querySelector(".message-body");
    const plainPre = plainBody && plainBody.querySelector("pre");
    if (!ackQuote || !ackPre || !plainBody || !plainPre)
      throw new Error("rendered code-block fixtures are incomplete");

    return {
      ack: codeBlockSnapshot(ackArticle, ackQuote, ackPre),
      composerCleared: textarea.value === "",
      messagesClientWidth: lane.messagesEl.clientWidth,
      messagesScrollWidth: lane.messagesEl.scrollWidth,
      plain: codeBlockSnapshot(plainArticle, plainBody, plainPre),
      submittedText: submittedPayload.text,
    };
  } finally {
    liveBusRequest = originalLiveBusRequest;
  }
}

function assertCodeBlockSnapshot(snapshot, config, label) {
  const fail = (message) => {
    throw new Error(label + " " + message + ": " + JSON.stringify(snapshot));
  };
  const tolerance = config.tolerancePx;
  if (snapshot.codeText !== config.longCommand)
    fail("changed the literal fenced content");
  if (
    snapshot.paragraphs.length !== 2 ||
    snapshot.paragraphs[0] !== "Before the fence." ||
    snapshot.paragraphs[1] !== "After the fence."
  )
    fail("lost the prose surrounding the fence");
  if (snapshot.content.left < snapshot.article.left - tolerance)
    fail("content escaped the article on the left");
  if (snapshot.content.right > snapshot.article.right + tolerance)
    fail("content escaped the article on the right");
  if (snapshot.pre.left < snapshot.content.left - tolerance)
    fail("pre escaped its content container on the left");
  if (snapshot.pre.right > snapshot.content.right + tolerance)
    fail("pre escaped its content container on the right");
  if (snapshot.articleScrollWidth > snapshot.articleClientWidth + tolerance)
    fail("article inherited horizontal overflow from fenced content");
  if (snapshot.preScrollWidth <= snapshot.preClientWidth + tolerance)
    fail("fixture did not create an internally scrollable long code line");
  if (!new Set(["auto", "scroll"]).has(snapshot.overflowX))
    fail("pre does not own horizontal overflow");
}

function assertCodeBlockContainment(result, config) {
  if (result.submittedText !== config.rawMessage)
    throw new Error("composer submit changed literal triple-backtick text");
  if (!result.composerCleared)
    throw new Error("accepted fenced-code submission did not clear composer");
  if (result.messagesScrollWidth > result.messagesClientWidth + config.tolerancePx)
    throw new Error(
      "fenced content widened the message lane: " + JSON.stringify(result),
    );
  assertCodeBlockSnapshot(result.ack, config, "ack quote");
  assertCodeBlockSnapshot(result.plain, config, "message body");
}

async function run() {
  return withServePage(
    {
      contextOptions: { viewport: { height: 800, width: 1280 } },
      path: "/?smoke=message-code-block-containment-" + Date.now(),
    },
    async ({ page, server }) => {
      await installIsolatedLaneFixture(page, {
        globals: ["renderMessage", "syncComposerShards"],
      });
      await page.addScriptTag({
        content: [
          codeBlockRect,
          codeBlockSuccessfulSendResult,
          codeBlockBuildItems,
          codeBlockSnapshot,
          codeBlockNextPaint,
          "const SUBMISSION_WAIT_ATTEMPTS = " + SUBMISSION_WAIT_ATTEMPTS + ";",
        ]
          .map((helper) => helper.toString())
          .join("\n"),
      });
      const config = {
        ackKey: "code-block-submit-key",
        longCommand: LONG_COMMAND,
        rawMessage: RAW_MESSAGE,
        renderedHtml: RENDERED_HTML,
        tolerancePx: CONTAINMENT_TOLERANCE_PX,
      };
      const result = await page.evaluate(runCodeBlockContainment, config);
      assertCodeBlockContainment(result, config);
      return { ...result, url: server.url };
    },
  );
}

if (require.main === module) {
  run()
    .then((result) => {
      console.log(
        "serve_message_code_block_containment_smoke ok " + JSON.stringify(result),
      );
    })
    .catch((error) => {
      console.error(error);
      process.exitCode = 1;
    });
}

module.exports = { run };
