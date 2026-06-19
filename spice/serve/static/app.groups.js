// Lane groups: explicit topology over concrete target lanes, backed by server
// teams. A gutter drop on a lane edge fuses (merges teams); the team menu
// exposes close and split actions. The visible host
// aggregates chrome and renders one merged, newest-first stream attributed per
// agent; member targets remain the concrete send/refresh/drain addresses.

const laneFuseGutterFraction = 0.2;
const laneDragThresholdPx = 6;
const laneLightPipSizePx = 9;
const laneLightRowGapPx = 4;
const laneLightColumnGapPx = 5;
const laneLightColumnCapacity = 2;
let laneDragState = null;
let composerMoveDragState = null;
let laneTeamMenuDismissHandler = null;

function reconcileLaneGroups(groupRuns) {
  const lifetimeStateByTargetId = new Map();
  const previousHostByMemberTargetId = currentLaneGroupHostByMemberTargetId();
  for (const lane of laneStates.values())
    lifetimeStateByTargetId.set(lane.targetId, laneLifetimeRuntimeState(lane));
  for (const lane of laneStates.values()) {
    lane.groupTopology = null;
    lane.element.classList.remove("lane--shadowed");
  }
  for (const run of groupRuns) {
    const members = run
      .map((targetId) => laneStates.get(targetId))
      .filter((lane) => lane && isLaneOpen(lane));
    if (members.length < 2) continue;
    const host = stableLaneGroupHost(members, previousHostByMemberTargetId);
    const shadows = members.filter((member) => member !== host);
    const memberTargetIds = members.map((member) => member.targetId);
    host.groupTopology = {
      role: "host",
      hostTargetId: host.targetId,
      memberTargetIds,
    };
    restoreLaneLifetimeRuntimeState(
      host,
      pendingLaneLifetimeStateForMembers(members, lifetimeStateByTargetId),
    );
    for (const shadow of shadows) {
      shadow.groupTopology = {
        role: "member",
        hostTargetId: host.targetId,
        memberTargetIds,
      };
      shadow.element.classList.add("lane--shadowed");
    }
    syncLaneGroupDomOrder(host);
  }
  for (const lane of laneStates.values()) {
    syncFusedLaneChrome(lane);
    renderMessagesIfChanged(lane);
  }
}

function currentLaneGroupHostByMemberTargetId() {
  const hosts = new Map();
  const seenHosts = new Set();
  for (const lane of laneStates.values()) {
    const host = laneGroupHost(lane);
    if (seenHosts.has(host.targetId)) continue;
    seenHosts.add(host.targetId);
    const memberTargetIds = laneGroupMemberTargetIds(host);
    if (memberTargetIds.length < 2) continue;
    for (const targetId of memberTargetIds) hosts.set(targetId, host.targetId);
  }
  return hosts;
}

function stableLaneGroupHost(members, previousHostByMemberTargetId) {
  for (const member of members) {
    const previousHostId = previousHostByMemberTargetId.get(member.targetId);
    const previousHost = members.find(
      (candidate) => candidate.targetId === previousHostId,
    );
    if (previousHost) return previousHost;
  }
  return members[0];
}

function pendingLaneLifetimeStateForMembers(members, lifetimeStateByTargetId) {
  for (const member of members) {
    const state = lifetimeStateByTargetId.get(member.targetId);
    if (state && state.pendingLifetimeCommit) return state;
  }
  return null;
}

function laneGroupRole(lane) {
  return lane.groupTopology ? lane.groupTopology.role : "standalone";
}

function isShadowLane(lane) {
  return laneGroupRole(lane) === "member";
}

function laneIsFusedHost(lane) {
  return (
    laneGroupRole(lane) === "host" && laneGroupMemberTargetIds(lane).length > 1
  );
}

function laneGroupHost(lane) {
  if (!isShadowLane(lane)) return lane;
  return laneStates.get(lane.groupTopology.hostTargetId) || lane;
}

function laneGroupMemberTargetIds(lane) {
  return lane.groupTopology
    ? lane.groupTopology.memberTargetIds
    : [lane.targetId];
}

function laneGroupMemberLanes(lane) {
  const host = laneGroupHost(lane);
  return laneGroupMemberTargetIds(host)
    .map((id) => laneStates.get(id))
    .filter((member) => member && isLaneOpen(member));
}

function syncLaneGroupDomOrder(host) {
  const groupHost = laneGroupHost(host);
  let previous = groupHost.element;
  for (const member of laneGroupMemberLanes(groupHost)) {
    if (member === groupHost) continue;
    const reference = previous.nextElementSibling;
    if (reference !== member.element)
      lanesEl.insertBefore(member.element, reference);
    previous = member.element;
  }
}

// One merged, newest-first stream across every member, attributed per agent.
function laneGroupMergedMessages(host) {
  const seen = new Set();
  const merged = [];
  for (const member of laneGroupMemberLanes(host)) {
    for (const item of member.knownMessages) {
      if (seen.has(item.key)) continue;
      seen.add(item.key);
      stampMessageProducer(item, member, member.activeThreadId || "");
      ensureLaneOccupant(host, item.threadId);
      merged.push(item);
    }
  }
  merged.sort((a, b) => {
    const at = a.timestamp || "";
    const bt = b.timestamp || "";
    if (at !== bt) return at < bt ? 1 : -1;
    return (b.index || 0) - (a.index || 0);
  });
  return merged;
}

