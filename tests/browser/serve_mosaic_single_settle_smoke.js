const { withServePage } = require("./serve_playwright_harness");
const { installScript } = require("./payload_factory");

// Closer for the parallel-load battery (LIVEBUS-1kCD3y6J, UI-1kCD5TCJ,
// LIVEBUS-1kCD6H9x, UI-1kCDFJh3): end-to-end proof of the operator-visible
// outcome. A cold reload with several bound agents -- one fused team plus a
// solo lane -- comes solely from the server team snapshot (no browser-hint
// restore), rides one batched lanes.subscribe, and paints every lane's merged
// interleaved stream in ONE settled pass. The positive observable is the
// per-host mosaic full-replay counter (host.mosaicEventLog full-replay events):
// it reaches its value at the first settled paint and stays there across a
// quiet window, so the fused mosaic never reshuffles member-by-member after the
// fact.

const SS_FUSED_IDS = ["ssm-a", "ssm-b"];
const SS_SOLO_ID = "ssm-c";
const SS_MESSAGES_PER_LANE = 3;
const SS_ACK_KEY = "ss-ack-1";
const SS_SNAPSHOT_REVISION = 60;
const SS_SETTLE_MS = 120;
const SS_QUIET_MS = 160;

function ssTargetPayload(id, threadId, teamId, teamRevision) {
  return window.spicePayloads.targetPayload({
    id,
    threadId,
    teamId,
    teamRevision,
    pendingPrefix: "p-",
  });
}

function ssTeam(teamId, ids, configRevision) {
  // Cold-load teams carry the shared interface defaults inside config so the
  // settled paint provably comes from the server snapshot, never a
  // browser-hint restore.
  return window.spicePayloads.teamPayload(
    { teamId, memberIds: ids, revision: configRevision },
    { config: { speechMode: "speak", selectedView: "compose" } },
  );
}

// start/step interleave the members so the merged host stream only reads
// correctly when every member's messages land together (fused a: 0/2/4, fused
// b: 1/3/5, solo c: 0/1/2). ackKeyForFirst tags the newest message with an ack
// so the payload's ackContexts must hydrate on the same settled paint.
function ssLaneMessages(id, start, step, perLane, ackKeyForFirst) {
  return Array.from({ length: perLane }, (unused, n) => {
    const index = start + n * step;
    const message = {
      ack_count: 0,
      ack_keys: [],
      index,
      key: "m-" + id + "-" + index,
      kind: "assistant",
      threadId: id + "-th",
      timestamp: new Date(2027, 0, 1, 0, index, 0).toISOString(),
      display_html: "settle line " + id + " " + index,
      display_text: "settle line " + id + " " + index,
      text: "settle line " + id + " " + index,
    };
    if (n === 0 && ackKeyForFirst) {
      message.ack_count = 1;
      message.ack_keys = [ackKeyForFirst];
      message.ack_segments = [{ keys: [ackKeyForFirst], html: "" }];
    }
    return message;
  });
}

function ssAckContexts(ackKey) {
  return [
    {
      key: ackKey,
      found: true,
      text: "acked steering context",
      html: "<p>acked steering context</p>",
      priority: "",
      attachments: [],
    },
  ];
}

function ssStatusLine(id) {
  return {
    pendingInboxCount: 0,
    pendingInboxKeys: [],
    pendingInboxRevision: "p-" + id,
    pendingInboxVersion: 1,
  };
}

