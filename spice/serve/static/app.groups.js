// Lane groups: explicit topology over concrete target lanes, backed by server
// teams. A gutter drop on a lane edge fuses (merges teams); the close control
// on a fused host splits the group instead of closing. The visible host
// aggregates chrome and renders one merged, newest-first stream attributed per
// agent; member targets remain the concrete send/refresh/drain addresses.

const laneFuseGutterFraction = 0.2;
const laneDragThresholdPx = 6;
const laneBreakIconSvg =
  '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M10 12H3M3 12l3-3M3 12l3 3M14 12h7M21 12l-3-3M21 12l-3 3" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>';
const laneCloseIconSvg =
  '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 6 18 18M18 6 6 18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>';
const laneLightPipSizePx = 9;
const laneLightRowGapPx = 4;
const laneLightColumnGapPx = 5;
const laneLightColumnCapacity = 2;
let laneDragState = null;
let composerMoveDragState = null;

function reconcileLaneGroups(groupRuns) {
  for (const lane of laneStates.values()) {
    lane.groupTopology = null;
    lane.element.classList.remove("lane--shadowed");
  }
  for (const run of groupRuns) {
    const members = run
      .map((targetId) => laneStates.get(targetId))
      .filter((lane) => lane && isLaneOpen(lane));
    if (members.length < 2) continue;
    const [host, ...shadows] = members;
    const memberTargetIds = members.map((member) => member.targetId);
    host.groupTopology = {
      role: "host",
      hostTargetId: host.targetId,
      memberTargetIds,
    };
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
  let previous = host.element;
  for (const member of laneGroupMemberLanes(host).slice(1)) {
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
  const members = laneGroupMemberLanes(lane);
  const fused = members.length > 1;
  syncLaneLights(lane, fused ? members : []);
  syncFusedLaneStatusLine(lane);
  syncLaneCloseButton(lane, fused);
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

function syncLaneCloseButton(lane, fused) {
  lane.closeButtonEl.innerHTML = fused ? laneBreakIconSvg : laneCloseIconSvg;
  const label = fused ? "Split lane group" : "Close lane";
  lane.closeButtonEl.title = label;
  lane.closeButtonEl.setAttribute("aria-label", label);
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

function breakLaneGroup(lane) {
  const host = laneGroupHost(lane);
  const members = laneGroupMemberLanes(host);
  splitLaneGroupOnServer(host, members).catch(() => {
    setLaneTransientStatus(host, "split lane group failed");
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
    beginComposerMoveDrag(host, targetId, event, handle);
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
  composerMoveDragState = {
    host,
    targetId,
    pointerId: event.pointerId,
    startX: event.clientX,
    startY: event.clientY,
    dragging: false,
    dropTarget: null,
    mouseCleanup: null,
  };
  handle.closest(".composer-band")?.classList.add("composer-band--drag-ready");
  return composerMoveDragState;
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
  state.dragging = true;
  handle.closest(".composer-band")?.classList.add("composer-band--dragging");
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
  state.dropTarget = null;
  const sourceMember = laneStates.get(state.targetId);
  if (!sourceMember) return;
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
  state.dropTarget = targetLane;
  underLane.classList.add("lane--composer-drop");
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
  moveComposerToTeamOnServer(host, dropTarget, targetId).catch(() => {
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
  const sourceMembers = laneGroupMemberLanes(laneGroupHost(sourceHost)).filter(
    (candidate) => candidate.targetId !== member.targetId,
  );
  const destinationMembers = [
    ...laneGroupMemberLanes(laneGroupHost(targetLane)).filter(
      (candidate) => candidate.targetId !== member.targetId,
    ),
    member,
  ];
  const runs = [];
  if (sourceMembers.length > 1)
    runs.push(sourceMembers.map((candidate) => candidate.targetId));
  if (destinationMembers.length > 1)
    runs.push(destinationMembers.map((candidate) => candidate.targetId));
  reconcileLaneGroups(runs);
}

function clearComposerMoveDrag(state) {
  if (!state) return;
  if (state.mouseCleanup) state.mouseCleanup();
  const band = document
    .querySelector('[data-composer-primary-target-id="' + state.targetId + '"]')
    ?.closest(".composer-band");
  if (band) band.classList.remove("composer-band--drag-ready", "composer-band--dragging");
  clearLaneFuseHighlights();
  composerMoveDragState = null;
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

function clearLaneDrag(state) {
  if (!state) return;
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
