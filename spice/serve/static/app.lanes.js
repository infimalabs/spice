// Targets and server-team-backed lane topology. Which lanes
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

function renderSpiceMenuIfAvailable() {
  if (typeof renderSpiceMenu === "function") renderSpiceMenu();
}

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
  if (spiceMenuEl) renderSpiceMenuIfAvailable();
  targetsLoadPromise = (async () => {
    try {
      const response = await liveBusRequest("targets.refresh");
      applyTargetsPayload(response.payload || {});
    } catch (error) {
      setGlobalTransientStatus("team refresh failed");
    } finally {
      targetsLoading = false;
      targetsLoadPromise = null;
      if (spiceMenuEl) renderSpiceMenuIfAvailable();
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
  if (spiceMenuEl) renderSpiceMenuIfAvailable();
}

// Targets carry statusLine and route facts in the same field names the lane
// payload uses; the shim only fills the names renderLaneChrome reads.
function targetPayloadShim(target) {
  if (!target) return { statusLine: {} };
  return {
    targetIdentity: target.targetIdentity,
    taskFilters: target.taskFilters || [],
    laneFilterVersion: target.laneFilterVersion || "",
    teamIdentity: target.teamIdentity,
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
  if (!sameStringSets(openBefore, laneStateTargetIds()))
    renderSpiceMenuIfAvailable();
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

// A team member is an explicit actor: target:<target-id> before a thread binds,
// then thread:<canonical-thread-id> once an agent exists.
function teamMemberTargetId(member, team = null, claimedTargetIds = []) {
  const agentId = String((member || {}).agentId || "");
  const actorId = normalizeTeamActorId(agentId);
  if (!actorId) return "";
  if (teamActorKind(actorId) === "target") {
    const targetId = teamActorValue(actorId);
    return targetById.has(targetId) ? targetId : "";
  }
  const laneTargetId = teamMemberLaneTargetId(actorId);
  if (laneTargetId) {
    renameTeamMemberTargetThread(laneTargetId, actorId);
    return laneTargetId;
  }
  for (const target of targets) {
    if (teamActorMatchesThread(actorId, targetIdentityThreadId(target.targetIdentity)))
      return target.id;
  }
  const renewedTargetId = renewedTeamSlotTargetId(team, actorId, claimedTargetIds);
  if (renewedTargetId) return renewedTargetId;
  return "";
}

function teamMemberLaneTargetId(actorId) {
  for (const lane of laneStates.values()) {
    if (!targetById.has(lane.targetId)) continue;
    if (
      teamActorMatchesThread(actorId, lane.targetThreadId) ||
      teamActorMatchesThread(actorId, lane.activeThreadId)
    )
      return lane.targetId;
  }
  return "";
}

function renewedTeamSlotTargetId(team, actorId, claimedTargetIds = []) {
  const teamId = String((team || {}).teamId || "");
  if (!teamId) return "";
  const actorIds = teamMemberActorIds(team);
  const claimed = new Set(claimedTargetIds);
  const candidates = [];
  for (const lane of laneStates.values()) {
    if (lane.emptyTeam || claimed.has(lane.targetId)) continue;
    const target = targetById.get(lane.targetId);
    if (!target) continue;
    if (String(lane.teamId || teamIdentityTeamId(target.teamIdentity)) !== teamId)
      continue;
    const laneActorId = laneTeamAgentId(lane);
    if (!laneActorId || actorIds.has(laneActorId)) continue;
    candidates.push(lane.targetId);
  }
  if (candidates.length !== 1) return "";
  renameTeamMemberTargetThread(candidates[0], actorId);
  return candidates[0];
}

function renameTeamMemberTargetThread(targetId, actorId) {
  const threadId = teamActorThreadId(actorId);
  if (!threadId) return;
  const target = targetById.get(targetId);
  if (target)
    target.targetIdentity = {
      ...(target.targetIdentity || {}),
      thread: { state: "bound", threadId },
    };
  const lane = laneStates.get(targetId);
  if (!lane) return;
  lane.targetThreadId = threadId;
  lane.activeThreadId = threadId;
  if (typeof ensureLaneOccupant === "function") ensureLaneOccupant(lane, threadId);
}

function preserveUnresolvedTeamLanes(team, openTargetIds) {
  const teamId = String((team || {}).teamId || "");
  if (!teamId) return;
  const actorIds = teamMemberActorIds(team);
  for (const lane of laneStates.values()) {
    if (lane.emptyTeam) continue;
    const target = targetById.get(lane.targetId);
    if (!target) continue;
    if (String(lane.teamId || teamIdentityTeamId(target.teamIdentity)) !== teamId)
      continue;
    const laneActorId = laneTeamAgentId(lane);
    if (laneActorId && actorIds.has(laneActorId)) continue;
    openTargetIds.add(lane.targetId);
  }
}

function teamMemberActorIds(team) {
  const actorIds = new Set();
  for (const member of (team || {}).members || []) {
    const actorId = normalizeTeamActorId((member || {}).agentId);
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
  const actor = threadTeamActorId(lane.targetThreadId);
  if (actor) return actor;
  const target = targetById.get(lane.targetId);
  const targetActor = target
    ? threadTeamActorId(targetIdentityThreadId(target.targetIdentity))
    : "";
  return targetActor || targetTeamActorId(lane.targetId);
}

function laneTeamAgentAliases(lane) {
  const actor = laneTeamAgentId(lane);
  const aliases = [
    targetTeamActorId(lane.targetId),
    String(lane.targetId || "").trim(),
    teamActorThreadId(actor),
  ].filter((alias) => alias && alias !== actor);
  return uniqueStringList(aliases);
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
          members: [targetTeamAgentId(target)],
          config: {
            ...defaultTeamConfig(),
            speechMode: hint ? hint.speechMode : defaultSpeechMode,
            selectedView: hint ? hint.selectedView : defaultLaneViewMode,
          },
        }),
      );
      if (keepMenuOpen) await refreshTargets();
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
  return targetIdentityDisplayLabel(target.targetIdentity);
}

function targetChoiceMetadata(target) {
  const parts = [];
  if (laneStates.has(target.id)) parts.push("open");
  const activity = relativeTime(targetChoiceLastAssistantAt(target));
  if (activity) parts.push(activity.trim());
  else if (!targetIdentityThreadId(target.targetIdentity)) parts.push("never");
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
    targetIdentityThreadState(target.targetIdentity) ||
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
  const byStatus =
    targetChoiceStatusOrder(left) - targetChoiceStatusOrder(right);
  if (byStatus) return byStatus;
  const byRecency = compareTargetChoiceRecency(left, right);
  if (byRecency) return byRecency;
  const byName = targetChoiceName(left).localeCompare(targetChoiceName(right));
  if (byName) return byName;
  return String(left.id || "").localeCompare(String(right.id || ""));
}

function compareTargetChoiceRecency(left, right) {
  if (targetChoiceIsRunning(left) || targetChoiceIsRunning(right)) return 0;
  const leftAt = targetChoiceLastAssistantAt(left);
  const rightAt = targetChoiceLastAssistantAt(right);
  if (leftAt && rightAt && leftAt !== rightAt) return leftAt > rightAt ? -1 : 1;
  if (leftAt && !rightAt) return -1;
  if (!leftAt && rightAt) return 1;
  return 0;
}

function targetChoiceIsRunning(target) {
  const status = targetChoiceStatus(target);
  return status === "running" || status === "running-stale";
}

function targetChoiceStatusOrder(target) {
  const status = targetChoiceStatus(target);
  const index = targetChoiceStatusValues.indexOf(status);
  return index === -1 ? targetChoiceStatusValues.length : index;
}

function targetChoiceStatus(target) {
  if (targetChoicePendingCount(target) > 0) return "pending";
  const statusLine = targetChoiceStatusLine(target);
  if (statusLine.error) return "unknown";
  const visualStatus = targetChoiceKnownStatus(liveAgentVisualStatus(statusLine));
  if (visualStatus && visualStatus !== "unknown") return visualStatus;
  const processStatus = targetChoiceKnownStatus(statusLine.agentProcessStatus);
  if (processStatus && processStatus !== "unknown") return processStatus;
  return statusLine.bindingStatus === "bound" ||
    targetIdentityThreadId(target.targetIdentity)
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
