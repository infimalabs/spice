// Live bus client: one WebSocket, request/response plus push, with heartbeat,
// liveness, exponential reconnect, and full resync after reconnect. Lane
// payloads merge into a known-message cache keyed by message key; rendering is
// fingerprint-gated so unchanged streams never repaint. Lane subscribes
// coalesce per microtask tick into one lanes.subscribe frame whose payloads
// apply state-first, then each fused host renders exactly once.

let liveBusSocket = null;
let liveBusOpenPromise = null;
let liveBusRequestSequence = 0;
const liveBusPendingRequests = new Map();
let liveBusHeartbeatTimer = null;
let liveBusReconnectTimer = null;
let liveBusReconnectAttempt = 0;
let liveBusLastInboundAt = 0;
let liveBusHasConnected = false;
const laneSubmitLatencySamples = [];
const laneSubmitLatencySampleLimit = 25;
const pendingLaneSendServerTimings = new Map();
const liveBusDiagnostics = { session: null, lastLaneSend: null };
let focusedLiveBusLaneTargetId = "";
if (typeof window !== "undefined")
  /** @type {any} */ (window).__spiceSubmitLatencySamples =
    laneSubmitLatencySamples;
if (typeof window !== "undefined")
  /** @type {any} */ (window).__spiceLiveBusDiagnostics = liveBusDiagnostics;

function liveBusUrl() {
  const protocol = window.location.protocol === "https:" ? "wss" : "ws";
  return protocol + "://" + window.location.host + "/api/live/bus";
}

function liveBusIsOpen() {
  return Boolean(liveBusSocket && liveBusSocket.readyState === WebSocket.OPEN);
}

function connectLiveBus() {
  if (liveBusIsOpen()) return Promise.resolve(liveBusSocket);
  if (
    liveBusSocket &&
    liveBusSocket.readyState === WebSocket.CONNECTING &&
    liveBusOpenPromise
  )
    return liveBusOpenPromise;
  const socket = new WebSocket(liveBusUrl());
  liveBusSocket = socket;
  liveBusOpenPromise = new Promise((resolve, reject) => {
    socket.addEventListener(
      "open",
      () => {
        const reconnected = liveBusHasConnected;
        liveBusHasConnected = true;
        liveBusReconnectAttempt = 0;
        noteLiveBusInbound();
        startLiveBusHeartbeat();
        resolve(socket);
        if (reconnected) resyncLiveBusAfterReconnect();
      },
      { once: true },
    );
    socket.addEventListener(
      "error",
      () => {
        reject(new Error("live bus unavailable"));
      },
      { once: true },
    );
  });
  socket.addEventListener("message", (event) => {
    noteLiveBusInbound();
    handleLiveBusMessage(event.data).catch((error) => {
      setGlobalTransientError(String(error || "live bus message failed"));
    });
  });
  socket.addEventListener("close", () => {
    if (liveBusSocket !== socket) return;
    liveBusSocket = null;
    liveBusOpenPromise = null;
    for (const lane of laneStates.values()) {
      lane.liveBusSubscribed = false;
      lane.liveBusSubscribePending = false;
      lane.liveBusWatcherActive = false;
    }
    stopLiveBusHeartbeat();
    rejectLiveBusRequests("live bus closed");
    scheduleLiveBusReconnect();
  });
  return liveBusOpenPromise;
}

function noteLiveBusInbound() {
  liveBusLastInboundAt = Date.now();
}

function startLiveBusHeartbeat() {
  stopLiveBusHeartbeat();
  liveBusHeartbeatTimer = setInterval(() => {
    if (!liveBusIsOpen()) return;
    liveBusRequest("bus.ping").catch(() => {});
    if (Date.now() - liveBusLastInboundAt > liveBusLivenessTimeoutMs) {
      // A stale half-open socket never fires close on its own; provoke it so
      // the normal close path drives the reconnect.
      try {
        liveBusSocket.close();
      } catch (error) {
        scheduleLiveBusReconnect();
      }
    }
  }, liveBusHeartbeatIntervalMs);
}

function stopLiveBusHeartbeat() {
  if (liveBusHeartbeatTimer) clearInterval(liveBusHeartbeatTimer);
  liveBusHeartbeatTimer = null;
}

