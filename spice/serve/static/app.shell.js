// The lane shell: one compact top row (status pip, view rail, close), a view
// stack (compose | filters | metrics | info), the status line, and the
// newest-first message stream. The rail is the lane's mode control; panels
// slide horizontally along --lane-view-position.

function addLane(targetId, hint = null) {
  if (!targetById.has(targetId) || laneStates.has(targetId)) return;
  const lane = createLaneState(targetId, hint);
  laneStates.set(targetId, lane);
  lanesEl.append(lane.element);
  renderSpiceMenuIfAvailable();
  renderFilterPills();
  subscribeLaneToLiveBus(lane);
}

function addEmptyTeamLane(team, options = {}) {
  const targetId = emptyTeamTargetId(team.teamId);
  if (laneStates.has(targetId)) return;
  const lane = createLaneState(targetId, null, {
    emptyTeam: true,
    team,
    canClose: Boolean(options.canClose),
  });
  laneStates.set(targetId, lane);
  lanesEl.append(lane.element);
  renderSpiceMenuIfAvailable();
  renderFilterPills();
}

const laneTemplate =
  '<div class="lane-top" data-lane-drag-handle tabindex="0" title="Drag lane to reorder; drop on a lane edge to fuse">' +
  '  <span class="lane-pip-stack">' +
  '    <span class="agent-status-pip" data-agent-status-pip aria-label="agent status"></span>' +
  '    <span class="lane-lights" data-lane-lights hidden></span>' +
  "  </span>" +
  '  <div class="lane-mode-rail" data-lane-mode-rail role="tablist" aria-label="Lane views">' +
  "  </div>" +
  '  <button class="icon-button lane-team-menu-button" type="button" data-lane-team-menu title="Team actions" aria-label="Team actions" aria-haspopup="menu" aria-expanded="false"><span class="lane-team-menu-icon" aria-hidden="true"></span></button>' +
  "</div>" +
  '<div class="lane-view-stack" data-lane-view-stack>' +
  '  <section class="lane-view-panel" data-lane-view-panel="compose" role="tabpanel">' +
  '    <form class="lane-composer">' +
  '      <div class="composer-shards" data-composer-shards></div>' +
  '      <div class="composer-controls">' +
  '        <label class="stack-slider"><input type="range" min="0" max="2" step="1" value="1" data-speech aria-label="Speech mode"><span data-speech-label>Speak</span></label>' +
  '        <label class="stack-slider"><input type="range" min="0" max="2" step="1" value="1" data-lifetime aria-label="Agent lifetime"><span data-lifetime-label>Drive</span></label>' +
  '        <button class="primary submit-action" type="submit" data-submit>Drive</button>' +
  "      </div>" +
  "    </form>" +
  "  </section>" +
  '  <section class="lane-view-panel" data-lane-view-panel="filters" role="tabpanel">' +
  '    <div class="lane-pane-head"><span>lane filters</span><span data-filters-summary></span></div>' +
  '    <div class="lane-filter-queue-summary" data-filters-queue></div>' +
  '    <div class="lane-filter-chips" data-filters-chips></div>' +
  "  </section>" +
  '  <section class="lane-view-panel" data-lane-view-panel="metrics" role="tabpanel">' +
  '    <div class="lane-pane-head"><span>lane metrics</span><span data-metrics-summary></span></div>' +
  '    <div class="lane-metrics-grid" data-metrics-grid></div>' +
  "  </section>" +
  '  <section class="lane-view-panel" data-lane-view-panel="info" role="tabpanel">' +
  '    <div class="lane-pane-head"><span>lane info</span><span data-info-summary></span></div>' +
  '    <div class="lane-info-grid" data-info-grid></div>' +
  "  </section>" +
  "</div>" +
  '<div class="lane-statusline">' +
  '  <span class="lane-status-error" data-status-error hidden></span>' +
  '  <span class="lane-status-time" data-status-time hidden></span>' +
  '  <span class="dot-separator" data-status-separator hidden>·</span>' +
  '  <span class="lane-status-preview" data-status-preview hidden></span>' +
  "</div>" +
  '<div class="messages" data-messages aria-live="polite" aria-label="Newest assistant messages first">' +
  '  <div class="history-sentinel" data-history-sentinel aria-hidden="true"></div>' +
  "</div>";

let composerBandMenuDismissHandler = null;

