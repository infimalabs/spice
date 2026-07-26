const os = require("os");
const path = require("path");
const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

// MESSAGE-1kBdGKyL: a NACK (reasoned refusal) must render in the mosaic exactly
// like an ACK -- same quote-of-steering + reasoned body + footer chip -- and
// differ only in color: the warn palette instead of the ack accent. This seeds
// three messages (pure ACK, pure NACK, mixed ACK+NACK), pre-resolves their
// steering contexts so the quotes render, then asserts the polarity classes /
// badges and captures a screenshot for the visual sign-off the operator asked
// for.

const SCREENSHOT_PATH = path.join(os.tmpdir(), "spice-nack-render.png");

// Pre-resolved steering contexts so the quote blocks render as content rather
// than pending skeletons.
const SEED_CONTEXTS = [
  ["ack-key", "Please add the doctor rollup."],
  ["nack-key", "Do not weaken the quality gate."],
  ["mixed-ack", "Curate the notes."],
  ["mixed-nack", "Also delete the changelog file."],
];

// One assistant message per polarity: pure ACK, pure NACK, and the combined
// ACK+NACK card the operator wants to prefer the warn tint.
const SEED_MESSAGES = [
  {
    index: 0,
    key: "2027-01-01T00:00:00.000000Z#ack",
    kind: "assistant",
    timestamp: "2027-01-01T00:00:00.000000Z",
    text: "ACK ack-key: done",
    display_text: "Done.",
    display_html: "<p>Done — doctor rollup landed.</p>",
    ack_count: 1,
    ack_keys: ["ack-key"],
    nack_count: 0,
    nack_keys: [],
    preamble_html: "",
    ack_segments: [
      {
        keys: ["ack-key"],
        disposition: "acked",
        html: "<p>Done — doctor rollup landed.</p>",
      },
    ],
  },
  {
    index: 1,
    key: "2027-01-01T00:00:01.000000Z#nack",
    kind: "assistant",
    timestamp: "2027-01-01T00:00:01.000000Z",
    text: "NACK nack-key: cannot",
    display_text: "Cannot.",
    display_html: "<p>Refused — that would weaken the gate.</p>",
    ack_count: 0,
    ack_keys: ["nack-key"],
    nack_count: 1,
    nack_keys: ["nack-key"],
    preamble_html: "",
    ack_segments: [
      {
        keys: ["nack-key"],
        disposition: "refused",
        html: "<p>Refused — that would weaken the gate.</p>",
      },
    ],
  },
  {
    index: 2,
    key: "2027-01-01T00:00:02.000000Z#mixed",
    kind: "assistant",
    timestamp: "2027-01-01T00:00:02.000000Z",
    text: "ACK mixed-ack: ok\nNACK mixed-nack: no",
    display_text: "Mixed.",
    display_html: "<p>Curated.</p>",
    ack_count: 1,
    ack_keys: ["mixed-ack", "mixed-nack"],
    nack_count: 1,
    nack_keys: ["mixed-nack"],
    preamble_html: "",
    ack_segments: [
      {
        keys: ["mixed-ack"],
        disposition: "acked",
        html: "<p>Curated the notes.</p>",
      },
      {
        keys: ["mixed-nack"],
        disposition: "refused",
        html: "<p>Kept the changelog file.</p>",
      },
    ],
  },
  // MESSAGE-1kH4VHSj: a refusal the archival authority left pending answered no
  // key, so it claims none and takes neither polarity. It still carries what the
  // message captured, so the body renders -- muted and quoteless, never dressed
  // as the acknowledgment it was not.
  {
    index: 3,
    key: "2027-01-01T00:00:03.000000Z#withheld",
    kind: "assistant",
    timestamp: "2027-01-01T00:00:03.000000Z",
    text: "NACK withheld-key:\nTASK serve.messages: follow up",
    display_text: "Follow up.",
    display_html: "<p>Follow up on the gate.</p>",
    ack_count: 0,
    ack_keys: [],
    nack_count: 0,
    nack_keys: [],
    preamble_html: "",
    ack_segments: [
      {
        keys: [],
        disposition: "withheld",
        html: "<p>Follow up on the gate.</p>",
      },
    ],
  },
];