function scheduleLiveBusReconnect() {
  if (liveBusReconnectTimer) return;
  const delay = Math.min(
    liveBusReconnectMaxMs,
    liveBusReconnectBaseMs * 2 ** liveBusReconnectAttempt,
  );
  liveBusReconnectAttempt += 1;
  liveBusReconnectTimer = setTimeout(() => {
    liveBusReconnectTimer = null;
    connectLiveBus().catch(() => scheduleLiveBusReconnect());
  }, delay);
}

function resyncLiveBusAfterReconnect() {
  refreshServerTopology()
    .then(resubscribeLiveBusLanes)
    .catch(() => {});
}

function rejectLiveBusRequests(reason) {
  for (const pending of liveBusPendingRequests.values()) {
    pending.reject(new Error(reason));
  }
  liveBusPendingRequests.clear();
}

async function liveBusRequest(type, fields = {}, timing = null) {
  const requestId = "bus-" + ++liveBusRequestSequence;
  if (timing) {
    timing.requestId = requestId;
    markLaneSubmitLatency(timing, "requestCreatedAt");
  }
  const response = new Promise((resolve, reject) => {
    liveBusPendingRequests.set(requestId, { resolve, reject, timing });
  });
  try {
    markLaneSubmitLatency(timing, "liveBusConnectStartAt");
    const socket = await connectLiveBus();
    markLaneSubmitLatency(timing, "liveBusConnectReadyAt");
    markLaneSubmitLatency(timing, "liveBusFrameSentAt");
    socket.send(JSON.stringify({ ...fields, type, requestId }));
  } catch (error) {
    liveBusPendingRequests.delete(requestId);
    throw error;
  }
  return response;
}

async function handleLiveBusMessage(data) {
  const message = JSON.parse(data || "{}");
  recordLiveBusDiagnostics(message);
  recordLaneSendTiming(message);
  if (resolveLiveBusPendingRequest(message)) return;
  await dispatchLiveBusPush(message);
}

function resolveLiveBusPendingRequest(message) {
  const pending = message.requestId
    ? liveBusPendingRequests.get(message.requestId)
    : null;
  if (!pending) return false;
  liveBusPendingRequests.delete(message.requestId);
  markLaneSubmitLatency(pending.timing, "liveBusResponseReceivedAt");
  if (message.type === "bus.error") pending.reject(new Error(message.error));
  else pending.resolve(message);
  return true;
}

function recordLiveBusDiagnostics(message) {
  if (message.diagnostics) liveBusDiagnostics.session = message.diagnostics;
}

function recordLaneSendTiming(message) {
  if (message.type !== "lane.sendTiming") return;
  liveBusDiagnostics.lastLaneSend = message;
  const sample = [...laneSubmitLatencySamples]
    .reverse()
    .find((item) => item.requestId === message.requestId);
  if (sample) sample.serverTiming = message.serverTiming || {};
  else pendingLaneSendServerTimings.set(message.requestId, message.serverTiming || {});
}

const liveBusPushHandlers = new Map([
  ["targets.payload", handleTargetsPush],
  ["teams.payload", handleTeamsPush],
  ["lane.payload", handleLanePayloadPush],
  ["lane.pending", handleLanePendingPush],
  ["lane.submission", handleLaneSubmissionPush],
  ["lane.append", handleLaneAppendPush],
  ["lanes.dirty", handleBackgroundLanesDirtyPush],
  ["bus.error", handleLiveBusErrorPush],
]);

async function dispatchLiveBusPush(message) {
  const handler = liveBusPushHandlers.get(message.type);
  if (handler) await handler(message);
}

function handleTargetsPush(message) {
  applyTargetsPayload(message.payload || {});
}

function handleTeamsPush(message) {
  applyTeamSnapshotPayload(message.payload || {});
}

async function handleLanePayloadPush(message) {
  const lane = matchingLiveBusLane(message);
  if (!lane) return;
  await applyLaneBusPayload(
    lane,
    message.payload || {},
    message.source || "bus",
  );
}

function handleLanePendingPush(message) {
  const lane = matchingLiveBusLane(message);
  if (lane) applyLanePendingBusPayload(lane, message.payload || {});
}

