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
  const initialSpeechMessages = wasSpeechPrimed
    ? []
    : initialPayloadSpeechMessages(lane, payload.messages || []);
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
  lane.latestPayload = payload;
  renderLaneChrome(lane, payload);
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

function initialPayloadSpeechMessages(lane, messages) {
  return messages.filter((item) => messageIsFreshForInitialSpeech(lane, item));
}

function messageIsFreshForInitialSpeech(lane, item) {
  const boundary = Number(lane.speechPrimeStartedAt) || Date.now();
  const timestamp = Date.parse(item.timestamp || "");
  return Number.isFinite(timestamp) && timestamp >= boundary;
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
  restoreMessageViewportAnchor(lane, viewportAnchor);
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
  for (const item of visibleItems) {
    const node = renderOrReuseMessageNode(lane, item, existingNodes);
    if (!node) continue;
    nodes.push(node);
    for (const member of sentinelMembersByMessageKey.get(item.key) || []) {
      nodes.push(historySentinelForLane(member));
    }
  }
  if (!nodes.length) nodes.push(historySentinelForLane(lane));
  return nodes;
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
  beginLanePendingSubmission(lane);
  sendLanePayload(lane, payload, sourceLane, options);
}

async function sendLanePayload(lane, payload, sourceLane = lane, options = {}) {
  lane.sendAwaitingBackendCount += 1;
  try {
    const response = await liveBusRequest("lane.send", {
      targetId: lane.targetId,
      payload,
    });
    const result = response.result || {};
    if (!isLaneOpen(lane)) return;
    applyLaneSendResult(lane, payload, result, sourceLane, options);
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
  resetLaneComposerDraft(sourceLane, lane.targetId);
  focusAfterComposerReset(options.focusAfterReset);
  finishLanePendingSubmission(lane, {
    accepted: true,
    inboxKey: result.key,
    pendingInboxCount: result.pendingInboxCount,
    pendingInboxKeys: result.pendingInboxKeys,
    pendingInboxRevision: result.pendingInboxRevision,
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
  const requestedLifetime = payload.lifetime;
  const lifetimeRequestId = Math.max(0, Number(host.lifetimeRequestId) || 0);
  const pendingLifetimeRequestId =
    host.pendingLifetimeCommit === requestedLifetime
      ? Math.max(0, Number(host.pendingLifetimeRequestId) || 0)
      : 0;
  liveBusRequest("lane.taskDrain", {
    targetId: commandLane.targetId,
    payload,
  })
    .then((response) => {
      const result = response.result || {};
      for (const recipient of recipients)
        applyTaskDrainRouteConfig(recipient, result, {
          supersedePending: false,
          lifetimeRequestId,
        });
      settleLaneLifetimeCommit(
        host,
        requestedLifetime,
        pendingLifetimeRequestId,
        result,
      );
      if (result.ok === false) {
        setLaneTransientStatus(host, result.error || "task drain update failed");
      }
    })
    .catch(() => {
      if (pendingLifetimeRequestId)
        rollbackLaneLifetimeCommit(host, requestedLifetime, "", {
          requestId: pendingLifetimeRequestId,
        });
      if (isLaneOpen(host))
        setLaneTransientStatus(host, "task drain update failed");
    });
}

function taskDrainRouteConfig(result) {
  return result.ok === false ? result : result.route;
}

function settleLaneLifetimeCommit(
  lane,
  requestedLifetime,
  requestedLifetimeRequestId,
  result,
) {
  if (!requestedLifetimeRequestId) return;
  const host = laneGroupHost(lane);
  const requestOptions = { requestId: requestedLifetimeRequestId };
  if (!laneLifetimeCommitMatches(host, requestedLifetime, requestOptions))
    return;
  const config = taskDrainRouteConfig(result) || {};
  if (result.ok !== false && config.lifetime === requestedLifetime) {
    clearLaneLifetimeCommit(host, requestedLifetime, requestOptions);
    return;
  }
  rollbackLaneLifetimeCommit(
    host,
    requestedLifetime,
    config.lifetime,
    requestOptions,
  );
}

function taskDrainLifetimeResponseIsCurrent(lane, options = {}) {
  if (options.lifetimeRequestId === undefined) return true;
  const requestId = Math.max(0, Number(options.lifetimeRequestId) || 0);
  const host = laneGroupHost(lane);
  const latestRequestId = Math.max(0, Number(host.lifetimeRequestId) || 0);
  return requestId >= latestRequestId;
}

function applyTaskDrainRouteConfig(lane, result, options = {}) {
  const config = taskDrainRouteConfig(result);
  if (!config) return;
  applyRouteConfigToTargetInventory(lane, config);
  if (payloadHasField(config, "targetIdentity"))
    applyLaneTargetIdentity(lane, config);
  if (Array.isArray(config.taskFilters)) {
    lane.taskFilters = uniqueStringList(config.taskFilters);
    lane.laneFilterVersion = String(config.laneFilterVersion || "");
  }
  if (payloadHasField(config, "teamIdentity")) {
    lane.teamId = teamIdentityTeamId(config.teamIdentity);
    lane.teamRevision = teamIdentityRevision(config.teamIdentity);
    lane.configRevision = teamIdentityConfigRevision(config.teamIdentity);
  }
  if (config.lifetime && taskDrainLifetimeResponseIsCurrent(lane, options))
    applyServerLaneLifetime(lane, config.lifetime, {
      configRevision: payloadHasField(config, "teamIdentity")
        ? teamIdentityConfigRevision(config.teamIdentity)
        : lane.configRevision,
      requestId: options.lifetimeRequestId,
      supersedePending: options.supersedePending,
    });
  renderLaneViewShell(laneGroupHost(lane));
  renderFilterPills();
}

function applyRouteConfigToTargetInventory(lane, config) {
  const target = targetById.get(lane.targetId);
  if (!target) return;
  if (payloadHasField(config, "targetIdentity"))
    target.targetIdentity = config.targetIdentity;
  if (payloadHasField(config, "teamIdentity"))
    target.teamIdentity = config.teamIdentity;
  if (Array.isArray(config.taskFilters))
    target.taskFilters = uniqueStringList(config.taskFilters);
  if (payloadHasField(config, "laneFilterVersion"))
    target.laneFilterVersion = String(config.laneFilterVersion || "");
  if (payloadHasField(config, "lifetime"))
    target.lifetime = String(config.lifetime || "");
}