// Runs in the browser: seed the lane with the polarity fixtures, render, then
// read back the polarity classes, badges, and resolved --warn quote accent.
async function applySeedAndMeasure(seed) {
  const lane = resolveIsolatedLane("nack-render-smoke-team");
  for (const [key, text] of seed.contexts) {
    lane.ackContextByKey.set(key, {
      key,
      text,
      html: "<p>" + text + "</p>",
      priority: "",
      attachments: [],
    });
  }
  lane.knownMessages = seed.messages;
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  await new Promise((r) => setTimeout(r, 250));

  const root = lane.messagesEl || document;
  const cardFor = (fragment) =>
    Array.from(root.querySelectorAll("article")).find((el) =>
      (el.dataset.messageKey || "").includes(fragment),
    );
  const badgeLabels = (el) =>
    Array.from(el.querySelectorAll(".badge .badge-label")).map(
      (b) => b.textContent,
    );
  const ackCard = cardFor("#ack");
  const nackCard = cardFor("#nack");
  const mixedCard = cardFor("#mixed");
  const withheldCard = cardFor("#withheld");
  const rootStyle = getComputedStyle(document.documentElement);
  const warn = rootStyle.getPropertyValue("--warn").trim();
  const muted = rootStyle.getPropertyValue("--muted").trim();
  const nackQuoteAccent = nackCard
    ? getComputedStyle(nackCard.querySelector(".ack-quote"))
        .getPropertyValue("--quote-accent")
        .trim()
    : "";
  const withheldBody = withheldCard
    ? withheldCard.querySelector(".message-body--withheld")
    : null;
  const withheldBodyAccent = withheldBody
    ? getComputedStyle(withheldBody).getPropertyValue("--quote-accent").trim()
    : "";
  return {
    ackHasAckedClass: Boolean(ackCard && ackCard.classList.contains("acked")),
    ackNotRefused: Boolean(ackCard && !ackCard.classList.contains("refused")),
    ackBadges: ackCard ? badgeLabels(ackCard) : [],
    nackHasRefusedClass: Boolean(
      nackCard && nackCard.classList.contains("refused"),
    ),
    nackNotAcked: Boolean(nackCard && !nackCard.classList.contains("acked")),
    nackBadges: nackCard ? badgeLabels(nackCard) : [],
    nackHasRefusedQuotes: Boolean(
      nackCard && nackCard.querySelector(".ack-quotes--refused"),
    ),
    nackHasRefusedBody: Boolean(
      nackCard && nackCard.querySelector(".message-body--refused"),
    ),
    nackQuoteAccentIsWarn: Boolean(warn) && nackQuoteAccent === warn,
    mixedBadges: mixedCard ? badgeLabels(mixedCard) : [],
    mixedHasBoth: Boolean(
      mixedCard &&
        mixedCard.classList.contains("acked") &&
        mixedCard.classList.contains("refused"),
    ),
    mixedRefusedSegments: mixedCard
      ? mixedCard.querySelectorAll(".ack-quotes--refused").length
      : 0,
    withheldHasMutedBody: Boolean(withheldBody),
    withheldBodyAccentIsMuted: Boolean(muted) && withheldBodyAccent === muted,
    withheldTakesNeitherPolarity: Boolean(
      withheldCard &&
        !withheldCard.classList.contains("acked") &&
        !withheldCard.classList.contains("refused"),
    ),
    withheldQuoteBlocks: withheldCard
      ? withheldCard.querySelectorAll(".ack-quotes").length
      : -1,
    withheldBadges: withheldCard ? badgeLabels(withheldCard) : [],
  };
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-nack-render",
      contextOptions: { viewport: { width: 1280, height: 900 } },
    },
    async ({ page }) => {
      await installIsolatedLaneFixture(page, {
        globals: ["renderMessagesIfChanged"],
      });
      const result = await page.evaluate(applySeedAndMeasure, {
        contexts: SEED_CONTEXTS,
        messages: SEED_MESSAGES,
      });
      await page.screenshot({ path: SCREENSHOT_PATH, fullPage: true });
      return { ...result, screenshotPath: SCREENSHOT_PATH };
    },
  );
}

function assertResult(result) {
  const checks = [
    ["ack card keeps its acked tint", result.ackHasAckedClass],
    ["ack card is not marked refused", result.ackNotRefused],
    ["ack card shows the ACK chip", result.ackBadges.includes("ACK")],
    ["ack card shows no NACK chip", !result.ackBadges.includes("NACK")],
    ["nack card takes the refused (warn) tint", result.nackHasRefusedClass],
    ["nack card is not marked acked", result.nackNotAcked],
    ["nack card shows the NACK chip", result.nackBadges.includes("NACK")],
    ["nack card shows no ACK chip", !result.nackBadges.includes("ACK")],
    ["nack quotes carry the refused class", result.nackHasRefusedQuotes],
    ["nack body carries the refused class", result.nackHasRefusedBody],
    ["nack quote accent resolves to --warn", result.nackQuoteAccentIsWarn],
    ["mixed card carries both polarities", result.mixedHasBoth],
    [
      "mixed card shows ACK and NACK chips",
      result.mixedBadges.includes("ACK") && result.mixedBadges.includes("NACK"),
    ],
    [
      "mixed card has exactly one refused quote block",
      result.mixedRefusedSegments === 1,
    ],
    ["withheld body carries the withheld class", result.withheldHasMutedBody],
    [
      "withheld body accent resolves to --muted",
      result.withheldBodyAccentIsMuted,
    ],
    [
      "withheld card takes neither polarity tint",
      result.withheldTakesNeitherPolarity,
    ],
    ["withheld card quotes no steering", result.withheldQuoteBlocks === 0],
    [
      "withheld card shows neither chip",
      !result.withheldBadges.includes("ACK") &&
        !result.withheldBadges.includes("NACK"),
    ],
  ];
  const failed = checks.filter(([, ok]) => !ok).map(([label]) => label);
  if (failed.length)
    throw new Error(
      "nack render smoke failed:\n  " +
        failed.join("\n  ") +
        "\n" +
        JSON.stringify(result, null, 2),
    );
}

if (require.main === module) {
  run()
    .then((result) => {
      assertResult(result);
      console.log(JSON.stringify(result, null, 2));
      console.log(
        "nack rendered warn-colored, ACK-identical; screenshot: " +
          result.screenshotPath,
      );
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run, assertResult };