function handleLaneSubmissionPush(message) {
  const lane = matchingLiveBusLane(message);
  if (!lane) return;
  applyLaneSubmissionLifecycle(lane, message.submission);
  syncLaneSubmissionLifecycleUi(lane);
}

async function handleLaneAppendPush(message) {
  const lane = laneStates.get(message.targetId);
  if (lane && isLaneOpen(lane))
    await applyLaneAppendBusPayload(lane, message.payload || {});
}

function handleLiveBusErrorPush(message) {
  setGlobalTransientError(message.error || "live bus error");
}

function handleBackgroundLanesDirtyPush(message) {
  for (const frame of message.lanes || []) {
    const lane = laneStates.get(frame.targetId);
    if (!lane || !isLaneOpen(lane)) continue;
    if (!liveBusPushMatchesSubscription(lane, frame)) continue;
    lane.liveBusDirty = true;
    if (liveBusLaneIsFocused(lane)) subscribeLaneToLiveBus(lane);
  }
}

function matchingLiveBusLane(message) {
  const lane = laneStates.get(message.targetId);
  if (!lane || !isLaneOpen(lane)) return null;
  return liveBusPushMatchesSubscription(lane, message) ? lane : null;
}

function isLaneOpen(lane) {
  return !lane.closed && laneStates.get(lane.targetId) === lane;
}

function liveBusPushMatchesSubscription(lane, message) {
  if (message.source !== "watch") return true;
  const generation = String(message.subscriptionGeneration || "");
  return (
    Boolean(generation) &&
    lane.liveBusWatcherActive === true &&
    generation === String(lane.liveBusSubscriptionGeneration || "")
  );
}

// ---- lane subscription ------------------------------------------------------

function laneMessageQuery(lane) {
  return {
    limit: lane.newestMessageKey ? lane.retainedMessageLimit : initialRequestLimit,
    after: lane.newestMessageKey || "",
    threadId: lane.targetThreadId || "",
    focused: liveBusLaneIsFocused(lane),
  };
}

function liveBusLaneIsFocused(lane) {
  return lane.targetId === focusedLiveBusLaneTargetId;
}

function installLiveBusLaneFocusTracking() {
  const focusFromEvent = (event) => {
    const element = event.target?.closest?.(".lane[data-target-id]");
    const lane = element
      ? laneStates.get(element.dataset.targetId || "")
      : null;
    if (lane && isLaneOpen(lane)) setFocusedLiveBusLane(lane);
  };
  lanesEl.addEventListener("pointerdown", focusFromEvent, true);
  lanesEl.addEventListener("focusin", focusFromEvent, true);
}

function setFocusedLiveBusLane(focusedLane) {
  if (!focusedLane || focusedLiveBusLaneTargetId === focusedLane.targetId) return;
  focusedLiveBusLaneTargetId = focusedLane.targetId;
  for (const lane of laneStates.values()) {
    if (!isLaneOpen(lane) || !lane.liveBusSubscribed || !liveBusIsOpen()) continue;
    if (lane === focusedLane && lane.liveBusDirty) {
      subscribeLaneToLiveBus(lane);
      continue;
    }
    liveBusRequest("lane.configure", {
      targetId: lane.targetId,
      query: laneMessageQuery(lane),
    }).catch(() => {});
  }
}

// Every subscribe path — snapshot mount, reconnect resync, thread change,
// config revision — marks its lane here; the marks flush once per microtask
// tick as ONE lanes.subscribe frame, so payloads arrive together instead of
// trickling in and reshuffling fused mosaics per member.
const pendingSubscribeLanes = new Set();
let subscribeFlushQueued = false;

function subscribeLaneToLiveBus(lane) {
  if (!isLaneOpen(lane)) return;
  if (lane.emptyTeam) return;
  if (!focusedLiveBusLaneTargetId) focusedLiveBusLaneTargetId = lane.targetId;
  lane.liveBusSubscribed = false;
  lane.liveBusSubscribePending = true;
  lane.liveBusWatcherActive = false;
  pendingSubscribeLanes.add(lane);
  if (subscribeFlushQueued) return;
  subscribeFlushQueued = true;
  queueMicrotask(() => {
    subscribeFlushQueued = false;
    flushPendingLaneSubscribes().catch((error) => {
      setGlobalTransientError(String(error || "lane subscribe failed"));
    });
  });
}

