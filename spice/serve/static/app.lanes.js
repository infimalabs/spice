// Targets and server-team-backed lane topology. Opening an agent creates a
// team, the team snapshot reconciles lanes (including fused groups), and
// closing a lane closes its team. The server snapshot is the sole topology
// source; localStorage keeps only per-target interface hints (speech mode,
// selected view) applied to whatever lanes the snapshot mounts.

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
  if (!targetsLoaded) setGlobalActivityStatus("loading teams");
  if (spiceMenuEl) renderSpiceMenuIfAvailable();
  targetsLoadPromise = (async () => {
    try {
      const response = /** @type {TargetsFrame} */ (
        await liveBusRequest("targets.refresh")
      );
      applyTargetsPayload(response.payload);
    } catch (error) {
      setGlobalTransientError("team refresh failed");
    } finally {
      targetsLoading = false;
      targetsLoadPromise = null;
      if (spiceMenuEl) renderSpiceMenuIfAvailable();
    }
  })();
  return targetsLoadPromise;
}

/** @param {TargetsPayload} payload */
function applyTargetsPayload(payload) {
  laneStore.replaceTargets(payload.workTrees || []);
  targetsLoaded = true;
  syncObserverNotice(payload.observerErrors || []);
  clearGlobalActivityStatus("loading teams");
  applyTaskFilterInventory(payload.taskFilterInventory || {});
  for (const lane of laneStore.lanesSnapshot()) {
    if (!laneStore.targetForId(lane.targetId) && !lane.emptyTeam)
      closeLaneCore(lane);
  }
  renderFilterPills();
  for (const lane of laneStore.lanesSnapshot()) {
    if (lane.emptyTeam) syncEmptyTeamLane(lane);
    else renderLaneChrome(lane, laneStore.targetForId(lane.targetId));
  }
  if (spiceMenuEl) renderSpiceMenuIfAvailable();
}

function syncObserverNotice(errors) {
  if (!observerModeEnabled) return;
  let notice = lanesEl.querySelector(".observer-notice");
  if (!errors.length) {
    if (notice) notice.remove();
    return;
  }
  if (!notice) {
    notice = document.createElement("p");
    notice.className = "observer-notice";
    notice.setAttribute("role", "alert");
    lanesEl.prepend(notice);
  }
  notice.classList.toggle(
    "observer-notice--empty",
    laneStore.targetsSnapshot().length === 0,
  );
  notice.textContent = errors.join("; ");
}

function applyTaskFilterInventory(inventory) {
  const acceptedInventory = inventory || {};
  if (!taskFilterInventoryIsFresh(acceptedInventory)) return false;
  taskFilterInventoryRevision = taskFilterInventoryRevisionValue(acceptedInventory);
  taskFilterStemPills = taskFilterStemPillsFromInventory(acceptedInventory);
  syncTaskFilterInventoryState(acceptedInventory);
  renderTaskFilterInventoryPanes();
  return true;
}

function syncTaskFilterInventoryState(inventory) {
  laneStore.replaceTargets(
    laneStore.targetsSnapshot().map((target) => ({
      ...target,
      taskFilterInventory: inventory,
    })),
  );
  for (const lane of laneStore.lanesSnapshot())
    lane.taskFilterInventory = inventory;
}

function renderTaskFilterInventoryPanes() {
  if (
    typeof laneGroupHost !== "function" ||
    typeof renderLaneFiltersPane !== "function"
  )
    return;
  const renderedHosts = new Set();
  for (const lane of laneStore.lanesSnapshot()) {
    const host = laneGroupHost(lane);
    const key = host.targetId || lane.targetId || "";
    if (renderedHosts.has(key)) continue;
    renderedHosts.add(key);
    renderLaneFiltersPane(host);
  }
}

function taskFilterInventoryIsFresh(inventory) {
  const incomingRevision = taskFilterInventoryRevisionValue(inventory);
  if (!incomingRevision) return !taskFilterInventoryRevision;
  if (!taskFilterInventoryRevision) return true;
  return compareNonnegativeIntegerStrings(
    incomingRevision,
    taskFilterInventoryRevision,
  ) >= 0;
}

function taskFilterInventoryRevisionValue(inventory) {
  return String((inventory || {}).revision || "");
}