// The agent's voice name leads; a nameless (or branch-named) agent shows the
// branch alone — never "main on main".
function laneMemberTargetLabel(member) {
  const agent = member.agentName || "";
  const branch = member.branchName || member.targetId || "this branch";
  if (!agent || agent === branch) return branch;
  return agent + " on " + branch;
}

function syncFusedLaneChrome(lane) {
  if (!lane || isShadowLane(lane)) return;
  if (lane.emptyTeam) {
    syncEmptyTeamLane(lane);
    return;
  }
  const members = laneGroupMemberLanes(lane);
  const fused = members.length > 1;
  syncLaneLights(lane, fused ? members : []);
  syncFusedLaneStatusLine(lane);
  syncLaneTeamMenuButton(lane);
  syncComposerShards(lane, fused ? members : [lane]);
  syncLaneEffectiveControls(lane);
}

function syncFusedLaneStatusLine(lane) {
  if (!lane || isShadowLane(lane)) return;
  if (!laneIsFusedHost(lane)) {
    restoreStandaloneLaneStatusLine(lane);
    return;
  }
  const statusLine = fusedLaneLatestStatusLine(laneGroupMemberLanes(lane));
  if (!statusLine) return;
  lane.renderedFusedStatusLine = true;
  setLaneStatus(lane, statusLine);
}

function restoreStandaloneLaneStatusLine(lane) {
  if (!lane.renderedFusedStatusLine) return;
  lane.renderedFusedStatusLine = false;
  lane.renderedStatusFingerprint = "";
  setLaneStatus(lane, lane.lastRenderedStatusLine || {});
}

function fusedLaneLatestStatusLine(members) {
  const statuses = members
    .map((member) => fusedLaneMemberStatusLine(member))
    .filter(Boolean);
  const error = statuses.find((statusLine) => statusLine.error);
  if (error) return error;
  return statuses.reduce((latest, statusLine) =>
    fusedLaneStatusTime(statusLine) > fusedLaneStatusTime(latest)
      ? statusLine
      : latest,
  );
}

function fusedLaneMemberStatusLine(member) {
  const statusLine = member.lastRenderedStatusLine || {};
  const error = statusLine.error || "";
  if (error) return { error, lastAssistantAt: "", preview: "" };
  const preview =
    statusLine.preview ||
    statusLine.latestActivityPreview ||
    statusLine.latestMessagePreview ||
    "";
  if (preview)
    return {
      error: "",
      lastAssistantAt: statusLine.lastAssistantAt || "",
      preview,
    };
  const visualStatus =
    statusLine.agentVisualStatus || statusLine.agentProcessStatus || "";
  return {
    error: "",
    lastAssistantAt: "",
    preview: fusedLaneStatusWord(visualStatus),
  };
}

function fusedLaneStatusTime(statusLine) {
  const timestamp = Date.parse(statusLine.lastAssistantAt || "");
  return Number.isFinite(timestamp) ? timestamp : 0;
}

function fusedLaneStatusWord(status) {
  if (status === "running-stale") return "quiet";
  if (status === "running") return "running";
  if (status === "idle") return "idle";
  if (status === "stopped") return "stopped";
  if (status === "unstarted") return "unstarted";
  return status || "unknown";
}

function syncLaneTeamMenuButton(lane) {
  lane.teamMenuButtonEl.disabled = false;
  lane.teamMenuButtonEl.removeAttribute("aria-hidden");
  lane.teamMenuButtonEl.removeAttribute("tabindex");
  lane.teamMenuButtonEl.title = "Team actions";
  lane.teamMenuButtonEl.setAttribute("aria-label", "Team actions");
}

function toggleLaneTeamMenu(lane, event = null) {
  if (event) event.stopPropagation();
  const host = laneGroupHost(lane);
  if (host.emptyTeam && !host.emptyTeamCanClose) return;
  const open = host.teamMenuButtonEl.getAttribute("aria-expanded") === "true";
  closeLaneTeamMenusExcept(host);
  closeLaneTeamMenu(host);
  if (open) return;
  openLaneTeamMenu(host);
}

function openLaneTeamMenu(host) {
  const menu = document.createElement("div");
  menu.className = "lane-team-menu spice-menu-actions";
  menu.setAttribute("role", "menu");
  menu.replaceChildren(
    ...laneTeamMenuActions(host).map((action) =>
      renderLaneTeamMenuAction(host, action),
    ),
  );
  if (host.emptyTeam) {
    menu.classList.add("lane-team-menu--empty-team-overlay");
    positionEmptyTeamMenuOverlay(host, menu);
    host.element.append(menu);
  } else {
    host.viewStackEl.append(menu);
  }
  host.element.classList.add("lane--team-menu-open");
  host.teamMenuButtonEl.setAttribute("aria-expanded", "true");
  syncLaneTeamMenuDismissHandler();
}

function positionEmptyTeamMenuOverlay(host, menu) {
  syncLanePaneMetrics(host);
  menu.style.setProperty(
    "--lane-team-menu-top",
    host.viewStackEl.offsetTop + "px",
  );
  menu.style.setProperty(
    "--lane-team-menu-height",
    lanePaneMaxHeight(host) + "px",
  );
}