async function flushPendingLaneSubscribes() {
  const lanes = [...pendingSubscribeLanes].filter(
    (lane) => isLaneOpen(lane) && !lane.emptyTeam,
  );
  pendingSubscribeLanes.clear();
  if (!lanes.length) return;
  const entries = lanes.map((lane) => {
    return { targetId: lane.targetId, query: laneMessageQuery(lane) };
  });
  let response;
  try {
    response = await liveBusRequest("lanes.subscribe", { entries });
  } catch (error) {
    for (const lane of lanes) {
      if (isLaneOpen(lane)) {
        lane.liveBusSubscribePending = false;
        setLaneTransientStatus(lane, "live bus unavailable");
      }
    }
    return;
  }
  applyLanesSubscribePayloads(lanes, (response && response.lanes) || []);
}

function applyLanesSubscribePayloads(lanes, laneFrames) {
  const frameByTargetId = new Map(
    laneFrames.map((frame) => [frame.targetId, frame]),
  );
  const hostsToRender = new Map();
  for (const lane of lanes) {
    if (!isLaneOpen(lane)) continue;
    const frame = frameByTargetId.get(lane.targetId);
    if (!frame) {
      lane.liveBusSubscribePending = false;
      setLaneTransientStatus(lane, "subscription reply missing");
      continue;
    }
    const payload = frame.payload || {};
    lane.liveBusSubscriptionGeneration = String(
      frame.subscriptionGeneration || "",
    );
    lane.liveBusWatcherActive = frame.watcherActive === true;
    lane.liveBusSubscribed = lane.liveBusWatcherActive;
    lane.liveBusSubscribePending = false;
    if (!lane.liveBusWatcherActive && frame.watcherError) {
      setLaneTransientStatus(lane, frame.watcherError);
      continue;
    }
    if (payload.error) {
      // A failed lane keeps its batch slot: it surfaces its own status while
      // sibling lanes apply and render normally.
      setLaneTransientStatus(lane, payload.error);
      continue;
    }
    applyLaneBusPayloadState(lane, payload, "bus");
    const host = laneGroupHost(lane);
    hostsToRender.set(host.targetId, host);
  }
  for (const host of hostsToRender.values()) renderMessagesIfChanged(host);
}

function resubscribeLiveBusLanes() {
  for (const lane of laneStates.values()) {
    if (isLaneOpen(lane) && !lane.emptyTeam) subscribeLaneToLiveBus(lane);
  }
}

function configureLiveBusLanes() {
  for (const lane of laneStates.values()) {
    if (
      lane.emptyTeam ||
      !isLaneOpen(lane) ||
      !lane.liveBusSubscribed ||
      !liveBusIsOpen()
    )
      continue;
    liveBusRequest("lane.configure", {
      targetId: lane.targetId,
      query: laneMessageQuery(lane),
    }).catch(() => {});
  }
}

function unsubscribeLaneFromLiveBus(lane) {
  if (!lane.liveBusSubscribed && !lane.liveBusSubscribePending) return;
  lane.liveBusSubscribed = false;
  lane.liveBusSubscribePending = false;
  lane.liveBusWatcherActive = false;
  liveBusRequest("lane.unsubscribe", { targetId: lane.targetId }).catch(
    () => {},
  );
  if (focusedLiveBusLaneTargetId === lane.targetId) {
    focusedLiveBusLaneTargetId = "";
    const replacement = [...laneStates.values()].find(
      (candidate) => candidate !== lane && isLaneOpen(candidate),
    );
    if (replacement) setFocusedLiveBusLane(replacement);
  }
}

// ---- payload application ------------------------------------------------------

async function applyLaneBusPayload(lane, payload, source) {
  applyLaneBusPayloadState(lane, payload, source);
  renderMessagesIfChanged(lane);
}

