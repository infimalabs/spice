// Message and chrome rendering. Messages lay out in the agent's order: leading
// prose, then each ACK as its steering-context quote(s) followed by that ACK's
// response. Status pips distinguish running from running-stale by transcript
// recency, and pending counts blend backend truth with optimistic sends.

const secondsPerMinute = 60;
const minutesPerHour = 60;
const hoursPerDay = 24;
const activeAssistantSeconds = 60;
const transientLaneStatusMilliseconds = 2500;
const transientGlobalStatusMilliseconds = 3000;
const messageOccupantAccentPalette = [
  "var(--accent-strong)",
  "var(--final-accent)",
  "var(--say-accent)",
  "var(--warn)",
  "var(--team-teal-accent)",
  "var(--team-plum-accent)",
];
let globalTransientStatusTimer = null;

function renderLaneChrome(lane, payload) {
  applyLaneTargetIdentity(lane, payload);
  applyLaneServeAgentIdentity(lane, payload);
  lane.taskFilters = uniqueStringList(payload.taskFilters || lane.taskFilters);
  if (payloadHasField(payload, "laneFilterVersion"))
    lane.laneFilterVersion = String(payload.laneFilterVersion || "");
  applyLaneTeamIdentity(lane, payload);
  if (payload.taskFilterInventory)
    lane.taskFilterInventory = payload.taskFilterInventory;
  if (payload.privateTaskCount !== undefined)
    lane.privateTaskCount = Math.max(0, Number(payload.privateTaskCount) || 0);
  if (payload.laneMetrics) lane.laneMetrics = payload.laneMetrics;
  if (payload.laneInfo) lane.laneInfo = payload.laneInfo;
  if (payload.renewalIntent) lane.renewalIntent = payload.renewalIntent;
  if (payload.lifetime)
    applyServerLaneLifetime(lane, payload.lifetime, {
      configRevision: payloadHasField(payload, "teamIdentity")
        ? teamIdentityConfigRevision(payload.teamIdentity)
        : lane.configRevision,
    });
  const statusLine = applyRetainedLaneStatus(lane, payload.statusLine || {});
  syncLaneBackendPending(lane, statusLine);
  renderLaneViewShell(laneGroupHost(lane));
  renderFilterPills();
  syncFusedLaneChrome(laneGroupHost(lane));
  syncComposerPlaceholders(laneGroupHost(lane));
}

function payloadHasField(payload, name) {
  return Object.prototype.hasOwnProperty.call(payload || {}, name);
}

function applyLaneTargetIdentity(lane, payload) {
  if (!payloadHasField(payload, "targetIdentity")) return;
  const identity = payload.targetIdentity || {};
  lane.branchName = targetIdentityBranch(identity);
  lane.agentName = targetIdentityAgentName(identity);
  lane.driverName = targetIdentityDriverName(identity);
  lane.driverModel = targetIdentityDriverModel(identity);
  lane.driverEffort = targetIdentityDriverEffort(identity);
  lane.driverDesiredName = lane.driverName;
  lane.driverDesiredModel = lane.driverModel;
  lane.driverDesiredEffort = lane.driverEffort;
  lane.driverActualName = "";
  lane.driverActualModel = "";
  lane.driverActualEffort = "";
  lane.driverTranscriptOwner = "";
  lane.driverIconName = lane.driverName;
  const threadId = targetIdentityThreadId(identity);
  lane.targetThreadId = threadId;
  lane.activeThreadId = threadId;
}