function createLaneState(targetId, hint = null, options = {}) {
  const emptyTeam = Boolean(options.emptyTeam);
  const target = targetById.get(targetId) || {};
  const targetIdentity = target.targetIdentity || {};
  const teamIdentity = target.teamIdentity || { state: "none" };
  const element = document.createElement("section");
  element.className = emptyTeam ? "lane lane--empty-team" : "lane";
  element.dataset.targetId = targetId;
  element.innerHTML = laneTemplate;
  const rail = element.querySelector("[data-lane-mode-rail]");
  for (const view of laneViewModes) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "lane-mode-button";
    button.dataset.laneViewButton = view;
    button.setAttribute("role", "tab");
    button.title = view;
    button.innerHTML =
      '<span class="lane-mode-word"></span>' +
      '<span class="lane-mode-badge" data-lane-view-badge hidden></span>';
    button.querySelector(".lane-mode-word").textContent = view;
    rail.append(button);
  }
  const lane = {
    targetId,
    emptyTeam,
    element,
    closed: false,
    laneId: "lane:" + targetId,
    agentName: emptyTeam ? "" : targetIdentityAgentName(targetIdentity),
    driverName: emptyTeam ? "" : targetIdentityDriverName(targetIdentity),
    driverModel: emptyTeam ? "" : targetIdentityDriverModel(targetIdentity),
    driverEffort: emptyTeam ? "" : targetIdentityDriverEffort(targetIdentity),
    branchName: emptyTeam
      ? "empty team"
      : targetIdentityBranch(targetIdentity),
    targetThreadId: emptyTeam ? "" : targetIdentityThreadId(targetIdentity),
    activeThreadId: emptyTeam ? "" : targetIdentityThreadId(targetIdentity),
    ...laneStreamState(target),
    speechMode: hint ? hint.speechMode : defaultSpeechMode,
    lifetime: target.lifetime || defaultAgentLifetime,
    serverLifetime: target.lifetime || defaultAgentLifetime,
    selectedView: hint ? hint.selectedView : defaultLaneViewMode,
    taskFilters: uniqueStringList(target.taskFilters || []),
    laneFilterVersion: target.laneFilterVersion || "",
    taskFilterInventory: target.taskFilterInventory || null,
    laneMetrics: target.laneMetrics || {},
    laneInfo: target.laneInfo || { summaryRows: [], members: [] },
    renewalIntent: target.renewalIntent || {},
    pendingLifetimeCommit: "",
    pendingLifetimeConfigRevision: 0,
    pendingLifetimeRequestId: 0,
    lifetimeRequestId: 0,
    privateTaskCount: Math.max(0, Number(target.privateTaskCount) || 0),
    teamId: emptyTeam ? "" : teamIdentityTeamId(teamIdentity),
    teamRevision: emptyTeam ? 0 : teamIdentityRevision(teamIdentity),
    emptyTeamCanClose: emptyTeam && Boolean(options.canClose),
    teamSplitBackAvailable: false,
    teamSplitBackMemberCount: 0,
    configRevision: emptyTeam ? 0 : teamIdentityConfigRevision(teamIdentity),
    groupTopology: null,
    backendPendingInboxCount: 0,
    backendPendingInboxKeys: new Set(),
    backendPendingInboxRevision: "",
    backendPendingInboxKeysAuthoritative: false,
    optimisticPendingInboxCount: 0,
    optimisticSubmittedInboxKeys: new Set(),
    optimisticPendingInboxFloor: 0,
    pendingSubmissionCount: 0,
    sendAwaitingBackendCount: 0,
    refreshInFlight: false,
    liveBusSubscribed: false,
    serverReachable: true,
    serverCloseRequested: false,
    teamImportOverlayOpen: false,
    teamImportOverlayEl: null,
    filterPickerOpen: false,
    filterPickerQuery: "",
    filterPickerPendingAssignments: new Set(),
    filterPickerOverlayEl: null,
    filterPickerOverlayPositionHandler: null,
    filterPickerOverlayDismissHandler: null,
    selectedFilterRemovals: new Set(),
    renderedFilterPaneFingerprint: "",
    shardTextareas: new Map(),
    shardAttachments: new Map(),
    quoteDrafts: new Map(),
    nextQuoteDraftId: 0,
    paneCollapsePx: 0,
    paneMaxHeight: 0,
    paneMetricsFrame: 0,
    paneResizeObserver: null,
    paneScrollSuppressed: false,
    paneTouchScrollHandoff: false,
    paneTouchY: null,
    previousMessageScrollTop: 0,
    modeRailDrag: null,
    modeRailSuppressClick: false,
    ...laneElementRefs(element),
  };
  lane.historySentinelEl.dataset.historyTargetId = targetId;
  wireLaneShell(lane);
  if (emptyTeam) syncEmptyTeamLane(lane, options.team || {}, options);
  else {
    syncComposerShards(lane, [lane]);
    syncLaneEffectiveControls(lane);
    renderLaneChrome(lane, targetPayloadShim(target));
  }
  return lane;
}