function laneTeamMenuActions(host) {
  if (host.emptyTeam) return [closeTeamMenuAction(host)];
  return [
    closeTeamMenuAction(host),
    importAgentMenuAction(host),
    splitIndividualsMenuAction(host),
    restorePreviousTeamMenuAction(host),
  ];
}

function closeTeamMenuAction(host) {
  return {
    label: "Close team",
    detail: host.emptyTeam
      ? "empty"
      : laneTeamMemberCountText(laneGroupMemberLanes(host).length),
    onClick: () => closeLane(host),
  };
}

function importAgentMenuAction(host) {
  return {
    label: "Import agent",
    detail: host.teamImportOverlayOpen ? "hide panel" : "cover messages",
    onClick: () => toggleTeamImportOverlay(host),
  };
}

function splitIndividualsMenuAction(host) {
  const members = laneGroupMemberLanes(host);
  return {
    label: "Split into individuals",
    detail: splitIndividualsMenuDetail(members.length),
    disabled: members.length < 2,
    onClick: () => splitLaneGroupIntoIndividuals(host),
  };
}

function restorePreviousTeamMenuAction(host) {
  return {
    label: "Restore previous team",
    detail: laneRestorePreviousTeamDetail(host),
    disabled: !host.teamSplitBackAvailable,
    onClick: () => restorePreviousTeam(host),
  };
}

function splitIndividualsMenuDetail(count) {
  if (count > 1) return "one team per agent";
  return "already individual";
}

function laneTeamMemberCountText(count) {
  if (count === 1) return "1 agent";
  return count + " agents";
}

function laneRestorePreviousTeamDetail(host) {
  const count = Math.max(0, Number(host.teamSplitBackMemberCount || 0));
  if (!host.teamSplitBackAvailable || !count) return "no saved subgroup";
  return laneTeamMemberCountText(count);
}

function renderLaneTeamMenuAction(host, action) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "lane-team-menu-action spice-menu-action";
  button.setAttribute("role", "menuitem");
  button.disabled = Boolean(action.disabled);
  button.innerHTML =
    '<span class="spice-menu-action-label"></span>' +
    '<span class="spice-menu-action-detail"></span>';
  button.querySelector(".spice-menu-action-label").textContent = action.label;
  button.querySelector(".spice-menu-action-detail").textContent =
    action.detail || "";
  button.addEventListener("click", () => {
    closeLaneTeamMenu(host);
    action.onClick();
  });
  return button;
}

function closeLaneTeamMenu(host) {
  host.element.querySelector(".lane-team-menu")?.remove();
  host.element.classList.remove("lane--team-menu-open");
  host.teamMenuButtonEl.setAttribute("aria-expanded", "false");
  syncLaneTeamMenuDismissHandler();
}

function closeLaneTeamMenusExcept(exceptHost = null) {
  for (const lane of laneStates.values()) {
    if (exceptHost && laneGroupHost(lane) === exceptHost) continue;
    closeLaneTeamMenu(laneGroupHost(lane));
  }
}

function syncLaneTeamMenuDismissHandler() {
  const hasOpenMenu = document.querySelector(".lane--team-menu-open");
  if (hasOpenMenu && !laneTeamMenuDismissHandler) {
    laneTeamMenuDismissHandler = dismissLaneTeamMenusOnPointerDown;
    document.addEventListener("pointerdown", laneTeamMenuDismissHandler, true);
  } else if (!hasOpenMenu && laneTeamMenuDismissHandler) {
    document.removeEventListener("pointerdown", laneTeamMenuDismissHandler, true);
    laneTeamMenuDismissHandler = null;
  }
}

function dismissLaneTeamMenusOnPointerDown(event) {
  const target = event.target;
  if (!(target instanceof Node)) return;
  for (const lane of laneStates.values()) {
    const host = laneGroupHost(lane);
    if (!host.element.classList.contains("lane--team-menu-open")) continue;
    if (host.element.querySelector(".lane-team-menu")?.contains(target))
      continue;
    if (host.teamMenuButtonEl.contains(target)) continue;
    closeLaneTeamMenu(host);
  }
}

function syncLaneLights(lane, members) {
  if (!members.length) {
    lane.laneLightsEl.hidden = true;
    lane.laneLightsEl.replaceChildren();
    clearLaneLightGridLayout(lane.laneLightsEl);
    lane.pipEl.hidden = false;
    return;
  }
  const layout = laneLightGridLayout(members.length);
  lane.pipEl.hidden = true;
  lane.laneLightsEl.hidden = false;
  applyLaneLightGridLayout(lane.laneLightsEl, layout);
  lane.laneLightsEl.replaceChildren(
    ...members.map((member, index) => {
      const light = document.createElement("span");
      light.className = "agent-status-pip lane-light";
      light.dataset.laneLightTargetId = member.targetId;
      light.dataset.agentStatus = member.pipEl.dataset.agentStatus || "unknown";
      light.title = member.branchName || member.targetId;
      applyLaneLightGridPosition(light, index, layout);
      return light;
    }),
  );
}