function applyLaneServeAgentIdentity(lane, payload) {
  if (!payloadHasField(payload, "serveAgentIdentity")) return;
  const identity = payload.serveAgentIdentity || {};
  lane.serveAgentIdentity = identity;
  const desiredDriver = serveAgentDesiredDriverName(identity);
  const actualDriver = serveAgentActualDriverName(identity);
  const transcriptOwner = serveAgentTranscriptOwner(identity);
  const desiredModel = serveAgentDesiredLaunchValue(identity, "model");
  const actualModel = serveAgentActualLaunchValue(identity, "model");
  const desiredEffort = serveAgentDesiredLaunchValue(identity, "effort");
  const actualEffort = serveAgentActualLaunchValue(identity, "effort");
  lane.driverDesiredName = desiredDriver;
  lane.driverActualName = actualDriver;
  lane.driverTranscriptOwner = transcriptOwner;
  lane.driverName = identityDisplayPair(actualDriver, desiredDriver);
  lane.driverIconName = actualDriver || transcriptOwner || desiredDriver;
  lane.driverDesiredModel = desiredModel;
  lane.driverActualModel = actualModel;
  lane.driverModel = identityDisplayPair(actualModel, desiredModel);
  lane.driverDesiredEffort = desiredEffort;
  lane.driverActualEffort = actualEffort;
  lane.driverEffort = identityDisplayPair(actualEffort, desiredEffort);
  const threadId = serveAgentThreadId(identity);
  lane.targetThreadId = threadId;
  lane.activeThreadId = threadId;
}

function applyLaneTeamIdentity(lane, payload) {
  if (!payloadHasField(payload, "teamIdentity")) return;
  const identity = payload.teamIdentity || {};
  const state = identityPayloadState(identity, "team identity");
  if (state === "none") {
    lane.teamId = "";
    lane.teamRevision = 0;
    lane.configRevision = 0;
    return;
  }
  if (state !== "member")
    throw new Error("invalid team identity state: " + (state || "-"));
  lane.teamId = requiredIdentityText(identity.teamId, "team id");
  lane.teamRevision = nonnegativeIdentityNumber(
    identity.teamRevision,
    "team revision",
  );
  lane.configRevision = nonnegativeIdentityNumber(
    identity.configRevision,
    "config revision",
  );
}

function targetIdentityBranch(identity) {
  return requiredIdentityText((identity || {}).branch, "target branch");
}

function targetIdentityAgentName(identity) {
  const agent = (identity || {}).agent || {};
  const state = identityPayloadState(agent, "agent identity");
  if (state === "unconfigured") return "";
  if (state !== "configured")
    throw new Error("invalid agent identity state: " + (state || "-"));
  return requiredIdentityText(agent.name, "agent name");
}

function targetIdentityDriverName(identity) {
  return requiredIdentityText(targetIdentityDriver(identity).name, "driver name");
}

function targetIdentityDriverModel(identity) {
  return requiredIdentityText(targetIdentityDriver(identity).model, "driver model");
}

function targetIdentityDriverEffort(identity) {
  return requiredIdentityText(targetIdentityDriver(identity).effort, "driver effort");
}

function targetIdentityDriver(identity) {
  return (identity || {}).driver || {};
}

function serveAgentDriver(identity) {
  return (identity || {}).driver || {};
}

function serveAgentDesiredDriverName(identity) {
  const driver = serveAgentDriver(identity);
  if (!driver.desired) return "";
  return requiredIdentityText(driver.desired, "desired driver");
}

function serveAgentActualDriverName(identity) {
  return String(serveAgentDriver(identity).actual || "").trim();
}

function serveAgentTranscriptOwner(identity) {
  return String(serveAgentDriver(identity).transcriptOwner || "").trim();
}

function serveAgentLaunch(identity, kind) {
  return (((identity || {}).launch || {})[kind] || {});
}

function serveAgentDesiredLaunchValue(identity, field) {
  const launch = serveAgentLaunch(identity, "desired");
  if (!launch[field]) return "";
  return requiredIdentityText(launch[field], "desired " + field);
}

function serveAgentActualLaunchValue(identity, field) {
  return String(serveAgentLaunch(identity, "actual")[field] || "").trim();
}

function serveAgentThreadId(identity) {
  const thread = (identity || {}).thread || {};
  const state = identityPayloadState(thread, "serve thread identity");
  if (state === "unbound") return "";
  if (state === "mismatch") {
    return thread.threadId === undefined
      ? ""
      : requiredIdentityText(thread.threadId, "thread id");
  }
  if (state !== "bound")
    throw new Error("invalid serve thread identity state: " + (state || "-"));
  return requiredIdentityText(thread.threadId, "thread id");
}

function identityDisplayPair(actual, desired) {
  const actualText = String(actual || "").trim();
  const desiredText = String(desired || "").trim();
  if (actualText && desiredText && actualText !== desiredText)
    return actualText + " -> " + desiredText;
  return desiredText || actualText;
}

