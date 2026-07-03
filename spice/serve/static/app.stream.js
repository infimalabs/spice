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

async function refreshLane(lane) {
  if (lane.refreshInFlight || !isLaneOpen(lane)) return;
  lane.refreshInFlight = true;
  try {
    const response = await liveBusRequest("lane.refresh", {
      targetId: lane.targetId,
      query: laneMessageQuery(lane),
    });
    if (!isLaneOpen(lane)) return;
    lane.serverReachable = true;
    await applyLaneBusPayload(lane, response.payload || {}, "refresh");
  } catch (error) {
    if (!isLaneOpen(lane)) return;
    lane.serverReachable = false;
    setLaneTransientStatus(lane, "server unreachable");
  } finally {
    lane.refreshInFlight = false;
  }
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

function syncLaneThreadId(lane, payload) {
  const previous = lane.targetThreadId || "";
  if (!payloadHasField(payload, "targetIdentity")) return false;
  const next = targetIdentityThreadId(payload.targetIdentity);
  if (!next) return false;
  lane.targetThreadId = next;
  lane.activeThreadId = next;
  ensureLaneOccupant(lane, next);
  return Boolean(previous && next !== previous);
}

function mergePayloadMessages(lane, payload) {
  const threadId = payloadHasField(payload, "targetIdentity")
    ? targetIdentityThreadId(payload.targetIdentity)
    : lane.activeThreadId || "";
  for (const item of [...(payload.messages || [])].reverse()) {
    stampMessageProducer(item, lane, threadId);
    upsertKnownMessage(lane, item, "newest");
  }
  trimKnownMessages(lane);
}

function removePayloadMessages(lane, payload) {
  const keys = new Set(payload.removedMessageKeys || []);
  if (!keys.size) return;
  lane.knownMessages = lane.knownMessages.filter((item) => !keys.has(item.key));
  lane.knownMessageKeys = new Set(lane.knownMessages.map((item) => item.key));
}

function mergeOlderPayloadMessages(lane, payload) {
  const threadId = payloadHasField(payload, "targetIdentity")
    ? targetIdentityThreadId(payload.targetIdentity)
    : lane.activeThreadId || "";
  let added = 0;
  for (const item of payload.messages || []) {
    stampMessageProducer(item, lane, threadId);
    if (upsertKnownMessage(lane, item, "oldest")) added += 1;
  }
  if (added > 0) lane.retainedMessageLimit += added;
  trimKnownMessages(lane);
  return added;
}

function upsertKnownMessage(lane, item, position) {
  const existingIndex = lane.knownMessages.findIndex(
    (known) => known.key === item.key,
  );
  if (existingIndex >= 0) {
    lane.knownMessages[existingIndex] = item;
    return false;
  }
  if (position === "oldest") lane.knownMessages.push(item);
  else lane.knownMessages.unshift(item);
  lane.knownMessageKeys.add(item.key);
  noteLaneOccupantMessage(lane, item.threadId);
  return true;
}

function compareKnownMessagesNewestFirst(a, b) {
  // Mirror the server's _message_sort_key (epoch, index, key), newest first.
  // upsert appends new keys at the front, so a full-window payload (which is
  // the only place task cards appear) would otherwise drop an older task card
  // ahead of newer transcript messages already cached from live appends.
  const aEpoch = Date.parse(a.timestamp || "");
  const bEpoch = Date.parse(b.timestamp || "");
  const aTime = Number.isFinite(aEpoch) ? aEpoch : 0;
  const bTime = Number.isFinite(bEpoch) ? bEpoch : 0;
  if (aTime !== bTime) return bTime - aTime;
  const aIndex = Number(a.index) || 0;
  const bIndex = Number(b.index) || 0;
  if (aIndex !== bIndex) return bIndex - aIndex;
  const aKey = a.key || "";
  const bKey = b.key || "";
  if (aKey === bKey) return 0;
  return aKey < bKey ? 1 : -1;
}

function trimKnownMessages(lane) {
  lane.knownMessages.sort(compareKnownMessagesNewestFirst);
  const visible = [];
  let latestPresence = null;
  for (const item of lane.knownMessages) {
    if (isPresenceMessage(item)) {
      if (!latestPresence) latestPresence = item;
      continue;
    }
    if (visible.length < lane.retainedMessageLimit) visible.push(item);
  }
  const kept = latestPresence ? [latestPresence, ...visible] : visible;
  const seen = new Set();
  lane.knownMessages = kept.filter((item) => {
    if (seen.has(item.key)) return false;
    seen.add(item.key);
    return true;
  });
  lane.knownMessageKeys = new Set(lane.knownMessages.map((item) => item.key));
  const bounds = lane.knownMessages.filter((item) => !isPresenceMessage(item));
  lane.newestMessageKey = bounds.length ? bounds[0].key : "";
  lane.oldestMessageKey = bounds.length ? bounds[bounds.length - 1].key : "";
  pruneAckContextCache(lane);
}

function isPresenceMessage(item) {
  return (item.kind || "").startsWith("presence:");
}

// A lane is an envelope over the agents (threads) that inhabit it. Occupants
// register by threadId in first-observation order so a renewed lane's merged
// stream can attribute each agent's messages.
function ensureLaneOccupant(lane, threadId) {
  if (!threadId) return null;
  let occupant = lane.occupants.get(threadId);
  if (!occupant) {
    occupant = { threadId, ordinal: lane.occupants.size, messageCount: 0 };
    lane.occupants.set(threadId, occupant);
  }
  return occupant;
}

function laneOccupantOrdinal(lane, threadId) {
  const occupant = threadId ? lane.occupants.get(threadId) : null;
  return occupant ? occupant.ordinal : 0;
}

function laneMessageAttributionAgentCount(lane) {
  const host = laneGroupHost(lane);
  return Math.max(laneGroupMemberLanes(host).length, host.occupants.size);
}

function laneShouldAttributeMessages(lane) {
  return (
    Boolean(laneGroupHost(lane).teamId) ||
    laneMessageAttributionAgentCount(lane) > 1
  );
}

function laneMemberAccentIndex(lane, member) {
  const host = laneGroupHost(lane);
  const index = laneGroupMemberTargetIds(host).indexOf(member.targetId);
  if (index < 0)
    throw new Error("team slot accent requires a lane group member");
  return index;
}

function laneMessageAccentIndex(lane, item) {
  const host = laneGroupHost(lane);
  const targetId = laneMessageProducerTargetId(host, item);
  if (targetId) {
    const index = laneGroupMemberTargetIds(host).indexOf(targetId);
    if (index >= 0) return index;
  }
  return laneOccupantOrdinal(host, item.threadId);
}

function laneMessageProducerTargetId(lane, item) {
  if (item.producerTargetId) return item.producerTargetId;
  const threadId = item.threadId || "";
  if (!threadId) return "";
  const host = laneGroupHost(lane);
  const member = laneGroupMemberLanes(host).find(
    (candidate) =>
      candidate.targetThreadId === threadId ||
      candidate.activeThreadId === threadId,
  );
  return member ? member.targetId : "";
}

function noteLaneOccupantMessage(lane, threadId) {
  const occupant = ensureLaneOccupant(lane, threadId);
  if (occupant) occupant.messageCount += 1;
}

function stampMessageProducer(item, lane, threadId) {
  item.producerTargetId = lane.targetId;
  if (!item.threadId && threadId) item.threadId = threadId;
}

// ---- history paging -----------------------------------------------------------

function syncLaneHistoryObserver(lane) {
  for (const member of historySentinelMemberLanes(lane)) {
    if (member === lane || !member.historyObserver) continue;
    member.historyObserver.disconnect();
    member.historyObserver = null;
  }
  if (lane.historyObserver) lane.historyObserver.disconnect();
  lane.historyObserver = new IntersectionObserver(
    (entries) => {
      const hydratedTargetIds = new Set();
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const member = historyLaneForSentinel(lane, entry.target);
        if (hydratedTargetIds.has(member.targetId)) continue;
        hydratedTargetIds.add(member.targetId);
        maybeHydrateOlderMessages(member);
      }
    },
    {
      root: lane.messagesEl,
      rootMargin: "0px 0px " + hydrateScrollThresholdPx + "px 0px",
      threshold: 0,
    },
  );
  for (const sentinel of lane.messagesEl.querySelectorAll(
    "[data-history-sentinel]",
  )) {
    lane.historyObserver.observe(sentinel);
  }
}

