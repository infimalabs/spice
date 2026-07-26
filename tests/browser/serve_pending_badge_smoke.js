const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const LARGE_MESSAGE_COUNT = 5000;
const MAX_COMPOSER_CLEAR_MS = 250;
const INITIAL_PENDING_VERSION = 10;
const SEND_PENDING_VERSION = 11;
const ACK_PENDING_VERSION = 13;
const LARGE_MESSAGE_BASE_TS = 1700000000000;
const PENDING_KEY = "pending-smoke-key";

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-pending-badge-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await waitForPendingSmokePage(page);
      await installPendingSmokeHelpers(page);
      await page.evaluate(setupPendingSmokePage, {
        largeMessageCount: LARGE_MESSAGE_COUNT,
        initialVersion: INITIAL_PENDING_VERSION,
        pendingKey: PENDING_KEY,
        sendVersion: SEND_PENDING_VERSION,
        timestampBase: LARGE_MESSAGE_BASE_TS,
      });
      const send = await page.evaluate(runPendingSmokeSubmissionPage);
      const ack = await page.evaluate(applyPendingAckSmokePage, {
        ackVersion: ACK_PENDING_VERSION,
      });
      await page.evaluate(cleanupPendingSmokePage);
      const result = { send, ack };
      assertPendingSmoke(result);
      return { ...result, url: server.url };
    },
  );
}

async function waitForPendingSmokePage(page) {
  await installIsolatedLaneFixture(page, {
    globals: [
      "submitLaneForm",
      "handleLiveBusMessage",
      "lanePendingDisplayCount",
      "renderLaneViewShell",
    ],
  });
}