function ensureEmptyTeamLane(team, options = {}) {
  if (!team.teamId) return;
  const targetId = emptyTeamTargetId(team.teamId);
  if (!laneStates.has(targetId)) addEmptyTeamLane(team, options);
  const lane = laneStates.get(targetId);
  if (lane) syncEmptyTeamLane(lane, team, options);
}

function syncEmptyTeamLane(lane, team = {}, options = {}) {
  if (!lane || !lane.emptyTeam) return;
  const config = team.config || {};
  const nextCanClose = Object.prototype.hasOwnProperty.call(options, "canClose")
    ? Boolean(options.canClose)
    : Boolean(lane.emptyTeamCanClose);
  lane.emptyTeamCanClose = nextCanClose;
  lane.element.classList.toggle("lane--empty-team-closable", nextCanClose);
  lane.teamId = String(team.teamId || lane.teamId || "");
  lane.teamRevision = Math.max(0, Number(team.revision || lane.teamRevision || 0));
  lane.configRevision = Math.max(
    0,
    Number(config.revision || lane.configRevision || 0),
  );
  lane.agentName = "";
  lane.driverName = "";
  lane.driverModel = "";
  lane.driverEffort = "";
  lane.branchName = "empty team";
  lane.targetThreadId = "";
  lane.activeThreadId = "";
  if (config.lifetime)
    applyServerLaneLifetime(lane, config.lifetime, {
      configRevision: config.revision,
    });
  if (config.speechMode && speechModes.includes(config.speechMode))
    lane.speechMode = config.speechMode;
  if (Array.isArray(config.taskFilters))
    lane.taskFilters = uniqueStringList(config.taskFilters);
  lane.shardTextareas.clear();
  lane.shardAttachments.clear();
  lane.quoteDrafts.clear();
  lane.shardsEl.classList.remove("composer-shards--move-drop-active");
  lane.shardsEl.replaceChildren();
  lane.renderedMessageFingerprint = "";
  lane.statusPreviewEl.hidden = false;
  lane.statusPreviewEl.textContent = "choose an agent to import";
  lane.statusTimeEl.hidden = true;
  lane.statusErrorEl.hidden = true;
  lane.statusSeparatorEl.hidden = true;
  lane.pipEl.hidden = true;
  lane.laneLightsEl.hidden = true;
  lane.laneLightsEl.replaceChildren();
  clearLaneLightGridLayout(lane.laneLightsEl);
  if (lane.emptyTeamCanClose) {
    lane.teamMenuButtonEl.disabled = false;
    lane.teamMenuButtonEl.removeAttribute("aria-hidden");
    lane.teamMenuButtonEl.removeAttribute("tabindex");
    lane.teamMenuButtonEl.title = "Team actions";
    lane.teamMenuButtonEl.setAttribute("aria-label", "Team actions");
  } else {
    lane.element.querySelector(".lane-team-menu")?.remove();
    lane.element.classList.remove("lane--team-menu-open");
    lane.teamMenuButtonEl.disabled = true;
    lane.teamMenuButtonEl.tabIndex = -1;
    lane.teamMenuButtonEl.setAttribute("aria-hidden", "true");
    lane.teamMenuButtonEl.setAttribute("aria-expanded", "false");
    lane.teamMenuButtonEl.title = "";
  }
  lane.selectedView = defaultLaneViewMode;
  syncLaneEffectiveControls(lane);
  lockEmptyTeamPane(lane);
  renderMessagesIfChanged(lane);
}

function lockEmptyTeamPane(lane) {
  if (!lane || !lane.emptyTeam) return;
  lane.selectedView = defaultLaneViewMode;
  setLanePaneCollapse(lane, lanePaneMaxHeight(lane));
}

function emptyTeamImportPanel(lane) {
  return teamImportPanel(lane);
}

function teamImportPanel(lane, options = {}) {
  const overlay = Boolean(options.overlay);
  const panel = document.createElement("div");
  panel.className = "empty-team-importer";
  if (overlay) panel.classList.add("team-import-overlay");
  panel.dataset.emptyTeamImporter = "";
  panel.dataset.teamImporter = "";
  const heading = document.createElement("div");
  heading.className = "empty-team-importer-heading";
  heading.textContent = "import agent";
  const list = document.createElement("div");
  list.className = "empty-team-import-list";
  const choices = teamImportTargets(lane);
  if (choices.length)
    list.replaceChildren(
      ...choices.map((target) => teamImportChoice(lane, target, { overlay })),
    );
  else list.textContent = "no available agents";
  panel.append(heading, list);
  return panel;
}

function emptyTeamImportChoice(lane, target) {
  return teamImportChoice(lane, target);
}

