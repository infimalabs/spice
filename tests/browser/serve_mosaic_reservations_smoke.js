const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const MOSAIC_RESERVATIONS_IMAGE_HEIGHT_PX = 140;
const MOSAIC_RESERVATIONS_IMAGE_LARGE_HEIGHT_PX = 252;

function mosaicReservationsResolveLane() {
  return resolveIsolatedLane("mosaic-reservations-smoke-team");
}

function mosaicReservationsRows(type, rootFontSizePx, gap, module) {
  const priorPx = mosaicReservationPriorPx(type, rootFontSizePx);
  return priorPx === null ? null : mosaicRowsFor(priorPx, gap, module);
}

function mosaicReservationsBuildAckItem(key, ackKey) {
  return {
    ack_count: 1,
    ack_keys: [ackKey],
    ack_segments: [{ keys: [ackKey], html: "<p>reply body</p>" }],
    display_html: "<p>reply body</p>",
    display_text: "reply body",
    index: 1,
    key,
    kind: "assistant",
    text: "reply body",
    timestamp: new Date().toISOString(),
  };
}

// Drives a single message through the real mosaic render path
// (renderMessagesIfChanged) against a fresh lattice, rather than calling
// renderMessage/packMessageStream directly -- the legacy DOM harness these
// measurements used before the mosaic-demolition sweep no longer exists.
function mosaicReservationsRenderSingle(lane, item) {
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
  if (!node) throw new Error(item.key + ": card was not rendered");
  return node;
}

function mosaicReservationsMeasurePendingAck() {
  const lane = mosaicReservationsResolveLane();
  lane.ackContextByKey.clear();
  lane.missingAckContextKeys.clear();
  const item = mosaicReservationsBuildAckItem("pending-ack-card", "ack-key-1");
  const node = mosaicReservationsRenderSingle(lane, item);
  const skeleton = node.querySelector(".ack-quote--pending");
  const rootFontSizePx = mosaicRootFontSizePx();
  const geometry = lane.mosaicGeometry;
  const reservedRows = mosaicReservationsRows(
    "ack",
    rootFontSizePx,
    geometry.gap,
    geometry.M,
  );
  const card = lane.mosaicCards.find((candidate) => candidate.key === item.key);
  return {
    articleReserve: node.dataset.mosaicReservationType,
    expectedReserve: mosaicReservationPriorPx("ack", rootFontSizePx),
    reservedRows,
    rowSpan: card ? card.n : null,
    skeletonKey: skeleton ? skeleton.dataset.ackContextKey : "",
    skeletonReserve: skeleton
      ? Number.parseFloat(skeleton.style.getPropertyValue("--ack-quote-reserve"))
      : 0,
  };
}

function mosaicReservationsMeasureResolvedAck() {
  const lane = mosaicReservationsResolveLane();
  lane.ackContextByKey.set("ack-key-2", {
    attachments: [],
    html: "<p>quoted context</p>",
    key: "ack-key-2",
    priority: "",
    text: "quoted context",
  });
  const item = mosaicReservationsBuildAckItem("resolved-ack-card", "ack-key-2");
  const node = mosaicReservationsRenderSingle(lane, item);
  const quote = node.querySelector(".ack-quote");
  return {
    articleReserve: node.dataset.mosaicReservationType || "",
    quoteText: quote ? quote.textContent.trim() : "",
  };
}

function mosaicReservationsMeasureWetFrozen() {
  const wet = [
    { b: 0, creationIndex: 0, frozen: true, n: 2, span: 12, t: 0 },
    { b: 30, creationIndex: 1, frozen: false, n: 4, span: 4, t: 8 },
  ];
  const wetResolved = mosaicWetReplay(wet, 12, 2);
  const frozen = [
    { b: 10, creationIndex: 0, frozen: true, n: 3, span: 4, t: 0 },
    { b: 5, creationIndex: 1, frozen: true, n: 2, span: 4, t: 0 },
  ];
  const padded = mosaicResolveFrozenResize(frozen, 0, 2);
  const rippled = mosaicResolveFrozenResize(frozen, 0, 5);
  return {
    paddedStable: padded === frozen,
    rippledBelowB: rippled[1].b,
    rippledGrowerRows: rippled[0].n,
    wetPlacedB: wetResolved.find((card) => card.creationIndex === 1).b,
  };
}

function mosaicReservationsBuildImageItem(key, imageOnly) {
  const html =
    '<p class="message-image-stack"><a class="message-image" href="data:image/gif;base64,R0lGODlhAQABAAAAACw="><img alt="fixture" src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></a></p>';
  return {
    ack_count: 0,
    ack_keys: [],
    display_html: html,
    display_text: "image",
    image_only: imageOnly,
    index: 2,
    key,
    kind: "assistant",
    text: "image",
    timestamp: new Date().toISOString(),
  };
}