function historyLaneForSentinel(host, sentinel) {
  const targetId = sentinel.dataset.historyTargetId || host.targetId;
  const member = laneStates.get(targetId);
  if (member && laneGroupHost(member) === host) return member;
  return host;
}

function maybeHydrateOlderMessages(lane) {
  if (
    lane.olderHydrationInFlight ||
    lane.olderHistoryExhausted ||
    !lane.oldestMessageKey
  )
    return;
  hydrateOlderMessages(lane);
}

async function hydrateOlderMessages(lane) {
  if (lane.olderHydrationInFlight || !isLaneOpen(lane)) return;
  lane.olderHydrationInFlight = true;
  try {
    const response = await liveBusRequest("lane.history", {
      targetId: lane.targetId,
      query: {
        limit: requestLimit,
        before: lane.oldestMessageKey || "",
        threadId: lane.targetThreadId || "",
      },
    });
    if (!isLaneOpen(lane)) return;
    const added = mergeOlderPayloadMessages(lane, response.payload || {});
    if (!added) lane.olderHistoryExhausted = true;
    await hydrateAckContextsForMessages(lane, lane.knownMessages);
    renderMessagesIfChanged(lane);
  } catch (error) {
    return;
  } finally {
    lane.olderHydrationInFlight = false;
  }
}

// ---- ack context cache ----------------------------------------------------------

