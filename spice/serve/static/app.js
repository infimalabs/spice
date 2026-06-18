// spice serve UI — operator interface over worktree lanes.
//
// A lane is an operator-owned container over one concrete worktree target; an
// agent (thread) is an occupant of the lane, not the lane itself. Which lanes
// exist is server truth: opening an agent creates a team, fusing lanes merges
// teams, closing a lane closes its team. One WebSocket live bus carries
// request/response (requestId echo) plus push frames; new transcript lines
// render the moment they land.

"use strict";

const lanesEl = /** @type {HTMLElement} */ (document.querySelector("#swimlanes"));
const openLaneButton = /** @type {HTMLButtonElement} */ (
  document.querySelector("#open-lane")
);
const filterStripEl = /** @type {HTMLElement} */ (
  document.querySelector("#filter-strip")
);
const globalStatusEl = /** @type {HTMLElement} */ (
  document.querySelector("#global-status")
);

const messageLimit = 400;
const initialRequestLimit = 25;
const requestLimit = 50;
const hydrateScrollThresholdPx = 64;
const lanePaneCollapseScrollRate = 1;
const liveBusHeartbeatIntervalMs = 15 * 1000;
const liveBusLivenessTimeoutMs = 35 * 1000;
const liveBusReconnectBaseMs = 500;
const liveBusReconnectMaxMs = 10 * 1000;
const relativeTimeTickMs = 1000;
const laneStorageKey = "spice.serve.laneConfigs";
const speechModes = ["quiet", "speak", "narrate"];
const defaultSpeechMode = "speak";
const maximPriority = "maxim";
const agentLifetimeLabels = ["Steer", "Drive", "Drain"];
const defaultAgentLifetimeIndex = 1;
const defaultAgentLifetime = agentLifetimeLabels[defaultAgentLifetimeIndex];
const serveBrandName = String(spiceServeBranding.name || "spice").trim() || "spice";
const agentLifetimeHelp = {
  Steer: "Manual filters only",
  Drive: "Auto-subscribe to projects this team creates or claims",
  Drain: "Boundary dissolved: see all assignable work",
};
const laneViewModes = ["compose", "filters", "metrics", "info"];
const defaultLaneViewMode = "compose";
const composerAttachmentMaxItems = 8;
const composerAttachmentMaxBytes = 8 * 1024 * 1024;

const laneStates = new Map();

function agentLifetimeAutoManagesTasks(lifetime) {
  return lifetime === "Drive";
}

function agentLifetimeUsesStoredTaskFilters(lifetime) {
  return lifetime === "Steer" || lifetime === "Drive";
}

function agentLifetimeDissolvesTaskBoundary(lifetime) {
  return lifetime === "Drain";
}

function agentLifetimeHelpText(lifetime) {
  return agentLifetimeHelp[lifetime] || agentLifetimeHelp[defaultAgentLifetime];
}

function serveBrandMenuTitle() {
  return "Open " + serveBrandName + " menu";
}
let targets = [];
let targetById = new Map();
let targetsLoaded = false;
let targetsLoading = false;
let targetsLoadPromise = null;
let taskFilterStemPills = [];
let renderedFilterPillsFingerprint = "";
let spiceMenuEl = null;
let spiceMenuPositionHandler = null;
let spiceMenuDismissHandler = null;
let spiceMenuKeyHandler = null;
let spiceMenuDragTargetId = "";
let spiceMenuTargetDragState = null;
let fastModeEnabled = false;
let teamSnapshotRevision = 0;
let sessionOpenTargetIds = new Set();

function laneViewMode(value) {
  return laneViewModes.includes(value || "") ? value : defaultLaneViewMode;
}

function laneViewModeIndex(view) {
  return laneViewModes.indexOf(laneViewMode(view));
}

function uniqueStringList(values) {
  const unique = [];
  for (const value of values || []) {
    if (value && !unique.includes(value)) unique.push(value);
  }
  return unique;
}

function canonicalThreadActorId(threadId) {
  return String(threadId || "")
    .trim()
    .replaceAll("-", "")
    .toLowerCase();
}

function browserStorage() {
  try {
    return window.localStorage || null;
  } catch (error) {
    return null;
  }
}

async function init() {
  await connectLiveBus();
  await refreshServerTopology();
  setInterval(updateLiveRelativeTimes, relativeTimeTickMs);
}

window.addEventListener("error", (event) => {
  setGlobalTransientStatus(event.message || "browser error");
});
window.addEventListener("unhandledrejection", (event) => {
  setGlobalTransientStatus(String(event.reason || "browser promise error"));
});
window.addEventListener("beforeunload", (event) => {
  if (!servePageHasUnsafeComposerState()) return;
  event.preventDefault();
  event.returnValue = unsafeDraftWarningText();
  return event.returnValue;
});
openLaneButton.addEventListener("click", (event) => {
  event.preventDefault();
  toggleSpiceMenu();
});

init().catch((error) => setGlobalTransientStatus(String(error)));
