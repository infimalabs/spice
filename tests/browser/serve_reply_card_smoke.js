const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
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
      await installIsolatedLaneFixture(page, {
        globals: ["renderMessagesIfChanged"],
      });
      return page.evaluate(async () => {
        const lane = resolveIsolatedLane("reply-card-smoke-team");

        const timestamp = "2027-01-01T00:00:00.000000Z";
        const replyKey = "reply-smoke-key";
        const replyBody = "Did the thing without exposing the raw key";
        lane.knownMessages = [
          {
            ack_count: 1,
            ack_keys: [replyKey],
            ack_segments: [{ keys: [replyKey], html: "<p>" + replyBody + "</p>" }],
            index: 0,
            key: timestamp + "#reply-card:0",
            kind: "reply",
            timestamp,
            display_html: "<p>" + replyBody + "</p>",
            display_text: replyBody,
            text: "ACK " + replyKey + ": " + replyBody,
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
        const bodyText = card?.querySelector(".message-body")?.textContent || "";
        return {
          rendered: Boolean(card),
          badgeLabels,
          hasAckBadge: badgeLabels.includes("ACK"),
          bodyText,
          rawKeyVisibleInBody: bodyText.includes(replyKey),
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
  if (result.rawKeyVisibleInBody)
    throw new Error("reply card exposed its raw ACK key: " + JSON.stringify(result));
  if (result.bodyText !== "Did the thing without exposing the raw key")
    throw new Error("reply card body changed unexpectedly: " + JSON.stringify(result));
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