function rememberAckContext(
  lane,
  key,
  text,
  html = "",
  priority = "",
  attachments = [],
) {
  if (!key || !text) return;
  lane.ackContextByKey.set(key, { key, text, html, priority, attachments });
  lane.missingAckContextKeys.delete(key);
  lane.recentSentAckKeys = [
    key,
    ...lane.recentSentAckKeys.filter((item) => item !== key),
  ].slice(0, 50);
}

function ackContextForKey(lane, key) {
  if (!key) return null;
  const direct = lane.ackContextByKey.get(key);
  if (direct) return direct;
  for (const member of laneGroupMemberLanes(laneGroupHost(lane))) {
    if (member === lane) continue;
    const context = member.ackContextByKey.get(key);
    if (context) return context;
  }
  return null;
}

function ackKeysForMessages(messages) {
  const keys = [];
  for (const item of messages) {
    if (isPresenceMessage(item)) continue;
    for (const key of item.ack_keys || []) {
      if (key && !keys.includes(key)) keys.push(key);
    }
  }
  return keys;
}

async function hydrateAckContextsForMessages(lane, messages) {
  const keys = ackKeysForMessages(messages).filter(
    (key) =>
      !lane.ackContextByKey.has(key) && !lane.missingAckContextKeys.has(key),
  );
  if (!keys.length) return;
  const query = keys.map((key) => "key=" + encodeURIComponent(key)).join("&");
  try {
    const response = await fetch(targetApi(lane.targetId, "/acks") + "?" + query, {
      cache: "no-store",
    });
    const payload = await response.json();
    for (const ack of payload.acks || []) {
      if (ack.found && ack.text) {
        lane.ackContextByKey.set(ack.key, {
          key: ack.key,
          text: ack.text,
          html: ack.html || "",
          priority: ack.priority || "",
          attachments: ack.attachments || [],
        });
        lane.missingAckContextKeys.delete(ack.key);
      } else if (ack.key) {
        lane.missingAckContextKeys.add(ack.key);
      }
    }
  } catch (error) {
    return;
  }
}

function pruneAckContextCache(lane) {
  const retainedKeys = new Set([
    ...ackKeysForMessages(lane.knownMessages),
    ...lane.recentSentAckKeys,
  ]);
  for (const key of lane.ackContextByKey.keys()) {
    if (!retainedKeys.has(key)) lane.ackContextByKey.delete(key);
  }
  for (const key of lane.missingAckContextKeys) {
    if (!retainedKeys.has(key)) lane.missingAckContextKeys.delete(key);
  }
}

// ---- fingerprint-gated message rendering ---------------------------------------

function renderMessagesIfChanged(lane) {
  // A shadow lane never paints itself: its messages flow into the host's one
  // merged stream. A fused host paints every member's messages interleaved.
  const host = laneGroupHost(lane);
  if (host !== lane) {
    renderMessagesIfChanged(host);
    return;
  }
  if (lane.emptyTeam) {
    renderEmptyTeamMessages(lane);
    return;
  }
  const renderItems = laneIsFusedHost(lane)
    ? laneGroupMergedMessages(lane)
    : lane.knownMessages;
  const visibleItems = renderItems.filter((item) => !isPresenceMessage(item));
  renderLaneViewShell(lane);
  const fingerprint = messageRenderFingerprint(lane, visibleItems);
  if (fingerprint === lane.renderedMessageFingerprint) return;
  const viewportAnchor = captureMessageViewportAnchor(lane);
  const existingNodes = existingMessageNodesByKey(lane);
  const nodes = messageStreamNodesWithHistorySentinels(
    lane,
    visibleItems,
    existingNodes,
  );
  suppressLanePaneScrollIntentForFrame(lane);
  lane.messagesEl.replaceChildren(...nodes);
  packMessageStream(lane);
  restoreMessageViewportAnchor(lane, viewportAnchor);
  syncMessagePackObserver(lane);
  syncLaneHistoryObserver(lane);
  syncTeamImportOverlay(lane);
  lane.renderedMessageFingerprint = fingerprint;
}

function renderEmptyTeamMessages(lane) {
  renderLaneViewShell(lane);
  const fingerprint = emptyTeamMessageFingerprint(lane);
  if (fingerprint === lane.renderedMessageFingerprint) return;
  const viewportAnchor = captureMessageViewportAnchor(lane);
  suppressLanePaneScrollIntentForFrame(lane);
  lane.messagesEl.replaceChildren(
    emptyTeamImportPanel(lane),
    lane.historySentinelEl,
  );
  resetMessagePackObserver(lane);
  restoreMessageViewportAnchor(lane, viewportAnchor);
  syncLaneHistoryObserver(lane);
  lane.renderedMessageFingerprint = fingerprint;
}