function agentBranchLabel(agentName, branchName) {
  const agent = agentName || "";
  const branch = branchName || "this branch";
  if (!agent || agent === branch) return branch;
  return agent + " on " + branch;
}

function targetIdentityDisplayLabel(identity) {
  return agentBranchLabel(
    targetIdentityAgentName(identity),
    targetIdentityBranch(identity),
  );
}

function targetIdentityThreadId(identity) {
  const thread = (identity || {}).thread || {};
  const state = targetIdentityThreadState(identity);
  if (state === "unbound") return "";
  if (state === "mismatch") {
    return thread.threadId === undefined
      ? ""
      : requiredIdentityText(thread.threadId, "thread id");
  }
  if (state !== "bound")
    throw new Error("invalid thread identity state: " + (state || "-"));
  return requiredIdentityText(thread.threadId, "thread id");
}

function targetIdentityThreadState(identity) {
  return identityPayloadState(((identity || {}).thread || {}), "thread identity");
}

function teamIdentityTeamId(identity) {
  const state = identityPayloadState(identity, "team identity");
  if (state === "none") return "";
  if (state !== "member")
    throw new Error("invalid team identity state: " + (state || "-"));
  return requiredIdentityText(identity.teamId, "team id");
}

function teamIdentityRevision(identity) {
  return teamIdentityNumber(identity, "teamRevision", "team revision");
}

function teamIdentityConfigRevision(identity) {
  return teamIdentityNumber(identity, "configRevision", "config revision");
}

function teamIdentityNumber(identity, field, label) {
  const state = identityPayloadState(identity, "team identity");
  if (state === "none") return 0;
  if (state !== "member")
    throw new Error("invalid team identity state: " + (state || "-"));
  return nonnegativeIdentityNumber(identity[field], label);
}

function identityPayloadState(identity, label) {
  const state = String((identity || {}).state || "").trim();
  if (!state) throw new Error(label + " state is required");
  return state;
}

function requiredIdentityText(value, label) {
  const text = String(value || "").trim();
  if (!text) throw new Error(label + " must be non-empty");
  return text;
}

function nonnegativeIdentityNumber(value, label) {
  const number = Number(value);
  if (!Number.isFinite(number) || number < 0)
    throw new Error(label + " must be non-negative");
  return Math.floor(number);
}

function statusLineWithRetainedSummary(lane, statusLine) {
  const previous = lane.lastRenderedStatusLine;
  if (!previous || statusLine.error || statusLine.latestActivityPreview)
    return statusLine;
  return {
    ...statusLine,
    lastAssistantAt:
      statusLine.lastAssistantAt || previous.lastAssistantAt || "",
    preview: statusLine.preview || previous.preview || "",
  };
}

function applyRetainedLaneStatus(lane, rawStatusLine) {
  const statusLine = statusLineWithRetainedSummary(lane, rawStatusLine);
  const liveStatusLine = {
    ...statusLine,
    agentVisualStatus: liveAgentVisualStatus(statusLine),
  };
  setAgentStatusPip(lane, liveStatusLine.agentVisualStatus);
  setLaneStatus(lane, liveStatusLine);
  lane.lastRenderedStatusLine = liveStatusLine;
  return liveStatusLine;
}

function liveAgentVisualStatus(statusLine) {
  const processStatus = statusLine.agentProcessStatus || "";
  const backendStatus =
    statusLine.agentVisualStatus || processStatus || "unknown";
  if (processStatus !== "running") return backendStatus;
  if (
    statusLine.activityStatus === "active-ish" ||
    statusLine.activityStatus === "inactive"
  )
    return "running-stale";
  const ageSeconds = relativeAgeSeconds(statusLine.lastAssistantAt);
  if (ageSeconds !== null && ageSeconds >= activeAssistantSeconds)
    return "running-stale";
  return backendStatus;
}

function setAgentStatusPip(lane, status) {
  const normalized = status || "unknown";
  lane.pipEl.dataset.agentStatus = normalized;
  lane.pipEl.title = agentStatusLabel(normalized);
}