function laneLightGridLayout(count) {
  const rows = Math.min(laneLightColumnCapacity, count);
  const columns = Math.ceil(count / laneLightColumnCapacity);
  return {
    rows,
    columns,
    widthPx:
      columns * laneLightPipSizePx + (columns - 1) * laneLightColumnGapPx,
    heightPx: rows * laneLightPipSizePx + (rows - 1) * laneLightRowGapPx,
  };
}

function applyLaneLightGridPosition(light, index, layout) {
  const columnFromInside = Math.floor(index / laneLightColumnCapacity);
  const rowFromBottom = index % laneLightColumnCapacity;
  light.style.gridColumn = String(layout.columns - columnFromInside);
  light.style.gridRow = String(layout.rows - rowFromBottom);
}

function applyLaneLightGridLayout(element, layout) {
  element.style.setProperty("--lane-light-rows", String(layout.rows));
  element.style.setProperty("--lane-light-columns", String(layout.columns));
  element.style.setProperty("--lane-light-width", layout.widthPx + "px");
  element.style.setProperty("--lane-light-height", layout.heightPx + "px");
}

function clearLaneLightGridLayout(element) {
  element.style.removeProperty("--lane-light-rows");
  element.style.removeProperty("--lane-light-columns");
  element.style.removeProperty("--lane-light-width");
  element.style.removeProperty("--lane-light-height");
}

function splitLaneGroupIntoIndividuals(lane) {
  const host = laneGroupHost(lane);
  const members = laneGroupMemberLanes(host);
  splitLaneGroupOnServer(host, members).catch(() => {
    setLaneTransientStatus(host, "split into individuals failed");
  });
}

async function splitLaneGroupOnServer(host, members) {
  if (members.length < 2) return;
  if (!host.teamId) throw new Error("split team requires host team id");
  await updateLaneGroupConfigOnServer(host);
  for (const member of members.slice(1)) {
    await splitComposerAgentFromTeamOnServer(host, member);
  }
}

function restorePreviousTeam(host) {
  restorePreviousTeamOnServer(laneGroupHost(host)).catch(() => {
    setLaneTransientStatus(laneGroupHost(host), "restore previous team failed");
  });
}

async function restorePreviousTeamOnServer(host) {
  if (!host.teamId) throw new Error("restore team requires host team id");
  if (!host.teamSplitBackAvailable)
    throw new Error("restore team requires a saved subgroup");
  await updateLaneGroupConfigOnServer(host);
  await requestTeamCommand(
    teamCommandPayload("splitTeamBack", {
      sourceTeamId: host.teamId,
    }),
  );
}

async function updateLaneGroupConfigOnServer(host) {
  await requestTeamCommand(
    teamCommandPayload("updateTeamConfig", {
      teamId: host.teamId,
      configPatch: {
        speechMode: laneEffectiveSpeechMode(host),
        lifetime: laneEffectiveLifetime(host),
        selectedView: host.selectedView,
        taskFilters: laneAssignedTaskFilters(host),
      },
    }),
  );
}

function splitComposerAgentFromTeam(lane, targetId) {
  const host = laneGroupHost(lane);
  const member = laneStates.get(targetId);
  if (!member) return;
  if (laneGroupMemberLanes(host).length < 2) return;
  if (laneComposerTargetDraftText(host, targetId).trim()) {
    if (!window.confirm(unsafeDraftWarningText())) return;
  }
  splitComposerAgentFromTeamOnServer(host, member).catch(() => {
    setLaneTransientStatus(host, "split composer failed");
  });
}

async function splitComposerAgentFromTeamOnServer(host, member) {
  if (!host.teamId) throw new Error("split composer requires host team id");
  await updateLaneGroupConfigOnServer(host);
  await requestTeamCommand(
    teamCommandPayload("splitTeam", {
      sourceTeamId: host.teamId,
      agentIds: [laneTeamAgentId(member)],
    }),
  );
}

async function mergeLaneGroupsOnServer(sourceLane, targetLane) {
  const destinationTeamId = laneGroupHost(targetLane).teamId;
  if (!destinationTeamId)
    throw new Error("merge team requires destination team id");
  const sourceTeamIds = uniqueStringList(
    laneGroupMemberLanes(sourceLane)
      .map((member) => member.teamId)
      .filter(Boolean),
  );
  for (const teamId of sourceTeamIds) {
    if (teamId === destinationTeamId) continue;
    await requestTeamCommand(
      teamCommandPayload("mergeTeams", {
        sourceTeamId: teamId,
        destinationTeamId,
      }),
    );
  }
}

function laneGroupCanFuse(source, target) {
  const sourceIds = new Set(
    laneGroupMemberLanes(laneGroupHost(source)).map((m) => m.targetId),
  );
  return !laneGroupMemberLanes(laneGroupHost(target)).some((member) =>
    sourceIds.has(member.targetId),
  );
}