function messageStreamNodesWithHistorySentinels(lane, visibleItems, existingNodes) {
  const sentinelMembersByMessageKey = historySentinelMembersByMessageKey(
    lane,
    visibleItems,
  );
  const nodes = [];
  let previousItem = null;
  for (const item of visibleItems) {
    const node = renderOrReuseMessageNode(lane, item, existingNodes);
    if (!node) continue;
    const rule = timeRuleBetween(previousItem, item);
    if (rule) nodes.push(rule);
    previousItem = item;
    nodes.push(node);
    for (const member of sentinelMembersByMessageKey.get(item.key) || []) {
      nodes.push(historySentinelForLane(member));
    }
  }
  if (!nodes.length) nodes.push(historySentinelForLane(lane));
  return nodes;
}

// Absolute-time rules re-baseline the masonry: one rule per crossed hour
// bucket run, labeled with the bucket start, so a multi-hour idle gap costs a
// single line instead of whitespace. Compaction dividers already carry their
// own timestamp, so a rule that would sit adjacent to one is suppressed.
// Rules derive deterministically from the visible items, which keeps the
// render fingerprint contract untouched.
const timeRuleIntervalMs = 60 * 60 * 1000;

function timeRuleBetween(previousItem, item) {
  if (!previousItem) return null;
  if (previousItem.kind === "compaction" || item.kind === "compaction")
    return null;
  const previousTs = Date.parse(previousItem.timestamp || "");
  const currentTs = Date.parse(item.timestamp || "");
  if (!Number.isFinite(previousTs) || !Number.isFinite(currentTs)) return null;
  const bucket = Math.floor(currentTs / timeRuleIntervalMs);
  if (Math.floor(previousTs / timeRuleIntervalMs) >= bucket) return null;
  return renderTimeRule(bucket * timeRuleIntervalMs);
}

function historySentinelMembersByMessageKey(lane, visibleItems) {
  const oldestMessageKeyByTargetId = new Map();
  for (const item of visibleItems) {
    const targetId = laneMessageProducerTargetId(lane, item) || lane.targetId;
    if (targetId) oldestMessageKeyByTargetId.set(targetId, item.key);
  }
  const membersByMessageKey = new Map();
  for (const member of historySentinelMemberLanes(lane)) {
    const messageKey = oldestMessageKeyByTargetId.get(member.targetId);
    if (!messageKey) continue;
    const members = membersByMessageKey.get(messageKey) || [];
    members.push(member);
    membersByMessageKey.set(messageKey, members);
  }
  return membersByMessageKey;
}

function historySentinelMemberLanes(lane) {
  return laneIsFusedHost(lane) ? laneGroupMemberLanes(lane) : [lane];
}

function historySentinelForLane(lane) {
  lane.historySentinelEl.dataset.historyTargetId = lane.targetId;
  return lane.historySentinelEl;
}

// Deterministic masonry: every card gets an explicit grid position. A card's
// column is chosen once (shortest column at first placement) and pinned in its
// dataset, so later height changes only slide columns vertically — a card can
// never flip sides while the operator is reading. Full-width items (compaction
// dividers, history sentinels, directive stacks) are barriers: both columns
// restart on the same row after one, which resets skew and stops reflow from
// propagating across it. Pins are scoped per barrier segment.
function packMessageStream(lane) {
  const host = lane.messagesEl;
  if (!host) return;
  const style = getComputedStyle(host);
  if (style.display !== "grid") return;
  const rowHeight = cssPixelValue(style.gridAutoRows) || 8;
  const rowGap = cssPixelValue(style.rowGap) || 0;
  const rowStride = rowHeight + rowGap;
  if (!Number.isFinite(rowStride) || rowStride <= 0) return;
  const columnCount = messagePackColumnCount(host, style);
  const bandRows = messagePackBandRows(style);
  const columnRows = new Array(Math.max(1, columnCount)).fill(0);
  let segmentKey = "top";
  for (const node of host.children) {
    if (!isMessagePackItem(node)) continue;
    const height = messagePackItemHeight(node);
    if (!Number.isFinite(height) || height <= 0) continue;
    const naturalSpan = Math.max(1, Math.ceil((height + rowGap) / rowStride));
    if (columnCount <= 1 || isMessagePackBarrier(node)) {
      setMessagePackRowSpan(node, naturalSpan);
      const start = Math.max(...columnRows);
      setMessagePackPosition(node, "", start + 1);
      columnRows.fill(start + naturalSpan);
      if (isMessagePackBarrier(node)) segmentKey = messagePackBarrierKey(node);
      continue;
    }
    // Cards round up to whole bands and stretch to fill them (CSS align-self),
    // so the quantization slack lives inside the card box instead of the grid.
    // Image-only cards keep their natural span: stretching would distort them,
    // and start-alignment keeps the slack invisible against the row below.
    const span = node.classList.contains("image-only")
      ? naturalSpan
      : Math.ceil(naturalSpan / bandRows) * bandRows;
    setMessagePackRowSpan(node, span);
    const column = stickyMessagePackColumn(node, segmentKey, columnRows, bandRows);
    setMessagePackPosition(node, String(column + 1), columnRows[column] + 1);
    columnRows[column] += span;
  }
}

