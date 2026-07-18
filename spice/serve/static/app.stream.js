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
  applyPayloadAckContexts(lane, payload);
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
  applyPayloadAckContexts(lane, payload);
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

// The render palette has this many accent colors (app.render.js
// messageOccupantAccentPalette; kept in sync by a static test). Every accent
// index is reduced into this range at its source so messageOccupantAccent
// never receives an out-of-range slot: the team cap keeps LIVE member indices
// in range, but a departed/renewed producer whose occupant ordinal has grown
// past the palette (driver switches each register an occupant), or a legacy
// team created before the cap, would otherwise make messageOccupantAccent
// throw and abort the whole render -- surfacing as a wholesale rebuild.
// Recycling a color is strictly better than crashing the board.
const MESSAGE_ACCENT_SLOT_COUNT = 6;

function laneMemberAccentIndex(lane, member) {
  const host = laneGroupHost(lane);
  const index = laneGroupMemberTargetIds(host).indexOf(member.targetId);
  if (index < 0)
    throw new Error("team slot accent requires a lane group member");
  return index % MESSAGE_ACCENT_SLOT_COUNT;
}

function laneMessageAccentIndex(lane, item) {
  const host = laneGroupHost(lane);
  const targetId = laneMessageProducerTargetId(host, item);
  if (targetId) {
    const index = laneGroupMemberTargetIds(host).indexOf(targetId);
    if (index >= 0) return index % MESSAGE_ACCENT_SLOT_COUNT;
  }
  return laneOccupantOrdinal(host, item.threadId) % MESSAGE_ACCENT_SLOT_COUNT;
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
    const response = /** @type {LaneFrame} */ (
      await liveBusRequest("lane.history", {
        targetId: lane.targetId,
        query: {
          limit: requestLimit,
          before: lane.oldestMessageKey || "",
          threadId: lane.targetThreadId || "",
        },
      })
    );
    if (!isLaneOpen(lane)) return;
    const added = mergeOlderPayloadMessages(lane, response.payload);
    if (!added) lane.olderHistoryExhausted = true;
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

// A key whose lookup came back found:false is CONFIRMED absent, not still
// loading -- the placeholder skeleton must be dropped rather than shimmer
// forever. Mirrors ackContextForKey's fused-member traversal.
function ackContextConfirmedMissing(lane, key) {
  if (!key) return false;
  if (lane.ackContextByKey.has(key)) return false;
  if (lane.missingAckContextKeys.has(key)) return true;
  for (const member of laneGroupMemberLanes(laneGroupHost(lane))) {
    if (member === lane) continue;
    if (member.ackContextByKey.has(key)) return false;
    if (member.missingAckContextKeys.has(key)) return true;
  }
  return false;
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

function applyPayloadAckContexts(lane, payload) {
  for (const ack of payload?.ackContexts || []) {
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
  // Accent color is a pure presentation mapping (member order -> border
  // color), NOT part of message identity: a composer reorder must recolor
  // in place, never re-measure or move a single card. It rides its own
  // fingerprint and a style-only pass over existing nodes, and is
  // deliberately absent from messageRenderFingerprint below.
  applyMessageAccentsIfChanged(lane);
  const fingerprint = messageRenderFingerprint(lane, visibleItems);
  if (fingerprint === lane.renderedMessageFingerprint) return;
  // One scroll authority: the mosaic owns viewport stability during its own
  // renders -- plane compensation (mosaicSyncPlane onCompensate) covers
  // frontier shifts for a scrolled reader, mosaicRestoreBackfillViewport
  // covers the backfill bottom seam, and every other movement class is a
  // defined lattice motion the reader is meant to see (wet settling, ripple
  // push, replay FLIP). The legacy grid-era capture/restore anchor is gone
  // from this path: it re-anchored against getBoundingClientRect mid-tween
  // and fought the compensation it duplicated.
  suppressLanePaneScrollIntentForFrame(lane);
  const mosaicRenderResult = mosaicRenderMessageStream(lane, visibleItems);
  // Deferred (unmeasurable host): nothing was painted, so the fingerprint
  // must stay uncommitted -- the reveal render re-enters here with the same
  // content and actually paints it.
  if (mosaicRenderResult && mosaicRenderResult.deferred) return;
  mosaicAttachHistorySentinels(lane, visibleItems);
  mosaicRestoreBackfillViewport(lane, mosaicRenderResult);
  syncLaneHistoryObserver(lane);
  syncTeamImportOverlay(lane);
  lane.renderedMessageFingerprint = fingerprint;
}

// History sentinels carry no lattice position of their own (the lattice
// has no concept of non-card stream elements); each
// tracks a specific fused member's oldest visible message purely for
// IntersectionObserver purposes, so appending it as a normal-flow child of
// that message's own (already positioned) card reuses the card's real
// geometry for free instead of computing a redundant position.
function mosaicAttachHistorySentinels(lane, visibleItems) {
  const membersByMessageKey = historySentinelMembersByMessageKey(lane, visibleItems);
  let attachedAny = false;
  for (const [messageKey, members] of membersByMessageKey) {
    const card = lane.mosaicPlaneEl?.querySelector(
      'article[data-message-key="' + CSS.escape(messageKey) + '"]',
    );
    if (!card) continue;
    for (const member of members) {
      card.append(historySentinelForLane(member));
      attachedAny = true;
    }
  }
  if (!attachedAny) lane.messagesEl.append(historySentinelForLane(lane));
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
  mosaicResetResizeObserver(lane);
  restoreMessageViewportAnchor(lane, viewportAnchor);
  syncLaneHistoryObserver(lane);
  lane.renderedMessageFingerprint = fingerprint;
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

function emptyTeamMessageFingerprint(lane) {
  return JSON.stringify({
    emptyTeam: true,
    teamId: lane.teamId || "",
    targets: laneStore.targetsSnapshot().map(emptyTeamTargetFingerprint),
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
    statusLine.latestActivityKind || "",
    target.pendingCount || 0,
    target.pendingInboxCount || 0,
    target.agentProcessStatus || "",
    target.agentVisualStatus || statusLine.agentVisualStatus || "",
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

// Style-only accent recolor: recomputes each rendered card's occupant
// accent from the CURRENT member order and writes only the color CSS var +
// slot dataset. No measurement, no fingerprint change, no mosaic touch --
// composer reorder recolors without moving anything. Gated by an accent
// fingerprint (attribution flag + ordered member target ids) so a no-op
// render writes nothing.
function laneAccentFingerprint(lane) {
  const host = laneGroupHost(lane);
  return JSON.stringify([
    laneShouldAttributeMessages(host),
    laneGroupMemberTargetIds(host),
  ]);
}

function applyMessageAccentsIfChanged(lane) {
  const fingerprint = laneAccentFingerprint(lane);
  if (fingerprint === lane.renderedAccentFingerprint) return;
  lane.renderedAccentFingerprint = fingerprint;
  const plane = lane.mosaicPlaneEl;
  if (!plane) return;
  const attribute = laneShouldAttributeMessages(lane);
  for (const node of plane.querySelectorAll(
    "article[data-message-key], [data-compaction-accent-node]",
  )) {
    if (!attribute) {
      delete node.dataset.accentSlot;
      node.style.removeProperty("--message-occupant-accent");
      node.style.removeProperty("--compaction-accent");
      continue;
    }
    const item = mosaicNodeAccentItem(node);
    const accentSlot = laneMessageAccentIndex(lane, item);
    node.dataset.accentSlot = String(accentSlot);
    const accent = messageOccupantAccent(accentSlot);
    if (node.matches("article[data-message-key]"))
      node.style.setProperty("--message-occupant-accent", accent);
    else node.style.setProperty("--compaction-accent", accent);
  }
}

// Minimal item shape laneMessageAccentIndex needs (producer target / thread),
// reconstructed from the node's own datasets so the recolor pass needs no
// message-list lookup.
function mosaicNodeAccentItem(node) {
  return {
    key: node.dataset.messageKey || "",
    threadId: node.dataset.threadId || "",
    producerTargetId: node.dataset.producerTargetId || "",
  };
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
    displayHtml: item.display_html,
    displayText: item.display_text,
    ackCount: item.ack_count,
    // Whether attribution shows at all (solo vs team) legitimately changes a
    // card's chrome, so the boolean stays in the identity. The member COUNT
    // does not: nothing per-card depends on it, and including it rebuilt and
    // re-measured every card each time an agent joined -- the one-by-one
    // flash on load. Accent (which does track member order) rides its own
    // fingerprint and a style-only recolor pass, not this identity.
    attributed: laneShouldAttributeMessages(lane),
    ackContexts: (item.ack_keys || []).map((key) => {
      const context = ackContextForKey(lane, key);
      if (context)
        return [key, context.text, context.html, context.priority, context.attachments || []];
      // Distinguish still-loading ("") from confirmed-missing ("missing") so
      // the card re-renders (dropping its skeleton) when a lookup fails.
      return [key, ackContextConfirmedMissing(lane, key) ? "missing" : "", "", "", []];
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
    const response = /** @type {LaneSendResultFrame} */ (
      await liveBusRequest(
        "lane.send",
        {
          targetId: lane.targetId,
          payload,
        },
        latencyProbe,
      )
    );
    markLaneSubmitLatency(latencyProbe, "responseResolvedAt");
    const result = response.result;
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
      focusAfterComposerReset(options.focusAfterReset);
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
  if (probe.requestId && pendingLaneSendServerTimings.has(probe.requestId)) {
    probe.serverTiming = pendingLaneSendServerTimings.get(probe.requestId) || {};
    pendingLaneSendServerTimings.delete(probe.requestId);
  }
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
  result = /** @type {WorkTreeSendResult} */ (result);
  const previousThreadId = lane.targetThreadId || "";
  applyTaskDrainRouteConfig(lane, result);
  if (!result.ok) {
    finishLanePendingSubmission(lane, { accepted: false });
    focusAfterComposerReset(options.focusAfterReset);
    setLaneTransientStatus(sourceLane, result.error || "send failed");
    return;
  }
  clearAcceptedComposerDrafts(sourceLane, lane.targetId);
  finishLanePendingSubmission(lane, {
    accepted: true,
    inboxKey: result.key,
    pendingInboxCount: result.pendingInboxCount,
    pendingInboxKeys: result.pendingInboxKeys,
    pendingInboxRevision: result.pendingInboxRevision,
    pendingInboxVersion: result.pendingInboxVersion,
  });
  focusAfterComposerReset(options.focusAfterReset);
  rememberAckContext(
    lane,
    result.key,
    result.requestText || payload.text,
    result.requestHtml || "",
    result.requestPriority || "",
    result.attachments || [],
  );
  applyLaneSubmissionLifecycle(lane, result.submission);
  syncLaneSubmissionLifecycleUi(lane);
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

/**
 * @param {RoutedResult} result
 * @returns {WorkTreeRoute|undefined}
 */
function taskDrainRouteConfig(result) {
  return result.ok === false ? undefined : result.route;
}

/** @param {RoutedResult} result */
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
  if (Array.isArray(config.effectiveTaskFilters))
    lane.effectiveTaskFilters = uniqueStringList(config.effectiveTaskFilters);
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
  laneStore.updateTarget(lane.targetId, (target) => {
    const updated = { ...target };
    if (payloadHasField(config, "targetIdentity"))
      updated.targetIdentity = config.targetIdentity;
    if (payloadHasField(config, "serveAgentIdentity"))
      updated.serveAgentIdentity = config.serveAgentIdentity;
    if (payloadHasField(config, "teamIdentity"))
      updated.teamIdentity = config.teamIdentity;
    if (Array.isArray(config.taskFilters))
      updated.taskFilters = uniqueStringList(config.taskFilters);
    if (Array.isArray(config.effectiveTaskFilters))
      updated.effectiveTaskFilters = uniqueStringList(config.effectiveTaskFilters);
    if (Array.isArray(config.taskFilterEntries))
      updated.taskFilterEntries = normalizedTaskFilterEntries(
        config.taskFilterEntries,
      );
    if (payloadHasField(config, "laneFilterVersion"))
      updated.laneFilterVersion = String(config.laneFilterVersion || "");
    if (payloadHasField(config, "lifetime"))
      updated.lifetime = String(config.lifetime || "");
    return updated;
  });
}
