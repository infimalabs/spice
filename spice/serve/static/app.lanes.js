// Targets, the spice context menu, and server-team-backed lane topology. Which lanes
// are open is server truth: opening an agent creates a team, the team snapshot
// reconciles lanes (including fused groups), closing a lane closes its team.
// localStorage keeps only per-target hints (speech mode, selected view).

const emptyTeamTargetPrefix = "empty-team:";
const targetChoiceStatusValues = [
  "pending",
  "running",
  "running-stale",
  "idle",
  "stopped",
  "unstarted",
  "bound",
  "unbound",
  "unknown",
];

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
        lanePayloadWithTargetPending(lane, targetById.get(lane.targetId)),
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
    renewalIntent: target.renewalIntent || {},
    taskFilterInventory: target.taskFilterInventory || {},
    laneMetrics: target.laneMetrics || {},
    laneInfo: target.laneInfo || { summaryRows: [], members: [] },
    privateTaskCount: Math.max(0, Number(target.privateTaskCount) || 0),
    statusLine: target.statusLine || {},
  };
}

function lanePayloadWithTargetPending(lane, target) {
  const targetPayload = targetPayloadShim(target);
  if (!lane.latestPayload) return targetPayload;
  const pending = targetFreshPendingIdentity(target);
  if (pending.count === null && pending.keys === null && !pending.revision)
    return lane.latestPayload;
  const pendingFields = pendingIdentityFields(pending);
  const statusLine = {
    ...(lane.latestPayload.statusLine || {}),
    ...pendingFields,
  };
  lane.latestPayload = {
    ...lane.latestPayload,
    ...pendingFields,
    statusLine,
  };
  return lane.latestPayload;
}

function targetFreshPendingIdentity(target) {
  const statusLine = (target && target.statusLine) || {};
  let count = null;
  for (const value of [
    statusLine.pendingInboxCount,
    target && target.pendingInboxCount,
    target && target.pendingCount,
  ]) {
    count = normalizedTargetChoiceCount(value);
    if (count !== null) break;
  }
  const sourceWithKeys = Array.isArray(statusLine.pendingInboxKeys)
    ? statusLine
    : target || {};
  const keys = Array.isArray(sourceWithKeys.pendingInboxKeys)
    ? sourceWithKeys.pendingInboxKeys.map((key) => String(key)).filter(Boolean)
    : null;
  return {
    count,
    keys,
    revision: String(sourceWithKeys.pendingInboxRevision || ""),
  };
}

function pendingIdentityFields(identity) {
  const fields = {};
  if (identity.count !== null) {
    fields.pendingInboxCount = identity.count;
    fields.pendingInboxLabel = String(identity.count);
  }
  if (identity.keys !== null) fields.pendingInboxKeys = identity.keys;
  if (identity.revision) fields.pendingInboxRevision = identity.revision;
  return fields;
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
  const canCloseEmptyTeam = teams.length > 1;
  const hints = laneHintsByTargetId();
  const openTargetIds = new Set();
  const groupRuns = [];
  for (const team of teams) {
    const members = team.members || [];
    const memberTargetIds = teamMemberTargetIds(team);
    if (!memberTargetIds.length) {
      if (members.length) {
        preserveUnresolvedTeamLanes(team, openTargetIds);
        continue;
      }
      if (!team.teamId) continue;
      const targetId = emptyTeamTargetId(team.teamId);
      openTargetIds.add(targetId);
      ensureEmptyTeamLane(team, { canClose: canCloseEmptyTeam });
      continue;
    }
    for (const targetId of memberTargetIds) {
      openTargetIds.add(targetId);
      ensureTeamMemberLane(targetId, team, hints.get(targetId));
    }
    if (memberTargetIds.length < members.length)
      preserveUnresolvedTeamLanes(team, openTargetIds);
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
    const targetId = teamMemberTargetId(member, team, targetIds);
    if (targetId && targetById.has(targetId) && !targetIds.includes(targetId))
      targetIds.push(targetId);
  }
  return targetIds;
}