// Stretch-aligned cards report their assigned grid cell as rect height, which
// would freeze a fresh card at the span-1 default forever. scrollHeight sees
// the natural content height through the stretch (plus the border, which
// scrollHeight excludes), so a card measures natural when fresh or grown and
// its settled band cell otherwise — a fixed point either way.
const messageCardBorderAllowancePx = 2;

function messagePackItemHeight(node) {
  const rectHeight = node.getBoundingClientRect().height;
  if (!node.matches("article[data-message-key]")) return rectHeight;
  return Math.max(rectHeight, node.scrollHeight + messageCardBorderAllowancePx);
}

// Segments are keyed by the identity of the barrier that opens them, never by
// ordinal position: history backfill or a newly synthesized time rule must not
// shift the key of untouched downstream segments and cascade re-pins while the
// operator is reading.
function messagePackBarrierKey(node) {
  return (
    node.dataset.messageKey ||
    node.dataset.messageTs ||
    node.dataset.historyTargetId ||
    node.className
  );
}

// Derive the track count from geometry, never from computed
// grid-template-columns: the stream uses repeat(auto-fit, ...), which
// collapses empty tracks — once every card sits in column 1, the computed
// track list shrinks to one entry and a computed-style reading locks the
// packer into a single ever-growing column. The auto-fit formula is
// deterministic from the host width, so recompute it directly.
function messagePackColumnCount(host, style) {
  const inner =
    host.clientWidth -
    cssPixelValue(style.paddingLeft) -
    cssPixelValue(style.paddingRight);
  if (!Number.isFinite(inner) || inner <= 0) return 1;
  const gap = cssPixelValue(style.columnGap);
  const minWidth = cssLengthValue(
    style.getPropertyValue("--message-card-min-width"),
    style,
  );
  const trackMin = Math.min(inner, minWidth > 0 ? minWidth : inner);
  if (trackMin <= 0) return 1;
  return Math.max(1, Math.floor((inner + gap) / (trackMin + gap)));
}

function cssLengthValue(value, style) {
  const text = String(value || "").trim();
  const parsed = Number.parseFloat(text);
  if (!Number.isFinite(parsed)) return 0;
  if (text.endsWith("rem")) {
    const rootSize = Number.parseFloat(
      getComputedStyle(document.documentElement).fontSize,
    );
    return parsed * (Number.isFinite(rootSize) ? rootSize : 16);
  }
  if (text.endsWith("em") && !text.endsWith("rem")) {
    const fontSize = cssPixelValue(style.fontSize) || 16;
    return parsed * fontSize;
  }
  return parsed;
}

function isMessagePackBarrier(node) {
  return node.matches(
    ".compaction-divider, .time-rule, [data-history-sentinel], article:has(.task-directive-stack)",
  );
}

function stickyMessagePackColumn(node, segmentKey, columnRows, bandRows) {
  const pinned = Number(node.dataset.messagePackColumn);
  if (
    node.dataset.messagePackSegment === segmentKey &&
    Number.isInteger(pinned) &&
    pinned >= 0 &&
    pinned < columnRows.length
  )
    return pinned;
  const column = leftmostFeasiblePackColumn(columnRows, bandRows);
  node.dataset.messagePackSegment = segmentKey;
  node.dataset.messagePackColumn = String(column);
  return column;
}

// One band of tolerance turns shortest-column masonry into row-major filling:
// a level fills left to right, then the next level starts, so chronological
// insertion keeps each visual band a contiguous time slice. The lowest column
// is always feasible, so the scan always returns.
const messagePackBandRowsDefault = 6;

function messagePackBandRows(style) {
  const parsed = Number.parseInt(
    style.getPropertyValue("--message-pack-band"),
    10,
  );
  return parsed > 0 ? parsed : messagePackBandRowsDefault;
}

function leftmostFeasiblePackColumn(columnRows, bandRows) {
  // Strictly less than one band: a column that already took a full band is
  // spent for this level, so equal-height cards fan left-to-right instead of
  // stacking pairs into the leftmost column.
  const lowest = Math.min(...columnRows);
  for (let index = 0; index < columnRows.length; index += 1) {
    if (columnRows[index] < lowest + bandRows) return index;
  }
  return columnRows.indexOf(lowest);
}

function setMessagePackPosition(node, columnStart, rowStart) {
  if (node.style.gridColumnStart !== columnStart)
    node.style.gridColumnStart = columnStart;
  const rowValue = String(rowStart);
  if (node.style.gridRowStart !== rowValue) node.style.gridRowStart = rowValue;
}

function scheduleMessageStreamPack(lane) {
  if (lane.messagePackFrame) return;
  lane.messagePackFrame = requestAnimationFrame(() => {
    lane.messagePackFrame = 0;
    if (lane.closed) return;
    const anchor = captureMessageViewportAnchor(lane);
    packMessageStream(lane);
    restoreMessageViewportAnchor(lane, anchor);
  });
}

