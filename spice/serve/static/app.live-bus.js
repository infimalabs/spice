// Live bus client: one WebSocket, request/response plus push, with heartbeat,
// liveness, exponential reconnect, and full resync after reconnect. Lane
// payloads merge into a known-message cache keyed by message key; rendering is
// fingerprint-gated so unchanged streams never repaint.

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
if (typeof window !== "undefined")
  /** @type {any} */ (window).__spiceSubmitLatencySamples =
    laneSubmitLatencySamples;

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
  const pending = message.requestId
    ? liveBusPendingRequests.get(message.requestId)
    : null;
  if (pending) {
    liveBusPendingRequests.delete(message.requestId);
    markLaneSubmitLatency(pending.timing, "liveBusResponseReceivedAt");
    if (message.type === "bus.error") pending.reject(new Error(message.error));
    else pending.resolve(message);
    return;
  }
  if (message.type === "targets.payload") {
    applyTargetsPayload(message.payload || {});
  } else if (message.type === "teams.payload") {
    applyTeamSnapshotPayload(message.payload || {});
  } else if (message.type === "lane.payload") {
    const lane = laneStates.get(message.targetId);
    if (lane && isLaneOpen(lane))
      await applyLaneBusPayload(
        lane,
        message.payload || {},
        message.source || "bus",
      );
  } else if (message.type === "lane.pending") {
    const lane = laneStates.get(message.targetId);
    if (lane && isLaneOpen(lane))
      applyLanePendingBusPayload(lane, message.payload || {});
  } else if (message.type === "lane.append") {
    const lane = laneStates.get(message.targetId);
    if (lane && isLaneOpen(lane))
      await applyLaneAppendBusPayload(lane, message.payload || {});
  } else if (message.type === "bus.error") {
    setGlobalTransientError(message.error || "live bus error");
  }
}

function isLaneOpen(lane) {
  return !lane.closed && laneStates.get(lane.targetId) === lane;
}

// ---- lane subscription ------------------------------------------------------

function laneMessageQuery(lane) {
  return {
    limit: lane.newestMessageKey ? lane.retainedMessageLimit : initialRequestLimit,
    after: lane.newestMessageKey || "",
    threadId: lane.targetThreadId || "",
  };
}

async function subscribeLaneToLiveBus(lane) {
  if (!isLaneOpen(lane)) return;
  if (lane.emptyTeam) return;
  lane.liveBusSubscribed = true;
  try {
    const response = await liveBusRequest("lane.subscribe", {
      targetId: lane.targetId,
      query: laneMessageQuery(lane),
    });
    if (response.payload)
      await applyLaneBusPayload(lane, response.payload, "bus");
  } catch (error) {
    if (isLaneOpen(lane)) setLaneTransientStatus(lane, "live bus unavailable");
  }
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
  if (!lane.liveBusSubscribed) return;
  lane.liveBusSubscribed = false;
  liveBusRequest("lane.unsubscribe", { targetId: lane.targetId }).catch(
    () => {},
  );
}

// ---- payload application ------------------------------------------------------

async function applyLaneBusPayload(lane, payload, source) {
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
  renderLaneChrome(lane, payload);
  cacheLaneLatestPayload(lane, payload);
  await hydrateAckContextsForMessages(lane, lane.knownMessages);
  renderMessagesIfChanged(lane);
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
  await hydrateAckContextsForMessages(lane, lane.knownMessages);
  renderMessagesIfChanged(lane);
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
