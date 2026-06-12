// Targets, the spice context menu, and server-team-backed lane topology. Which lanes
// are open is server truth: opening a tree creates a team, the team snapshot
// reconciles lanes (including fused groups), closing a lane closes its team.
// localStorage keeps only per-target hints (speech mode, selected view).

async function refreshServerTopology() {
  await refreshTargets();
  await refreshTeamSnapshot({ force: true });
}

function refreshTargets() {
  if (targetsLoadPromise) return targetsLoadPromise;
  targetsLoading = true;
  if (!targetsLoaded) globalStatusEl.textContent = "loading work trees";
  if (spiceMenuEl) renderSpiceMenu();
  targetsLoadPromise = (async () => {
    try {
      const response = await liveBusRequest("targets.refresh");
      applyTargetsPayload(response.payload || {});
    } catch (error) {
      setGlobalTransientStatus("work tree refresh failed");
    } finally {
      targetsLoading = false;
      targetsLoadPromise = null;
      if (spiceMenuEl) renderSpiceMenu();
    }
  })();
  return targetsLoadPromise;
}

function applyTargetsPayload(payload) {
  targets = payload.workTrees || [];
  targetById = new Map(targets.map((target) => [target.id, target]));
  targetsLoaded = true;
  globalStatusEl.textContent = "";
  taskFilterStemPills = taskFilterStemPillsFromInventory(
    payload.taskFilterInventory || {},
  );
  for (const lane of [...laneStates.values()]) {
    if (!targetById.has(lane.targetId)) closeLaneCore(lane);
  }
  renderFilterPills();
  for (const lane of laneStates.values()) {
    renderLaneChrome(
      lane,
      lane.latestPayload || targetPayloadShim(targetById.get(lane.targetId)),
    );
  }
  if (spiceMenuEl) renderSpiceMenu();
}

// Targets carry statusLine and route facts in the same field names the lane
// payload uses; the shim only fills the names renderLaneChrome reads.
function targetPayloadShim(target) {
  if (!target) return { statusLine: {} };
  return {
    targetBranch: target.branch || target.displayName || "",
    targetAgentName: target.agentName || "",
    targetThreadId: target.threadId || "",
    taskFilters: target.taskFilters || [],
    laneFilterVersion: target.laneFilterVersion || "",
    teamId: target.teamId || "",
    teamRevision: target.teamRevision || 0,
    configRevision: target.configRevision || 0,
    lifetime: target.lifetime || "",
    taskFilterInventory: target.taskFilterInventory || {},
    laneMetrics: target.laneMetrics || {},
    laneInfo: target.laneInfo || { summaryRows: [], members: [] },
    privateTaskCount: Math.max(0, Number(target.privateTaskCount) || 0),
    statusLine: target.statusLine || {},
  };
}

// ---- team snapshot reconciliation ---------------------------------------------

async function refreshTeamSnapshot(options = {}) {
  const query = {};
  if (!options.force && teamSnapshotRevision)
    query.sinceRevision = teamSnapshotRevision;
  const response = await liveBusRequest("teams.refresh", { query });
  applyTeamSnapshotPayload(response.payload || {}, options);
}

async function requestTeamCommand(payload) {
  const response = await liveBusRequest("teams.command", { payload });
  const result = response.result || {};
  if (result.snapshot)
    applyTeamSnapshotPayload(
      { revision: result.revision, changed: true, snapshot: result.snapshot },
      { force: true },
    );
  if (result.ok === false)
    throw new Error(result.error || "team command failed");
  return result;
}

function teamCommandPayload(command, fields = {}) {
  return { command, expectedRevision: teamSnapshotRevision, ...fields };
}

function applyTeamSnapshotPayload(payload, options = {}) {
  const revision = Math.max(
    0,
    Number(payload.revision || (payload.snapshot || {}).globalRevision || 0),
  );
  if (revision < teamSnapshotRevision) return;
  const changed = payload.changed !== false || options.force;
  teamSnapshotRevision = revision;
  if (!changed) return;
  const openBefore = laneStateTargetIds();
  const teams = (payload.snapshot || {}).teams || [];
  const hints = laneHintsByTargetId();
  const openTargetIds = new Set();
  const groupRuns = [];
  for (const team of teams) {
    const memberTargetIds = teamMemberTargetIds(team);
    if (!memberTargetIds.length) continue;
    for (const targetId of memberTargetIds) {
      openTargetIds.add(targetId);
      ensureTeamMemberLane(targetId, team, hints.get(targetId));
    }
    if (memberTargetIds.length > 1) groupRuns.push(memberTargetIds);
  }
  for (const lane of [...laneStates.values()]) {
    if (openTargetIds.has(lane.targetId)) continue;
    if (laneHasUnsafeDraft(lane) && !lane.serverCloseRequested) continue;
    closeLaneCore(lane);
  }
  reconcileLaneGroups(groupRuns);
  persistLaneHints();
  if (!sameStringSets(openBefore, laneStateTargetIds())) renderSpiceMenu();
  renderFilterPills();
}