function syncMessagePackObserver(lane) {
  if (typeof ResizeObserver === "undefined" || !lane.messagesEl) return;
  if (!lane.messagePackResizeObserver)
    lane.messagePackResizeObserver = new ResizeObserver(() => {
      scheduleMessageStreamPack(lane);
    });
  const current = new Set();
  for (const node of lane.messagesEl.children) {
    if (!isMessagePackItem(node)) continue;
    current.add(node);
    if (!lane.messagePackObservedNodes?.has(node))
      lane.messagePackResizeObserver.observe(node);
  }
  for (const node of lane.messagePackObservedNodes || []) {
    if (!current.has(node)) lane.messagePackResizeObserver.unobserve(node);
  }
  lane.messagePackObservedNodes = current;
}

function resetMessagePackObserver(lane) {
  if (lane.messagePackFrame) {
    cancelAnimationFrame(lane.messagePackFrame);
    lane.messagePackFrame = 0;
  }
  if (lane.messagePackResizeObserver) lane.messagePackResizeObserver.disconnect();
  lane.messagePackObservedNodes = new Set();
}

function isMessagePackItem(node) {
  if (!node || typeof node.matches !== "function") return false;
  return node.matches(
    "article[data-message-key], .compaction-divider, .time-rule, [data-history-sentinel]",
  );
}

function cssPixelValue(value) {
  const parsed = Number.parseFloat(String(value || ""));
  return Number.isFinite(parsed) ? parsed : 0;
}

function setMessagePackRowSpan(node, span) {
  const value = String(span);
  if (node.style.getPropertyValue("--message-pack-row-span") === value) return;
  node.style.setProperty("--message-pack-row-span", value);
}

function emptyTeamMessageFingerprint(lane) {
  return JSON.stringify({
    emptyTeam: true,
    teamId: lane.teamId || "",
    targets: targets.map(emptyTeamTargetFingerprint),
  });
}

function emptyTeamTargetFingerprint(target) {
  const statusLine = target.statusLine || {};
  return [
    target.id || "",
    targetIdentityBranch(target.targetIdentity),
    targetIdentityAgentName(target.targetIdentity),
    targetIdentityThreadId(target.targetIdentity),
    target.lastAssistantAt || "",
    statusLine.lastAssistantAt || "",
    target.pendingCount || 0,
    target.pendingInboxCount || 0,
    target.agentProcessStatus || "",
    targetIdentityThreadState(target.targetIdentity),
  ];
}

function captureMessageViewportAnchor(lane) {
  if (lane.messagesEl.scrollTop <= 1) return null;
  const scrollerTop = lane.messagesEl.getBoundingClientRect().top;
  for (const node of lane.messagesEl.querySelectorAll(
    "article[data-message-key]",
  )) {
    const rect = node.getBoundingClientRect();
    if (rect.bottom <= scrollerTop) continue;
    return {
      key: node.dataset.messageKey || "",
      offsetTop: rect.top - scrollerTop,
    };
  }
  return null;
}

function restoreMessageViewportAnchor(lane, anchor) {
  if (!anchor || !anchor.key) return;
  const node = lane.messagesEl.querySelector(
    'article[data-message-key="' + CSS.escape(anchor.key) + '"]',
  );
  if (!node) return;
  const scrollerTop = lane.messagesEl.getBoundingClientRect().top;
  const delta =
    node.getBoundingClientRect().top - scrollerTop - anchor.offsetTop;
  if (!Number.isFinite(delta) || Math.abs(delta) < 1) return;
  setLaneScrollTopWithoutPaneIntent(lane, lane.messagesEl.scrollTop + delta);
}

function existingMessageNodesByKey(lane) {
  const nodes = new Map();
  for (const node of lane.messagesEl.children) {
    const key = node.dataset ? node.dataset.messageKey || "" : "";
    if (key) nodes.set(key, node);
  }
  return nodes;
}

function renderOrReuseMessageNode(lane, item, existingNodes) {
  const key = String(item.key);
  const fingerprint = JSON.stringify(messageFingerprintParts(lane, item));
  const existing = existingNodes.get(key);
  if (existing && existing.dataset.renderFingerprint === fingerprint)
    return existing;
  const node = renderMessage(lane, item);
  if (!node) return null;
  node.dataset.messageKey = key;
  node.dataset.renderFingerprint = fingerprint;
  return node;
}

function messageRenderFingerprint(lane, messages) {
  return JSON.stringify(
    messages.map((item) => messageFingerprintParts(lane, item)),
  );
}

