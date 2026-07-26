// Message and chrome rendering. Messages lay out in the agent's order: leading
// prose, then each ACK as its steering-context quote(s) followed by that ACK's
// response. Status pips show transcript-activity recency independently of
// lifecycle labels, and pending counts blend backend truth with optimistic sends.

const secondsPerMinute = 60;
const minutesPerHour = 60;
const hoursPerDay = 24;
const activeAssistantSeconds = 60;
const activeishAssistantSeconds = 5 * secondsPerMinute;
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
/** @type {number | null} */
let globalTransientStatusTimer = null;
let globalTransientStatusText = "";
let globalTransientStatusIsError = false;
let globalTransientStatusTimestamp = "";
let globalActivityStatusText = "";

/** Apply the one server-owned chrome envelope carried by an ingress payload. */
// Parsing a subtree out of an innerHTML string and querying a child back returns
// a node the checker only knows as possibly null, so the text assignment that
// always follows cannot be checked -- rename one of those classes and nothing
// fails until the browser runs. Building the span hands back a node present by
// construction, which turns that rename into a reference error instead.
function serveSpanWithClass(className) {
  const span = document.createElement("span");
  span.className = className;
  return span;
}

function applyLaneChromePayload(payload) {
  const chrome = (payload || {}).chrome;
  return chrome ? laneStore.applyLaneChrome(chrome) : null;
}

function laneChromeRecord(subject) {
  const targetId =
    typeof subject === "string" ? subject : String((subject || {}).targetId || "");
  return laneStore.laneChrome(targetId);
}

function laneChromeTaskBoard(subject) {
  return (laneChromeRecord(subject) || {}).taskBoard || {};
}

/** @returns {LaneChromePendingInbox} */
function laneChromePendingInbox(subject) {
  return (laneChromeRecord(subject) || {}).pendingInbox || {
    count: 0,
    label: "0",
    keys: [],
  };
}

function laneChromeRenewal(subject) {
  return (laneChromeRecord(subject) || {}).renewal || {};
}

function laneChromeTeamIdentity(subject) {
  return (
    ((laneChromeRecord(subject) || {}).teamConfig || {}).teamIdentity || {
      state: "none",
    }
  );
}

function laneChromeActivity(subject) {
  return (laneChromeRecord(subject) || {}).activity || {};
}

function laneChromeLifecycle(subject) {
  return (laneChromeRecord(subject) || {}).lifecycle || {};
}

function laneChromeConfigRevision(subject) {
  return teamIdentityConfigRevision(laneChromeTeamIdentity(subject));
}

function laneChromeTeamRevision(subject) {
  return teamIdentityRevision(laneChromeTeamIdentity(subject));
}

function laneChromeTeamId(subject) {
  return teamIdentityTeamId(laneChromeTeamIdentity(subject));
}

/**
 * Browser-only status projection. Transport StatusLine owns structural agent
 * status; pending and transcript recency are overlaid from canonical chrome.
 *
 * @typedef {StatusLine & {
 *   pendingInboxCount: number,
 *   pendingInboxLabel: string,
 *   pendingInboxKeys: !Array<string>,
 *   lastAssistantAt: string
 * }} LaneStatusPresentation
 */

/**
 * @param {Object} lane
 * @param {StatusLine} statusLine
 * @returns {LaneStatusPresentation}
 */
function laneChromeStatusLine(lane, statusLine = {}) {
  const pending = laneChromePendingInbox(lane);
  const activity = laneChromeActivity(lane);
  const lifecycle = laneChromeLifecycle(lane);
  return {
    ...statusLine,
    pendingInboxCount: Math.max(0, Number(pending.count) || 0),
    pendingInboxLabel: String(pending.label || pending.count || 0),
    pendingInboxKeys: Array.isArray(pending.keys) ? pending.keys : [],
    lastAssistantAt: String(activity.lastAssistantAt || ""),
    agentProcessStatus:
      String(lifecycle.processStatus || "") ||
      String(statusLine.agentProcessStatus || ""),
    agentVisualStatus:
      String(lifecycle.visualStatus || "") ||
      String(statusLine.agentVisualStatus || ""),
    bindingStatus:
      String(lifecycle.bindingStatus || "") ||
      String(statusLine.bindingStatus || ""),
    rolloutStatus:
      String(lifecycle.rolloutStatus || "") ||
      String(statusLine.rolloutStatus || ""),
  };
}