function wireComposerMoveDrag(host, handle, targetId) {
  handle.dataset.composerMoveHandle = targetId;
  handle.style.touchAction = "none";
  handle.addEventListener("pointerdown", (event) => {
    if (event.target.closest("button, a")) return;
    if (event.button !== undefined && event.button !== 0) return;
    event.preventDefault();
    const state = beginComposerMoveDrag(host, targetId, event, handle);
    state.pointerCleanup = wireComposerMovePointerDocumentEvents(handle);
    handle.setPointerCapture(event.pointerId);
  });
  handle.addEventListener("pointermove", (event) => {
    if (!composerMoveDragMatches(event)) return;
    updateComposerMoveDragFromEvent(composerMoveDragState, event, handle);
  });
  handle.addEventListener("pointerup", (event) => {
    if (!composerMoveDragMatches(event)) return;
    finishComposerMoveDrag(composerMoveDragState, event.clientX, event.clientY);
  });
  handle.addEventListener("pointercancel", (event) => {
    if (!composerMoveDragMatches(event)) return;
    clearComposerMoveDrag(composerMoveDragState);
  });
  handle.addEventListener("mousedown", (event) => {
    if (composerMoveDragState) return;
    if (event.target.closest("button, a")) return;
    if (event.button !== 0) return;
    const state = beginComposerMoveDrag(host, targetId, event, handle);
    state.mouseCleanup = wireComposerMoveMouseDocumentEvents(handle);
    event.preventDefault();
  });
}

function beginComposerMoveDrag(host, targetId, event, handle) {
  const sourceBand = handle.closest(".composer-band");
  const sourceShard = sourceBand?.closest(".composer-shard") || null;
  const rect = (sourceShard || sourceBand)?.getBoundingClientRect() || null;
  composerMoveDragState = {
    host,
    targetId,
    pointerId: event.pointerId,
    startX: event.clientX,
    startY: event.clientY,
    dragging: false,
    dropTarget: null,
    sourceBand,
    sourceShard,
    dragGhost: null,
    ghostOffsetX: rect ? event.clientX - rect.left : 0,
    ghostOffsetY: rect ? event.clientY - rect.top : 0,
    pointerCleanup: null,
    mouseCleanup: null,
  };
  sourceBand?.classList.add("composer-band--drag-ready");
  return composerMoveDragState;
}

function wireComposerMovePointerDocumentEvents(handle) {
  const onMove = (event) => {
    if (!composerMoveDragMatches(event)) return;
    updateComposerMoveDragFromEvent(composerMoveDragState, event, handle);
  };
  const onUp = (event) => {
    if (!composerMoveDragMatches(event)) return;
    finishComposerMoveDrag(composerMoveDragState, event.clientX, event.clientY);
  };
  const onCancel = (event) => {
    if (!composerMoveDragMatches(event)) return;
    clearComposerMoveDrag(composerMoveDragState);
  };
  document.addEventListener("pointermove", onMove);
  document.addEventListener("pointerup", onUp);
  document.addEventListener("pointercancel", onCancel);
  return () => {
    document.removeEventListener("pointermove", onMove);
    document.removeEventListener("pointerup", onUp);
    document.removeEventListener("pointercancel", onCancel);
  };
}

function wireComposerMoveMouseDocumentEvents(handle) {
  const onMove = (event) => {
    if (!composerMoveDragState) return;
    updateComposerMoveDragFromEvent(composerMoveDragState, event, handle);
  };
  const onUp = (event) => {
    if (!composerMoveDragState) return;
    finishComposerMoveDrag(composerMoveDragState, event.clientX, event.clientY);
  };
  document.addEventListener("mousemove", onMove);
  document.addEventListener("mouseup", onUp, { once: true });
  return () => {
    document.removeEventListener("mousemove", onMove);
    document.removeEventListener("mouseup", onUp);
  };
}

function updateComposerMoveDragFromEvent(state, event, handle) {
  const moved =
    Math.abs(event.clientX - state.startX) >= laneDragThresholdPx ||
    Math.abs(event.clientY - state.startY) >= laneDragThresholdPx;
  if (!state.dragging && !moved) return;
  if (!state.dragging) {
    state.dragging = true;
    state.sourceBand?.classList.add("composer-band--dragging");
    state.sourceShard?.classList.add("composer-shard--dragging");
    ensureComposerMoveDragGhost(state);
  }
  updateComposerMoveDragGhost(state, event.clientX, event.clientY);
  updateComposerMoveDragTarget(state, event.clientX, event.clientY);
  event.preventDefault();
}

function composerMoveDragMatches(event) {
  return (
    composerMoveDragState && composerMoveDragState.pointerId === event.pointerId
  );
}

function updateComposerMoveDragTarget(state, clientX, clientY) {
  clearLaneFuseHighlights();
  clearComposerMoveDropHighlights();
  state.dropTarget = null;
  const sourceMember = laneStates.get(state.targetId);
  if (!sourceMember) return;
  const reorderTarget = composerReorderDropTarget(state, clientX, clientY);
  if (reorderTarget) {
    if (reorderTarget.kind !== "noop") state.dropTarget = reorderTarget;
    return;
  }
  const under = visibleLaneElements().find((element) => {
    const rect = element.getBoundingClientRect();
    return (
      clientX >= rect.left &&
      clientX <= rect.right &&
      clientY >= rect.top &&
      clientY <= rect.bottom
    );
  });
  if (!under) return;
  const underLane = /** @type {HTMLElement} */ (under);
  const targetLane = laneStates.get(underLane.dataset.targetId || "");
  if (!targetLane || !composerCanMoveToLane(sourceMember, targetLane)) return;
  state.dropTarget = { kind: "move", lane: targetLane };
  underLane.classList.add("lane--composer-drop");
}

