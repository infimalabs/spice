const { withServePage } = require("./serve_playwright_harness");

const MOSAIC_RESERVATIONS_IMAGE_HEIGHT_PX = 140;
const MOSAIC_RESERVATIONS_IMAGE_LARGE_HEIGHT_PX = 252;

function mosaicReservationsResolveLane() {
  let lane = Array.from(laneStates.values()).find((item) => !item.emptyTeam);
  if (!lane && targets.length) {
    addLane(targets[0].id);
    lane = laneStates.get(targets[0].id);
  }
  if (!lane) throw new Error("no lane available for mosaic reservations smoke");
  return lane;
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

function mosaicReservationsMeasurePendingAck() {
  const lane = mosaicReservationsResolveLane();
  const host = lane.messagesEl;
  host.innerHTML = "";
  lane.ackContextByKey.clear();
  lane.missingAckContextKeys.clear();
  const item = mosaicReservationsBuildAckItem("pending-ack-card", "ack-key-1");
  const node = renderMessage(lane, item);
  host.append(node);
  packMessageStream(lane);
  const skeleton = node.querySelector(".ack-quote--pending");
  const rootFontSizePx = mosaicRootFontSizePx();
  const style = getComputedStyle(host);
  const rowGap = Number.parseFloat(style.rowGap) || 0;
  const rowStride = (Number.parseFloat(style.gridAutoRows) || 0) + rowGap;
  const reservedRows = mosaicReservationRows(
    "ack",
    rootFontSizePx,
    rowGap,
    rowStride,
  );
  return {
    articleReserve: node.dataset.mosaicReservationType,
    expectedReserve: mosaicReservationPriorPx("ack", rootFontSizePx),
    reservedRows,
    rowSpan: Number.parseInt(
      node.style.getPropertyValue("--message-pack-row-span"),
      10,
    ),
    skeletonKey: skeleton ? skeleton.dataset.ackContextKey : "",
    skeletonReserve: skeleton
      ? Number.parseFloat(skeleton.style.getPropertyValue("--ack-quote-reserve"))
      : 0,
  };
}

function mosaicReservationsMeasureResolvedAck() {
  const lane = mosaicReservationsResolveLane();
  const host = lane.messagesEl;
  host.innerHTML = "";
  lane.ackContextByKey.set("ack-key-2", {
    attachments: [],
    html: "<p>quoted context</p>",
    key: "ack-key-2",
    priority: "",
    text: "quoted context",
  });
  const item = mosaicReservationsBuildAckItem("resolved-ack-card", "ack-key-2");
  const node = renderMessage(lane, item);
  host.append(node);
  packMessageStream(lane);
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
  const host = lane.messagesEl;
  host.innerHTML = "";
  const regular = renderMessage(
    lane,
    mosaicReservationsBuildImageItem("regular-image-card", false),
  );
  const large = renderMessage(
    lane,
    mosaicReservationsBuildImageItem("large-image-card", true),
  );
  host.append(regular, large);
  packMessageStream(lane);
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
  await page.waitForFunction(
    () =>
      typeof renderMessage === "function" &&
      typeof packMessageStream === "function" &&
      typeof mosaicReservationPriorPx === "function" &&
      typeof mosaicReservationRows === "function" &&
      typeof mosaicWetReplay === "function" &&
      typeof mosaicResolveFrozenResize === "function" &&
      Array.isArray(targets) &&
      targets.length > 0,
    { timeout: 10000 },
  );
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