// The render-free half of payload application: batch subscribes run this per
// lane, then render each fused host once so the merged stream paints its
// final interleaving in a single lattice insertion sequence.
function applyLaneBusPayloadState(lane, payload, source) {
  lane.liveBusDirty = false;
  const wasSpeechPrimed = lane.speechPrimed;
  const knownBefore = new Set(lane.knownMessageKeys);
  // Auto-narration is gated in queueSpeechForMessages against the lane's UI
  // materialization instant, so the whole initial payload can flow through it:
  // nothing older than this browser's lane ever plays, and primeSpeechBoundary
  // still marks every known message so it is never reconsidered.
  const initialSpeechMessages = wasSpeechPrimed ? [] : payload.messages || [];
  lane.serverReachable = true;
  const threadChanged = syncLaneThreadId(lane, payload);
  if (threadChanged) {
    // A renewal hands the lane to a new agent UUID, but the lane is the
    // operator's space, not the agent's: history survives the handoff, only
    // render fingerprints drop so the merged stream repaints cleanly.
    lane.renderedMessageFingerprint = "";
    lane.renderedStatusFingerprint = "";
  }
  removePayloadMessages(lane, payload);
  mergePayloadMessages(lane, payload);
  reconcileLaneSubmissionMessages(lane, lane.knownMessages);
  renderLaneChrome(lane, payload);
  cacheLaneLatestPayload(lane, payload);
  if (source === "watch" && (payload.messages || []).length)
    refreshServerTopology().catch(() => {});
  if (!lane.speechPrimed) {
    queueSpeechForMessages(lane, initialSpeechMessages);
    primeSpeechBoundary(lane);
  } else if (wasSpeechPrimed) {
    const fresh = (payload.messages || []).filter(
      (item) => item.key && !knownBefore.has(item.key),
    );
    queueSpeechForMessages(lane, fresh);
  }
  if (threadChanged) subscribeLaneToLiveBus(lane);
}

function applyLanePendingBusPayload(lane, payload) {
  lane.serverReachable = true;
  if (!syncLaneBackendPending(lane, payload)) return;
  if (lane.latestPayload) cacheLaneLatestPayload(lane, lane.latestPayload);
  renderLaneViewShell(laneGroupHost(lane));
  syncComposerPlaceholders(laneGroupHost(lane));
}

function cacheLaneLatestPayload(lane, payload) {
  const latestPayload = payload || {};
  const version = Math.max(0, Number(lane.backendPendingInboxVersion) || 0);
  if (!version) {
    lane.latestPayload = latestPayload;
    return;
  }
  const statusLine = statusLineWithLanePendingIdentity(
    lane,
    latestPayload.statusLine || {},
  );
  lane.latestPayload = {
    ...latestPayload,
    pendingInboxCount: statusLine.pendingInboxCount,
    pendingInboxLabel: statusLine.pendingInboxLabel,
    pendingInboxKeys: statusLine.pendingInboxKeys,
    pendingInboxRevision: statusLine.pendingInboxRevision,
    pendingInboxVersion: statusLine.pendingInboxVersion,
    statusLine,
  };
}

async function applyLaneAppendBusPayload(lane, payload) {
  const messages = laneAppendMessages(payload);
  const wasSpeechPrimed = lane.speechPrimed;
  const knownBefore = new Set(lane.knownMessageKeys);
  const initialSpeechMessages = wasSpeechPrimed ? [] : messages;
  lane.serverReachable = true;
  removePayloadMessages(lane, payload);
  mergePayloadMessages(lane, { ...payload, messages });
  const lifecycleChanged = reconcileLaneSubmissionMessages(
    lane,
    lane.knownMessages,
  );
  renderMessagesIfChanged(lane);
  if (lifecycleChanged) syncLaneSubmissionLifecycleUi(lane);
  if (!lane.speechPrimed) {
    queueSpeechForMessages(lane, initialSpeechMessages);
    primeSpeechBoundary(lane);
  } else if (wasSpeechPrimed) {
    const fresh = messages.filter((item) => item.key && !knownBefore.has(item.key));
    queueSpeechForMessages(lane, fresh);
  }
}

function laneAppendMessages(payload) {
  if (!payload || !Array.isArray(payload.messages))
    throw new Error("lane.append messages are required");
  return payload.messages;
}
