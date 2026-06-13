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
      setGlobalTransientStatus(String(error || "live bus message failed"));
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

async function liveBusRequest(type, fields = {}) {
  const requestId = "bus-" + ++liveBusRequestSequence;
  const response = new Promise((resolve, reject) => {
    liveBusPendingRequests.set(requestId, { resolve, reject });
  });
  try {
    const socket = await connectLiveBus();
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
      await applyLaneBusPayload(lane, message.payload || {}, message.source || "bus");
  } else if (message.type === "bus.error") {
    setGlobalTransientStatus(message.error || "live bus error");
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
    fastMode: fastModeEnabled,
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
  lane.serverReachable = true;
  const threadChanged = syncLaneThreadId(lane, payload);
  if (threadChanged) {
    // A renewal hands the lane to a new agent UUID, but the lane is the
    // operator's space, not the agent's: history survives the handoff, only
    // render fingerprints drop so the merged stream repaints cleanly.
    lane.renderedMessageFingerprint = "";
    lane.renderedStatusFingerprint = "";
  }
  mergePayloadMessages(lane, payload);
  if (!lane.speechPrimed) primeSpeechBoundary(lane);
  lane.latestPayload = payload;
  renderLaneChrome(lane, payload);
  await hydrateAckContextsForMessages(lane, lane.knownMessages);
  renderMessagesIfChanged(lane);
  if (source === "watch" && (payload.messages || []).length)
    refreshServerTopology().catch(() => {});
  if (wasSpeechPrimed && source === "watch") {
    const fresh = (payload.messages || []).filter(
      (item) => item.key && !knownBefore.has(item.key),
    );
    queueSpeechForMessages(lane, fresh);
  }
  if (threadChanged) subscribeLaneToLiveBus(lane);
}

function syncLaneThreadId(lane, payload) {
  const previous = lane.targetThreadId || "";
  const next = payload.targetThreadId || "";
  if (!next) return false;
  lane.targetThreadId = next;
  lane.activeThreadId = next;
  ensureLaneOccupant(lane, next);
  return Boolean(previous && next !== previous);
}

function mergePayloadMessages(lane, payload) {
  const threadId = payload.targetThreadId || lane.activeThreadId || "";
  for (const item of [...(payload.messages || [])].reverse()) {
    stampMessageProducer(item, lane, threadId);
    upsertKnownMessage(lane, item, "newest");
  }
  trimKnownMessages(lane);
}

function mergeOlderPayloadMessages(lane, payload) {
  const threadId = payload.targetThreadId || lane.activeThreadId || "";
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

function trimKnownMessages(lane) {
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
  return laneMessageAttributionAgentCount(lane) > 1;
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
  if (lane.historyObserver) lane.historyObserver.disconnect();
  lane.historyObserver = new IntersectionObserver(
    (entries) => {
      if (entries.some((entry) => entry.isIntersecting))
        maybeHydrateOlderMessages(lane);
    },
    {
      root: lane.messagesEl,
      rootMargin: "0px 0px " + hydrateScrollThresholdPx + "px 0px",
      threshold: 0,
    },
  );
  lane.historyObserver.observe(lane.historySentinelEl);
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
  const nodes = visibleItems
    .map((item) => renderOrReuseMessageNode(lane, item, existingNodes))
    .filter((node) => node !== null);
  suppressLanePaneScrollIntentForFrame(lane);
  lane.messagesEl.replaceChildren(...nodes, lane.historySentinelEl);
  restoreMessageViewportAnchor(lane, viewportAnchor);
  syncLaneHistoryObserver(lane);
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
  restoreMessageViewportAnchor(lane, viewportAnchor);
  syncLaneHistoryObserver(lane);
  lane.renderedMessageFingerprint = fingerprint;
}

function emptyTeamMessageFingerprint(lane) {
  return JSON.stringify({
    emptyTeam: true,
    teamId: lane.teamId || "",
    targets: targets.map((target) => [
      target.id || "",
      target.agentName || "",
      target.branch || "",
      target.status || "",
      target.pendingInboxCount || 0,
    ]),
  });
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
    occupant: laneOccupantOrdinal(lane, item.threadId),
    displayHtml: item.display_html,
    displayText: item.display_text,
    ackCount: item.ack_count,
    sayCount: item.say_count,
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

function enqueueSend(lane, payload, sourceLane = lane) {
  if (!isLaneOpen(lane)) return;
  if (!payload.text.trim()) {
    setLaneTransientStatus(sourceLane, "Message text is required.");
    return;
  }
  if (lane.sendAwaitingBackendCount > 0) {
    setLaneTransientStatus(sourceLane, "send already in progress");
    return;
  }
  beginLanePendingSubmission(lane);
  sendLanePayload(lane, payload, sourceLane);
}

async function sendLanePayload(lane, payload, sourceLane = lane) {
  lane.sendAwaitingBackendCount += 1;
  try {
    const response = await liveBusRequest("lane.send", {
      targetId: lane.targetId,
      payload,
    });
    const result = response.result || {};
    if (!isLaneOpen(lane)) return;
    applyLaneSendResult(lane, payload, result, sourceLane);
    await refreshLane(lane);
  } catch (error) {
    if (isLaneOpen(lane)) {
      finishLanePendingSubmission(lane, { accepted: false });
      setLaneTransientStatus(sourceLane, "steer failed");
    }
  } finally {
    lane.sendAwaitingBackendCount = Math.max(
      0,
      lane.sendAwaitingBackendCount - 1,
    );
  }
}

function applyLaneSendResult(lane, payload, result, sourceLane = lane) {
  applyTaskDrainRouteConfig(lane, result);
  if (!result.ok) {
    finishLanePendingSubmission(lane, { accepted: false });
    setLaneTransientStatus(sourceLane, result.error || "send failed");
    return;
  }
  resetLaneComposerDraft(sourceLane, lane.targetId);
  finishLanePendingSubmission(lane, {
    accepted: true,
    pendingInboxCount: result.pendingInboxCount,
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
    setLaneTransientStatus(sourceLane, ensure.error || "agent launch failed");
  if (ensure.threadId) {
    const changed = ensure.threadId !== lane.targetThreadId;
    lane.targetThreadId = ensure.threadId;
    lane.activeThreadId = ensure.threadId;
    ensureLaneOccupant(lane, ensure.threadId);
    if (changed) {
      refreshServerTopology().catch(() => {});
      subscribeLaneToLiveBus(lane);
    }
  }
}

// ---- task drain (lifetime + filter routing) -------------------------------------

function updateTaskDrainForLane(lane, fields = {}) {
  const host = laneGroupHost(lane);
  const recipients = laneGroupMemberLanes(host);
  if (!recipients.length) return;
  const commandLane = recipients[0];
  const payload = {
    threadId: commandLane.targetThreadId || "",
    lifetime: laneEffectiveLifetime(host),
    teamId: commandLane.teamId || "",
    teamRevision: commandLane.teamRevision || 0,
    configRevision: commandLane.configRevision || 0,
    laneFilterVersion: commandLane.laneFilterVersion || "",
    ...fields,
  };
  liveBusRequest("lane.taskDrain", {
    targetId: commandLane.targetId,
    payload,
  })
    .then((response) => {
      const result = response.result || {};
      for (const recipient of recipients)
        applyTaskDrainRouteConfig(recipient, result);
      if (result.ok === false)
        setLaneTransientStatus(host, result.error || "task drain update failed");
    })
    .catch(() => {
      if (isLaneOpen(host))
        setLaneTransientStatus(host, "task drain update failed");
    });
}

function applyTaskDrainRouteConfig(lane, result) {
  const config = result.ok === false ? result : result.route;
  if (!config || !Array.isArray(config.taskFilters)) return;
  lane.taskFilters = uniqueStringList(config.taskFilters);
  lane.laneFilterVersion = String(config.laneFilterVersion || "");
  if (config.teamId) lane.teamId = String(config.teamId);
  if (config.lifetime) applyServerLaneLifetime(lane, config.lifetime);
  renderLaneViewShell(laneGroupHost(lane));
  renderFilterPills();
}