function laneStateTargetIds() {
  return new Set(laneStates.keys());
}

function sameStringSets(left, right) {
  if (left.size !== right.size) return false;
  for (const value of left) {
    if (!right.has(value)) return false;
  }
  return true;
}

function teamMemberTargetIds(team) {
  const targetIds = [];
  for (const member of team.members || []) {
    const targetId = teamMemberTargetId(member);
    if (targetId && targetById.has(targetId) && !targetIds.includes(targetId))
      targetIds.push(targetId);
  }
  return targetIds;
}

// A team member is an actor (canonical thread id) or, before any thread is
// bound, the worktree target id itself.
function teamMemberTargetId(member) {
  const agentId = String((member || {}).agentId || "");
  if (targetById.has(agentId)) return agentId;
  for (const target of targets) {
    if (canonicalThreadActorId(target.threadId) === agentId && agentId)
      return target.id;
  }
  return "";
}

function ensureTeamMemberLane(targetId, team, hint = null) {
  if (!laneStates.has(targetId)) addLane(targetId, hint);
  const lane = laneStates.get(targetId);
  if (!lane) return;
  const config = team.config || {};
  lane.teamId = String(team.teamId || "");
  lane.teamRevision = Math.max(0, Number(team.revision || 0));
  lane.configRevision = Math.max(0, Number(config.revision || 0));
  if (Array.isArray(config.taskFilters))
    lane.taskFilters = uniqueStringList(config.taskFilters);
  if (config.lifetime && agentLifetimeLabels.includes(config.lifetime))
    lane.lifetime = config.lifetime;
  if (!hint && config.speechMode && speechModes.includes(config.speechMode))
    lane.speechMode = config.speechMode;
  syncLaneEffectiveControls(lane);
}

function laneTeamAgentId(lane) {
  const actor = canonicalThreadActorId(lane.targetThreadId);
  if (actor) return actor;
  const target = targetById.get(lane.targetId);
  return canonicalThreadActorId(target ? target.threadId : "") || lane.targetId;
}

function laneTeamAgentAliases(lane) {
  const aliases = [];
  if (lane.targetId && lane.targetId !== laneTeamAgentId(lane))
    aliases.push(lane.targetId);
  return aliases;
}

// ---- open / close ---------------------------------------------------------------

async function openTargetTeam(targetId) {
  if (laneStates.has(targetId)) {
    closeSpiceMenu();
    return;
  }
  const target = targetById.get(targetId);
  if (!target) throw new Error("open tree requires a known target");
  sessionOpenTargetIds.add(targetId);
  try {
    await refreshTeamSnapshot({ force: true });
    if (!laneStates.has(targetId)) {
      const hint = laneHintsByTargetId().get(targetId);
      await requestTeamCommand(
        teamCommandPayload("createTeam", {
          members: [canonicalThreadActorId(target.threadId) || targetId],
          config: {
            ...defaultTeamConfig(),
            speechMode: hint ? hint.speechMode : defaultSpeechMode,
            selectedView: hint ? hint.selectedView : defaultLaneViewMode,
          },
        }),
      );
    }
    closeSpiceMenu();
  } catch (error) {
    sessionOpenTargetIds.delete(targetId);
    throw error;
  }
}

function closeLane(lane) {
  const host = laneGroupHost(lane);
  const members = laneGroupMemberLanes(host);
  if (members.length > 1) {
    breakLaneGroup(host);
    return;
  }
  if (laneHasUnsafeDraft(lane)) {
    if (!window.confirm(unsafeDraftWarningText())) return;
  }
  sessionOpenTargetIds.delete(lane.targetId);
  lane.serverCloseRequested = true;
  requestTeamCommand(
    teamCommandPayload("closeTeam", { teamId: lane.teamId }),
  ).catch(() => {
    lane.serverCloseRequested = false;
    setLaneTransientStatus(lane, "close tree failed");
  });
}

