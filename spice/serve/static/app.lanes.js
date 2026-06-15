// Targets, the spice context menu, and server-team-backed lane topology. Which lanes
// are open is server truth: opening an agent creates a team, the team snapshot
// reconciles lanes (including fused groups), closing a lane closes its team.
// localStorage keeps only per-target hints (speech mode, selected view).

const emptyTeamTargetPrefix = "empty-team:";

function emptyTeamTargetId(teamId) {
  return emptyTeamTargetPrefix + teamId;
}

async function refreshServerTopology() {
  await refreshTargets();
  await refreshTeamSnapshot({ force: true });
}

function refreshTargets() {
  if (targetsLoadPromise) return targetsLoadPromise;
  targetsLoading = true;
  if (!targetsLoaded) globalStatusEl.textContent = "loading teams";
  if (spiceMenuEl) renderSpiceMenu();
  targetsLoadPromise = (async () => {
    try {
      const response = await liveBusRequest("targets.refresh");
      applyTargetsPayload(response.payload || {});
    } catch (error) {
      setGlobalTransientStatus("team refresh failed");
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
    if (!targetById.has(lane.targetId) && !lane.emptyTeam) closeLaneCore(lane);
  }
  renderFilterPills();
  for (const lane of laneStates.values()) {
    if (lane.emptyTeam) syncEmptyTeamLane(lane);
    else
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
    if (!memberTargetIds.length) {
      if (!team.teamId) continue;
      const targetId = emptyTeamTargetId(team.teamId);
      openTargetIds.add(targetId);
      ensureEmptyTeamLane(team);
      continue;
    }
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
    const lane = laneStates.get(targetId);
    if (lane)
      lane.element.scrollIntoView({ block: "nearest", inline: "nearest" });
    closeSpiceMenu();
    return;
  }
  const target = targetById.get(targetId);
  if (!target) throw new Error("open team requires a known target");
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
  if (lane.emptyTeam) return;
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
    setLaneTransientStatus(lane, "close team failed");
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
  syncNarrationMediaSession();
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
  const laneGrid = lanesEl.getBoundingClientRect();
  const laneGridStyle = window.getComputedStyle(lanesEl);
  const paddingLeft = cssPixelValue(laneGridStyle.paddingLeft);
  const paddingTop = cssPixelValue(laneGridStyle.paddingTop);
  const paddingBottom = cssPixelValue(laneGridStyle.paddingBottom);
  const visibleLane = visibleLaneElements()[0] || null;
  const laneLeft = laneGrid.left + paddingLeft;
  const laneWidth = visibleLane
    ? visibleLane.getBoundingClientRect().width
    : spiceMenuMinimumLaneWidthPx();
  const width = spiceMenuWidth(visibleLane, laneLeft, laneWidth, margin);
  const left = spiceMenuLeft(visibleLane, laneLeft, width, margin);
  const top = laneGrid.top + paddingTop;
  const availableHeight = Math.max(1, window.innerHeight - top - margin);
  const laneGridHeight = Math.max(
    1,
    laneGrid.height - paddingTop - paddingBottom,
  );
  const height = Math.min(availableHeight, laneGridHeight);
  spiceMenuEl.style.width = width + "px";
  spiceMenuEl.style.left = left + "px";
  spiceMenuEl.style.top = top + "px";
  spiceMenuEl.style.height = visibleLane ? height + "px" : "";
  spiceMenuEl.style.maxHeight = height + "px";
}

function spiceMenuWidth(visibleLane, laneLeft, laneWidth, margin) {
  if (!visibleLane && spiceMenuUsesViewportWidth()) return window.innerWidth;
  const availableWidth = visibleLane
    ? Math.max(1, window.innerWidth - laneLeft - margin)
    : Math.max(1, window.innerWidth - margin * 2);
  return Math.min(
    availableWidth,
    Math.max(spiceMenuMinimumLaneWidthPx(), laneWidth),
  );
}

function spiceMenuLeft(visibleLane, laneLeft, width, margin) {
  if (visibleLane) return laneLeft;
  if (spiceMenuUsesViewportWidth()) return 0;
  const buttonRect = openLaneButton.getBoundingClientRect();
  return Math.max(
    margin,
    Math.min(buttonRect.right - width, window.innerWidth - width - margin),
  );
}

function spiceMenuUsesViewportWidth() {
  return window.matchMedia("(max-width: 720px)").matches;
}

function spiceMenuMinimumLaneWidthPx() {
  const fontSize =
    Number.parseFloat(window.getComputedStyle(document.documentElement).fontSize) ||
    16;
  return 20 * fontSize;
}

function cssPixelValue(value) {
  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : 0;
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
  heading.textContent = "open team";
  const list = document.createElement("div");
  list.className = "spice-menu-target-list";
  if (!targetsLoaded) {
    list.textContent = targetsLoading
      ? "loading teams"
      : "team list unavailable";
  } else {
    const choices = targets
      .slice()
      .sort(compareTargetChoices);
    const groups = spiceMenuTeamGroups(choices);
    list.replaceChildren(...groups.map(renderSpiceMenuTeamGroup));
    if (!groups.length) list.textContent = "no agents available";
  }
  section.append(heading, list);
  return section;
}

function spiceMenuTeamGroups(choices) {
  const grouped = new Map();
  const unassigned = [];
  for (const target of choices) {
    const teamId = target.teamId || "";
    if (!teamId) {
      unassigned.push(target);
      continue;
    }
    if (!grouped.has(teamId)) {
      grouped.set(teamId, {
        teamId,
        totalCount: targets.filter((item) => item.teamId === teamId).length,
        targets: [],
        unassigned: false,
      });
    }
    grouped.get(teamId).targets.push(target);
  }
  const groups = [...grouped.values()];
  for (const group of groups) group.targets.sort(compareTargetChoices);
  groups.sort(compareSpiceMenuTeamGroups);
  if (unassigned.length) {
    unassigned.sort(compareTargetChoices);
    groups.push({
      teamId: "",
      totalCount: unassigned.length,
      targets: unassigned,
      unassigned: true,
    });
  }
  return groups;
}

function compareSpiceMenuTeamGroups(left, right) {
  const byChoice = compareTargetChoices(
    left.targets[0] || {},
    right.targets[0] || {},
  );
  if (byChoice) return byChoice;
  return String(left.teamId || "").localeCompare(String(right.teamId || ""));
}

function renderSpiceMenuTeamGroup(group) {
  const container = document.createElement("section");
  container.className = group.unassigned
    ? "spice-menu-team spice-menu-team--unassigned"
    : "spice-menu-team";
  if (!group.unassigned) wireSpiceMenuTeamDropTarget(container, group);
  const header = document.createElement("div");
  header.className = "spice-menu-team-header";
  const label = document.createElement("span");
  label.className = "spice-menu-team-label";
  label.textContent = group.unassigned
    ? "agents without team"
    : spiceMenuTeamTitle(group);
  const detail = document.createElement("span");
  detail.className = "spice-menu-team-detail";
  detail.textContent = group.unassigned
    ? "open one to create a new team"
    : spiceMenuTeamDetail(group);
  const choices = document.createElement("div");
  choices.className = "spice-menu-team-targets";
  choices.replaceChildren(
    ...group.targets.map((target) => renderTargetChoice(target, group)),
  );
  header.append(label, detail);
  container.append(header, choices);
  return container;
}

function spiceMenuTeamTitle(group) {
  const names = group.targets.map(targetChoiceName);
  const visible = names.slice(0, 2).join(" + ");
  const overflow = names.length > 2 ? " +" + (names.length - 2) : "";
  return "team " + visible + overflow;
}

function spiceMenuTeamDetail(group) {
  const count = Math.max(group.totalCount || 0, group.targets.length);
  if (count <= 1) return "opens this team";
  return "open any member; " + count + " agents open together";
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

function renderTargetChoice(target, group = null) {
  const alreadyOpen = laneStates.has(target.id);
  let actionLabel = "Create team";
  if (alreadyOpen) actionLabel = "Show team";
  else if (group && !group.unassigned) actionLabel = "Open team";
  const button = targetChoiceButton(target, actionLabel, () => {
    openTargetTeam(target.id).catch(() => {
      setGlobalTransientStatus("open team failed");
    });
  });
  button.classList.toggle("target-choice--open", alreadyOpen);
  wireSpiceMenuTargetDrag(button, target);
  return button;
}

function wireSpiceMenuTargetDrag(button, target) {
  button.draggable = true;
  button.classList.add("target-choice--draggable");
  button.dataset.spiceMenuDragTargetId = target.id;
  button.append(spiceMenuTargetDragAffordance());
  button.addEventListener("dragstart", (event) => {
    spiceMenuDragTargetId = target.id;
    button.classList.add("target-choice--dragging");
    if (event.dataTransfer) {
      event.dataTransfer.effectAllowed = "move";
      event.dataTransfer.setData("text/plain", target.id);
      event.dataTransfer.setData("application/x-spice-target-id", target.id);
    }
  });
  button.addEventListener("dragend", () => {
    spiceMenuDragTargetId = "";
    button.classList.remove("target-choice--dragging");
    clearSpiceMenuTeamDropHighlights();
  });
}

function spiceMenuTargetDragAffordance() {
  const marker = document.createElement("span");
  marker.className = "target-choice-drag-affordance";
  marker.setAttribute("aria-hidden", "true");
  marker.textContent = "↕";
  return marker;
}

function wireSpiceMenuTeamDropTarget(container, group) {
  container.dataset.spiceMenuTeamId = group.teamId;
  container.addEventListener("dragenter", (event) => {
    if (!spiceMenuCanDropOnTeam(group, spiceMenuDragTargetId)) return;
    event.preventDefault();
    container.classList.add("spice-menu-team--drop-ready");
  });
  container.addEventListener("dragover", (event) => {
    if (!spiceMenuCanDropOnTeam(group, spiceMenuDragTargetId)) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
    container.classList.add("spice-menu-team--drop-ready");
  });
  container.addEventListener("dragleave", (event) => {
    if (
      event.relatedTarget instanceof Node &&
      container.contains(event.relatedTarget)
    )
      return;
    container.classList.remove("spice-menu-team--drop-ready");
  });
  container.addEventListener("drop", (event) => {
    const targetId = spiceMenuDroppedTargetId(event);
    if (!spiceMenuCanDropOnTeam(group, targetId)) return;
    event.preventDefault();
    container.classList.remove("spice-menu-team--drop-ready");
    moveTargetToMenuTeam(group.teamId, targetId).catch(() => {
      setGlobalTransientStatus("move to team failed");
      refreshServerTopology().catch(() => {});
    });
  });
}

function clearSpiceMenuTeamDropHighlights() {
  const dropTargets = document.querySelectorAll(".spice-menu-team--drop-ready");
  for (const element of dropTargets)
    element.classList.remove("spice-menu-team--drop-ready");
}

function spiceMenuDroppedTargetId(event) {
  return (
    event.dataTransfer?.getData("application/x-spice-target-id") ||
    event.dataTransfer?.getData("text/plain") ||
    spiceMenuDragTargetId ||
    ""
  );
}

function spiceMenuCanDropOnTeam(group, targetId) {
  if (!group.teamId || !targetId) return false;
  const target = targetById.get(targetId);
  if (!target) return false;
  return (target.teamId || "") !== group.teamId;
}

async function moveTargetToMenuTeam(teamId, targetId) {
  const target = targetById.get(targetId);
  if (!target || !teamId) throw new Error("move target requires team and target");
  await requestTeamCommand(
    teamCommandPayload("moveAgentToTeam", {
      teamId,
      agentId: targetTeamAgentId(target),
      agentAliases: targetTeamAgentAliases(target),
    }),
  );
  await refreshServerTopology();
  setGlobalTransientStatus("team updated");
}

function targetTeamAgentId(target) {
  return canonicalThreadActorId(target.threadId) || target.id;
}

function targetTeamAgentAliases(target) {
  const actor = canonicalThreadActorId(target.threadId);
  return actor && actor !== target.id ? [target.id] : [];
}

function targetChoiceButton(target, actionLabel, onClick, role = "menuitem") {
  const button = document.createElement("button");
  button.type = "button";
  const status = targetChoiceStatus(target);
  const metadata = targetChoiceMetadata(target);
  const name = targetChoiceName(target);
  button.className = "target-choice target-choice--" + status;
  if (role) button.setAttribute("role", role);
  button.title = actionLabel + " " + name + "; " + metadata;
  button.innerHTML =
    '<span class="target-choice-signal" aria-hidden="true"></span>' +
    '<span class="target-choice-copy"><strong></strong><span></span></span>';
  button.querySelector("strong").textContent = name;
  button.querySelector(".target-choice-copy span").textContent = metadata;
  button.addEventListener("click", onClick);
  return button;
}

function targetChoiceName(target) {
  return target.branch || target.displayName || target.id;
}

function targetChoiceMetadata(target) {
  const parts = [];
  if (laneStates.has(target.id)) parts.push("open");
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
  const pillModels = taskFilterStemPills.map((stem) => ({
    stem,
    drainability: taskFilterStemDrainability(stem),
  }));
  const hidden = pillModels.length ? "false" : "true";
  const fingerprint = JSON.stringify({
    hidden,
    pills: pillModels.map((model) => ({
      name: model.stem.name,
      openTaskCount: Math.max(0, Number(model.stem.openTaskCount) || 0),
      drainable: model.drainability.drainable,
      drainableCount: model.drainability.count,
    })),
  });
  filterStripEl.setAttribute("aria-hidden", hidden);
  if (fingerprint === renderedFilterPillsFingerprint) return;
  renderedFilterPillsFingerprint = fingerprint;
  const nodes = [];
  for (const model of pillModels) {
    const { stem, drainability } = model;
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