function teamImportChoice(lane, target, options = {}) {
  const button = targetChoiceButton(
    target,
    "Import",
    (event) => {
      event.preventDefault();
      importTargetIntoTeam(lane, target.id)
        .then(() => {
          if (options.overlay) closeTeamImportOverlay(lane);
        })
        .catch(() => {
          setLaneTransientStatus(lane, "import agent failed");
        });
    },
    "",
  );
  button.dataset.emptyTeamImportTargetId = target.id;
  button.dataset.teamImportTargetId = target.id;
  return button;
}

async function importTargetIntoEmptyTeam(lane, targetId) {
  return importTargetIntoTeam(lane, targetId);
}

async function importTargetIntoTeam(lane, targetId) {
  const host = laneGroupHost(lane);
  const target = targetById.get(targetId);
  if (!host || !host.teamId || !target)
    throw new Error("import requires team and target");
  setLaneTransientStatus(lane, "importing agent");
  await requestTeamCommand(
    teamCommandPayload("moveAgentToTeam", {
      teamId: host.teamId,
      agentId:
        canonicalThreadActorId(targetIdentityThreadId(target.targetIdentity)) ||
        target.id,
      agentAliases: teamImportAliases(target),
    }),
  );
  await refreshTeamSnapshot({ force: true });
}

function emptyTeamImportAliases(target) {
  return teamImportAliases(target);
}

function teamImportAliases(target) {
  const actor = canonicalThreadActorId(
    targetIdentityThreadId(target.targetIdentity),
  );
  return actor && actor !== target.id ? [target.id] : [];
}

function teamImportTargets(lane) {
  const host = laneGroupHost(lane);
  const memberTargetIds = new Set(laneGroupMemberTargetIds(host));
  return targets
    .filter((target) => !memberTargetIds.has(target.id))
    .sort(compareTargetChoices);
}

function toggleTeamImportOverlay(lane) {
  const host = laneGroupHost(lane);
  if (!host || host.emptyTeam) return;
  if (host.teamImportOverlayOpen) closeTeamImportOverlay(host);
  else openTeamImportOverlay(host);
}

function openTeamImportOverlay(lane) {
  const host = laneGroupHost(lane);
  if (!host || host.emptyTeam) return;
  closeLaneTeamMenusExcept(host);
  host.teamImportOverlayOpen = true;
  syncTeamImportOverlay(host);
}

function closeTeamImportOverlay(lane) {
  const host = laneGroupHost(lane);
  if (!host) return;
  host.teamImportOverlayOpen = false;
  syncTeamImportOverlay(host);
}

function syncTeamImportOverlay(lane) {
  const host = laneGroupHost(lane);
  if (!host || host.emptyTeam || !host.messagesEl) return;
  if (!host.teamImportOverlayOpen) {
    host.teamImportOverlayEl?.remove();
    host.teamImportOverlayEl = null;
    if (!host.element.querySelector(".lane-team-menu")) {
      host.element.classList.remove("lane--team-menu-open");
      host.teamMenuButtonEl.setAttribute("aria-expanded", "false");
    }
    syncLaneTeamMenuDismissHandler();
    return;
  }
  const overlay = teamImportPanel(host, { overlay: true });
  positionTeamImportOverlay(host, overlay);
  host.teamImportOverlayEl?.remove();
  host.teamImportOverlayEl = overlay;
  host.element.append(overlay);
  host.element.classList.add("lane--team-menu-open");
  host.teamMenuButtonEl.setAttribute("aria-expanded", "true");
  syncLaneTeamMenuDismissHandler();
}

function positionTeamImportOverlay(host, overlay) {
  syncLanePaneMetrics(host);
  overlay.style.setProperty(
    "--team-import-overlay-top",
    host.messagesEl.offsetTop + "px",
  );
}

// The lane's transcript-stream state: known messages, cursors, render
// fingerprints, ACK context cache, and the spoken-message boundary.
function laneStreamState(target) {
  return {
    occupants: new Map(),
    knownMessages: [],
    knownMessageKeys: new Set(),
    newestMessageKey: "",
    oldestMessageKey: "",
    retainedMessageLimit: messageLimit,
    olderHydrationInFlight: false,
    olderHistoryExhausted: false,
    historyObserver: null,
    renderedMessageFingerprint: "",
    renderedStatusFingerprint: "",
    renderedFusedStatusLine: false,
    lastRenderedStatusLine: target.statusLine || null,
    renewalIntent: target.renewalIntent || {},
    statusTransientTimer: null,
    latestPayload: null,
    ackContextByKey: new Map(),
    missingAckContextKeys: new Set(),
    recentSentAckKeys: [],
    spokenMessageKeys: new Set(),
    speechPrimeStartedAt: Date.now(),
    speechPrimed: false,
    speechAbortVersion: 0,
  };
}