function closeLaneCore(lane) {
  lane.closed = true;
  unsubscribeLaneFromLiveBus(lane);
  if (lane.historyObserver) lane.historyObserver.disconnect();
  if (lane.paneResizeObserver) lane.paneResizeObserver.disconnect();
  if (lane.paneMetricsFrame) cancelAnimationFrame(lane.paneMetricsFrame);
  lane.paneMetricsFrame = 0;
  abortLaneSpeech(lane);
  lane.element.remove();
  laneStates.delete(lane.targetId);
  renderFilterPills();
}

function laneHasUnsafeDraft(lane) {
  if (!isLaneOpen(lane)) return false;
  if (laneComposerDraftText(lane).trim()) return true;
  return lane.sendAwaitingBackendCount > 0;
}

function servePageHasUnsafeComposerState() {
  for (const lane of laneStates.values()) {
    if (laneHasUnsafeDraft(lane)) return true;
  }
  return false;
}

function unsafeDraftWarningText() {
  return "Unsubmitted steering has not received a backend key yet. Leave anyway?";
}

// ---- lane hints (operator-local presentation state) ------------------------------

function laneHintsByTargetId() {
  const storage = browserStorage();
  const hints = new Map();
  if (!storage) return hints;
  let parsed = [];
  try {
    parsed = JSON.parse(storage.getItem(laneStorageKey) || "[]");
  } catch (error) {
    parsed = [];
  }
  if (!Array.isArray(parsed)) return hints;
  for (const value of parsed) {
    if (!value || typeof value !== "object") continue;
    const targetId = String(value.targetId || "");
    if (!targetId || hints.has(targetId)) continue;
    hints.set(targetId, {
      targetId,
      speechMode: speechModes.includes(value.speechMode)
        ? value.speechMode
        : defaultSpeechMode,
      selectedView: laneViewMode(value.selectedView),
    });
  }
  return hints;
}

function persistLaneHints() {
  const storage = browserStorage();
  if (!storage) return;
  const hints = [];
  for (const lane of laneStates.values()) {
    if (!isLaneOpen(lane)) continue;
    hints.push({
      targetId: lane.targetId,
      speechMode: lane.speechMode,
      selectedView: lane.selectedView,
    });
  }
  storage.setItem(laneStorageKey, JSON.stringify(hints));
}

// ---- spice context menu -----------------------------------------------------------

function toggleSpiceMenu() {
  if (spiceMenuEl) closeSpiceMenu();
  else openSpiceMenu();
}

function openSpiceMenu() {
  if (spiceMenuEl) {
    positionSpiceMenu();
    return;
  }
  spiceMenuEl = document.createElement("div");
  spiceMenuEl.className = "spice-context-menu";
  spiceMenuEl.setAttribute("role", "menu");
  spiceMenuEl.setAttribute("aria-label", "spice menu");
  document.body.append(spiceMenuEl);
  openLaneButton.setAttribute("aria-expanded", "true");
  spiceMenuPositionHandler = () => positionSpiceMenu();
  spiceMenuDismissHandler = (event) => {
    const target = event.target;
    if (spiceMenuEl && target instanceof Node && spiceMenuEl.contains(target))
      return;
    if (target instanceof Node && openLaneButton.contains(target)) return;
    closeSpiceMenu();
  };
  spiceMenuKeyHandler = (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      closeSpiceMenu();
      openLaneButton.focus();
    }
  };
  window.addEventListener("resize", spiceMenuPositionHandler);
  window.addEventListener("scroll", spiceMenuPositionHandler, true);
  document.addEventListener("pointerdown", spiceMenuDismissHandler, true);
  document.addEventListener("keydown", spiceMenuKeyHandler, true);
  renderSpiceMenu();
  positionSpiceMenu();
  refreshTargets().finally(() => {
    renderSpiceMenu();
    positionSpiceMenu();
  });
}

function closeSpiceMenu() {
  if (!spiceMenuEl) return;
  if (spiceMenuPositionHandler) {
    window.removeEventListener("resize", spiceMenuPositionHandler);
    window.removeEventListener("scroll", spiceMenuPositionHandler, true);
    spiceMenuPositionHandler = null;
  }
  if (spiceMenuDismissHandler) {
    document.removeEventListener("pointerdown", spiceMenuDismissHandler, true);
    spiceMenuDismissHandler = null;
  }
  if (spiceMenuKeyHandler) {
    document.removeEventListener("keydown", spiceMenuKeyHandler, true);
    spiceMenuKeyHandler = null;
  }
  spiceMenuEl.remove();
  spiceMenuEl = null;
  openLaneButton.setAttribute("aria-expanded", "false");
}

function renderSpiceMenu() {
  if (!spiceMenuEl) return;
  spiceMenuEl.replaceChildren(
    renderSpiceMenuActions(),
    renderSpiceMenuTargets(),
  );
}