function messageFingerprintParts(lane, item) {
  return {
    key: item.key,
    index: item.index,
    timestamp: item.timestamp,
    kind: item.kind,
    threadId: item.threadId || "",
    accentSlot: laneMessageAccentIndex(lane, item),
    displayHtml: item.display_html,
    displayText: item.display_text,
    ackCount: item.ack_count,
    attributed: laneShouldAttributeMessages(lane),
    attributionAgents: laneMessageAttributionAgentCount(lane),
    ackContexts: (item.ack_keys || []).map((key) => {
      const context = ackContextForKey(lane, key);
      return context
        ? [key, context.text, context.html, context.priority, context.attachments || []]
        : [key, "", "", "", []];
    }),
  };
}

// ---- send -----------------------------------------------------------------------

function enqueueSend(lane, payload, sourceLane = lane, options = {}) {
  if (!isLaneOpen(lane)) return;
  if (!payload.text.trim()) {
    setLaneTransientStatus(sourceLane, "Message text is required.");
    return;
  }
  if (lane.sendAwaitingBackendCount > 0) {
    setLaneTransientStatus(sourceLane, "send already in progress");
    return;
  }
  const latencyProbe = startLaneSubmitLatencyProbe(lane, payload);
  beginLanePendingSubmission(lane);
  markLaneSubmitLatency(latencyProbe, "optimisticRenderedAt");
  sendLanePayload(lane, payload, sourceLane, { ...options, latencyProbe });
}

async function sendLanePayload(lane, payload, sourceLane = lane, options = {}) {
  const latencyProbe =
    options.latencyProbe || startLaneSubmitLatencyProbe(lane, payload);
  lane.sendAwaitingBackendCount += 1;
  try {
    markLaneSubmitLatency(latencyProbe, "requestAwaitStartAt");
    const response = await liveBusRequest("lane.send", {
      targetId: lane.targetId,
      payload,
    }, latencyProbe);
    markLaneSubmitLatency(latencyProbe, "responseResolvedAt");
    const result = response.result || {};
    latencyProbe.serverTiming = result.serverTiming || {};
    if (!isLaneOpen(lane)) {
      finishLaneSubmitLatencyProbe(latencyProbe, "closed");
      return;
    }
    applyLaneSendResult(lane, payload, result, sourceLane, options);
    markLaneSubmitLatency(latencyProbe, "resultAppliedAt");
    finishLaneSubmitLatencyProbe(
      latencyProbe,
      result.ok ? "accepted" : "rejected",
    );
  } catch (error) {
    markLaneSubmitLatency(latencyProbe, "errorAt");
    if (isLaneOpen(lane)) {
      finishLanePendingSubmission(lane, { accepted: false });
      setLaneTransientStatus(sourceLane, "steer failed");
    }
    finishLaneSubmitLatencyProbe(latencyProbe, "error");
  } finally {
    lane.sendAwaitingBackendCount = Math.max(
      0,
      lane.sendAwaitingBackendCount - 1,
    );
  }
}

function startLaneSubmitLatencyProbe(lane, payload) {
  const text = String((payload || {}).text || "");
  return {
    targetId: lane.targetId || "",
    textLength: text.length,
    marks: { startedAt: laneSubmitLatencyNow() },
  };
}

function markLaneSubmitLatency(probe, name) {
  if (!probe || !name) return;
  if (!probe.marks) probe.marks = {};
  if (probe.marks[name] === undefined) probe.marks[name] = laneSubmitLatencyNow();
}

function finishLaneSubmitLatencyProbe(probe, status) {
  if (!probe || probe.completed) return;
  probe.completed = true;
  probe.status = status;
  markLaneSubmitLatency(probe, "completedAt");
  probe.durations = laneSubmitLatencyDurations(probe.marks || {});
  laneSubmitLatencySamples.push(probe);
  while (laneSubmitLatencySamples.length > laneSubmitLatencySampleLimit)
    laneSubmitLatencySamples.shift();
}

function laneSubmitLatencyDurations(marks) {
  const responseAt =
    marks.liveBusResponseReceivedAt === undefined
      ? marks.responseResolvedAt
      : marks.liveBusResponseReceivedAt;
  const resolvedAt =
    marks.responseResolvedAt === undefined
      ? marks.liveBusResponseReceivedAt
      : marks.responseResolvedAt;
  return {
    optimisticRenderMs: laneSubmitLatencyDelta(
      marks.startedAt,
      marks.optimisticRenderedAt,
    ),
    liveBusOpenMs: laneSubmitLatencyDelta(
      marks.liveBusConnectStartAt,
      marks.liveBusConnectReadyAt,
    ),
    sendResultWaitMs: laneSubmitLatencyDelta(marks.liveBusFrameSentAt, responseAt),
    responseHandlingMs: laneSubmitLatencyDelta(
      resolvedAt,
      marks.resultAppliedAt || marks.errorAt,
    ),
    totalMs: laneSubmitLatencyDelta(marks.startedAt, marks.completedAt),
  };
}

function laneSubmitLatencyDelta(start, end) {
  if (!Number.isFinite(start) || !Number.isFinite(end)) return null;
  return Math.max(0, end - start);
}