function laneElementRefs(element) {
  return {
    formEl: element.querySelector("form"),
    pipEl: element.querySelector("[data-agent-status-pip]"),
    laneLightsEl: element.querySelector("[data-lane-lights]"),
    viewStackEl: element.querySelector("[data-lane-view-stack]"),
    shardsEl: element.querySelector("[data-composer-shards]"),
    speechRangeEl: element.querySelector("[data-speech]"),
    speechLabelEl: element.querySelector("[data-speech-label]"),
    lifetimeRangeEl: element.querySelector("[data-lifetime]"),
    lifetimeLabelEl: element.querySelector("[data-lifetime-label]"),
    submitEl: element.querySelector("[data-submit]"),
    teamMenuButtonEl: element.querySelector("[data-lane-team-menu]"),
    modeRailEl: element.querySelector("[data-lane-mode-rail]"),
    filtersSummaryEl: element.querySelector("[data-filters-summary]"),
    filtersQueueEl: element.querySelector("[data-filters-queue]"),
    filtersChipsEl: element.querySelector("[data-filters-chips]"),
    metricsSummaryEl: element.querySelector("[data-metrics-summary]"),
    metricsGridEl: element.querySelector("[data-metrics-grid]"),
    infoSummaryEl: element.querySelector("[data-info-summary]"),
    infoGridEl: element.querySelector("[data-info-grid]"),
    statusErrorEl: element.querySelector("[data-status-error]"),
    statusTimeEl: element.querySelector("[data-status-time]"),
    statusSeparatorEl: element.querySelector("[data-status-separator]"),
    statusPreviewEl: element.querySelector("[data-status-preview]"),
    messagesEl: element.querySelector("[data-messages]"),
    historySentinelEl: element.querySelector("[data-history-sentinel]"),
  };
}

function wireLaneShell(lane) {
  lane.formEl.addEventListener("submit", (event) => submitLaneForm(lane, event));
  lane.teamMenuButtonEl.addEventListener("click", (event) =>
    toggleLaneTeamMenu(lane, event),
  );
  for (const button of lane.element.querySelectorAll(
    "[data-lane-view-button]",
  )) {
    button.addEventListener("click", () =>
      setLaneSelectedView(lane, button.dataset.laneViewButton),
    );
    button.addEventListener("keydown", (event) =>
      handleLaneViewKeydown(lane, button.dataset.laneViewButton, event),
    );
  }
  wireLaneModeRailDrag(lane);
  lane.speechRangeEl.addEventListener("input", () => {
    const index = Number(lane.speechRangeEl.value) || 0;
    setLaneSpeechMode(lane, speechModes[index] || defaultSpeechMode);
  });
  lane.lifetimeRangeEl.addEventListener("input", () => {
    const index = Number(lane.lifetimeRangeEl.value) || 0;
    setLaneLifetime(lane, agentLifetimeLabels[index] || defaultAgentLifetime);
  });
  initializeLanePaneCollapse(lane);
  wireLaneDrag(lane);
}

function handleLaneViewKeydown(lane, view, event) {
  const index = laneViewModeIndex(view);
  let nextIndex = index;
  if (event.key === "ArrowLeft") nextIndex = Math.max(0, index - 1);
  else if (event.key === "ArrowRight")
    nextIndex = Math.min(laneViewModes.length - 1, index + 1);
  else if (event.key === "Home") nextIndex = 0;
  else if (event.key === "End") nextIndex = laneViewModes.length - 1;
  else return;
  event.preventDefault();
  const nextView = laneViewModes[nextIndex];
  setLaneSelectedView(lane, nextView);
  const button = lane.element.querySelector(
    '[data-lane-view-button="' + nextView + '"]',
  );
  if (button) button.focus();
}

function wireLaneModeRailDrag(lane) {
  const rail = lane.modeRailEl;
  rail.style.touchAction = "none";
  rail.addEventListener("click", (event) => {
    if (!lane.modeRailSuppressClick) return;
    lane.modeRailSuppressClick = false;
    event.preventDefault();
    event.stopImmediatePropagation();
  }, true);
  rail.addEventListener("pointerdown", (event) => {
    if (event.button !== undefined && event.button !== 0) return;
    lane.modeRailDrag = {
      pointerId: event.pointerId,
      startX: event.clientX,
      dragging: false,
    };
    rail.setPointerCapture(event.pointerId);
  });
  rail.addEventListener("pointermove", (event) => {
    const drag = lane.modeRailDrag;
    if (!drag || drag.pointerId !== event.pointerId) return;
    if (!drag.dragging && Math.abs(event.clientX - drag.startX) < laneDragThresholdPx)
      return;
    drag.dragging = true;
    lane.modeRailSuppressClick = true;
    setLaneSelectedView(lane, laneViewModeFromRailPoint(lane, event.clientX));
  });
  rail.addEventListener("pointerup", (event) => {
    const drag = lane.modeRailDrag;
    if (!drag || drag.pointerId !== event.pointerId) return;
    setLaneSelectedView(lane, laneViewModeFromRailPoint(lane, event.clientX));
    lane.modeRailDrag = null;
  });
  rail.addEventListener("pointercancel", (event) => {
    const drag = lane.modeRailDrag;
    if (!drag || drag.pointerId !== event.pointerId) return;
    lane.modeRailDrag = null;
  });
}