function positionSpiceMenu() {
  if (!spiceMenuEl) return;
  const margin = 8;
  const anchor = openLaneButton.getBoundingClientRect();
  const width = Math.min(360, window.innerWidth - margin * 2);
  const left = Math.max(
    margin,
    Math.min(window.innerWidth - width - margin, anchor.right - width),
  );
  spiceMenuEl.style.width = width + "px";
  spiceMenuEl.style.left = left + "px";
  spiceMenuEl.style.top = anchor.bottom + margin + "px";
  const rect = spiceMenuEl.getBoundingClientRect();
  if (rect.bottom <= window.innerHeight - margin) return;
  spiceMenuEl.style.top =
    Math.max(margin, anchor.top - rect.height - margin) + "px";
}

function renderSpiceMenuActions() {
  const section = document.createElement("section");
  section.className = "spice-menu-section";
  const heading = document.createElement("div");
  heading.className = "spice-menu-heading";
  heading.textContent = "global";
  const actions = document.createElement("div");
  actions.className = "spice-menu-actions";
  actions.append(
    renderSpiceMenuAction({
      label: "Fast mode",
      detail: fastModeEnabled ? "on" : "off",
      pressed: fastModeEnabled,
      onClick: () => setFastModeEnabled(!fastModeEnabled),
    }),
    renderSpiceMenuAction({
      label: "New empty team",
      detail: "no agents",
      onClick: () => createEmptyTeamFromMenu(),
    }),
  );
  section.append(heading, actions);
  return section;
}

function renderSpiceMenuAction({ label, detail = "", pressed = null, onClick }) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "spice-menu-action";
  button.setAttribute(
    "role",
    pressed === null ? "menuitem" : "menuitemcheckbox",
  );
  if (pressed !== null) button.setAttribute("aria-checked", String(pressed));
  button.innerHTML =
    '<span class="spice-menu-action-label"></span>' +
    '<span class="spice-menu-action-detail"></span>';
  button.querySelector(".spice-menu-action-label").textContent = label;
  button.querySelector(".spice-menu-action-detail").textContent = detail;
  button.addEventListener("click", onClick);
  return button;
}

function renderSpiceMenuTargets() {
  const section = document.createElement("section");
  section.className = "spice-menu-section spice-menu-targets";
  const heading = document.createElement("div");
  heading.className = "spice-menu-heading";
  heading.textContent = "open tree";
  const list = document.createElement("div");
  list.className = "spice-menu-target-list";
  if (!targetsLoaded) {
    list.textContent = targetsLoading
      ? "loading work trees"
      : "work tree list unavailable";
  } else {
    const openIds = new Set(laneStates.keys());
    const choices = targets
      .filter((target) => !openIds.has(target.id))
      .sort(compareTargetChoices);
    list.replaceChildren(...choices.map(renderTargetChoice));
    if (!choices.length) list.textContent = "all work trees are open";
  }
  section.append(heading, list);
  return section;
}

function setFastModeEnabled(enabled) {
  fastModeEnabled = Boolean(enabled);
  openLaneButton.classList.toggle("spice-menu-button--fast", fastModeEnabled);
  openLaneButton.title = fastModeEnabled
    ? "Open spice menu - fast mode on"
    : "Open spice menu";
  renderSpiceMenu();
  configureLiveBusLanes();
  setGlobalTransientStatus(fastModeEnabled ? "fast mode on" : "fast mode off");
}

function createEmptyTeamFromMenu() {
  requestTeamCommand(
    teamCommandPayload("createTeam", {
      config: defaultTeamConfig(),
    }),
  )
    .then(() => {
      setGlobalTransientStatus("empty team created");
      closeSpiceMenu();
    })
    .catch(() => {
      setGlobalTransientStatus("create team failed");
    });
}

function defaultTeamConfig() {
  return {
    speechMode: defaultSpeechMode,
    lifetime: defaultAgentLifetime,
    selectedView: defaultLaneViewMode,
  };
}

function renderTargetChoice(target) {
  const button = document.createElement("button");
  button.type = "button";
  const status = targetChoiceStatus(target);
  const metadata = targetChoiceMetadata(target);
  const name = target.branch || target.displayName || target.id;
  button.className = "target-choice target-choice--" + status;
  button.setAttribute("role", "menuitem");
  button.title = "Open " + name + " lane; " + metadata;
  button.innerHTML =
    '<span class="target-choice-signal" aria-hidden="true"></span>' +
    '<span class="target-choice-copy"><strong></strong><span></span></span>';
  button.querySelector("strong").textContent = name;
  button.querySelector(".target-choice-copy span").textContent = metadata;
  button.addEventListener("click", () => {
    openTargetTeam(target.id).catch(() => {
      setGlobalTransientStatus("open tree failed");
    });
  });
  return button;
}