function ssSettle(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function ssSubscribeFrames(state) {
  return state.frames.filter((frame) => frame.type === "lanes.subscribe");
}

function ssFullReplayCount(host) {
  const events = (host && host.mosaicEventLog && host.mosaicEventLog.events) || [];
  return events.filter((event) => event.type === "full-replay").length;
}

function ssHostMessageBearingRenders(state, host) {
  return state.mosaicRenders.filter((render) => {
    return render.targetId === host.targetId && render.keys.length > 0;
  });
}

// Stub the transport before any lane mounts so every subscribe rides the one
// batched frame, and wrap the mosaic render so we can count message-bearing
// paints per host.
function ssInstallStubs(state, config) {
  // eslint-disable-next-line no-global-assign
  liveBusRequest = async (type, fields = {}) => {
    state.frames.push({ type, ...fields });
    if (type !== "lanes.subscribe") return { type: "bus.ok", payload: {} };
    return {
      type: "lanes.payload",
      lanes: (fields.entries || []).map((entry) => {
        return {
          targetId: entry.targetId,
          subscriptionGeneration: "single-settle:" + entry.targetId,
          watcherActive: true,
          watcherError: "",
          payload: state.payloadByTargetId[entry.targetId],
        };
      }),
    };
  };
  state.originalMosaicRender = mosaicRenderMessageStream;
  // eslint-disable-next-line no-global-assign
  mosaicRenderMessageStream = function (lane, items) {
    state.mosaicRenders.push({
      targetId: lane.targetId,
      keys: items.map((item) => item.key),
    });
    return state.originalMosaicRender(lane, items);
  };
}

function ssBuildPayloads(config) {
  const payloadByTargetId = {};
  payloadByTargetId[config.fusedIds[0]] = {
    messages: window.__ssLaneMessages(config.fusedIds[0], 0, 2, config.perLane, config.ackKey),
    ackContexts: window.__ssAckContexts(config.ackKey),
    statusLine: window.__ssStatusLine(config.fusedIds[0]),
  };
  payloadByTargetId[config.fusedIds[1]] = {
    messages: window.__ssLaneMessages(config.fusedIds[1], 1, 2, config.perLane, null),
    statusLine: window.__ssStatusLine(config.fusedIds[1]),
  };
  payloadByTargetId[config.soloId] = {
    messages: window.__ssLaneMessages(config.soloId, 0, 1, config.perLane, null),
    statusLine: window.__ssStatusLine(config.soloId),
  };
  return payloadByTargetId;
}

function ssExpectedKeys(id, start, step, perLane) {
  return window
    .__ssLaneMessages(id, start, step, perLane, null)
    .map((item) => item.key);
}

// Cold-load the whole board from a single server team snapshot (fused team of
// two members + a solo team of one), let the batched subscribe settle once,
// then measure the settled board and every host's full-replay count. A second
// quiet window with no new payloads must add zero full-replays.
async function ssMeasure(config) {
  const state = { frames: [], mosaicRenders: [], payloadByTargetId: {} };
  window.__ssInstallStubs(state, config);
  state.payloadByTargetId = window.__ssBuildPayloads(config);

  laneStore.replaceTargets([
    window.__ssTarget(config.fusedIds[0], config.fusedIds[0] + "-th", "team-fused", 1),
    window.__ssTarget(config.fusedIds[1], config.fusedIds[1] + "-th", "team-fused", 1),
    window.__ssTarget(config.soloId, config.soloId + "-th", "team-solo", 1),
  ]);

  applyTeamSnapshotPayload(
    window.spicePayloads.teamSnapshot({
      revision: config.snapshotRevision,
      teams: [
        window.__ssTeam("team-fused", config.fusedIds, 1),
        window.__ssTeam("team-solo", [config.soloId], 1),
      ],
    }),
    { force: true },
  );

  await window.__ssSettle(config.settleMs);

  const fusedHost = laneGroupHost(laneStore.laneForId(config.fusedIds[0]));
  const soloHost = laneGroupHost(laneStore.laneForId(config.soloId));
  const fusedExpectedKeys = window
    .__ssExpectedKeys(config.fusedIds[0], 0, 2, config.perLane)
    .concat(window.__ssExpectedKeys(config.fusedIds[1], 1, 2, config.perLane));
  const soloExpectedKeys = window.__ssExpectedKeys(config.soloId, 0, 1, config.perLane);
  const fusedCardKeys = fusedHost.mosaicCards.map((card) => card.key);
  const soloCardKeys = soloHost.mosaicCards.map((card) => card.key);

  const frames = window.__ssSubscribeFrames(state);
  const fusedReplayInitial = window.__ssFullReplayCount(fusedHost);
  const soloReplayInitial = window.__ssFullReplayCount(soloHost);

  await window.__ssSettle(config.quietMs);

  return {
    subscribeFrameCount: frames.length,
    subscribeFrameEntryIds: frames.length
      ? (frames[0].entries || []).map((entry) => entry.targetId).sort()
      : [],
    fusedHostTargetId: fusedHost.targetId,
    fusedExpectedCount: fusedExpectedKeys.length,
    fusedPresentCount: fusedExpectedKeys.filter((key) => fusedCardKeys.includes(key)).length,
    soloExpectedCount: soloExpectedKeys.length,
    soloPresentCount: soloExpectedKeys.filter((key) => soloCardKeys.includes(key)).length,
    fusedMessageRenderCount: window.__ssHostMessageBearingRenders(state, fusedHost).length,
    soloMessageRenderCount: window.__ssHostMessageBearingRenders(state, soloHost).length,
    ackContextState: fusedHost.ackContextByKey.has(config.ackKey)
      ? "resolved"
      : fusedHost.missingAckContextKeys.has(config.ackKey)
        ? "missing"
        : "pending",
    ackQuoteText:
      fusedHost.messagesEl.querySelector(".ack-quote")?.textContent.trim() || "",
    fusedReplayInitial,
    soloReplayInitial,
    fusedReplayFinal: window.__ssFullReplayCount(fusedHost),
    soloReplayFinal: window.__ssFullReplayCount(soloHost),
  };
}

const SS_PAGE_HELPERS = {
  __ssTarget: ssTargetPayload,
  __ssTeam: ssTeam,
  __ssLaneMessages: ssLaneMessages,
  __ssAckContexts: ssAckContexts,
  __ssStatusLine: ssStatusLine,
  __ssSettle: ssSettle,
  __ssSubscribeFrames: ssSubscribeFrames,
  __ssFullReplayCount: ssFullReplayCount,
  __ssHostMessageBearingRenders: ssHostMessageBearingRenders,
  __ssInstallStubs: ssInstallStubs,
  __ssBuildPayloads: ssBuildPayloads,
  __ssExpectedKeys: ssExpectedKeys,
};

function ssPageHelperScript() {
  return Object.entries(SS_PAGE_HELPERS)
    .map(([name, helper]) => "window." + name + " = " + helper.toString() + ";")
    .join("\n");
}

function assertSingleSettleResult(result) {
  const allIds = SS_FUSED_IDS.concat([SS_SOLO_ID]).sort();
  const failures = {
    "cold load did not issue exactly one lanes.subscribe frame":
      result.subscribeFrameCount !== 1,
    "the single subscribe frame missed a cold-load lane":
      result.subscribeFrameEntryIds.join(",") !== allIds.join(","),
    "fused host settled paint missed a member's initial messages":
      result.fusedPresentCount !== result.fusedExpectedCount,
    "solo lane settled paint missed its initial messages":
      result.soloPresentCount !== result.soloExpectedCount,
    "fused host painted more than one settled message render on cold load":
      result.fusedMessageRenderCount !== 1,
    "solo lane painted more than one settled message render on cold load":
      result.soloMessageRenderCount !== 1,
    "ack context did not resolve on the fused host's first settled paint":
      result.ackContextState !== "resolved",
    "resolved ack context was not visible on the first settled paint":
      result.ackQuoteText !== "acked steering context",
    "fused host did not record a settled full-replay at mount":
      result.fusedReplayInitial < 1,
    "solo lane did not record a settled full-replay at mount":
      result.soloReplayInitial < 1,
    "fused host reshuffled after settle: full-replay count grew over the quiet window":
      result.fusedReplayFinal !== result.fusedReplayInitial,
    "solo lane reshuffled after settle: full-replay count grew over the quiet window":
      result.soloReplayFinal !== result.soloReplayInitial,
  };
  for (const [message, failed] of Object.entries(failures)) {
    if (failed) throw new Error(message + ": " + JSON.stringify(result));
  }
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=mosaic-single-settle-" + Date.now(),
      contextOptions: { viewport: { width: 1900, height: 1000 } },
    },
    async ({ page }) => {
      await page.waitForFunction(
        () =>
          typeof applyTeamSnapshotPayload === "function" &&
          typeof subscribeLaneToLiveBus === "function" &&
          typeof mosaicRenderMessageStream === "function" &&
          typeof laneGroupHost === "function",
        { timeout: 10000 },
      );
      await page.addScriptTag({ content: installScript });
      await page.addScriptTag({ content: ssPageHelperScript() });
      const result = await page.evaluate(ssMeasure, {
        fusedIds: SS_FUSED_IDS,
        soloId: SS_SOLO_ID,
        perLane: SS_MESSAGES_PER_LANE,
        ackKey: SS_ACK_KEY,
        snapshotRevision: SS_SNAPSHOT_REVISION,
        settleMs: SS_SETTLE_MS,
        quietMs: SS_QUIET_MS,
      });
      assertSingleSettleResult(result);
      return result;
    },
  );
}

run().then(
  (result) => {
    console.log("serve_mosaic_single_settle_smoke ok " + JSON.stringify(result));
  },
  (error) => {
    console.error(error);
    process.exit(1);
  },
);