function laneViewModeFromRailPoint(lane, clientX) {
  const buttons = [...lane.modeRailEl.querySelectorAll("[data-lane-view-button]")];
  const containing = buttons.find((button) => {
    const rect = button.getBoundingClientRect();
    return clientX >= rect.left && clientX <= rect.right;
  });
  if (containing) return containing.dataset.laneViewButton;
  let nearest = buttons[0];
  let nearestDistance = Number.POSITIVE_INFINITY;
  for (const button of buttons) {
    const rect = button.getBoundingClientRect();
    const center = rect.left + rect.width / 2;
    const distance = Math.abs(clientX - center);
    if (distance < nearestDistance) {
      nearest = button;
      nearestDistance = distance;
    }
  }
  return nearest ? nearest.dataset.laneViewButton : defaultLaneViewMode;
}

function setLaneSelectedView(lane, view) {
  const host = laneGroupHost(lane);
  if (host.emptyTeam) {
    lockEmptyTeamPane(host);
    renderLaneViewShell(host);
    return;
  }
  lane.selectedView = laneViewMode(view);
  persistLaneHints();
  renderLaneViewShell(lane);
  expandLanePane(host);
}

function initializeLanePaneCollapse(lane) {
  lane.previousMessageScrollTop = lane.messagesEl.scrollTop;
  syncLanePaneMetrics(lane);
  observeLanePaneMetrics(lane);
  wireLanePaneMouseEvents(lane);
  wireLanePaneTouchEvents(lane);
}

function observeLanePaneMetrics(lane) {
  if (typeof ResizeObserver !== "undefined") {
    lane.paneResizeObserver = new ResizeObserver(lanePaneMetricsListener(lane));
    lane.paneResizeObserver.observe(lane.viewStackEl);
    for (const panel of lane.element.querySelectorAll("[data-lane-view-panel]"))
      lane.paneResizeObserver.observe(panel);
  }
}

function lanePaneMetricsListener(lane) {
  return function onLanePaneMetricsResize() {
    scheduleLanePaneMetricsSync(lane);
  };
}

function scheduleLanePaneMetricsSync(lane) {
  if (lane.paneMetricsFrame) return;
  lane.paneMetricsFrame = requestAnimationFrame(() => {
    lane.paneMetricsFrame = 0;
    if (!lane.closed) syncLanePaneMetrics(lane);
  });
}

function wireLanePaneMouseEvents(lane) {
  lane.messagesEl.addEventListener(
    "wheel",
    lanePaneWheelListener(lane),
    activeLanePaneEventOptions(),
  );
  lane.messagesEl.addEventListener("scroll", lanePaneScrollListener(lane));
}

function wireLanePaneTouchEvents(lane) {
  lane.messagesEl.addEventListener(
    "touchstart",
    lanePaneTouchStartListener(lane),
    passiveLanePaneEventOptions(),
  );
  lane.messagesEl.addEventListener(
    "touchmove",
    lanePaneTouchMoveListener(lane),
    activeLanePaneEventOptions(),
  );
  lane.messagesEl.addEventListener(
    "touchend",
    lanePaneTouchEndListener(lane),
    passiveLanePaneEventOptions(),
  );
  lane.messagesEl.addEventListener(
    "touchcancel",
    lanePaneTouchEndListener(lane),
    passiveLanePaneEventOptions(),
  );
}

function lanePaneWheelListener(lane) {
  return function onLanePaneWheel(event) {
    handleLanePaneWheel(lane, event);
  };
}

function lanePaneScrollListener(lane) {
  return function onLanePaneScroll() {
    handleLanePaneScroll(lane);
  };
}

function lanePaneTouchStartListener(lane) {
  return function onLanePaneTouchStart(event) {
    beginLanePaneTouch(lane, event);
  };
}

function lanePaneTouchMoveListener(lane) {
  return function onLanePaneTouchMove(event) {
    handleLanePaneTouchMove(lane, event);
  };
}

function lanePaneTouchEndListener(lane) {
  return function onLanePaneTouchEnd() {
    endLanePaneTouch(lane);
  };
}

function activeLanePaneEventOptions() {
  return { passive: false };
}

function passiveLanePaneEventOptions() {
  return { passive: true };
}