function compareNonnegativeIntegerStrings(left, right) {
  const lhs = normalizedNonnegativeIntegerString(left);
  const rhs = normalizedNonnegativeIntegerString(right);
  if (!lhs && !rhs) return 0;
  if (!lhs) return -1;
  if (!rhs) return 1;
  if (lhs.length !== rhs.length) return lhs.length < rhs.length ? -1 : 1;
  if (lhs === rhs) return 0;
  return lhs < rhs ? -1 : 1;
}

function normalizedNonnegativeIntegerString(value) {
  const text = String(value || "");
  if (!/^\d+$/.test(text)) return "";
  return text.replace(/^0+(?=\d)/, "");
}

// ---- team snapshot reconciliation ---------------------------------------------

async function refreshTeamSnapshot(options = {}) {
  const query = {};
  if (!options.force && laneStore.teamSnapshotRevision())
    query.sinceRevision = laneStore.teamSnapshotRevision();
  const response = /** @type {TeamsFrame} */ (
    await liveBusRequest("teams.refresh", { query })
  );
  applyTeamSnapshotPayload(response.payload, options);
}

async function requestTeamCommand(payload) {
  const response = /** @type {TeamCommandFrame} */ (
    await liveBusRequest("teams.command", { payload })
  );
  const result = response.result;
  if (result.snapshot)
    applyTeamSnapshotPayload(
      { revision: result.revision, changed: true, snapshot: result.snapshot },
      { force: true },
    );
  if (result.ok === false) {
    await refreshTeamSnapshot({ force: true });
    throw new Error(result.error || "team command failed");
  }
  return result;
}

function teamCommandPayload(command, fields = {}) {
  return laneStore.teamCommandPayload(command, fields);
}

/**
 * @param {TeamSnapshotResponse} payload
 * @param {Object=} options
 */
function applyTeamSnapshotPayload(payload, options = {}) {
  return laneStore.applyTeamSnapshot(payload, {
    force: Boolean(options.force),
    emptyTeamTargetId,
    resolveMember: resolveTeamSnapshotMember,
    unresolvedLaneTargetIds: unresolvedTeamLaneTargetIds,
    canRemoveLane: (lane) =>
      !laneHasUnsafeDraft(lane) || Boolean(lane.serverCloseRequested),
  });
}

laneStore.subscribe((change) => {
  if (change.kind !== "teamSnapshot") return;
  materializeTeamSnapshotTransition(change.transition);
});

// Menu refresh, filter panes, and lane-hint persistence are explicit store
// subscribers rather than an imperative tail on lane materialization. Each
// gates on the fields it consumes, so a quiet retained-only snapshot repaints
// nothing; membership changes refresh the menu, and any lane add/update/remove
// refreshes the filter pills.
laneStore.subscribe((change) => {
  if (change.kind !== "teamSnapshot") return;
  const transition = change.transition;
  if (transition.disposition !== "applied") return;
  if (targetsLoaded) persistLaneHints();
  if (transition.adds.length || transition.removes.length)
    renderSpiceMenuIfAvailable();
  if (
    transition.adds.length ||
    transition.updates.length ||
    transition.removes.length
  )
    renderFilterPills();
});

// Media-session narration state follows the open-lane set: a lane leaving the
// store can flip whether any narration is active, so it reconciles as an
// explicit lane-store subscriber instead of an imperative tail on lane close.
laneStore.subscribe((change) => {
  if (change.kind !== "lanes" || change.transition !== "removed") return;
  syncNarrationMediaSession();
});

function materializeTeamSnapshotTransition(transition) {
  if (transition.disposition !== "applied") return;
  applyGlobalSettingsPayload(transition.globalSettings);
  const hints = laneHintsByTargetId();
  for (const renewal of transition.renewals)
    renameTeamMemberTargetThread(renewal.targetId, renewal.actorId);
  for (const change of [...transition.adds, ...transition.updates]) {
    if (change.kind === "emptyTeam")
      ensureEmptyTeamLane(change.team, { canClose: change.canClose });
    else
      ensureTeamMemberLane(
        change.targetId,
        change.team,
        hints.get(change.targetId),
        change.member,
      );
  }
  for (const lane of transition.removes) closeLaneCore(lane);
  reconcileLaneGroups(transition.groupRuns);
}