function composerReorderDropTarget(state, clientX, clientY) {
  const host = laneGroupHost(state.host);
  const members = laneGroupMemberLanes(host);
  if (members.length < 2 || !host.shardsEl) return null;
  const shardsRect = host.shardsEl.getBoundingClientRect();
  if (
    clientX < shardsRect.left ||
    clientX > shardsRect.right ||
    clientY < shardsRect.top ||
    clientY > shardsRect.bottom
  )
    return null;
  const visualShards = [...host.shardsEl.querySelectorAll(".composer-shard")]
    .map((element) => ({
      element,
      rect: element.getBoundingClientRect(),
      targetId: element.dataset.shardTargetId || "",
    }))
    .filter((item) => item.targetId && item.rect.width && item.rect.height)
    .sort((left, right) => left.rect.left - right.rect.left);
  if (visualShards.length < 2) return null;
  if (!visualShards.some((item) => item.targetId === state.targetId))
    return null;
  const zone = composerVisualInsertionZone(visualShards, clientX, clientY);
  if (!zone) return null;
  host.shardsEl.classList.add("composer-shards--move-drop-active");
  zone.shard.element.classList.add(
    zone.side === "left"
      ? "composer-shard--composer-drop-left"
      : "composer-shard--composer-drop-right",
  );
  const currentVisualIds = visualShards.map((item) => item.targetId);
  const sourceIndex = currentVisualIds.indexOf(state.targetId);
  const visualIdsWithoutSource = currentVisualIds.filter(
    (targetId) => targetId !== state.targetId,
  );
  let insertionIndex = zone.index;
  if (sourceIndex < insertionIndex) insertionIndex -= 1;
  insertionIndex = Math.max(
    0,
    Math.min(visualIdsWithoutSource.length, insertionIndex),
  );
  const nextVisualIds = visualIdsWithoutSource.slice();
  nextVisualIds.splice(insertionIndex, 0, state.targetId);
  const nextLogicalIds = composerVisualIdsToLogicalIds(host.shardsEl, nextVisualIds);
  const currentLogicalIds = members.map((member) => member.targetId);
  if (sameStringArrays(nextLogicalIds, currentLogicalIds))
    return { kind: "noop" };
  return { kind: "reorder", host, orderedTargetIds: nextLogicalIds };
}

function composerVisualInsertionZone(visualShards, clientX, clientY) {
  const hit = visualShards.find(
    (item) =>
      clientX >= item.rect.left &&
      clientX <= item.rect.right &&
      clientY >= item.rect.top &&
      clientY <= item.rect.bottom,
  );
  if (hit) {
    const before = clientX < hit.rect.left + hit.rect.width / 2;
    return {
      index: visualShards.indexOf(hit) + (before ? 0 : 1),
      shard: hit,
      side: before ? "left" : "right",
    };
  }
  const first = visualShards[0];
  const last = visualShards[visualShards.length - 1];
  if (clientX < first.rect.left)
    return { index: 0, shard: first, side: "left" };
  if (clientX > last.rect.right)
    return { index: visualShards.length, shard: last, side: "right" };
  for (let index = 0; index < visualShards.length - 1; index += 1) {
    const left = visualShards[index];
    const right = visualShards[index + 1];
    if (clientX < left.rect.right || clientX > right.rect.left) continue;
    const closerToLeft =
      clientX - left.rect.right <= right.rect.left - clientX;
    return {
      index: index + 1,
      shard: closerToLeft ? left : right,
      side: closerToLeft ? "right" : "left",
    };
  }
  return null;
}

function composerVisualIdsToLogicalIds(shardsEl, visualIds) {
  const direction = window.getComputedStyle(shardsEl).flexDirection || "";
  return direction.includes("reverse") ? visualIds.slice().reverse() : visualIds;
}

function sameStringArrays(left, right) {
  if (left.length !== right.length) return false;
  return left.every((value, index) => value === right[index]);
}

function composerCanMoveToLane(sourceMember, targetLane) {
  return !laneGroupMemberLanes(laneGroupHost(targetLane)).some(
    (member) => member.targetId === sourceMember.targetId,
  );
}

function finishComposerMoveDrag(state, clientX, clientY) {
  updateComposerMoveDragTarget(state, clientX, clientY);
  const { host, targetId, dropTarget, dragging } = state;
  clearComposerMoveDrag(state);
  if (!dragging || !dropTarget) return;
  if (dropTarget.kind === "reorder") {
    reorderComposersOnServer(dropTarget.host, dropTarget.orderedTargetIds).catch(
      () => {
        setLaneTransientStatus(dropTarget.host, "reorder composer failed");
        refreshTeamSnapshot({ force: true }).catch(() => {});
      },
    );
    return;
  }
  moveComposerToTeamOnServer(host, dropTarget.lane, targetId).catch(() => {
    setLaneTransientStatus(host, "move composer failed");
    refreshTeamSnapshot({ force: true }).catch(() => {});
  });
}

async function moveComposerToTeamOnServer(sourceHost, targetLane, targetId) {
  const member = laneStates.get(targetId);
  const destinationTeamId = laneGroupHost(targetLane).teamId;
  if (!member || !destinationTeamId)
    throw new Error("move composer requires destination team id");
  moveComposerOptimisticUi(sourceHost, targetLane, member);
  await requestTeamCommand(
    teamCommandPayload("moveComposerToTeam", {
      teamId: destinationTeamId,
      agentId: laneTeamAgentId(member),
      agentAliases: laneTeamAgentAliases(member),
    }),
  );
}