function agentStatusLabel(status) {
  if (status === "running") return "agent running";
  if (status === "running-stale") return "agent running, quiet";
  if (status === "idle") return "agent idle";
  if (status === "stopped") return "agent stopped";
  if (status === "unstarted") return "agent unstarted";
  return status ? "agent " + status : "agent status unknown";
}

// ---- pending counts (backend truth + optimistic sends) -----------------------------

function beginLanePendingSubmission(lane) {
  lane.pendingSubmissionCount += 1;
  lane.optimisticPendingInboxCount = lanePendingDisplayCount(lane) + 1;
  renderLaneViewShell(laneGroupHost(lane));
}

function finishLanePendingSubmission(lane, options = {}) {
  const accepted = Boolean(options.accepted);
  const hasBackendCount = Number.isFinite(Number(options.pendingInboxCount));
  const inboxKey = String(options.inboxKey || "");
  lane.pendingSubmissionCount = Math.max(0, lane.pendingSubmissionCount - 1);
  if (hasBackendCount) applyLaneBackendPendingPayload(lane, options);
  const submittedPendingFloor = hasBackendCount
    ? lane.backendPendingInboxCount
    : lane.optimisticPendingInboxCount;
  if (accepted && inboxKey && submittedPendingFloor > 0) {
    lane.optimisticSubmittedInboxKeys.add(inboxKey);
    lane.optimisticPendingInboxFloor = Math.max(
      lane.optimisticPendingInboxFloor,
      submittedPendingFloor,
    );
  }
  if (accepted && !hasBackendCount) {
    lane.optimisticPendingInboxCount = Math.max(
      lane.backendPendingInboxCount,
      lane.optimisticPendingInboxCount,
    );
  } else if (!lane.pendingSubmissionCount) {
    lane.optimisticPendingInboxCount = Math.max(
      lane.backendPendingInboxCount,
      laneSubmittedMessagePendingFloor(lane),
    );
  }
  renderLaneViewShell(laneGroupHost(lane));
  syncComposerPlaceholders(laneGroupHost(lane));
}

function syncLaneBackendPending(lane, payload) {
  applyLaneBackendPendingPayload(lane, payload);
  reconcileSubmittedMessagePredictions(lane);
  clearDrainedSubmittedMessagePredictions(lane);
  if (lane.pendingSubmissionCount > 0) {
    lane.optimisticPendingInboxCount = Math.max(
      lane.optimisticPendingInboxCount,
      lane.backendPendingInboxCount,
      laneSubmittedMessagePendingFloor(lane),
    );
  } else {
    lane.optimisticPendingInboxCount = Math.max(
      lane.backendPendingInboxCount,
      laneSubmittedMessagePendingFloor(lane),
    );
  }
}

function applyLaneBackendPendingPayload(lane, payload) {
  const identity = pendingIdentityFromPayload(payload);
  lane.backendPendingInboxCount = identity.count;
  if (identity.keys !== null) {
    lane.backendPendingInboxKeys = new Set(identity.keys);
    lane.backendPendingInboxRevision = identity.revision;
    lane.backendPendingInboxKeysAuthoritative = true;
  } else {
    lane.backendPendingInboxKeysAuthoritative = false;
  }
}

function pendingIdentityFromPayload(payload) {
  const source =
    payload && typeof payload === "object" ? payload : { pendingInboxCount: payload };
  const count = Math.max(0, Number(source.pendingInboxCount) || 0);
  const keys = Array.isArray(source.pendingInboxKeys)
    ? source.pendingInboxKeys.map((key) => String(key)).filter(Boolean)
    : null;
  return {
    count,
    keys,
    revision: String(source.pendingInboxRevision || ""),
  };
}

function lanePendingDisplayCount(lane) {
  return Math.max(
    lane.backendPendingInboxCount || 0,
    lane.optimisticPendingInboxCount || 0,
  );
}

function clearDrainedSubmittedMessagePredictions(lane) {
  if (lane.backendPendingInboxCount > 0) return;
  if (Math.max(0, Number(lane.pendingSubmissionCount) || 0) > 0) return;
  if (!lane.optimisticSubmittedInboxKeys.size) return;
  lane.optimisticSubmittedInboxKeys.clear();
  lane.optimisticPendingInboxFloor = 0;
}