// A team member is an actor (canonical thread id) or, before any thread is
// bound, the worktree target id itself.
function teamMemberTargetId(member, team = null, claimedTargetIds = []) {
  const agentId = String((member || {}).agentId || "");
  const actorId = canonicalThreadActorId(agentId);
  if (!actorId) return "";
  if (targetById.has(agentId)) return agentId;
  const laneTargetId = teamMemberLaneTargetId(actorId);
  if (laneTargetId) {
    renameTeamMemberTargetThread(laneTargetId, agentId);
    return laneTargetId;
  }
  for (const target of targets) {
    if (canonicalThreadActorId(target.threadId) === actorId)
      return target.id;
  }
  const renewedTargetId = renewedTeamSlotTargetId(team, agentId, claimedTargetIds);
  if (renewedTargetId) return renewedTargetId;
  return "";
}

function teamMemberLaneTargetId(actorId) {
  for (const lane of laneStates.values()) {
    if (!targetById.has(lane.targetId)) continue;
    if (
      canonicalThreadActorId(lane.targetThreadId) === actorId ||
      canonicalThreadActorId(lane.activeThreadId) === actorId
    )
      return lane.targetId;
  }
  return "";
}

function renewedTeamSlotTargetId(team, agentId, claimedTargetIds = []) {
  const teamId = String((team || {}).teamId || "");
  if (!teamId) return "";
  const actorIds = teamMemberActorIds(team);
  const claimed = new Set(claimedTargetIds);
  const candidates = [];
  for (const lane of laneStates.values()) {
    if (lane.emptyTeam || claimed.has(lane.targetId)) continue;
    const target = targetById.get(lane.targetId);
    if (!target) continue;
    if (String(lane.teamId || target.teamId || "") !== teamId) continue;
    const laneActorId = canonicalThreadActorId(
      lane.targetThreadId || target.threadId || lane.activeThreadId,
    );
    if (!laneActorId || actorIds.has(laneActorId)) continue;
    candidates.push(lane.targetId);
  }
  if (candidates.length !== 1) return "";
  renameTeamMemberTargetThread(candidates[0], agentId);
  return candidates[0];
}

function renameTeamMemberTargetThread(targetId, agentId) {
  const target = targetById.get(targetId);
  if (target) target.threadId = agentId;
  const lane = laneStates.get(targetId);
  if (!lane) return;
  lane.targetThreadId = agentId;
  lane.activeThreadId = agentId;
  if (typeof ensureLaneOccupant === "function") ensureLaneOccupant(lane, agentId);
}

function preserveUnresolvedTeamLanes(team, openTargetIds) {
  const teamId = String((team || {}).teamId || "");
  if (!teamId) return;
  const actorIds = teamMemberActorIds(team);
  for (const lane of laneStates.values()) {
    if (lane.emptyTeam) continue;
    const target = targetById.get(lane.targetId);
    if (!target) continue;
    if (String(lane.teamId || target.teamId || "") !== teamId) continue;
    const laneActorId = canonicalThreadActorId(
      lane.targetThreadId || target.threadId || lane.activeThreadId,
    );
    if (laneActorId && actorIds.has(laneActorId)) continue;
    openTargetIds.add(lane.targetId);
  }
}

function teamMemberActorIds(team) {
  const actorIds = new Set();
  for (const member of (team || {}).members || []) {
    const actorId = canonicalThreadActorId((member || {}).agentId);
    if (actorId) actorIds.add(actorId);
  }
  return actorIds;
}

