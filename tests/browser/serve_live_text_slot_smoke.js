const { withServePage } = require("./serve_playwright_harness");

// Mosaic §9 live text rule: per-tick text (relative ages) must sit in a
// fixed-width single-line slot so a digit-count change on tick can never
// wrap a line, grow a card, or trigger a repack/ripple. Exercises the real
// production code path (setRelativeTimeText against a `.message-age` node)
// rather than waiting on the wall-clock interval.
async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-live-text-slot-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 900 } },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof renderMessage === "function" &&
          typeof packMessageStream === "function" &&
          typeof setRelativeTimeText === "function" &&
          Array.isArray(targets) &&
          targets.length > 0,
        { timeout: 10000 },
      );
      const result = await page.evaluate(() => {
        let lane = Array.from(laneStates.values()).find(
          (item) => !item.emptyTeam,
        );
        if (!lane && targets.length) {
          addLane(targets[0].id);
          lane = laneStates.get(targets[0].id);
        }
        if (!lane) throw new Error("no lane available for live-text-slot smoke");
        const host = lane.messagesEl;
        host.innerHTML = "";

        const now = Date.now();
        const isoMinutesAgo = (minutes) =>
          new Date(now - minutes * 60000).toISOString();
        const isoDaysAgo = (days) =>
          new Date(now - days * 24 * 3600000 - 3600000).toISOString();

        function measureTick(label, beforeIso, afterIso) {
          const item = {
            ack_count: 0,
            ack_keys: [],
            index: 1,
            key: "live-text-slot-" + label,
            kind: "assistant",
            timestamp: beforeIso,
            display_text: "live text slot smoke",
            text: "live text slot smoke",
          };
          host.innerHTML = "";
          const node = renderMessage(lane, item);
          host.append(node);
          packMessageStream(lane);
          const time = node.querySelector(".message-age");
          if (!time) throw new Error(label + ": card is missing .message-age");
          const slotStyle = getComputedStyle(time);
          const before = {
            text: time.textContent,
            height: node.getBoundingClientRect().height,
            rowSpan: node.style.getPropertyValue("--message-pack-row-span"),
            whiteSpace: slotStyle.whiteSpace,
            width: slotStyle.width,
          };
          time.dataset.relativeTimestamp = afterIso;
          setRelativeTimeText(time);
          const after = {
            text: time.textContent,
            height: node.getBoundingClientRect().height,
            rowSpan: node.style.getPropertyValue("--message-pack-row-span"),
            width: getComputedStyle(time).width,
          };
          return { label, before, after };
        }

        return [
          measureTick("digit-growth", isoMinutesAgo(9), isoMinutesAgo(10)),
          measureTick("realistic-ceiling", isoDaysAgo(99), isoDaysAgo(100)),
        ];
      });

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
            tick.label + ": card height changed across the tick " +
              JSON.stringify(tick),
          );
        if (tick.before.rowSpan !== tick.after.rowSpan)
          throw new Error(
            tick.label + ": row span changed across the tick " +
              JSON.stringify(tick),
          );
      }

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