function laneSubmittedMessagePendingFloor(lane) {
  return lane.optimisticSubmittedInboxKeys.size
    ? Math.max(0, Number(lane.optimisticPendingInboxFloor) || 0)
    : 0;
}

function reconcileSubmittedMessagePredictions(lane) {
  if (!lane.optimisticSubmittedInboxKeys.size) {
    lane.optimisticPendingInboxFloor = 0;
    return;
  }
  if (lane.backendPendingInboxKeysAuthoritative) {
    for (const key of [...lane.optimisticSubmittedInboxKeys]) {
      if (!lane.backendPendingInboxKeys.has(key))
        lane.optimisticSubmittedInboxKeys.delete(key);
    }
    if (!lane.optimisticSubmittedInboxKeys.size)
      lane.optimisticPendingInboxFloor = 0;
    return;
  }
  const ackedKeys = new Set(ackKeysForMessages(lane.knownMessages));
  for (const key of [...lane.optimisticSubmittedInboxKeys]) {
    if (ackedKeys.has(key)) lane.optimisticSubmittedInboxKeys.delete(key);
  }
  if (!lane.optimisticSubmittedInboxKeys.size)
    lane.optimisticPendingInboxFloor = 0;
}

// ---- status line ------------------------------------------------------------------

function setLaneStatus(lane, statusLine) {
  const preview = statusLine.preview || "";
  const previewHasTime = Boolean(preview && statusLine.lastAssistantAt);
  const status = statusLine.error
    ? { error: statusLine.error, time: "", preview: "" }
    : {
        error: "",
        time: previewHasTime ? relativeTime(statusLine.lastAssistantAt) : "",
        preview: previewHasTime ? preview : "",
      };
  const fingerprint = status.error + "\u0000" + status.time + "\u0000" + status.preview;
  if (fingerprint === lane.renderedStatusFingerprint) return;
  lane.renderedStatusFingerprint = fingerprint;
  setLaneStatusText(lane.statusErrorEl, status.error);
  const hasTime = setLaneStatusText(lane.statusTimeEl, status.time);
  const hasPreview = setLaneStatusText(lane.statusPreviewEl, status.preview);
  lane.statusSeparatorEl.hidden = !(hasTime && hasPreview);
  if (status.time) {
    lane.statusTimeEl.dataset.relativeTimestamp = statusLine.lastAssistantAt;
  }
}

function setLaneStatusText(node, text) {
  const value = text || "";
  if (node.textContent !== value) node.textContent = value;
  node.hidden = !value;
  return Boolean(value);
}

function setLaneTransientStatus(lane, text) {
  if (lane.statusTransientTimer) clearTimeout(lane.statusTransientTimer);
  setLaneStatusText(lane.statusErrorEl, text);
  lane.statusTransientTimer = setTimeout(() => {
    lane.statusTransientTimer = null;
    lane.renderedStatusFingerprint = "";
    if (lane.lastRenderedStatusLine)
      setLaneStatus(lane, { ...lane.lastRenderedStatusLine, error: "" });
  }, transientLaneStatusMilliseconds);
}

function setGlobalTransientStatus(text) {
  if (globalTransientStatusTimer) clearTimeout(globalTransientStatusTimer);
  globalStatusEl.textContent = text;
  globalTransientStatusTimer = setTimeout(() => {
    globalTransientStatusTimer = null;
    if (globalStatusEl.textContent === text) globalStatusEl.textContent = "";
  }, transientGlobalStatusMilliseconds);
}

// ---- messages -----------------------------------------------------------------------

function renderMessage(lane, item) {
  if (item.kind === "compaction") return renderCompactionDivider(lane, item);
  if (isPresenceMessage(item)) return null;
  const article = document.createElement("article");
  const maximAckCount = itemMaximAckCount(lane, item);
  if (item.ack_count) article.classList.add("acked");
  if (item.kind === "final") article.classList.add("final");
  if (item.image_only) article.classList.add("image-only");
  article.dataset.messageKey = item.key;
  article.id = messageDomId(item.key);
  if (item.threadId) article.dataset.threadId = item.threadId;
  if (laneShouldAttributeMessages(lane)) {
    const accentSlot = laneMessageAccentIndex(lane, item);
    article.dataset.accentSlot = String(accentSlot);
    article.style.setProperty(
      "--message-occupant-accent",
      messageOccupantAccent(accentSlot),
    );
  }
  if (messageIsCurrentSpeech(lane, item)) article.classList.add("now-playing");
  article.append(renderMessageContent(lane, item));
  article.append(renderMessageFooter(lane, item, maximAckCount));
  return article;
}