function ensureTeamMemberLane(targetId, team, hint = null) {
  if (!laneStates.has(targetId)) addLane(targetId, hint);
  const lane = laneStates.get(targetId);
  if (!lane) return;
  const config = team.config || {};
  const splitBack = team.splitBack || {};
  const member = teamMemberForTargetId(team, targetId);
  lane.teamId = String(team.teamId || "");
  lane.teamRevision = Math.max(0, Number(team.revision || 0));
  lane.teamSplitBackAvailable = Boolean(splitBack.available);
  lane.teamSplitBackMemberCount = Math.max(
    0,
    Number(splitBack.memberCount || 0),
  );
  lane.configRevision = Math.max(0, Number(config.revision || 0));
  if (member && member.renewalIntent) lane.renewalIntent = member.renewalIntent;
  if (Array.isArray(config.taskFilters))
    lane.taskFilters = uniqueStringList(config.taskFilters);
  if (config.lifetime)
    applyServerLaneLifetime(lane, config.lifetime, {
      configRevision: config.revision,
    });
  if (!hint && config.speechMode && speechModes.includes(config.speechMode))
    lane.speechMode = config.speechMode;
  syncLaneEffectiveControls(lane);
}

function teamMemberForTargetId(team, targetId) {
  for (const member of team.members || []) {
    if (teamMemberTargetId(member) === targetId) return member;
  }
  return null;
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

async function openTargetTeam(targetId, options = {}) {
  const keepMenuOpen = Boolean(options.keepMenuOpen);
  if (laneStates.has(targetId)) {
    const lane = laneStates.get(targetId);
    if (lane)
      lane.element.scrollIntoView({ block: "nearest", inline: "nearest" });
    if (!keepMenuOpen) closeSpiceMenu();
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
    if (!keepMenuOpen) closeSpiceMenu();
  } catch (error) {
    sessionOpenTargetIds.delete(targetId);
    throw error;
  }
}

function closeLane(lane) {
  const host = laneGroupHost(lane);
  if (host.emptyTeam && !host.emptyTeamCanClose) return;
  if (!host.teamId) return;
  if (!host.emptyTeam && laneHasUnsafeDraft(host)) {
    if (!window.confirm(unsafeDraftWarningText())) return;
  }
  for (const member of laneGroupMemberLanes(host))
    sessionOpenTargetIds.delete(member.targetId);
  host.serverCloseRequested = true;
  requestTeamCommand(
    teamCommandPayload("closeTeam", { teamId: host.teamId }),
  ).catch(() => {
    host.serverCloseRequested = false;
    setLaneTransientStatus(host, "close team failed");
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
  clearSpiceMenuTargetDrag();
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
  clearSpiceMenuTargetDrag();
  spiceMenuEl.replaceChildren(
    renderSpiceMenuActions(),
    renderSpiceMenuTargets(),
  );
}

function positionSpiceMenu() {
  if (!spiceMenuEl) return;
  const margin = 8;
  const buttonRect = openLaneButton.getBoundingClientRect();
  const viewportHeight =
    window.innerHeight || document.documentElement.clientHeight;
  const viewportWidth = window.innerWidth || document.documentElement.clientWidth;
  const top = Math.max(margin, buttonRect.bottom + margin);
  const width = spiceMenuWidthForButton(buttonRect, viewportWidth, margin);
  const left = spiceMenuLeftForButton(buttonRect, width, viewportWidth, margin);
  const height = Math.max(220, viewportHeight - top - margin);
  spiceMenuEl.style.width = width + "px";
  spiceMenuEl.style.left = left + "px";
  spiceMenuEl.style.top = top + "px";
  spiceMenuEl.style.height = "";
  spiceMenuEl.style.maxHeight = height + "px";
}

function spiceMenuWidthForButton(buttonRect, viewportWidth, margin) {
  if (spiceMenuUsesViewportWidth(viewportWidth)) return viewportWidth;
  const availableWidth = Math.max(1, viewportWidth - margin * 2);
  return Math.min(
    availableWidth,
    Math.max(spiceMenuMinimumLaneWidthPx(), buttonRect.width),
  );
}

function spiceMenuLeftForButton(buttonRect, width, viewportWidth, margin) {
  if (spiceMenuUsesViewportWidth(viewportWidth)) return 0;
  const rightAlignedLeft = buttonRect.right - width;
  return Math.max(
    margin,
    Math.min(rightAlignedLeft, viewportWidth - width - margin),
  );
}

function spiceMenuUsesViewportWidth(viewportWidth) {
  return viewportWidth < spiceMenuMinimumLaneWidthPx() + 20;
}

function spiceMenuMinimumLaneWidthPx() {
  const fontSize =
    Number.parseFloat(window.getComputedStyle(document.documentElement).fontSize) ||
    16;
  return 20 * fontSize;
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
      .sort(compareSpiceMenuTargetChoices);
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
  for (const group of groups) group.targets.sort(compareSpiceMenuTargetChoices);
  groups.sort(compareSpiceMenuTeamGroups);
  unassigned.sort(compareSpiceMenuTargetChoices);
  if (choices.length)
    groups.push({
      teamId: "",
      totalCount: unassigned.length,
      targets: unassigned,
      unassigned: true,
    });
  return groups;
}

function compareSpiceMenuTeamGroups(left, right) {
  const byName = spiceMenuTeamSortKey(left).localeCompare(
    spiceMenuTeamSortKey(right),
  );
  if (byName) return byName;
  return String(left.teamId || "").localeCompare(String(right.teamId || ""));
}

function spiceMenuTeamSortKey(group) {
  return group.targets.map(targetChoiceName).join("\n");
}

function compareSpiceMenuTargetChoices(left, right) {
  const byName = targetChoiceName(left).localeCompare(targetChoiceName(right));
  if (byName) return byName;
  return String(left.id || "").localeCompare(String(right.id || ""));
}

function renderSpiceMenuTeamGroup(group) {
  const container = document.createElement("section");
  container.className = group.unassigned
    ? "spice-menu-team spice-menu-team--unassigned"
    : "spice-menu-team";
  wireSpiceMenuTeamDropTarget(container, group);
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
    ? "drop here to remove from team"
    : spiceMenuTeamDetail(group);
  const choices = document.createElement("div");
  choices.className = "spice-menu-team-targets";
  const targetChoices = group.targets.map((target) =>
    renderTargetChoice(target, group),
  );
  if (group.unassigned && !targetChoices.length)
    targetChoices.push(spiceMenuEmptyUnassignedDropHint());
  choices.replaceChildren(...targetChoices);
  header.append(label, detail);
  container.append(header, choices);
  return container;
}

function spiceMenuEmptyUnassignedDropHint() {
  const hint = document.createElement("div");
  hint.className = "spice-menu-team-empty-drop";
  hint.textContent = "Drop agent here";
  return hint;
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
  button.classList.add("target-choice--draggable");
  button.dataset.spiceMenuDragTargetId = target.id;
  button.style.touchAction = "none";
  button.append(spiceMenuTargetDragAffordance());

  let suppressNextClick = false;

  button.addEventListener("click", (event) => {
    if (suppressNextClick) {
      suppressNextClick = false;
      event.preventDefault();
      event.stopPropagation();
    }
  });

  button.addEventListener("pointerdown", (event) => {
    if (event.button !== undefined && event.button !== 0) return;
    clearSpiceMenuTargetDrag();
    spiceMenuTargetDragState = {
      button,
      targetId: target.id,
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      offsetX: event.clientX - button.getBoundingClientRect().left,
      offsetY: event.clientY - button.getBoundingClientRect().top,
      dragging: false,
      dragGhost: null,
      overContainer: null,
      overDesktop: false,
    };
    button.setPointerCapture(event.pointerId);
  });

  button.addEventListener("pointermove", (event) => {
    const state = spiceMenuTargetDragState;
    if (!spiceMenuTargetDragMatches(state, event, target.id)) return;
    if (!state.dragging) {
      const dx = event.clientX - state.startX;
      const dy = event.clientY - state.startY;
      if (Math.abs(dx) < 6 && Math.abs(dy) < 6) return;
      state.dragging = true;
      suppressNextClick = true;
      spiceMenuDragTargetId = target.id;
      button.classList.add("target-choice--dragging");
      state.dragGhost = createSpiceMenuTargetDragGhost(button);
    }
    updateSpiceMenuTargetDragGhost(state, event);
    const el = document.elementFromPoint(event.clientX, event.clientY);
    const container = /** @type {HTMLElement | null} */ (el?.closest("[data-spice-menu-team-id]") || null);
    if (container !== state.overContainer) {
      state.overContainer?.classList.remove("spice-menu-team--drop-ready");
      state.overContainer = null;
      const teamId = container ? spiceMenuDropTeamId(container) : "";
      if (container && spiceMenuCanDropTargetOnTeamId(teamId, target.id)) {
        container.classList.add("spice-menu-team--drop-ready");
        state.overContainer = container;
      }
    }
    state.overDesktop = spiceMenuDesktopDropTargetFromPoint(
      event.clientX,
      event.clientY,
    );
    lanesEl.classList.toggle(
      "swimlanes--menu-drop-ready",
      state.overDesktop && !state.overContainer,
    );
    event.preventDefault();
  });

  button.addEventListener("pointerup", (event) => {
    const state = spiceMenuTargetDragState;
    if (!spiceMenuTargetDragMatches(state, event, target.id)) return;
    if (state.dragging && state.overContainer) {
      const teamId = spiceMenuDropTeamId(
        /** @type {HTMLElement} */ (state.overContainer),
      );
      moveTargetToMenuTeam(teamId, target.id).catch(() => {
        setGlobalTransientStatus(
          teamId ? "move to team failed" : "remove from team failed",
        );
        refreshServerTopology().catch(() => {});
      });
    } else if (state.dragging && state.overDesktop) {
      openTargetTeam(target.id, { keepMenuOpen: true }).catch(() => {
        setGlobalTransientStatus("open team failed");
      });
    }
    endMenuTargetDrag(state);
    spiceMenuTargetDragState = null;
    event.preventDefault();
  });

  button.addEventListener("pointercancel", (event) => {
    const state = spiceMenuTargetDragState;
    if (!spiceMenuTargetDragMatches(state, event, target.id)) return;
    endMenuTargetDrag(state);
    spiceMenuTargetDragState = null;
  });
}

function spiceMenuTargetDragMatches(state, event, targetId) {
  return (
    state &&
    state.pointerId === event.pointerId &&
    state.targetId === targetId
  );
}

function endMenuTargetDrag(state) {
  spiceMenuDragTargetId = "";
  state.button?.classList.remove("target-choice--dragging");
  state.dragGhost?.remove();
  state.dragGhost = null;
  lanesEl.classList.remove("swimlanes--menu-drop-ready");
  clearSpiceMenuTeamDropHighlights();
}

function clearSpiceMenuTargetDrag() {
  if (spiceMenuTargetDragState) {
    endMenuTargetDrag(spiceMenuTargetDragState);
    spiceMenuTargetDragState = null;
  }
  for (const ghost of document.querySelectorAll(".target-choice-drag-ghost"))
    ghost.remove();
  for (const choice of document.querySelectorAll(".target-choice--dragging"))
    choice.classList.remove("target-choice--dragging");
}

function createSpiceMenuTargetDragGhost(button) {
  const ghost = /** @type {HTMLElement} */ (button.cloneNode(true));
  const rect = button.getBoundingClientRect();
  ghost.classList.remove("target-choice--dragging");
  ghost.classList.add("target-choice-drag-ghost");
  ghost.style.width = rect.width + "px";
  document.body.append(ghost);
  return ghost;
}

function updateSpiceMenuTargetDragGhost(state, event) {
  if (!state.dragGhost) return;
  const left = event.clientX - state.offsetX;
  const top = event.clientY - state.offsetY;
  state.dragGhost.style.transform =
    "translate(" + left + "px, " + top + "px)";
}

function spiceMenuDesktopDropTargetFromPoint(clientX, clientY) {
  const element = document.elementFromPoint(clientX, clientY);
  if (!(element instanceof Element)) return false;
  if (spiceMenuEl?.contains(element)) return false;
  return lanesEl.contains(element);
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
  container.dataset.spiceMenuUnassigned = group.unassigned ? "true" : "false";
}

function clearSpiceMenuTeamDropHighlights() {
  const dropTargets = document.querySelectorAll(".spice-menu-team--drop-ready");
  for (const element of dropTargets)
    element.classList.remove("spice-menu-team--drop-ready");
}

function spiceMenuCanDropTargetOnTeamId(teamId, targetId) {
  if (!targetId) return false;
  const target = targetById.get(targetId);
  if (!target) return false;
  return (target.teamId || "") !== (teamId || "");
}

async function moveTargetToMenuTeam(teamId, targetId) {
  const target = targetById.get(targetId);
  if (!target) throw new Error("move target requires target");
  if (teamId) {
    await requestTeamCommand(
      teamCommandPayload("moveAgentToTeam", {
        teamId,
        agentId: targetTeamAgentId(target),
        agentAliases: targetTeamAgentAliases(target),
      }),
    );
  } else {
    const currentTeamId = target.teamId || "";
    if (!currentTeamId) throw new Error("remove target requires current team");
    await requestTeamCommand(
      teamCommandPayload("removeAgentFromTeam", {
        teamId: currentTeamId,
        agentId: targetTeamAgentId(target),
        agentAliases: targetTeamAgentAliases(target),
      }),
    );
  }
  await refreshServerTopology();
  setGlobalTransientStatus(teamId ? "team updated" : "agent removed from team");
}

function spiceMenuDropTeamId(container) {
  if (container.dataset.spiceMenuUnassigned === "true") return "";
  return container.dataset.spiceMenuTeamId || "";
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
  const name = targetChoiceName(target);
  button.className = "target-choice";
  button.dataset.targetChoiceId = target.id;
  button.dataset.targetChoiceActionLabel = actionLabel;
  if (role) button.setAttribute("role", role);
  button.innerHTML =
    '<span class="target-choice-signal" aria-hidden="true"></span>' +
    '<span class="target-choice-copy"><span class="target-choice-name"></span><span class="target-choice-meta"></span></span>';
  button.querySelector(".target-choice-name").textContent = name;
  updateTargetChoiceButtonPresentation(button, target, actionLabel);
  button.addEventListener("click", onClick);
  return button;
}

function updateLiveTargetChoiceMetadata() {
  for (const element of document.querySelectorAll("[data-target-choice-id]")) {
    const button = /** @type {HTMLElement} */ (element);
    const target = targetById.get(button.dataset.targetChoiceId || "");
    if (!target) continue;
    updateTargetChoiceButtonPresentation(
      button,
      target,
      button.dataset.targetChoiceActionLabel || "Open",
    );
  }
}

function updateTargetChoiceButtonPresentation(button, target, actionLabel) {
  const status = targetChoiceStatus(target);
  const metadata = targetChoiceMetadata(target);
  setTargetChoiceStatusClass(button, status);
  syncTargetChoiceNameAccent(button, target);
  button.title = actionLabel + " " + targetChoiceName(target) + "; " + metadata;
  const metadataEl = button.querySelector(".target-choice-meta");
  if (metadataEl) metadataEl.textContent = metadata;
}

function syncTargetChoiceNameAccent(button, target) {
  const accent = targetChoiceNameAccent(target);
  if (accent) button.style.setProperty("--target-choice-name-accent", accent);
  else button.style.removeProperty("--target-choice-name-accent");
}

function targetChoiceNameAccent(target) {
  const lane = laneStates.get(target.id);
  if (!lane) return "";
  const host = laneGroupHost(lane);
  const members = laneGroupMemberLanes(host);
  if (!host.teamId && members.length <= 1) return "";
  return messageOccupantAccent(laneMemberAccentIndex(host, lane));
}

function setTargetChoiceStatusClass(button, status) {
  for (const value of targetChoiceStatusValues)
    button.classList.remove("target-choice--" + value);
  button.classList.add("target-choice--" + status);
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
  const pending = targetChoicePendingCount(target);
  if (pending > 0) parts.push(pending + " pending");
  return parts.join(" · ");
}

function targetChoiceLastAssistantAt(target) {
  return targetChoiceStatusLine(target).lastAssistantAt || "";
}

function targetChoiceStatusLine(target) {
  const statusLine = target.statusLine || {};
  const laneStatusLine = targetChoiceLaneStatusLine(target);
  const merged = { ...statusLine, ...laneStatusLine };
  merged.lastAssistantAt =
    laneStatusLine.lastAssistantAt ||
    target.lastAssistantAt ||
    statusLine.lastAssistantAt ||
    "";
  merged.agentProcessStatus =
    laneStatusLine.agentProcessStatus ||
    target.agentProcessStatus ||
    statusLine.agentProcessStatus ||
    "";
  merged.agentVisualStatus =
    laneStatusLine.agentVisualStatus ||
    target.agentVisualStatus ||
    statusLine.agentVisualStatus ||
    merged.agentProcessStatus ||
    "";
  merged.activityStatus =
    laneStatusLine.activityStatus || statusLine.activityStatus || "";
  merged.bindingStatus =
    laneStatusLine.bindingStatus ||
    target.bindingStatus ||
    statusLine.bindingStatus ||
    "";
  return merged;
}

function targetChoiceLaneStatusLine(target) {
  const lane = laneStates.get(target.id);
  return (lane && lane.lastRenderedStatusLine) || {};
}

function targetChoicePendingCount(target) {
  const statusLine = target.statusLine || {};
  const laneStatusLine = targetChoiceLaneStatusLine(target);
  for (const value of [
    laneStatusLine.pendingInboxCount,
    target.pendingCount,
    target.pendingInboxCount,
    statusLine.pendingInboxCount,
  ]) {
    const count = normalizedTargetChoiceCount(value);
    if (count !== null) return count;
  }
  return 0;
}

function normalizedTargetChoiceCount(value) {
  if (value === undefined || value === null || value === "") return null;
  const count = Number(value);
  if (!Number.isFinite(count)) return null;
  return Math.max(0, count);
}

function compareTargetChoices(left, right) {
  const leftTime = Date.parse(targetChoiceLastAssistantAt(left)) || 0;
  const rightTime = Date.parse(targetChoiceLastAssistantAt(right)) || 0;
  if (leftTime !== rightTime) return rightTime - leftTime;
  return String(left.branch || "").localeCompare(String(right.branch || ""));
}

function targetChoiceStatus(target) {
  if (targetChoicePendingCount(target) > 0) return "pending";
  const statusLine = targetChoiceStatusLine(target);
  if (statusLine.error) return "unknown";
  const visualStatus = targetChoiceKnownStatus(liveAgentVisualStatus(statusLine));
  if (visualStatus && visualStatus !== "unknown") return visualStatus;
  const processStatus = targetChoiceKnownStatus(statusLine.agentProcessStatus);
  if (processStatus && processStatus !== "unknown") return processStatus;
  return statusLine.bindingStatus === "bound" || target.threadId
    ? "bound"
    : "unbound";
}

function targetChoiceKnownStatus(status) {
  const value = String(status || "");
  return targetChoiceStatusValues.includes(value) ? value : "";
}

function targetChoiceStatusLabel(target) {
  const status = targetChoiceStatus(target);
  if (status === "pending") return "steering queued";
  if (
    status === "running" ||
    status === "running-stale" ||
    status === "idle" ||
    status === "stopped" ||
    status === "unstarted"
  )
    return agentStatusLabel(status);
  if (status === "bound") return "agent bound";
  if (status === "unknown") return "agent status unknown";
  return "agent unbound";
}

// ---- global filter pills -----------------------------------------------------------

const taskFilterHeaderExtraStems = ["agent", "oops"];

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
  const pillModels = filterPillModels();
  const hidden = pillModels.length ? "false" : "true";
  const fingerprint = JSON.stringify({
    hidden,
    pills: pillModels.map((model) => ({
      kind: model.kind,
      name: model.label,
      openTaskCount: model.openTaskCount,
      drainable: model.drainability.drainable,
      drainableCount: model.drainability.count,
      boundaryDissolved: Boolean(model.drainability.boundaryDissolved),
    })),
  });
  filterStripEl.setAttribute("aria-hidden", hidden);
  if (fingerprint === renderedFilterPillsFingerprint) return;
  renderedFilterPillsFingerprint = fingerprint;
  const nodes = [];
  for (const model of pillModels) {
    const pill = document.createElement("span");
    const classes = ["filter-pill", ...model.classes];
    if (
      model.drainability.boundaryDissolved &&
      model.drainability.drainable
    )
      classes.push("filter-pill--implicit");
    classes.push(
      model.drainability.drainable
        ? "filter-pill--drainable"
        : "filter-pill--undrainable",
    );
    pill.className = classes.join(" ");
    pill.title = model.title;
    pill.innerHTML =
      '<span class="filter-pill-label"></span>' +
      '<span class="filter-pill-count"></span>';
    pill.querySelector(".filter-pill-label").textContent = model.label;
    pill.querySelector(".filter-pill-count").textContent = String(model.openTaskCount);
    nodes.push(pill);
  }
  filterStripEl.replaceChildren(...nodes);
}

function filterPillModels() {
  return taskFilterStemPills.map(taskFilterStemPillModel);
}

function taskFilterStemPillModel(stem) {
  const drainability = taskFilterStemDrainability(stem);
  const label = stem.name;
  const openTaskCount = Math.max(0, Number(stem.openTaskCount) || 0);
  const classes = [];
  if (stem.name === "agent") classes.push("filter-pill--private");
  if (stem.name === "oops") classes.push("filter-pill--system");
  return {
    kind: "stem",
    label,
    openTaskCount,
    classes,
    drainability,
    title:
      openTaskCount +
      " open across " +
      taskFilterStemScopeLabel(label) +
      "; " +
      (drainability.drainable
        ? "drained by " + drainability.count
        : "not currently drained"),
  };
}

function taskFilterStemScopeLabel(stemName) {
  return stemName === "oops" ? "oops" : stemName + ".*";
}

function taskFilterStemIsSystem(stemName) {
  return stemName === "agent" || stemName === "oops";
}

function taskFilterStemDrainability(stem) {
  const covered = new Set(uniqueStringList([stem.name, ...(stem.filters || [])]));
  let count = 0;
  let boundaryDissolved = false;
  for (const lane of laneStates.values()) {
    if (!isLaneOpen(lane) || isShadowLane(lane)) continue;
    const lifetime = laneEffectiveLifetime(lane);
    if (
      !taskFilterStemIsSystem(stem.name) &&
      agentLifetimeDissolvesTaskBoundary(lifetime)
    ) {
      boundaryDissolved = true;
      count += laneGroupMemberLanes(lane).filter(laneMemberCanDrain).length;
      continue;
    }
    if (!agentLifetimeUsesStoredTaskFilters(lifetime)) continue;
    const assigned = laneAssignedTaskFilters(lane);
    if (!assigned.some((filter) => covered.has(filter))) continue;
    count += laneGroupMemberLanes(lane).filter(laneMemberCanDrain).length;
  }
  return { drainable: count > 0, count, boundaryDissolved };
}

function laneMemberCanDrain(member) {
  const statusLine = member.lastRenderedStatusLine || {};
  const fromTarget = targetById.get(member.targetId) || {};
  return (
    (statusLine.agentProcessStatus || fromTarget.agentProcessStatus) ===
    "running"
  );
}