function renderLaneChromeTransition(transition) {
  if (!transition || transition.disposition !== "applied") return;
  const changed = new Set(transition.changedFacets || []);
  const lane = laneStore.laneForId(transition.targetId);
  let inventoryChanged = false;
  if (changed.has("taskBoard")) {
    const board = transition.record.taskBoard || {};
    inventoryChanged = applyTaskFilterInventory(board.taskFilterInventory || {});
  }
  if (!lane) return;
  const host = laneGroupHost(lane);
  if (changed.has("pendingInbox")) {
    reconcileLanePendingChrome(lane);
    if (lane.lastRenderedStatusLine && lane.pipEl)
      applyRetainedLaneStatus(
        lane,
        laneChromeStatusLine(
          lane,
          (lane.latestPayload || {}).statusLine || lane.lastRenderedStatusLine,
        ),
      );
    renderLaneViewBadge(host, "compose");
    syncComposerPlaceholders(host);
  }
  if (changed.has("taskBoard")) {
    if (!inventoryChanged) renderLaneFiltersPane(host);
    renderLaneViewBadge(host, "filters");
    renderFilterPills();
  }
  if (changed.has("renewal")) {
    const renewal = transition.record.renewal || {};
    applyServerLaneLifetime(lane, renewal.lifetime, {
      configRevision: laneChromeConfigRevision(lane),
      supersedePending: true,
    });
    renderLaneFiltersPane(host);
    renderLaneViewBadge(host, "filters");
    renderFilterPills();
    syncComposerShards(
      host,
      laneIsFusedHost(host) ? laneGroupMemberLanes(host) : [host],
    );
  }
  if (
    changed.has("identity") ||
    changed.has("lifecycle") ||
    changed.has("activity")
  ) {
    const statusLine = laneChromeStatusLine(
      lane,
      (lane.latestPayload || {}).statusLine || lane.lastRenderedStatusLine || {},
    );
    applyRetainedLaneStatus(lane, statusLine);
    syncFusedLaneLights(host);
    syncFusedLaneStatusLine(host);
  }
  if (changed.has("teamConfig")) syncLaneTeamMenuButton(host);
}

function subscribeLaneChromeRendering(store) {
  return store.subscribe((change) => {
    if (change.kind === "laneChrome")
      renderLaneChromeTransition(change.transition);
  });
}

if (typeof laneStore !== "undefined") subscribeLaneChromeRendering(laneStore);

/**
 * Apply the non-faceted part of a lane payload. The chrome envelope must have
 * reached ServeLaneStore before this function is allowed to paint.
 *
 * @param {Object} lane
 * @param {LaneChromeSourcePayload} payload
 */
function renderLanePayloadPresentation(lane, payload) {
  const hasIdentity =
    payloadHasField(payload, "targetIdentity") ||
    payloadHasField(payload, "serveAgentIdentity");
  const previousIdentity = hasIdentity
    ? laneIdentityPresentationFingerprint(lane)
    : "";
  applyLaneTargetIdentity(lane, payload);
  applyLaneServeAgentIdentity(lane, payload);
  const identityChanged =
    hasIdentity && previousIdentity !== laneIdentityPresentationFingerprint(lane);
  if (payload.laneInfo) {
    lane.laneInfo = payload.laneInfo;
    renderLaneInfoPane(laneGroupHost(lane));
  }
  if (payload.statusLine) {
    applyRetainedLaneStatus(
      lane,
      laneChromeStatusLine(lane, payload.statusLine),
    );
  }
  const host = laneGroupHost(lane);
  if (identityChanged || payload.statusLine) {
    syncComposerShards(
      host,
      laneIsFusedHost(host) ? laneGroupMemberLanes(host) : [host],
    );
    syncComposerPlaceholders(host);
    syncFusedLaneLights(host);
    syncFusedLaneStatusLine(host);
  }
}