function messageOccupantAccent(occupant) {
  const index = Math.max(0, Number(occupant) || 0);
  if (index < messageOccupantAccentPalette.length)
    return messageOccupantAccentPalette[index];
  throw new Error("team slot accent requires one of six team slots");
}

function messageDomId(key) {
  return "message-" + String(key || "").replace(/[^A-Za-z0-9_-]+/g, "-");
}

function renderMessageContent(lane, item) {
  const frag = document.createDocumentFragment();
  const segments = item.ack_segments || [];
  if (!segments.length) {
    frag.append(
      makeMessageBody(item.display_html, item.display_text || item.text),
    );
    return frag;
  }
  if (item.preamble_html) frag.append(makeMessageBody(item.preamble_html, ""));
  for (const segment of segments) {
    const quotes = renderSegmentQuotes(lane, segment.keys || []);
    if (quotes) frag.append(quotes);
    if (segment.html) frag.append(makeMessageBody(segment.html, ""));
  }
  return frag;
}

function makeMessageBody(html, fallbackText) {
  const body = document.createElement("div");
  body.className = "message-body";
  body.innerHTML = html || "";
  if (!body.childNodes.length && fallbackText)
    body.append(document.createTextNode(fallbackText));
  return body;
}

function renderSegmentQuotes(lane, keys) {
  const contexts = (keys || [])
    .map((key) => ackContextForKey(lane, key))
    .filter((context) => context && context.text);
  if (!contexts.length) return null;
  const wrap = document.createElement("div");
  wrap.className = "ack-quotes";
  for (const context of contexts) {
    const quote = document.createElement("blockquote");
    quote.className = "ack-quote";
    if (context.priority === maximPriority) quote.classList.add("maxim-quote");
    quote.innerHTML = context.html || "";
    if (!quote.childNodes.length)
      quote.append(document.createTextNode(context.text));
    quote.querySelectorAll("hr").forEach((rule) => rule.remove());
    const attachments = renderAckAttachments(context.attachments || []);
    if (attachments) quote.append(attachments);
    wrap.append(quote);
  }
  return wrap;
}

function renderAckAttachments(attachments) {
  if (!attachments.length) return null;
  const wrap = document.createElement("div");
  wrap.className = "ack-attachments";
  for (const attachment of attachments) {
    const href = attachment.url || "";
    let item;
    if (href) {
      const anchor = document.createElement("a");
      anchor.href = href;
      anchor.target = "_blank";
      anchor.rel = "noopener";
      item = anchor;
    } else {
      item = document.createElement("div");
    }
    item.className = "ack-attachment";
    const img = document.createElement("img");
    img.src = href || "";
    img.alt = attachment.name || "attached image";
    const label = document.createElement("span");
    label.textContent = attachment.name || "image";
    item.append(img, label);
    wrap.append(item);
  }
  return wrap;
}

function renderMessageFooter(lane, item, maximAckCount) {
  const footer = document.createElement("div");
  footer.className = "message-footer";
  const left = document.createElement("div");
  left.className = "message-footer-left";
  const time = document.createElement("time");
  time.dateTime = item.timestamp;
  time.title = item.timestamp;
  time.dataset.relativeTimestamp = item.timestamp;
  time.dataset.relativeFallback = "line " + item.index;
  setRelativeTimeText(time);
  left.append(time);
  const badges = renderBadges(
    item.ack_count || 0,
    item.kind,
    maximAckCount,
    item.task_card_count || 0,
  );
  if (badges) left.append(renderDotSeparator(), badges);
  footer.append(left);
  const right = document.createElement("div");
  right.className = "message-footer-right";
  const agentName = renderMessageAgentName(item);
  if (agentName) right.append(agentName, renderDotSeparator());
  if (!item.image_only) appendSpeechAction(right, lane, item);
  appendQuoteAction(right, lane, item);
  if (!item.image_only) appendCopyAction(right, lane, item);
  footer.append(right);
  return footer;
}

