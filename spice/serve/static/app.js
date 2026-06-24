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
const fastModeStorageKey = "spice.serve.fastMode";
const speechModes = ["quiet", "speak", "narrate"];
const defaultSpeechMode = "speak";
const maximPriority = "maxim";
const agentLifetimeLabels = ["Steer", "Drive", "Drain"];
const serveBrandName = String(spiceServeBranding.name || "spice").trim() || "spice";
// Default startup lifetime from [tool.spice.serve] default_lifetime; falls back to
// "Drive" so autonomy-on-startup is a stated config choice, not a hidden constant.
const defaultAgentLifetime = agentLifetimeLabels.includes(
  spiceServeBranding.defaultLifetime,
)
  ? spiceServeBranding.defaultLifetime
  : "Drive";
const defaultAgentLifetimeIndex = agentLifetimeLabels.indexOf(defaultAgentLifetime);
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
let taskFilterInventoryRevision = "";
let renderedFilterPillsFingerprint = "";
let spiceMenuEl = null;
let spiceMenuPositionHandler = null;
let spiceMenuDismissHandler = null;
let spiceMenuKeyHandler = null;
let spiceMenuDragTargetId = "";
let spiceMenuTargetDragState = null;
let spiceMenuRenderPending = false;
let spiceMenuNewTeamPlacementHints = [];
let fastModeEnabled = storedFastModeEnabled();
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

function targetTeamActorId(targetId) {
  const id = String(targetId || "").trim();
  return id ? "target:" + id : "";
}

function threadTeamActorId(threadId) {
  const actor = canonicalThreadActorId(threadId);
  return actor ? "thread:" + actor : "";
}

function teamActorKind(actorId) {
  const actor = String(actorId || "").trim();
  if (actor.startsWith("target:")) return "target";
  if (actor.startsWith("thread:")) return "thread";
  return "";
}

function teamActorValue(actorId) {
  const actor = String(actorId || "").trim();
  return teamActorKind(actor) ? actor.slice(actor.indexOf(":") + 1) : actor;
}

function teamActorThreadId(actorId) {
  const actor = String(actorId || "").trim();
  if (actor.startsWith("thread:")) return canonicalThreadActorId(actor.slice(7));
  if (actor.startsWith("target:")) return "";
  return canonicalThreadActorId(actor);
}

function teamActorMatchesThread(actorId, threadId) {
  const actorThreadId = teamActorThreadId(actorId);
  return Boolean(actorThreadId && actorThreadId === canonicalThreadActorId(threadId));
}

function normalizeTeamActorId(actorId) {
  const actor = String(actorId || "").trim();
  if (!actor) return "";
  if (actor.startsWith("target:")) return targetTeamActorId(actor.slice(7));
  if (actor.startsWith("thread:")) return threadTeamActorId(actor.slice(7));
  if (targetById.has(actor)) return targetTeamActorId(actor);
  return threadTeamActorId(actor);
}

function targetTeamAgentId(target) {
  const threadActor = threadTeamActorId(
    targetIdentityThreadId(target.targetIdentity),
  );
  return threadActor || targetTeamActorId(target.id);
}

function targetTeamAgentAliases(target) {
  const actor = targetTeamAgentId(target);
  const aliases = [targetTeamActorId(target.id)].filter(
    (alias) => alias && alias !== actor,
  );
  return uniqueStringList(aliases);
}

function browserStorage() {
  try {
    return window.localStorage || null;
  } catch (error) {
    return null;
  }
}

function storedFastModeEnabled() {
  const storage = browserStorage();
  return storage ? storage.getItem(fastModeStorageKey) === "true" : false;
}

function persistFastModeEnabled(enabled) {
  const storage = browserStorage();
  if (!storage) return;
  storage.setItem(fastModeStorageKey, enabled ? "true" : "false");
}

async function init() {
  await connectLiveBus();
  await refreshServerTopology();
  setInterval(updateLiveRelativeTimes, relativeTimeTickMs);
}

window.addEventListener("error", (event) => {
  setGlobalTransientError(event.message || "browser error");
});
window.addEventListener("unhandledrejection", (event) => {
  setGlobalTransientError(String(event.reason || "browser promise error"));
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

if (typeof syncFastModeButtonState === "function") syncFastModeButtonState();

init().catch((error) => setGlobalTransientError(String(error)));