function syncLanePaneMetrics(lane) {
  const previousMax = lane.paneMaxHeight || 0;
  const wasCollapsed = previousMax > 0 && lane.paneCollapsePx >= previousMax - 1;
  lane.paneMaxHeight = lanePaneExpandedHeight(lane);
  setLanePaneCollapse(lane, wasCollapsed ? lane.paneMaxHeight : lane.paneCollapsePx);
}

function lanePaneExpandedHeight(lane) {
  const controls = lane.element.querySelector(".composer-controls");
  if (!controls) return 120;
  const style = getComputedStyle(controls);
  const children = [...controls.children].filter((child) => {
    const box = child.getBoundingClientRect();
    return box.width > 0 && box.height > 0;
  });
  const childHeight = children.reduce(
    (total, child) => total + child.getBoundingClientRect().height,
    0,
  );
  const gap = parseFloat(style.rowGap || "0") || 0;
  const padding =
    (parseFloat(style.paddingTop || "0") || 0) +
    (parseFloat(style.paddingBottom || "0") || 0);
  const totalGap = Math.max(0, children.length - 1) * gap;
  return Math.max(120, Math.ceil(childHeight + totalGap + padding));
}

function expandLanePane(lane) {
  if (!lane || !lane.viewStackEl) return;
  syncLanePaneMetrics(lane);
  setLanePaneCollapse(lane, 0);
  suppressLanePaneScrollIntentForFrame(lane);
}

function handleLanePaneWheel(lane, event) {
  const delta = lanePaneWheelDelta(lane, event);
  if (!lanePaneCanConsumeDelta(lane, delta)) return;
  event.preventDefault();
  applyLanePaneInputDelta(lane, delta);
}

function beginLanePaneTouch(lane, event) {
  const touch = event.touches[0];
  lane.paneTouchY = touch ? touch.clientY : null;
  lane.paneTouchScrollHandoff = false;
}

function handleLanePaneTouchMove(lane, event) {
  const touch = event.touches[0];
  if (!touch || lane.paneTouchY === null) return;
  const delta = lane.paneTouchY - touch.clientY;
  lane.paneTouchY = touch.clientY;
  const canConsume = lanePaneCanConsumeDelta(lane, delta);
  const keepsCanceledGesture =
    lane.paneTouchScrollHandoff && Number.isFinite(delta) && delta !== 0;
  if (!canConsume && !keepsCanceledGesture) return;
  event.preventDefault();
  if (canConsume) lane.paneTouchScrollHandoff = true;
  applyLanePaneInputDelta(lane, delta);
}

function endLanePaneTouch(lane) {
  lane.paneTouchY = null;
  lane.paneTouchScrollHandoff = false;
}

function handleLanePaneScroll(lane) {
  const current = lane.messagesEl.scrollTop;
  const delta = current - lane.previousMessageScrollTop;
  lane.previousMessageScrollTop = current;
  if (lane.paneScrollSuppressed) return;
  const appliedCollapsePx = applyLanePaneScrollIntent(lane, delta);
  compensateLaneMessageScrollForPane(lane, appliedCollapsePx);
}

function lanePaneWheelDelta(lane, event) {
  if (event.deltaMode === WheelEvent.DOM_DELTA_PAGE)
    return event.deltaY * lane.messagesEl.clientHeight;
  if (event.deltaMode === WheelEvent.DOM_DELTA_LINE) return event.deltaY * 16;
  return event.deltaY;
}

function lanePaneCanConsumeDelta(lane, delta) {
  if (lane.emptyTeam) return false;
  if (delta > 0) return lane.paneCollapsePx < lanePaneMaxHeight(lane) - 1;
  if (delta < 0) return lane.paneCollapsePx > 1;
  return false;
}

function lanePaneMaxHeight(lane) {
  return lane.paneMaxHeight || lanePaneExpandedHeight(lane);
}

function applyLanePaneInputDelta(lane, delta) {
  const appliedCollapsePx = applyLanePaneScrollIntent(lane, delta);
  const remainingDelta = delta - appliedCollapsePx;
  if (Math.abs(remainingDelta) < 1) return;
  setLaneScrollTopWithoutPaneIntent(
    lane,
    clampScrollTop(lane.messagesEl, lane.messagesEl.scrollTop + remainingDelta),
  );
}

function applyLanePaneScrollIntent(lane, delta) {
  if (!Number.isFinite(delta) || Math.abs(delta) < 1) return 0;
  const previousCollapsePx = lane.paneCollapsePx;
  setLanePaneCollapse(
    lane,
    lane.paneCollapsePx + delta * lanePaneCollapseScrollRate,
  );
  return lane.paneCollapsePx - previousCollapsePx;
}

function compensateLaneMessageScrollForPane(lane, appliedCollapsePx) {
  if (!Number.isFinite(appliedCollapsePx) || Math.abs(appliedCollapsePx) < 1)
    return;
  setLaneScrollTopWithoutPaneIntent(
    lane,
    clampScrollTop(lane.messagesEl, lane.messagesEl.scrollTop - appliedCollapsePx),
  );
}