function moveComposerOptimisticUi(sourceHost, targetLane, member) {
  const sourceGroupHost = laneGroupHost(sourceHost);
  const destinationGroupHost = laneGroupHost(targetLane);
  const sourceMembers = laneGroupMemberLanes(laneGroupHost(sourceHost)).filter(
    (candidate) => candidate.targetId !== member.targetId,
  );
  const destinationMembers = [
    ...laneGroupMemberLanes(destinationGroupHost).filter(
      (candidate) => candidate.targetId !== member.targetId,
    ),
    member,
  ];
  reconcileLaneGroups(
    currentLaneGroupRunsWithReplacements(
      new Map([
        [
          sourceGroupHost.targetId,
          sourceMembers.map((candidate) => candidate.targetId),
        ],
        [
          destinationGroupHost.targetId,
          destinationMembers.map((candidate) => candidate.targetId),
        ],
      ]),
    ),
  );
}

async function reorderComposersOnServer(host, orderedTargetIds) {
  const teamId = laneGroupHost(host).teamId;
  const members = orderedTargetIds
    .map((targetId) => laneStates.get(targetId))
    .filter(Boolean);
  if (!teamId || members.length !== orderedTargetIds.length)
    throw new Error("reorder composers requires a complete team");
  reorderComposerOptimisticUi(host, orderedTargetIds);
  await requestTeamCommand(
    teamCommandPayload("reorderTeamAgents", {
      teamId,
      agentIds: members.map((member) => laneTeamAgentId(member)),
    }),
  );
}

function reorderComposerOptimisticUi(host, orderedTargetIds) {
  const groupHost = laneGroupHost(host);
  reconcileLaneGroups(
    currentLaneGroupRunsWithReplacements(
      new Map([[groupHost.targetId, orderedTargetIds]]),
    ),
  );
}

function currentLaneGroupRunsWithReplacements(replacements) {
  const runs = [];
  const seenHosts = new Set();
  for (const lane of laneStates.values()) {
    const host = laneGroupHost(lane);
    if (seenHosts.has(host.targetId)) continue;
    seenHosts.add(host.targetId);
    const memberTargetIds =
      replacements.get(host.targetId) ||
      laneGroupMemberLanes(host).map((member) => member.targetId);
    if (memberTargetIds.length > 1) runs.push(memberTargetIds);
  }
  return runs;
}

function ensureComposerMoveDragGhost(state) {
  const source = state.sourceShard || state.sourceBand;
  if (state.dragGhost || !source) return;
  const rect = source.getBoundingClientRect();
  const ghost = source.cloneNode(true);
  for (const element of [ghost, ...ghost.querySelectorAll(".composer-band")])
    element.classList.remove(
      "composer-band--drag-ready",
      "composer-band--dragging",
      "composer-band--drop-ready",
      "composer-band--menu-open",
      "composer-shard--dragging",
    );
  ghost.classList.add(
    state.sourceShard ? "composer-shard--drag-ghost" : "composer-band--drag-ghost",
  );
  ghost.style.width = Math.max(1, Math.round(rect.width)) + "px";
  ghost.style.height = Math.max(1, Math.round(rect.height)) + "px";
  for (const element of ghost.querySelectorAll("textarea, button, a"))
    element.setAttribute("tabindex", "-1");
  document.body.append(ghost);
  state.dragGhost = ghost;
}

function updateComposerMoveDragGhost(state, clientX, clientY) {
  if (!state.dragGhost) return;
  const left = Math.round(clientX - state.ghostOffsetX);
  const top = Math.round(clientY - state.ghostOffsetY);
  state.dragGhost.style.transform = "translate(" + left + "px, " + top + "px)";
}

function clearComposerMoveDrag(state) {
  if (!state) return;
  if (state.pointerCleanup) state.pointerCleanup();
  if (state.mouseCleanup) state.mouseCleanup();
  if (state.dragGhost) state.dragGhost.remove();
  const band =
    state.sourceBand ||
    document
      .querySelector('[data-composer-primary-target-id="' + state.targetId + '"]')
      ?.closest(".composer-band");
  if (band)
    band.classList.remove("composer-band--drag-ready", "composer-band--dragging");
  state.sourceShard?.classList.remove("composer-shard--dragging");
  clearLaneFuseHighlights();
  clearComposerMoveDropHighlights();
  composerMoveDragState = null;
}

function clearComposerMoveDropHighlights() {
  for (const element of lanesEl.querySelectorAll(
    ".composer-shards--move-drop-active, .composer-shard--composer-drop-left, .composer-shard--composer-drop-right",
  )) {
    element.classList.remove(
      "composer-shards--move-drop-active",
      "composer-shard--composer-drop-left",
      "composer-shard--composer-drop-right",
    );
  }
}

// ---- drag: reorder in the middle, fuse on the edge fifths ------------------------

