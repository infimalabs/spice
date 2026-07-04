const { withServePage } = require("./serve_playwright_harness");

// Mosaic live text rule: per-tick text (relative ages) must sit in a
// fixed-width single-line slot so a digit-count change on tick can never
// wrap a line, grow a card, or trigger a repack/ripple. Exercises the real
// production code path (setRelativeTimeText against a `.message-age` node)
// rather than waiting on the wall-clock interval.

function measureLiveTextSlotTick(label, beforeIso, afterIso) {
  let lane = Array.from(laneStates.values()).find((item) => !item.emptyTeam);
  if (!lane && targets.length) {
    addLane(targets[0].id);
    lane = laneStates.get(targets[0].id);
  }
  if (!lane) throw new Error("no lane available for live-text-slot smoke");

  const item = {
    ack_count: 0,
    ack_keys: [],
    index: 1,
    key: "live-text-slot-" + label,
    kind: "assistant",
    timestamp: beforeIso,
    threadId: lane.targetThreadId || "live-text-slot-thread",
    display_text: "live text slot smoke",
    text: "live text slot smoke",
  };
  // Fresh lattice per tick: this test wants one isolated card, not an
  // accumulating stream across ticks.
  delete lane.mosaicCards;
  lane.messagesEl.innerHTML = "";
  lane.knownMessages = [item];
  lane.knownMessageKeys = new Set([item.key]);
  lane.retainedMessageLimit = 1;
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  const node = lane.messagesEl.querySelector(
    'article[data-message-key="' + item.key + '"]',
  );
  if (!node) throw new Error(label + ": card was not rendered");
  const time = node.querySelector(".message-age");
  if (!time) throw new Error(label + ": card is missing .message-age");
  const card = lane.mosaicCards.find((candidate) => candidate.key === item.key);
  const slotStyle = getComputedStyle(time);
  const before = {
    text: time.textContent,
    height: node.getBoundingClientRect().height,
    rowSpan: card ? card.n : null,
    whiteSpace: slotStyle.whiteSpace,
    width: slotStyle.width,
  };
  time.dataset.relativeTimestamp = afterIso;
  setRelativeTimeText(time);
  const afterCard = lane.mosaicCards.find((candidate) => candidate.key === item.key);
  const after = {
    text: time.textContent,
    height: node.getBoundingClientRect().height,
    rowSpan: afterCard ? afterCard.n : null,
    width: getComputedStyle(time).width,
  };
  return { label, before, after };
}

function runLiveTextSlotSmokePage() {
  const now = Date.now();
  const isoMinutesAgo = (minutes) => new Date(now - minutes * 60000).toISOString();
  const isoDaysAgo = (days) =>
    new Date(now - days * 24 * 3600000 - 3600000).toISOString();
  return [
    measureLiveTextSlotTick("digit-growth", isoMinutesAgo(9), isoMinutesAgo(10)),
    measureLiveTextSlotTick("realistic-ceiling", isoDaysAgo(99), isoDaysAgo(100)),
  ];
}

function assertLiveTextSlotResult(result) {
  const nonWrappingWhiteSpace = new Set(["nowrap", "pre"]);
  for (const tick of result) {
    if (!nonWrappingWhiteSpace.has(tick.before.whiteSpace))
      throw new Error(
        tick.label + ": .message-age is not a single-line slot " +
          JSON.stringify(tick),
      );
    if (tick.before.width !== tick.after.width)
      throw new Error(
        tick.label + ": .message-age slot width changed across the tick " +
          JSON.stringify(tick),
      );
    if (tick.before.text === tick.after.text)
      throw new Error(
        tick.label + ": tick did not change the rendered text " +
          JSON.stringify(tick),
      );
    if (tick.before.height !== tick.after.height)
      throw new Error(
        tick.label + ": card height changed across the tick " + JSON.stringify(tick),
      );
    if (tick.before.rowSpan !== tick.after.rowSpan)
      throw new Error(
        tick.label + ": row span changed across the tick " + JSON.stringify(tick),
      );
  }
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-live-text-slot-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 900 } },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof renderMessagesIfChanged === "function" &&
          typeof setRelativeTimeText === "function" &&
          Array.isArray(targets) &&
          targets.length > 0,
        { timeout: 10000 },
      );
      await page.addScriptTag({
        content: [measureLiveTextSlotTick, runLiveTextSlotSmokePage]
          .map((helper) => helper.toString())
          .join("\n"),
      });
      const result = await page.evaluate(runLiveTextSlotSmokePage);
      assertLiveTextSlotResult(result);
      return { result, url: server.url };
    },
  );
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