// A team member is an explicit actor: target:<target-id> before a thread binds,
// then thread:<canonical-thread-id> once an agent exists.
function resolveTeamSnapshotMember(member, team, claimedTargetIds = []) {
  const agentId = String((member || {}).agentId || "");
  const actorId = normalizeTeamActorId(agentId);
  if (!actorId) return {};
  if (teamActorKind(actorId) === "target") {
    const targetId = teamActorValue(actorId);
    return laneStore.targetForId(targetId) ? { targetId, actorId } : {};
  }
  const laneTargetId = teamMemberLaneTargetId(actorId);
  if (laneTargetId)
    return {
      targetId: laneTargetId,
      actorId,
      threadId: teamActorThreadId(actorId),
    };
  for (const target of laneStore.targetsSnapshot()) {
    if (teamActorMatchesThread(actorId, targetIdentityThreadId(target.targetIdentity)))
      return { targetId: target.id, actorId };
  }
  const renewedTargetId = renewedTeamSlotTargetId(team, actorId, claimedTargetIds);
  if (renewedTargetId)
    return {
      targetId: renewedTargetId,
      actorId,
      threadId: teamActorThreadId(actorId),
    };
  return {};
}

function teamMemberLaneTargetId(actorId) {
  for (const lane of laneStore.lanesSnapshot()) {
    if (!laneStore.targetForId(lane.targetId)) continue;
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
  for (const lane of laneStore.lanesSnapshot()) {
    if (lane.emptyTeam || claimed.has(lane.targetId)) continue;
    const target = laneStore.targetForId(lane.targetId);
    if (!target) continue;
    if (String(lane.teamId || teamIdentityTeamId(target.teamIdentity)) !== teamId)
      continue;
    const laneActorId = laneTeamAgentId(lane);
    if (!laneActorId || actorIds.has(laneActorId)) continue;
    candidates.push(lane.targetId);
  }
  if (candidates.length !== 1) return "";
  return candidates[0];
}

function renameTeamMemberTargetThread(targetId, actorId) {
  const threadId = teamActorThreadId(actorId);
  if (!threadId) return;
  laneStore.updateTarget(targetId, (target) => ({
    ...target,
    targetIdentity: {
      ...(target.targetIdentity || {}),
      thread: { state: "bound", threadId },
    },
  }));
  const lane = laneStore.laneForId(targetId);
  if (!lane) return;
  lane.targetThreadId = threadId;
  lane.activeThreadId = threadId;
  if (typeof ensureLaneOccupant === "function") ensureLaneOccupant(lane, threadId);
}

function unresolvedTeamLaneTargetIds(team) {
  const teamId = String((team || {}).teamId || "");
  if (!teamId) return [];
  const actorIds = teamMemberActorIds(team);
  const targetIds = [];
  for (const lane of laneStore.lanesSnapshot()) {
    if (lane.emptyTeam) continue;
    const target = laneStore.targetForId(lane.targetId);
    if (!target) continue;
    if (String(lane.teamId || teamIdentityTeamId(target.teamIdentity)) !== teamId)
      continue;
    const laneActorId = laneTeamAgentId(lane);
    if (laneActorId && actorIds.has(laneActorId)) continue;
    targetIds.push(lane.targetId);
  }
  return targetIds;
}

function teamMemberActorIds(team) {
  const actorIds = new Set();
  for (const member of (team || {}).members || []) {
    const actorId = normalizeTeamActorId((member || {}).agentId);
    if (actorId) actorIds.add(actorId);
  }
  return actorIds;
}

function ensureTeamMemberLane(targetId, team, hint = null, member = null) {
  if (!laneStore.hasLane(targetId)) addLane(targetId, hint, { persist: false });
  const lane = laneStore.laneForId(targetId);
  if (!lane) return;
  const previousConfigRevision = lane.configRevision || 0;
  const config = team.config || {};
  const splitBack = team.splitBack || {};
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
  if (Array.isArray(config.effectiveTaskFilters))
    lane.effectiveTaskFilters = uniqueStringList(config.effectiveTaskFilters);
  if (Array.isArray(config.taskFilterEntries))
    lane.taskFilterEntries = normalizedTaskFilterEntries(config.taskFilterEntries);
  if (config.lifetime)
    applyServerLaneLifetime(lane, config.lifetime, {
      configRevision: config.revision,
    });
  syncLaneEffectiveControls(lane);
  // The server no longer wakes lanes on a team config change (it would reflow
  // every team). Instead, a lane whose OWN team config revision advanced
  // re-fetches its now-differently-filtered tasks; lanes on unchanged teams
  // keep the same revision and never resubscribe, so they do not reflow.
  if (
    lane.liveBusSubscribed &&
    previousConfigRevision > 0 &&
    lane.configRevision > previousConfigRevision &&
    typeof subscribeLaneToLiveBus === "function"
  )
    subscribeLaneToLiveBus(lane);
}

function laneTeamAgentId(lane) {
  const actor = threadTeamActorId(lane.targetThreadId);
  if (actor) return actor;
  const target = laneStore.targetForId(lane.targetId);
  const targetActor = target
    ? threadTeamActorId(targetIdentityThreadId(target.targetIdentity))
    : "";
  return targetActor || targetTeamActorId(lane.targetId);
}

// Every actor form this member could be persisted under, minus the primary:
// the target actor (stable) AND the thread actor (whichever the primary
// isn't). Membership rows may be stored under either form depending on how
// the member joined, so a reorder/move resolves against the widest net
// rather than guessing one form.
function laneTeamAgentAliases(lane) {
  const actor = laneTeamAgentId(lane);
  const aliases = [
    targetTeamActorId(lane.targetId),
    threadTeamActorId(lane.targetThreadId),
    threadTeamActorId(lane.activeThreadId),
  ].filter((alias) => alias && alias !== actor);
  return uniqueStringList(aliases);
}

// ---- open / close ---------------------------------------------------------------

async function openTargetTeam(targetId, options = {}) {
  const keepMenuOpen = Boolean(options.keepMenuOpen);
  if (laneStore.hasLane(targetId)) {
    const lane = laneStore.laneForId(targetId);
    if (lane)
      lane.element.scrollIntoView({ block: "nearest", inline: "nearest" });
    if (!keepMenuOpen) closeSpiceMenu();
    return;
  }
  const target = laneStore.targetForId(targetId);
  if (!target) throw new Error("open team requires a known target");
  await refreshTeamSnapshot({ force: true });
  if (!laneStore.hasLane(targetId)) {
    await requestTeamCommand(
      teamCommandPayload("createTeam", {
        members: [targetTeamAgentId(target)],
        config: defaultTeamConfig(),
      }),
    );
    if (keepMenuOpen) await refreshTargets();
  }
  if (!keepMenuOpen) closeSpiceMenu();
}

function closeLane(lane) {
  const host = laneGroupHost(lane);
  if (host.emptyTeam && !host.emptyTeamCanClose) return;
  if (!host.teamId) return;
  if (!host.emptyTeam && laneHasUnsafeDraft(host)) {
    if (!window.confirm(unsafeDraftWarningText())) return;
  }
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
  laneStore.removeLane(lane.targetId);
}

function laneHasUnsafeDraft(lane) {
  if (!isLaneOpen(lane)) return false;
  if (laneComposerDraftText(lane).trim()) return true;
  return lane.sendAwaitingBackendCount > 0;
}

function servePageHasUnsafeComposerState() {
  for (const lane of laneStore.lanesSnapshot()) {
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
  for (const lane of laneStore.lanesSnapshot()) {
    if (!isLaneOpen(lane)) continue;
    if (lane.emptyTeam || !laneStore.targetForId(lane.targetId)) continue;
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
    const target = laneStore.targetForId(button.dataset.targetChoiceId || "");
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
  const lane = laneStore.laneForId(target.id);
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
  if (laneStore.hasLane(target.id)) parts.push("open");
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
  merged.latestActivityKind = payloadHasField(
    laneStatusLine,
    "latestActivityKind",
  )
    ? String(laneStatusLine.latestActivityKind || "")
    : String(statusLine.latestActivityKind || "");
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
  const lane = laneStore.laneForId(target.id);
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
const taskFilterStemStateCountFields = [
  "readyTaskCount",
  "inFlightTaskCount",
  "blockedTaskCount",
  "deferredTaskCount",
];

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
      readyTaskCount: model.readyTaskCount,
      inFlightTaskCount: model.inFlightTaskCount,
      blockedTaskCount: model.blockedTaskCount,
      deferredTaskCount: model.deferredTaskCount,
      unavailableTaskCount: model.unavailableTaskCount,
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
    const tone = taskFilterStemPillTone(model);
    if (
      tone === "ready" &&
      model.drainability.boundaryDissolved &&
      model.drainability.drainable
    )
      classes.push("filter-pill--implicit");
    classes.push("filter-pill--" + tone);
    pill.className = classes.join(" ");
    pill.title = model.title;
    pill.dataset.readyTaskCount = String(model.readyTaskCount);
    pill.dataset.openTaskCount = String(model.openTaskCount);
    pill.dataset.inFlightTaskCount = String(model.inFlightTaskCount);
    pill.dataset.blockedTaskCount = String(model.blockedTaskCount);
    pill.dataset.deferredTaskCount = String(model.deferredTaskCount);
    pill.dataset.unavailableTaskCount = String(model.unavailableTaskCount);
    pill.innerHTML =
      '<span class="filter-pill-label"></span>' +
      '<span class="filter-pill-count"></span>';
    pill.querySelector(".filter-pill-label").textContent = model.label;
    pill.querySelector(".filter-pill-count").textContent =
      taskFilterStemPillCountText(model);
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
  for (const field of taskFilterStemStateCountFields) {
    if (!Object.prototype.hasOwnProperty.call(stem, field))
      throw new Error("task-filter stem missing " + field + ": " + label);
  }
  const readyTaskCount = Math.max(0, Number(stem.readyTaskCount) || 0);
  const inFlightTaskCount = Math.max(
    0,
    Number(stem.inFlightTaskCount) || 0,
  );
  const blockedTaskCount = Math.max(0, Number(stem.blockedTaskCount) || 0);
  const deferredTaskCount = Math.max(0, Number(stem.deferredTaskCount) || 0);
  const unavailableTaskCount = blockedTaskCount + deferredTaskCount;
  const stateTaskCount =
    readyTaskCount + inFlightTaskCount + unavailableTaskCount;
  if (stateTaskCount !== openTaskCount)
    throw new Error(
      "task-filter stem state counts total " +
        stateTaskCount +
        " but openTaskCount is " +
        openTaskCount +
        ": " +
        label,
    );
  const classes = [];
  if (stem.name === "agent") classes.push("filter-pill--private");
  if (stem.name === "oops") classes.push("filter-pill--system");
  return {
    kind: "stem",
    label,
    openTaskCount,
    readyTaskCount,
    inFlightTaskCount,
    blockedTaskCount,
    deferredTaskCount,
    unavailableTaskCount,
    classes,
    drainability,
    title: taskFilterStemPillTitle(
      label,
      {
        openTaskCount,
        readyTaskCount,
        inFlightTaskCount,
        blockedTaskCount,
        deferredTaskCount,
      },
      drainability,
    ),
  };
}

function taskFilterStemPillCountText(model) {
  if (model.label === "oops") return String(model.openTaskCount);
  let text = String(model.readyTaskCount);
  if (model.inFlightTaskCount > 0 || model.unavailableTaskCount > 0)
    text += "/" + model.inFlightTaskCount;
  if (model.unavailableTaskCount > 0)
    text += "/" + model.unavailableTaskCount;
  return text;
}

function taskFilterStemPillTone(model) {
  if (model.readyTaskCount > 0) return "ready";
  if (model.inFlightTaskCount > 0) return "active";
  return "dormant";
}

function taskFilterStemPillTitle(label, counts, drainability) {
  const {
    openTaskCount,
    readyTaskCount,
    inFlightTaskCount,
    blockedTaskCount,
    deferredTaskCount,
  } = counts;
  if (label === "oops")
    return (
      openTaskCount +
      " oops/deferred triage tasks; inspect with `spice task oops`"
    );
  return (
    readyTaskCount +
    " ready, " +
    inFlightTaskCount +
    " active/in flight, " +
    blockedTaskCount +
    " blocked, " +
    deferredTaskCount +
    " deferred; " +
    openTaskCount +
    " open across " +
    taskFilterStemScopeLabel(label) +
    "; " +
    (readyTaskCount > 0
      ? drainability.drainable
        ? "ready work drained by " + drainability.count
        : "ready work not currently drained"
      : inFlightTaskCount > 0
        ? "work in flight"
        : "no task currently movable")
  );
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
  for (const lane of laneStore.lanesSnapshot()) {
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
  const fromTarget = laneStore.targetForId(member.targetId) || {};
  return (
    (statusLine.agentProcessStatus || fromTarget.agentProcessStatus) ===
    "running"
  );
}
