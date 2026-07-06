const { withServePage } = require("./serve_playwright_harness");

// UI-1kBNL1Rs: a command-path reply card (kind "reply") must render in a live
// lane exactly like a prose ACK -- as a mosaic card carrying an ACK chip -- so
// the operator sees a `spice agent reply` even though no assistant prose was
// emitted. Synthetic injection (per serve_mosaic_ack_resolution_smoke.js): seed
// one reply-card message and assert the rendered card shows its ACK badge.

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-reply-card-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page }) => {
      await page.waitForSelector(".lane", { timeout: 10000 });
      await page.waitForFunction(
        () =>
          typeof renderMessagesIfChanged === "function" &&
          typeof laneStates !== "undefined" &&
          typeof addLane === "function" &&
          Array.isArray(targets),
        { timeout: 10000 },
      );
      return page.evaluate(async () => {
        let lane = Array.from(laneStates.values()).find((item) => !item.emptyTeam);
        if (!lane && targets.length) {
          addLane(targets[0].id);
          lane = laneStates.get(targets[0].id);
        }
        if (!lane) throw new Error("no lane available for reply-card smoke");

        const timestamp = "2027-01-01T00:00:00.000000Z";
        lane.knownMessages = [
          {
            ack_count: 1,
            ack_keys: ["reply-smoke-key"],
            ack_segments: [{ keys: ["reply-smoke-key"], html: "" }],
            index: 0,
            key: timestamp + "#reply-card:0",
            kind: "reply",
            timestamp,
            display_html: "",
            display_text: "",
            text: "ACK reply-smoke-key: did the thing",
          },
        ];
        lane.renderedMessageFingerprint = "";
        renderMessagesIfChanged(lane);
        await new Promise((r) => setTimeout(r, 200));

        const root = lane.messagesEl || document;
        const card = root.querySelector('[data-mosaic-card]');
        const badgeLabels = Array.from(root.querySelectorAll(".badge .badge-label")).map(
          (el) => el.textContent,
        );
        return {
          rendered: Boolean(card),
          badgeLabels,
          hasAckBadge: badgeLabels.includes("ACK"),
        };
      });
    },
  );
}

function assertResult(result) {
  if (!result.rendered)
    throw new Error("reply card did not render as a mosaic card: " + JSON.stringify(result));
  if (!result.hasAckBadge)
    throw new Error("reply card rendered without its ACK chip: " + JSON.stringify(result));
}

if (require.main === module) {
  run()
    .then((result) => {
      assertResult(result);
      console.log(JSON.stringify(result, null, 2));
      console.log("reply card rendered with its ACK chip");
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run, assertResult };