function laneIdentityPresentationFingerprint(lane) {
  return JSON.stringify([
    lane.branchName || "",
    lane.agentName || "",
    lane.driverName || "",
    lane.driverModel || "",
    lane.driverEffort || "",
    lane.driverIconName || "",
    lane.targetThreadId || "",
    lane.activeThreadId || "",
    lane.serveAgentIdentity || {},
  ]);
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

function driverIconAssetPath(driver) {
  const iconPaths = {
    claude: "/static/icons/claude.svg",
    codex: "/static/icons/openai.svg",
    openai: "/static/icons/openai.svg",
  };
  return iconPaths[driver] || "";
}

function driverDisplayLabel(driver) {
  const labels = {
    claude: "Claude driver",
    codex: "Codex driver",
    openai: "OpenAI driver",
  };
  return labels[driver] || "Agent driver";
}

function driverIdentityTooltip(fields) {
  const driver = String(fields.driver || "").trim().toLowerCase();
  const driverName = String(fields.driverName || "").trim();
  const model = String(fields.model || "").trim();
  const effort = String(fields.effort || "").trim();
  const threadId = String(fields.threadId || "").trim();
  const session = String(fields.session || "").trim();
  const parts = [driverDisplayLabel(driver)];
  if (driverName && driverName.toLowerCase() !== driver)
    parts.push("driver: " + driverName);
  if (model) parts.push("model: " + model);
  if (effort) parts.push("effort: " + effort);
  parts.push("thread: " + (threadId || "unbound"));
  if (session) parts.push("session: " + session);
  return parts.join("; ");
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

function agentBranchLabel(agentName, branchName, separator = " on ") {
  const agent = agentName || "";
  const branch = branchName || "this branch";
  if (!agent || agent === branch) return branch;
  return agent + separator + branch;
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
    latestActivityKind: payloadHasField(statusLine, "latestActivityKind")
      ? String(statusLine.latestActivityKind || "")
      : String(previous.latestActivityKind || ""),
    preview: statusLine.preview || previous.preview || "",
  };
}

function applyRetainedLaneStatus(lane, rawStatusLine) {
  const statusLine = statusLineWithRetainedSummary(lane, rawStatusLine);
  const agentVisualStatus = liveAgentVisualStatus(statusLine);
  const liveStatusLine = {
    ...statusLine,
    agentActivityStatus: liveAgentActivityStatus(statusLine, agentVisualStatus),
    agentVisualStatus,
  };
  setAgentStatusPip(
    lane,
    liveStatusLine.agentVisualStatus,
    liveStatusLine.agentActivityStatus,
  );
  setLaneStatus(lane, liveStatusLine);
  lane.lastRenderedStatusLine = liveStatusLine;
  return liveStatusLine;
}

function liveAgentVisualStatus(statusLine) {
  const processStatus = statusLine.agentProcessStatus || "";
  const backendStatus =
    statusLine.agentVisualStatus || processStatus || "unknown";
  const latestActivityKind = String(statusLine.latestActivityKind || "");
  if (processStatus === "running" && latestActivityKind === "final")
    return "idle";
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

function liveAgentActivityStatus(statusLine, visualStatus) {
  if (visualStatus === "running") return "active";
  if (visualStatus === "starting") return "active-ish";
  const ageSeconds = relativeAgeSeconds(statusLine.lastAssistantAt);
  if (ageSeconds !== null) {
    if (ageSeconds < activeAssistantSeconds) return "active";
    if (ageSeconds < activeishAssistantSeconds) return "active-ish";
    return "inactive";
  }
  const backendStatus = String(statusLine.activityStatus || "");
  if (
    backendStatus === "active" ||
    backendStatus === "active-ish" ||
    backendStatus === "inactive"
  )
    return backendStatus;
  return "unknown";
}

function setAgentStatusPip(lane, status, activityStatus) {
  const normalized = status || "unknown";
  lane.pipEl.dataset.agentStatus = normalized;
  lane.pipEl.dataset.agentActivity = activityStatus || "unknown";
  lane.pipEl.title = agentStatusLabel(normalized);
}

function agentStatusLabel(status) {
  if (status === "starting") return "agent starting";
  if (status === "running") return "agent running";
  if (status === "running-stale") return "agent running, quiet";
  if (status === "stopping") return "agent stopping after startup stall";
  if (status === "startup-stalled") return "agent startup stalled";
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
  syncComposerPlaceholders(laneGroupHost(lane));
  syncPendingSubmissionComposerState(lane);
}

function finishLanePendingSubmission(lane, options = {}) {
  const accepted = Boolean(options.accepted);
  const inboxKey = String(options.inboxKey || "");
  lane.pendingSubmissionCount = Math.max(0, lane.pendingSubmissionCount - 1);
  const backendCount = laneChromePendingCount(lane);
  const submittedPendingFloor = Math.max(
    backendCount,
    lane.optimisticPendingInboxCount,
  );
  if (accepted && inboxKey && submittedPendingFloor > 0) {
    lane.optimisticSubmittedInboxKeys.add(inboxKey);
    lane.optimisticPendingInboxFloor = Math.max(
      lane.optimisticPendingInboxFloor,
      submittedPendingFloor,
    );
  }
  if (accepted) {
    lane.optimisticPendingInboxCount = Math.max(
      backendCount,
      lane.optimisticPendingInboxCount,
    );
  } else if (!lane.pendingSubmissionCount) {
    lane.optimisticPendingInboxCount = Math.max(
      backendCount,
      laneSubmittedMessagePendingFloor(lane),
    );
  }
  renderLaneViewShell(laneGroupHost(lane));
  syncComposerPlaceholders(laneGroupHost(lane));
  syncPendingSubmissionComposerState(lane);
}

function syncPendingSubmissionComposerState(lane) {
  if (typeof syncComposerPendingSendState === "function")
    syncComposerPendingSendState(laneGroupHost(lane));
}

function reconcileLanePendingChrome(lane) {
  const backendCount = laneChromePendingCount(lane);
  if (lane.pendingSubmissionCount > 0) {
    lane.optimisticPendingInboxCount = Math.max(
      lane.optimisticPendingInboxCount,
      backendCount,
      laneSubmittedMessagePendingFloor(lane),
    );
    return true;
  }
  reconcileSubmittedMessagePredictions(lane);
  clearDrainedSubmittedMessagePredictions(lane);
  lane.optimisticPendingInboxCount = Math.max(
    backendCount,
    laneSubmittedMessagePendingFloor(lane),
  );
  return true;
}

function laneChromePendingCount(lane) {
  return Math.max(0, Number(laneChromePendingInbox(lane).count) || 0);
}

function lanePendingDisplayCount(lane) {
  return Math.max(
    laneChromePendingCount(lane),
    lane.optimisticPendingInboxCount || 0,
  );
}

function clearDrainedSubmittedMessagePredictions(lane) {
  if (laneChromePendingCount(lane) > 0) return;
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
  const backendKeys = new Set(laneChromePendingInbox(lane).keys || []);
  for (const key of [...lane.optimisticSubmittedInboxKeys]) {
    if (!backendKeys.has(key)) lane.optimisticSubmittedInboxKeys.delete(key);
  }
  if (!lane.optimisticSubmittedInboxKeys.size)
    lane.optimisticPendingInboxFloor = 0;
}

// ---- status line ------------------------------------------------------------------

function setLaneStatus(lane, statusLine) {
  const preview = statusLine.preview || "";
  const previewHasTime = Boolean(preview && statusLine.lastAssistantAt);
  const status =
    globalStatusLineDisplay() ||
    (statusLine.error
      ? {
          source: "lane-error",
          error: statusLine.error,
          time: "",
          timeTimestamp: "",
          preview: "",
        }
      : {
          source: "lane",
          error: "",
          time: previewHasTime ? relativeTime(statusLine.lastAssistantAt) : "",
          timeTimestamp: "",
          preview: previewHasTime ? preview : "",
        });
  const fingerprint =
    status.source +
    "\u0000" +
    status.error +
    "\u0000" +
    status.time +
    "\u0000" +
    (status.timeTimestamp || "") +
    "\u0000" +
    status.preview;
  if (fingerprint === lane.renderedStatusFingerprint) return;
  lane.renderedStatusFingerprint = fingerprint;
  const hasError = setLaneStatusText(lane.statusErrorEl, status.error);
  const hasTime = setLaneStatusText(lane.statusTimeEl, status.time);
  const hasPreview = setLaneStatusText(lane.statusPreviewEl, status.preview);
  lane.statusErrorSeparatorEl.hidden = !(hasError && hasTime);
  lane.statusSeparatorEl.hidden = !(hasTime && hasPreview);
  if (status.time) {
    lane.statusTimeEl.dataset.relativeTimestamp =
      status.timeTimestamp || statusLine.lastAssistantAt;
  } else {
    delete lane.statusTimeEl.dataset.relativeTimestamp;
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
  globalTransientStatusText = text || "";
  globalTransientStatusIsError = false;
  globalTransientStatusTimestamp = "";
  rerenderGlobalStatusLines();
  globalTransientStatusTimer = setTimeout(() => {
    globalTransientStatusTimer = null;
    if (globalTransientStatusText === text) {
      globalTransientStatusText = "";
      globalTransientStatusIsError = false;
      globalTransientStatusTimestamp = "";
      rerenderGlobalStatusLines();
    }
  }, transientGlobalStatusMilliseconds);
}

function setGlobalTransientError(text) {
  if (globalTransientStatusTimer) clearTimeout(globalTransientStatusTimer);
  globalTransientStatusText = text || "";
  globalTransientStatusIsError = true;
  globalTransientStatusTimestamp = globalTransientStatusText
    ? new Date().toISOString()
    : "";
  rerenderGlobalStatusLines();
  globalTransientStatusTimer = setTimeout(() => {
    globalTransientStatusTimer = null;
    if (globalTransientStatusText === text) {
      globalTransientStatusText = "";
      globalTransientStatusIsError = false;
      globalTransientStatusTimestamp = "";
      rerenderGlobalStatusLines();
    }
  }, transientGlobalStatusMilliseconds);
}

function setGlobalActivityStatus(text) {
  globalActivityStatusText = text || "";
  rerenderGlobalStatusLines();
}

function clearGlobalActivityStatus(text) {
  if (text && globalActivityStatusText !== text) return;
  globalActivityStatusText = "";
  rerenderGlobalStatusLines();
}

function globalStatusLineDisplay() {
  if (globalTransientStatusText)
    return globalTransientStatusIsError
      ? {
          source: "global-error",
          error: globalTransientStatusText,
          time: relativeTime(globalTransientStatusTimestamp),
          timeTimestamp: globalTransientStatusTimestamp,
          preview: "",
        }
      : {
          source: "global-transient",
          error: "",
          time: "",
          timeTimestamp: "",
          preview: globalTransientStatusText,
        };
  if (globalActivityStatusText)
    return {
      source: "global-activity",
      error: "",
      time: "",
      timeTimestamp: "",
      preview: globalActivityStatusText,
    };
  return null;
}

function rerenderGlobalStatusLines() {
  for (const lane of laneStore.lanesSnapshot()) {
    lane.renderedStatusFingerprint = "";
    setLaneStatus(lane, lane.lastRenderedStatusLine || {});
  }
}

// ---- messages -----------------------------------------------------------------------

function renderMessage(lane, item) {
  if (item.kind === "compaction") return renderCompactionDivider(lane, item);
  if (isPresenceMessage(item)) return null;
  const article = document.createElement("article");
  const maximAckCount = itemMaximAckCount(lane, item);
  if (item.ack_count) article.classList.add("acked");
  if (item.nack_count) article.classList.add("refused");
  if (item.kind === "final") article.classList.add("final");
  if (item.image_only) article.classList.add("image-only");
  article.dataset.messageKey = item.key;
  const reservationType = messageReservationType(lane, item);
  if (reservationType) article.dataset.mosaicReservationType = reservationType;
  article.id = messageDomId(item.key);
  stampMessageTimestamp(article, item);
  if (item.threadId) article.dataset.threadId = item.threadId;
  if (laneShouldAttributeMessages(lane)) {
    // Stamp the resolved producer so the style-only accent pass
    // (app.stream.js) recomputes the identical slot on composer reorder.
    const producerTargetId = laneMessageProducerTargetId(lane, item);
    if (producerTargetId) article.dataset.producerTargetId = producerTargetId;
    const accentSlot = laneMessageAccentIndex(lane, item);
    article.dataset.accentSlot = String(accentSlot);
    article.style.setProperty(
      "--message-occupant-accent",
      messageOccupantAccent(accentSlot),
    );
  }
  if (messageIsCurrentSpeech(lane, item)) article.classList.add("now-playing");
  article.append(renderMessageContent(lane, item));
  if (article.querySelector(".message-image, .ack-attachment"))
    article.classList.add("media-rich");
  // Multi-image stacking rule: a message-image-stack lays its
  // images out in a single horizontal row (messages.css .message-image-stack
  // is flex-row, not a vertical stack), all at the same letterbox height --
  // so the reservation is exactly one image slot regardless of how many
  // .message-image elements the card contains, never a per-image sum.
  if (!article.dataset.mosaicReservationType && article.querySelector(".message-image"))
    article.dataset.mosaicReservationType = "image";
  article.append(renderMessageFooter(lane, item, maximAckCount));
  return article;
}

function messageReservationType(lane, item) {
  if (messageHasPendingAckContext(lane, item)) return "ack";
  if (item.image_only) return "imageLarge";
  return "";
}

function messageHasPendingAckContext(lane, item) {
  for (const key of messageAckSegmentKeys(item)) {
    // A key that is not yet resolved reserves its quote block whether the
    // lookup is still outstanding (skeleton) or came back missing (a
    // same-height "unavailable" block) -- both hold the reservation so a
    // failed ack never reflows the card. Only a resolved-with-content key
    // measures real content.
    if (key && !ackContextForKey(lane, key)) return true;
  }
  return false;
}

function messageAckSegmentKeys(item) {
  const keys = [];
  for (const segment of item.ack_segments || []) {
    for (const key of segment.keys || []) {
      if (key && !keys.includes(key)) keys.push(key);
    }
  }
  return keys;
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
    // ACK and NACK render through one path; a refusal only flips the polarity
    // class so its quotes and body take the warn accent instead of the ack one.
    // A withheld refusal took neither disposition: it answered no key and names
    // none, so it renders muted and quoteless -- a record of what the message
    // captured, never styled as the acknowledgment it was not.
    const modifier = messageSegmentModifier(segment.disposition);
    const quotes = renderSegmentQuotes(
      lane,
      segment.keys || [],
      modifier === "refused",
    );
    if (quotes) frag.append(quotes);
    if (segment.html) frag.append(makeMessageBody(segment.html, "", modifier));
  }
  return frag;
}

// The body modifier a segment's disposition earns. An acked segment takes the
// bare class, so only the dispositions that restyle it appear here.
const messageSegmentModifiers = Object.freeze({
  refused: "refused",
  withheld: "withheld",
});

function messageSegmentModifier(disposition) {
  return messageSegmentModifiers[String(disposition || "")] || "";
}

function makeMessageBody(html, fallbackText, modifier) {
  const body = document.createElement("div");
  body.className = modifier
    ? "message-body message-body--" + modifier
    : "message-body";
  body.innerHTML = html || "";
  if (!body.childNodes.length && fallbackText)
    body.append(document.createTextNode(fallbackText));
  return body;
}

function renderSegmentQuotes(lane, keys, refused) {
  const refs = (keys || [])
    .filter((key) => key)
    .map((key) => ({
      key,
      context: ackContextForKey(lane, key),
      missing: ackContextConfirmedMissing(lane, key),
    }));
  if (!refs.length) return null;
  const wrap = document.createElement("div");
  wrap.className = refused ? "ack-quotes ack-quotes--refused" : "ack-quotes";
  for (const ref of refs) {
    if (ref.context && ref.context.text) wrap.append(renderAckQuote(ref.context));
    // A confirmed-missing key swaps the shimmer for a same-height failed
    // block so the reservation is honored without a reflow; a still-loading
    // key keeps shimmering.
    else if (ref.missing) wrap.append(renderMissingAckQuote(ref.key));
    else wrap.append(renderPendingAckQuote(ref.key));
  }
  return wrap;
}

function renderAckQuote(context) {
  const quote = document.createElement("blockquote");
  quote.className = "ack-quote";
  if (context.priority === maximPriority) quote.classList.add("maxim-quote");
  quote.innerHTML = context.html || "";
  if (!quote.childNodes.length) quote.append(document.createTextNode(context.text));
  quote.querySelectorAll("hr").forEach((rule) => rule.remove());
  const attachments = renderAckAttachments(context.attachments || []);
  if (attachments) quote.append(attachments);
  return quote;
}

function renderPendingAckQuote(key) {
  const quote = document.createElement("blockquote");
  quote.className = "ack-quote ack-quote--pending";
  quote.dataset.ackContextKey = key;
  quote.dataset.mosaicReservationType = "ack";
  quote.setAttribute("aria-hidden", "true");
  const reservePx = pendingAckQuoteReservePx();
  if (Number.isFinite(reservePx) && reservePx > 0)
    quote.style.setProperty("--ack-quote-reserve", reservePx + "px");
  for (const width of ["wide", "mid"]) {
    const line = document.createElement("span");
    line.className = "ack-quote-skeleton-line ack-quote-skeleton-line--" + width;
    quote.append(line);
  }
  return quote;
}

// Same reserved footprint as the pending skeleton, so a lookup that resolves
// missing swaps in place with zero reflow -- but static (no shimmer) and
// labeled, so the reader sees the context failed to load rather than a
// spinner that never resolves.
function renderMissingAckQuote(key) {
  const quote = document.createElement("blockquote");
  quote.className = "ack-quote ack-quote--missing";
  quote.dataset.ackContextKey = key;
  quote.dataset.mosaicReservationType = "ack";
  const reservePx = pendingAckQuoteReservePx();
  if (Number.isFinite(reservePx) && reservePx > 0)
    quote.style.setProperty("--ack-quote-reserve", reservePx + "px");
  const label = document.createElement("span");
  label.className = "ack-quote-missing-label";
  label.textContent = "context unavailable";
  quote.append(label);
  return quote;
}

function pendingAckQuoteReservePx() {
  if (typeof mosaicReservationPriorPx === "function") {
    const px = mosaicReservationPriorPx("ack", pendingAckRootFontSizePx());
    if (Number.isFinite(px)) return px;
  }
  return 56;
}

function pendingAckRootFontSizePx() {
  if (typeof mosaicRootFontSizePx === "function") return mosaicRootFontSizePx();
  const parsed = Number.parseFloat(
    getComputedStyle(document.documentElement).fontSize,
  );
  return Number.isFinite(parsed) ? parsed : 16;
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
  time.className = "message-age";
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
    item.nack_count || 0,
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
  for (const lane of laneStore.lanesSnapshot()) {
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
  return laneStore.laneForId(targetId) || null;
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

function renderBadges(ackCount, kind, maximAckCount, taskCardCount, nackCount) {
  const visibleAckCount = Math.max(0, ackCount - maximAckCount);
  const visibleNackCount = Math.max(0, Number(nackCount) || 0);
  const visibleTaskCount = Math.max(0, Number(taskCardCount) || 0);
  if (
    !maximAckCount &&
    !visibleAckCount &&
    !visibleNackCount &&
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
  if (visibleNackCount) add("NACK", "nack-badge", visibleNackCount);
  if (visibleTaskCount) add("TASK", "task-badge", visibleTaskCount);
  if (kind === "final") add("FINAL", "final-badge");
  if (maximAckCount) add("MAXIM", "maxim-badge");
  return badges;
}

// The packer and stream barrier synthesis read time from this dataset stamp,
// never by re-parsing <time> elements out of rendered content.
function stampMessageTimestamp(node, item) {
  const parsed = Date.parse(item.timestamp || "");
  if (Number.isFinite(parsed)) node.dataset.messageTs = String(parsed);
}

function renderCompactionDivider(lane, item) {
  const divider = document.createElement("div");
  divider.className = "compaction-divider";
  divider.title = item.timestamp;
  stampMessageTimestamp(divider, item);
  // Marked + thread-stamped so the style-only accent pass (app.stream.js)
  // can recolor it on composer reorder without a message-list lookup.
  divider.dataset.compactionAccentNode = "";
  if (item.threadId) divider.dataset.threadId = item.threadId;
  const producerTargetId = laneMessageProducerTargetId(lane, item);
  if (producerTargetId) divider.dataset.producerTargetId = producerTargetId;
  const accentSlot = laneMessageAccentIndex(lane, item);
  divider.dataset.accentSlot = String(accentSlot);
  divider.style.setProperty("--compaction-accent", messageOccupantAccent(accentSlot));
  const time = document.createElement("time");
  time.className = "message-age";
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

// Hour-bucket rule: absolute time only, no relative refresh — the label is a
// fixed anchor the operator aligns against, not a drifting age.
function renderTimeRule(bucketStartMs) {
  const rule = document.createElement("div");
  rule.className = "time-rule";
  rule.dataset.messageTs = String(bucketStartMs);
  const date = new Date(bucketStartMs);
  const time = document.createElement("time");
  time.dateTime = date.toISOString();
  time.textContent = timeRuleLabel(date);
  rule.append(time);
  return rule;
}

function timeRuleLabel(date) {
  const sameDay = date.toDateString() === new Date().toDateString();
  if (sameDay)
    return date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return date.toLocaleString([], {
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
    month: "short",
  });
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
  const fusedHosts = new Set();
  for (const element of document.querySelectorAll(
    "[data-relative-timestamp]",
  )) {
    setRelativeTimeText(element);
  }
  for (const lane of laneStore.lanesSnapshot()) {
    if (lane.latestPayload) {
      applyRetainedLaneStatus(lane, lane.latestPayload.statusLine || {});
      fusedHosts.add(laneGroupHost(lane));
    }
  }
  updateLiveTargetChoiceMetadata();
  for (const host of fusedHosts) {
    syncFusedLaneLights(host);
    syncFusedLaneStatusLine(host);
  }
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