function targetChoiceMetadata(target) {
  const parts = [];
  const activity = relativeTime(targetChoiceLastAssistantAt(target));
  if (activity) parts.push(activity.trim());
  else if (!target.threadId) parts.push("never");
  parts.push(targetChoiceStatusLabel(target));
  if (target.pendingCount > 0) parts.push(target.pendingCount + " pending");
  return parts.join(" · ");
}

function targetChoiceLastAssistantAt(target) {
  return (
    target.lastAssistantAt || (target.statusLine || {}).lastAssistantAt || ""
  );
}

function compareTargetChoices(left, right) {
  const leftTime = Date.parse(targetChoiceLastAssistantAt(left)) || 0;
  const rightTime = Date.parse(targetChoiceLastAssistantAt(right)) || 0;
  if (leftTime !== rightTime) return rightTime - leftTime;
  return String(left.branch || "").localeCompare(String(right.branch || ""));
}

function targetChoiceStatus(target) {
  if (target.pendingCount > 0) return "pending";
  const status = target.agentProcessStatus || "";
  if (status === "running") return "running";
  if (status === "idle") return "idle";
  return target.bindingStatus === "bound" || target.threadId
    ? "bound"
    : "unbound";
}

function targetChoiceStatusLabel(target) {
  const status = targetChoiceStatus(target);
  if (status === "pending") return "steering queued";
  if (status === "running") return "agent running";
  if (status === "idle") return "agent idle";
  if (status === "bound") return "agent bound";
  return "agent unbound";
}

// ---- global filter pills -----------------------------------------------------------

const taskFilterHeaderExtraStems = ["agent"];

function taskFilterStemPillsFromInventory(inventory) {
  const catalog = (inventory || {}).catalog || {};
  const stemsByName = new Map(
    ((inventory || {}).primaryStems || []).map((stem) => [stem.name, stem]),
  );
  const pills = [];
  for (const stemName of uniqueStringList([
    ...(catalog.approvedStems || []),
    ...taskFilterHeaderExtraStems,
  ])) {
    const stem = stemsByName.get(stemName);
    if (stem && stem.openTaskCount > 0) pills.push(stem);
  }
  return pills;
}

function renderFilterPills() {
  if (!filterStripEl) return;
  filterStripEl.setAttribute(
    "aria-hidden",
    taskFilterStemPills.length ? "false" : "true",
  );
  const nodes = [];
  for (const stem of taskFilterStemPills) {
    const drainability = taskFilterStemDrainability(stem);
    const pill = document.createElement("span");
    const classes = ["filter-pill"];
    if (stem.name === "agent") classes.push("filter-pill--private");
    classes.push(
      drainability.drainable
        ? "filter-pill--drainable"
        : "filter-pill--undrainable",
    );
    pill.className = classes.join(" ");
    pill.title =
      stem.openTaskCount +
      " open across " +
      stem.name +
      ".*; " +
      (drainability.drainable
        ? "drainable by " + drainability.count
        : "not drainable");
    pill.innerHTML =
      '<span class="filter-pill-label"></span>' +
      '<span class="filter-pill-count"></span>';
    pill.querySelector(".filter-pill-label").textContent = stem.name;
    pill.querySelector(".filter-pill-count").textContent = String(
      stem.openTaskCount,
    );
    nodes.push(pill);
  }
  filterStripEl.replaceChildren(...nodes);
}

function taskFilterStemDrainability(stem) {
  const covered = new Set(uniqueStringList([stem.name, ...(stem.filters || [])]));
  let count = 0;
  for (const lane of laneStates.values()) {
    if (!isLaneOpen(lane) || isShadowLane(lane)) continue;
    if (laneEffectiveLifetime(lane) !== "Drive") continue;
    const assigned = laneAssignedTaskFilters(lane);
    if (!assigned.some((filter) => covered.has(filter))) continue;
    count += laneGroupMemberLanes(lane).filter(laneMemberCanDrain).length;
  }
  return { drainable: count > 0, count };
}

function laneMemberCanDrain(member) {
  const statusLine = member.lastRenderedStatusLine || {};
  const fromTarget = targetById.get(member.targetId) || {};
  return (
    (statusLine.agentProcessStatus || fromTarget.agentProcessStatus) ===
    "running"
  );
}