// The producing agent's name leads the footer actions; clicking it copies that
// agent's UUID.
function renderMessageAgentName(item) {
  const threadId = item.threadId || "";
  const button = document.createElement("button");
  button.type = "button";
  button.className = "message-agent-name";
  const name = agentNameForThread(threadId) || "Agent";
  button.textContent = name;
  if (!threadId) {
    button.disabled = true;
    return button;
  }
  button.title = "Copy agent id\n" + threadId;
  button.addEventListener("click", () => {
    writeClipboardText(threadId).then((ok) => {
      if (ok) flashCopied(button);
    });
  });
  return button;
}

function agentNameForThread(threadId) {
  if (!threadId) return "";
  for (const lane of laneStates.values()) {
    if (lane.activeThreadId === threadId)
      return lane.agentName || lane.branchName || "";
  }
  return "";
}

function appendSpeechAction(parent, lane, item) {
  const speech = messageSpeechUtterances(item);
  if (!speech.length) return;
  const speechLane = speechLaneForMessage(lane, item);
  if (!speechLane) return;
  const play = document.createElement("button");
  play.type = "button";
  play.className = "icon-button speech-button";
  play.dataset.speechFor = item.key;
  applySpeechButtonState(play, messageIsCurrentSpeech(lane, item));
  play.addEventListener("click", () =>
    toggleMessageSpeech(lane, item, speechLane),
  );
  parent.append(play);
}

function speechLaneForMessage(lane, item) {
  const targetId = item.producerTargetId || lane.targetId;
  return laneStates.get(targetId) || null;
}

function appendQuoteAction(parent, lane, item) {
  const quote = document.createElement("button");
  quote.type = "button";
  quote.className = "icon-button quote-button";
  quote.title = "Quote message";
  quote.textContent = "❝";
  quote.addEventListener("click", () => quoteMessageIntoComposer(lane, item));
  parent.append(quote);
}

function appendCopyAction(parent, lane, item) {
  const copy = document.createElement("button");
  copy.type = "button";
  copy.className = "icon-button copy-button";
  copy.title = "Copy message";
  copy.textContent = "⧉";
  copy.addEventListener("click", () => {
    const text = messageCopyText(lane, item);
    if (!text) return;
    writeClipboardText(text).then((ok) => {
      if (ok) flashCopied(copy);
    });
  });
  parent.append(copy);
}

function messageCopyText(lane, item) {
  const body = item.display_text || item.text || "";
  const quotes = [];
  const seen = new Set();
  for (const key of item.ack_keys || []) {
    if (!key || seen.has(key)) continue;
    seen.add(key);
    const context = ackContextForKey(lane, key);
    if (context && context.text) quotes.push(markdownBlockQuote(context.text));
  }
  const parts = [...quotes];
  if (body) parts.push(body);
  return parts.join("\n\n");
}

function markdownBlockQuote(raw) {
  return String(raw || "")
    .replace(/\r\n?/g, "\n")
    .split("\n")
    .map((line) => "> " + line)
    .join("\n")
    .trim();
}

function itemMaximAckCount(lane, item) {
  let count = 0;
  for (const key of item.ack_keys || []) {
    const context = ackContextForKey(lane, key);
    if (context && context.priority === maximPriority) count += 1;
  }
  return count;
}

function renderBadges(ackCount, kind, maximAckCount, taskCardCount) {
  const visibleAckCount = Math.max(0, ackCount - maximAckCount);
  const visibleTaskCount = Math.max(0, Number(taskCardCount) || 0);
  if (
    !maximAckCount &&
    !visibleAckCount &&
    !visibleTaskCount &&
    kind !== "final"
  )
    return null;
  const badges = document.createElement("div");
  badges.className = "badges";
  const add = (label, className, count) => {
    const badge = document.createElement("span");
    badge.className = className ? "badge " + className : "badge";
    const text = document.createElement("span");
    text.className = "badge-label";
    text.textContent = label;
    badge.append(text);
    if (count !== undefined && count !== null && count !== "") {
      const countEl = document.createElement("span");
      countEl.className = "badge-count";
      countEl.textContent = String(count);
      badge.append(countEl);
    }
    badges.append(badge);
  };
  if (visibleAckCount) add("ACK", "", visibleAckCount);
  if (visibleTaskCount) add("TASK", "task-badge", visibleTaskCount);
  if (kind === "final") add("FINAL", "final-badge");
  if (maximAckCount) add("MAXIM", "maxim-badge");
  return badges;
}