function setLanePaneCollapse(lane, collapsePx) {
  const maxHeight = lanePaneMaxHeight(lane);
  const requestedCollapsePx = lane.emptyTeam ? maxHeight : collapsePx;
  const next = Math.max(0, Math.min(maxHeight, requestedCollapsePx));
  lane.paneCollapsePx = next;
  const visibleHeight = Math.max(0, maxHeight - next);
  setStylePropertyIfChanged(
    lane.viewStackEl,
    "--lane-pane-expanded-height",
    maxHeight + "px",
  );
  setStylePropertyIfChanged(
    lane.viewStackEl,
    "--lane-pane-visible-height",
    visibleHeight + "px",
  );
  setStylePropertyIfChanged(
    lane.viewStackEl,
    "--lane-pane-collapse-offset",
    next + "px",
  );
  lane.viewStackEl.classList.toggle("lane-view-stack--collapsed", visibleHeight < 1);
}

function setStylePropertyIfChanged(element, name, value) {
  if (element.style.getPropertyValue(name) === value) return;
  element.style.setProperty(name, value);
}

function setLaneScrollTopWithoutPaneIntent(lane, scrollTop) {
  suppressLanePaneScrollIntentForFrame(lane);
  lane.messagesEl.scrollTop = scrollTop;
  lane.previousMessageScrollTop = lane.messagesEl.scrollTop;
}

function suppressLanePaneScrollIntentForFrame(lane) {
  lane.paneScrollSuppressed = true;
  lane.previousMessageScrollTop = lane.messagesEl.scrollTop;
  requestAnimationFrame(() => {
    lane.previousMessageScrollTop = lane.messagesEl.scrollTop;
    lane.paneScrollSuppressed = false;
  });
}

function clampScrollTop(element, value) {
  const max = Math.max(0, element.scrollHeight - element.clientHeight);
  return Math.max(0, Math.min(max, value));
}

function renderLaneViewShell(lane) {
  const selectedView = laneViewMode(lane.selectedView);
  const position = laneViewModeIndex(selectedView);
  const disabled = Boolean(lane.emptyTeam);
  lane.viewStackEl.style.setProperty("--lane-view-position", String(position));
  lane.modeRailEl.classList.toggle("lane-mode-rail--disabled", disabled);
  lane.modeRailEl.setAttribute("aria-disabled", disabled ? "true" : "false");
  for (const button of lane.element.querySelectorAll(
    "[data-lane-view-button]",
  )) {
    const view = button.dataset.laneViewButton;
    const active = view === selectedView;
    button.classList.toggle("lane-mode-button--active", active);
    button.disabled = disabled;
    button.setAttribute("aria-disabled", disabled ? "true" : "false");
    button.setAttribute("aria-selected", active ? "true" : "false");
    button.tabIndex = disabled ? -1 : active ? 0 : -1;
    syncLaneViewBadge(lane, view, button);
  }
  for (const panel of lane.element.querySelectorAll("[data-lane-view-panel]")) {
    const view = panel.dataset.laneViewPanel;
    const offset = (laneViewModeIndex(view) - position) * 100;
    const active = view === selectedView;
    panel.style.setProperty("--lane-view-x", offset + "%");
    panel.classList.toggle("lane-view-panel--active", active);
    if (active) panel.removeAttribute("inert");
    else panel.setAttribute("inert", "");
  }
  renderLaneFiltersPane(lane);
  renderLaneMetricsPane(lane);
  renderLaneInfoPane(lane);
}

function syncLaneViewBadge(lane, view, button) {
  const badge = button.querySelector("[data-lane-view-badge]");
  if (!badge) return;
  let count = 0;
  if (view === "compose") count = laneComposeBadgeCount(lane);
  else if (view === "filters") count = laneAssignedTaskFilters(lane).length;
  else if (view === "info") {
    const members = laneGroupMemberTargetIds(lane).length;
    count = members > 1 ? members : 0;
  }
  badge.textContent = count > 0 ? String(count) : "";
  badge.hidden = count <= 0;
  const lifetime = laneEffectiveLifetime(lane);
  const activeFilters =
    agentLifetimeDissolvesTaskBoundary(lifetime) ||
    laneAssignedTaskFilters(lane).length > 0;
  badge.classList.toggle(
    "lane-mode-badge--inactive",
    view === "filters" && !activeFilters,
  );
}

function laneComposeBadgeCount(lane) {
  const members = laneGroupMemberLanes(laneGroupHost(lane));
  return members.reduce(
    (total, member) => total + lanePendingDisplayCount(member),
    0,
  );
}