function mosaicReservationsMeasureImages() {
  const lane = mosaicReservationsResolveLane();
  const regularItem = mosaicReservationsBuildImageItem("regular-image-card", false);
  const largeItem = mosaicReservationsBuildImageItem("large-image-card", true);
  delete lane.mosaicCards;
  lane.messagesEl.innerHTML = "";
  lane.knownMessages = [largeItem, regularItem];
  lane.knownMessageKeys = new Set([largeItem.key, regularItem.key]);
  lane.retainedMessageLimit = 2;
  lane.renderedMessageFingerprint = "";
  renderMessagesIfChanged(lane);
  const regular = lane.messagesEl.querySelector(
    'article[data-message-key="' + regularItem.key + '"]',
  );
  const large = lane.messagesEl.querySelector(
    'article[data-message-key="' + largeItem.key + '"]',
  );
  if (!regular || !large) throw new Error("image reservation cards were not rendered");
  const regularImg = regular.querySelector(".message-image img");
  const largeImg = large.querySelector(".message-image img");
  return {
    largeHeight: Number.parseFloat(getComputedStyle(largeImg).height),
    largeReserve: large.dataset.mosaicReservationType,
    regularHeight: Number.parseFloat(getComputedStyle(regularImg).height),
    regularReserve: regular.dataset.mosaicReservationType,
  };
}

function assertMosaicReservations(result) {
  const pending = result.pending;
  if (pending.articleReserve !== "ack")
    throw new Error(
      "pending ack card did not use ack reservation " + JSON.stringify(pending),
    );
  if (pending.skeletonKey !== "ack-key-1")
    throw new Error(
      "pending skeleton did not keep the ack key " + JSON.stringify(pending),
    );
  if (Math.abs(pending.skeletonReserve - pending.expectedReserve) > 0.01)
    throw new Error(
      "pending skeleton reserve did not match prior " + JSON.stringify(pending),
    );
  if (pending.rowSpan < pending.reservedRows)
    throw new Error(
      "pending ack row span is below reserved rows " + JSON.stringify(pending),
    );

  const resolved = result.resolved;
  if (resolved.quoteText !== "quoted context")
    throw new Error(
      "resolved ack quote did not replace skeleton " + JSON.stringify(resolved),
    );

  const wetFrozen = result.wetFrozen;
  if (wetFrozen.wetPlacedB !== 2)
    throw new Error(
      "wet resolution did not replay against frozen floor " +
        JSON.stringify(wetFrozen),
    );
  if (!wetFrozen.paddedStable)
    throw new Error(
      "frozen shrink did not pad in place " + JSON.stringify(wetFrozen),
    );
  if (wetFrozen.rippledGrowerRows !== 5 || wetFrozen.rippledBelowB !== 3)
    throw new Error(
      "frozen growth did not ripple below card " + JSON.stringify(wetFrozen),
    );

  const images = result.images;
  if (
    images.regularReserve !== "image" ||
    images.regularHeight !== MOSAIC_RESERVATIONS_IMAGE_HEIGHT_PX
  )
    throw new Error(
      "regular image did not use the fixed image cap " + JSON.stringify(images),
    );
  if (
    images.largeReserve !== "imageLarge" ||
    images.largeHeight !== MOSAIC_RESERVATIONS_IMAGE_LARGE_HEIGHT_PX
  )
    throw new Error(
      "image-only card did not use the large fixed image cap " +
        JSON.stringify(images),
    );
}

async function installMosaicReservationsHelpers(page) {
  await page.addScriptTag({
    content: [
      mosaicReservationsResolveLane,
      mosaicReservationsRows,
      mosaicReservationsRenderSingle,
      mosaicReservationsBuildAckItem,
      mosaicReservationsMeasurePendingAck,
      mosaicReservationsMeasureResolvedAck,
      mosaicReservationsMeasureWetFrozen,
      mosaicReservationsBuildImageItem,
      mosaicReservationsMeasureImages,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

async function waitForMosaicReservationsGlobals(page) {
  await installIsolatedLaneFixture(page, {
    globals: [
      "renderMessagesIfChanged",
      "mosaicReservationPriorPx",
      "mosaicRowsFor",
      "mosaicWetReplay",
      "mosaicResolveFrozenResize",
    ],
  });
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-mosaic-reservations-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 900 } },
    },
    async ({ page, server }) => {
      await waitForMosaicReservationsGlobals(page);
      await installMosaicReservationsHelpers(page);
      const result = await page.evaluate(() => ({
        pending: mosaicReservationsMeasurePendingAck(),
        resolved: mosaicReservationsMeasureResolvedAck(),
        wetFrozen: mosaicReservationsMeasureWetFrozen(),
        images: mosaicReservationsMeasureImages(),
      }));
      assertMosaicReservations(result);
      return { ...result, url: server.url };
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