function laneSubmitLatencyNow() {
  return typeof performance !== "undefined" && typeof performance.now === "function"
    ? performance.now()
    : Date.now();
}

function applyLaneSendResult(
  lane,
  payload,
  result,
  sourceLane = lane,
  options = {},
) {
  const previousThreadId = lane.targetThreadId || "";
  applyTaskDrainRouteConfig(lane, result);
  if (!result.ok) {
    finishLanePendingSubmission(lane, { accepted: false });
    setLaneTransientStatus(sourceLane, result.error || "send failed");
    return;
  }
  clearAcceptedComposerDrafts(
    sourceLane,
    lane.targetId,
    result.requestText || payload.text,
  );
  focusAfterComposerReset(options.focusAfterReset);
  finishLanePendingSubmission(lane, {
    accepted: true,
    inboxKey: result.key,
    pendingInboxCount: result.pendingInboxCount,
    pendingInboxKeys: result.pendingInboxKeys,
    pendingInboxRevision: result.pendingInboxRevision,
    pendingInboxVersion: result.pendingInboxVersion,
  });
  rememberAckContext(
    lane,
    result.key,
    result.requestText || payload.text,
    result.requestHtml || "",
    result.requestPriority || "",
    result.attachments || [],
  );
  const ensure = result.agentEnsure || {};
  if (ensure.ok === false)
    setLaneTransientStatus(sourceLane, agentEnsureFailureStatus(ensure));
  if (ensure.threadId) {
    const changed = ensure.threadId !== previousThreadId;
    lane.targetThreadId = ensure.threadId;
    lane.activeThreadId = ensure.threadId;
    ensureLaneOccupant(lane, ensure.threadId);
    if (changed) {
      refreshServerTopology().catch(() => {});
      subscribeLaneToLiveBus(lane);
    }
  }
}

function agentEnsureFailureStatus(ensure) {
  const parts = [ensure.error || "agent launch failed"];
  if (ensure.deadletteredInboxKey)
    parts.push("parked inbox " + ensure.deadletteredInboxKey);
  if (ensure.deadletterRequeueCommand)
    parts.push("requeue: " + ensure.deadletterRequeueCommand);
  return parts.join("; ");
}

function focusAfterComposerReset(element) {
  if (!element) return;
  if (!(element instanceof HTMLElement))
    throw new Error("composer focus target must be an element");
  if (!document.contains(element))
    throw new Error("composer focus target must remain in the document");
  element.focus({ preventScroll: true });
}

// ---- route application -----------------------------------------------------------

function taskDrainRouteConfig(result) {
  return result.ok === false ? result : result.route;
}

function applyTaskDrainRouteConfig(lane, result) {
  const config = taskDrainRouteConfig(result);
  if (!config) return;
  applyRouteConfigToTargetInventory(lane, config);
  if (payloadHasField(config, "targetIdentity"))
    applyLaneTargetIdentity(lane, config);
  if (payloadHasField(config, "serveAgentIdentity"))
    applyLaneServeAgentIdentity(lane, config);
  if (Array.isArray(config.taskFilters)) {
    lane.taskFilters = uniqueStringList(config.taskFilters);
    lane.laneFilterVersion = String(config.laneFilterVersion || "");
  }
  if (Array.isArray(config.taskFilterEntries))
    lane.taskFilterEntries = normalizedTaskFilterEntries(config.taskFilterEntries);
  if (payloadHasField(config, "teamIdentity")) {
    lane.teamId = teamIdentityTeamId(config.teamIdentity);
    lane.teamRevision = teamIdentityRevision(config.teamIdentity);
    lane.configRevision = teamIdentityConfigRevision(config.teamIdentity);
  }
  if (config.lifetime)
    applyServerLaneLifetime(lane, config.lifetime, {
      configRevision: payloadHasField(config, "teamIdentity")
        ? teamIdentityConfigRevision(config.teamIdentity)
        : lane.configRevision,
    });
  renderLaneViewShell(laneGroupHost(lane));
  renderFilterPills();
}

function applyRouteConfigToTargetInventory(lane, config) {
  const target = targetById.get(lane.targetId);
  if (!target) return;
  if (payloadHasField(config, "targetIdentity"))
    target.targetIdentity = config.targetIdentity;
  if (payloadHasField(config, "serveAgentIdentity"))
    target.serveAgentIdentity = config.serveAgentIdentity;
  if (payloadHasField(config, "teamIdentity"))
    target.teamIdentity = config.teamIdentity;
  if (Array.isArray(config.taskFilters))
    target.taskFilters = uniqueStringList(config.taskFilters);
  if (Array.isArray(config.taskFilterEntries))
    target.taskFilterEntries = normalizedTaskFilterEntries(config.taskFilterEntries);
  if (payloadHasField(config, "laneFilterVersion"))
    target.laneFilterVersion = String(config.laneFilterVersion || "");
  if (payloadHasField(config, "lifetime"))
    target.lifetime = String(config.lifetime || "");
}