function wireLaneDrag(lane) {
  const handle = lane.element.querySelector("[data-lane-drag-handle]");
  handle.style.touchAction = "none";
  handle.addEventListener("pointerdown", (event) => {
    if (event.target.closest("button")) return;
    if (event.button !== undefined && event.button !== 0) return;
    laneDragState = {
      lane,
      pointerId: event.pointerId,
      startX: event.clientX,
      dragging: false,
      dropTarget: null,
      dropSide: null,
      dragGhost: null,
      ghostOffsetX: event.clientX - lane.element.getBoundingClientRect().left,
      ghostOffsetY: event.clientY - lane.element.getBoundingClientRect().top,
    };
    handle.setPointerCapture(event.pointerId);
  });
  handle.addEventListener("pointermove", (event) => {
    if (!laneDragMatches(event)) return;
    const state = laneDragState;
    if (
      !state.dragging &&
      Math.abs(event.clientX - state.startX) < laneDragThresholdPx
    )
      return;
    state.dragging = true;
    state.lane.element.classList.add("lane--dragging");
    ensureLaneDragGhost(state);
    updateLaneDragGhost(state, event.clientX, event.clientY);
    updateLaneDragTarget(state, event.clientX);
  });
  handle.addEventListener("pointerup", (event) => {
    if (!laneDragMatches(event)) return;
    finishLaneDrag(laneDragState, event.clientX);
  });
  handle.addEventListener("pointercancel", (event) => {
    if (!laneDragMatches(event)) return;
    clearLaneDrag(laneDragState);
  });
}

function laneDragMatches(event) {
  return laneDragState && laneDragState.pointerId === event.pointerId;
}

function visibleLaneElements() {
  return [...lanesEl.querySelectorAll(".lane[data-target-id]")].filter(
    (element) => !element.classList.contains("lane--shadowed"),
  );
}

function updateLaneDragTarget(state, clientX) {
  clearLaneFuseHighlights();
  state.dropTarget = null;
  state.dropSide = null;
  const under = visibleLaneElements().find((element) => {
    if (element === state.lane.element) return false;
    const rect = element.getBoundingClientRect();
    return clientX >= rect.left && clientX <= rect.right;
  });
  if (!under) return;
  const underLane = /** @type {HTMLElement} */ (under);
  const rect = underLane.getBoundingClientRect();
  const offset = (clientX - rect.left) / Math.max(1, rect.width);
  const targetLane = laneStates.get(underLane.dataset.targetId || "");
  if (offset <= laneFuseGutterFraction || offset >= 1 - laneFuseGutterFraction) {
    if (targetLane && laneGroupCanFuse(state.lane, targetLane)) {
      state.dropTarget = targetLane;
      state.dropSide = offset <= laneFuseGutterFraction ? "left" : "right";
      underLane.classList.add(
        offset <= laneFuseGutterFraction
          ? "lane--fuse-left"
          : "lane--fuse-right",
      );
    }
    return;
  }
  state.dropTarget = targetLane;
  state.dropSide = "swap";
  underLane.classList.add("lane--swap-target");
}

function finishLaneDrag(state, clientX) {
  updateLaneDragTarget(state, clientX);
  const { lane, dropTarget, dropSide, dragging } = state;
  clearLaneDrag(state);
  if (!dragging || !dropTarget) return;
  if (dropSide === "swap") {
    const dragged = laneGroupMemberLanes(laneGroupHost(lane)).map(
      (member) => member.element,
    );
    const reference = laneGroupHost(dropTarget).element;
    for (const element of dragged) lanesEl.insertBefore(element, reference);
    return;
  }
  mergeLaneGroupsOnServer(lane, dropTarget).catch(() => {
    setLaneTransientStatus(lane, "fuse lanes failed");
  });
}

function ensureLaneDragGhost(state) {
  if (state.dragGhost) return;
  const source = state.lane.element;
  const rect = source.getBoundingClientRect();
  const ghost = source.cloneNode(true);
  ghost.classList.remove("lane--dragging");
  ghost.classList.add("lane-drag-ghost");
  ghost.setAttribute("aria-hidden", "true");
  ghost.style.width = Math.max(1, Math.round(rect.width)) + "px";
  ghost.style.height = Math.max(1, Math.round(rect.height)) + "px";
  for (const element of ghost.querySelectorAll("textarea, button, a"))
    element.setAttribute("tabindex", "-1");
  document.body.append(ghost);
  state.dragGhost = ghost;
}

function updateLaneDragGhost(state, clientX, clientY) {
  if (!state.dragGhost) return;
  const left = Math.round(clientX - state.ghostOffsetX);
  const top = Math.round(clientY - state.ghostOffsetY);
  state.dragGhost.style.transform = "translate(" + left + "px, " + top + "px)";
}

function clearLaneDrag(state) {
  if (!state) return;
  state.dragGhost?.remove();
  state.lane.element.classList.remove("lane--dragging");
  clearLaneFuseHighlights();
  laneDragState = null;
}

function clearLaneFuseHighlights() {
  for (const element of lanesEl.querySelectorAll(
    ".lane--fuse-target, .lane--fuse-left, .lane--fuse-right, .lane--composer-drop, .lane--swap-target",
  )) {
    element.classList.remove(
      "lane--fuse-target",
      "lane--fuse-left",
      "lane--fuse-right",
      "lane--composer-drop",
      "lane--swap-target",
    );
  }
}