async function installPendingSmokeHelpers(page) {
  await page.addScriptTag({
    content: [
      pendingSmokeLane,
      pendingSmokeTextarea,
      pendingSmokeBadgeText,
      pendingSmokeChrome,
      pendingSmokePayload,
      pendingSmokeSubmission,
      pendingSmokeLiveBusRequest,
      largePendingSmokeMessages,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

function setupPendingSmokePage(config) {
  const lane = pendingSmokeLane();
  activateIsolatedLaneWatch(lane, "pending-badge-smoke");
  const textarea = pendingSmokeTextarea(lane);
  const messages = largePendingSmokeMessages(
    config.largeMessageCount,
    lane.targetThreadId || "thread-smoke",
    config.timestampBase,
  );
  lane.knownMessages = messages;
  lane.knownMessageKeys = new Set(messages.map((message) => message.key));
  lane.oldestMessageKey = messages[0].key;
  lane.newestMessageKey = messages[messages.length - 1].key;
  laneStore.applyLaneChrome(
    pendingSmokeChrome(lane.targetId, 0, [], config.initialVersion),
  );
  lane.lastRenderedStatusLine = pendingSmokePayload(
    lane.targetId,
    0,
    [],
    "rev-initial",
    config.initialVersion,
  ).statusLine;
  lane.latestPayload = { statusLine: lane.lastRenderedStatusLine };
  renderLaneViewShell(laneGroupHost(lane));
  const originalLiveBusRequest = liveBusRequest;
  const smoke = {
    calls: [],
    lane,
    originalLiveBusRequest,
    textarea,
  };
  liveBusRequest = (type, fields = {}) =>
    pendingSmokeLiveBusRequest(smoke, config, type, fields);
  window.__spicePendingSmoke = smoke;
}

function pendingSmokeLiveBusRequest(smoke, config, type, fields) {
  smoke.calls.push({
    type,
    targetId: fields.targetId || "",
    text: ((fields.payload || {}).text || "").trim(),
  });
  if (type === "lane.send")
    return Promise.resolve({
      result: {
        ok: true,
        key: config.pendingKey,
        requestText: (fields.payload || {}).text || "",
        chrome: pendingSmokeChrome(
          smoke.lane.targetId,
          1,
          [config.pendingKey],
          config.sendVersion,
        ),
        submission: pendingSmokeSubmission(config.pendingKey),
        agentEnsure: { ok: true, threadId: smoke.lane.targetThreadId || "" },
      },
    });
  return smoke.originalLiveBusRequest(type, fields);
}

function pendingSmokeSubmission(key) {
  const at = new Date().toISOString();
  return {
    key,
    stage: "accepted",
    disposition: "",
    stages: { accepted: { at, source: "inbox-write", evidence: key } },
    durationsMs: {},
  };
}

async function runPendingSmokeSubmissionPage() {
  const smoke = window.__spicePendingSmoke;
  const { lane, textarea } = smoke;
  textarea.value = "fast pending smoke " + Date.now();
  textarea.dispatchEvent(new Event("input", { bubbles: true }));
  const startedAt = performance.now();
  lane.formEl.dispatchEvent(
    new Event("submit", { bubbles: true, cancelable: true }),
  );
  await Promise.resolve();
  await Promise.resolve();
  return {
    badgeAfterSend: pendingSmokeBadgeText(lane),
    clearMs: performance.now() - startedAt,
    composerTextAfterSend: textarea.value,
    largeMessageCount: lane.knownMessages.length,
    pendingAfterSend: lanePendingDisplayCount(lane),
    placeholderAfterSend: textarea.placeholder,
    refreshCalls: smoke.calls.filter((call) => call.type === "lane.refresh").length,
    sendCalls: smoke.calls.filter((call) => call.type === "lane.send").length,
    submittedText: smoke.calls.find((call) => call.type === "lane.send").text,
  };
}

async function applyPendingAckSmokePage(config) {
  const smoke = window.__spicePendingSmoke;
  const { lane } = smoke;
  const refreshCallsBeforeAck = smoke.calls.filter(
    (call) => call.type === "lane.refresh",
  ).length;
  await handleLiveBusMessage(
    JSON.stringify({
      payload: pendingSmokePayload(
        lane.targetId,
        0,
        [],
        "rev-ack",
        config.ackVersion,
      ),
      source: "watch",
      subscriptionGeneration: lane.liveBusSubscriptionGeneration,
      targetId: lane.targetId,
      type: "lane.pending",
    }),
  );
  await Promise.resolve();
  return {
    badgeAfterAck: pendingSmokeBadgeText(lane),
    latestPayloadPending: lane.latestPayload.statusLine.pendingInboxCount,
    pendingAfterAck: lanePendingDisplayCount(lane),
    placeholderAfterAck: smoke.textarea.placeholder,
    refreshCallsAfterAck: smoke.calls.filter((call) => call.type === "lane.refresh")
      .length,
    refreshCallsBeforeAck,
    statusPending: lane.lastRenderedStatusLine.pendingInboxCount,
  };
}

function cleanupPendingSmokePage() {
  const smoke = window.__spicePendingSmoke;
  if (!smoke) return;
  liveBusRequest = smoke.originalLiveBusRequest;
  delete window.__spicePendingSmoke;
}

function pendingSmokeLane() {
  const lane = resolveIsolatedLane("pending-badge-smoke-team");
  syncComposerShards(laneGroupHost(lane), laneGroupMemberLanes(laneGroupHost(lane)));
  return lane;
}

function pendingSmokeTextarea(lane) {
  const textarea =
    lane.shardTextareas.get(lane.targetId) || lane.element.querySelector("textarea");
  if (!textarea) throw new Error("no composer textarea available");
  return textarea;
}

function pendingSmokeBadgeText(lane) {
  const badge = lane.element.querySelector(
    '[data-lane-view-button="compose"] [data-lane-view-badge]',
  );
  return badge && !badge.hidden ? badge.textContent : "";
}

function pendingSmokeChrome(targetId, count, keys, version) {
  return {
    targetId,
    pendingInbox: {
      authority: "inbox",
      order: { epoch: "", revision: version },
      value: { count, label: String(count), keys },
    },
  };
}

function pendingSmokePayload(targetId, count, keys, revision, version) {
  return {
    chrome: pendingSmokeChrome(targetId, count, keys, version),
    statusLine: {
      pendingInboxCount: count,
      pendingInboxKeys: keys,
      pendingInboxRevision: revision,
      pendingInboxVersion: version,
    },
  };
}

function largePendingSmokeMessages(count, threadId, timestampBase) {
  return Array.from({ length: count }, (_value, index) => ({
    ack_count: 0,
    ack_keys: [],
    display_html: "<p>message " + index + "</p>",
    display_text: "message " + index,
    index,
    key: "large-message-" + index,
    kind: "assistant",
    threadId,
    timestamp: new Date(timestampBase + index).toISOString(),
  }));
}

function assertPendingSmoke(result) {
  if (result.send.largeMessageCount !== LARGE_MESSAGE_COUNT)
    throw new Error("large message cache was not installed");
  if (result.send.sendCalls !== 1)
    throw new Error("expected one lane.send call");
  if (result.send.refreshCalls !== 0)
    throw new Error("unexpected lane.refresh call after send");
  if (!result.send.submittedText.includes("fast pending smoke"))
    throw new Error("submitted text was not sent");
  if (result.send.composerTextAfterSend !== "")
    throw new Error("composer text did not clear after send");
  if (result.send.clearMs > MAX_COMPOSER_CLEAR_MS)
    throw new Error("composer clear took " + result.send.clearMs + "ms");
  if (result.send.pendingAfterSend !== 1 || result.send.badgeAfterSend !== "1")
    throw new Error("pending badge did not show submitted inbox");
  if (!result.send.placeholderAfterSend.includes("\n1 pending"))
    throw new Error("composer placeholder did not show submitted inbox");
  if (result.ack.pendingAfterAck !== 0 || result.ack.badgeAfterAck !== "")
    throw new Error("pending badge did not clear after lane.pending ack");
  if (!result.ack.placeholderAfterAck.includes("\n0 pending"))
    throw new Error("composer placeholder did not clear after lane.pending ack");
  if (result.ack.statusPending !== 0 || result.ack.latestPayloadPending !== 0)
    throw new Error(
      "lane.pending ack did not update cached status payload: " +
        JSON.stringify(result.ack),
    );
  if (result.ack.refreshCallsAfterAck !== result.ack.refreshCallsBeforeAck)
    throw new Error("lane.pending ack triggered an unexpected refresh");
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