function renderCompactionDivider(lane, item) {
  const divider = document.createElement("div");
  divider.className = "compaction-divider";
  divider.title = item.timestamp;
  const accentSlot = laneMessageAccentIndex(lane, item);
  divider.dataset.accentSlot = String(accentSlot);
  divider.style.setProperty("--compaction-accent", messageOccupantAccent(accentSlot));
  const time = document.createElement("time");
  time.dateTime = item.timestamp;
  time.dataset.relativeTimestamp = item.timestamp;
  setRelativeTimeText(time);
  const label = document.createElement("span");
  label.className = "compaction-label";
  label.textContent = compactionAgentLabel(lane, item) + " compacted context";
  const meta = document.createElement("span");
  meta.className = "compaction-meta";
  meta.append(time, renderDotSeparator(), label);
  divider.append(meta);
  return divider;
}

function compactionAgentLabel(lane, item) {
  return (
    agentNameForThread(item.threadId || "") ||
    lane.agentName ||
    lane.branchName ||
    "Agent"
  );
}

function renderDotSeparator() {
  const dot = document.createElement("span");
  dot.className = "dot-separator";
  dot.textContent = "·";
  return dot;
}

// ---- relative time --------------------------------------------------------------------

function updateLiveRelativeTimes() {
  for (const element of document.querySelectorAll(
    "[data-relative-timestamp]",
  )) {
    setRelativeTimeText(element);
  }
  for (const lane of laneStates.values()) {
    if (lane.latestPayload)
      applyRetainedLaneStatus(lane, lane.latestPayload.statusLine || {});
  }
  updateLiveTargetChoiceMetadata();
  for (const lane of laneStates.values()) syncFusedLaneStatusLine(lane);
}

function setRelativeTimeText(element) {
  const timestamp = element.dataset.relativeTimestamp || "";
  const fallback = element.dataset.relativeFallback || "";
  const text = relativeTime(timestamp);
  element.textContent = text || fallback;
}

function relativeTime(raw) {
  const seconds = relativeAgeSeconds(raw);
  if (seconds === null) return "";
  if (seconds < secondsPerMinute) return fixedRelativeTime(seconds, "s");
  const minutes = Math.floor(seconds / secondsPerMinute);
  if (minutes < minutesPerHour) return fixedRelativeTime(minutes, "m");
  const hours = Math.floor(minutes / minutesPerHour);
  if (hours < hoursPerDay) return fixedRelativeTime(hours, "h");
  return fixedRelativeTime(Math.floor(hours / hoursPerDay), "d");
}

function relativeAgeSeconds(raw) {
  const parsed = Date.parse(raw || "");
  if (Number.isNaN(parsed)) return null;
  return Math.max(0, Math.floor((Date.now() - parsed) / 1000));
}

function fixedRelativeTime(value, unit) {
  return String(value).padStart(2, " ") + unit;
}

// ---- clipboard ------------------------------------------------------------------------

function writeClipboardText(text) {
  if (navigator.clipboard && navigator.clipboard.writeText) {
    return navigator.clipboard
      .writeText(text)
      .then(() => true)
      .catch(() => textareaCopyText(text));
  }
  return Promise.resolve(textareaCopyText(text));
}

function textareaCopyText(text) {
  // navigator.clipboard is unavailable over plain-http LAN access (phones),
  // so use a hidden textarea + execCommand for those sessions.
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.append(area);
  area.select();
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch (error) {
    ok = false;
  }
  area.remove();
  return ok;
}

function flashCopied(button) {
  button.classList.add("icon-button--active");
  setTimeout(() => button.classList.remove("icon-button--active"), 600);
}

function targetApi(targetId, path = "") {
  return "/api/work/trees/" + encodeURIComponent(targetId) + path;
}
