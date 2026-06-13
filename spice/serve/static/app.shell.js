// The lane shell: one compact top row (status pip, view rail, close), a view
// stack (compose | filters | metrics | info), the status line, and the
// newest-first message stream. The rail is the lane's mode control; panels
// slide horizontally along --lane-view-position.

function addLane(targetId, hint = null) {
  if (!targetById.has(targetId) || laneStates.has(targetId)) return;
  const lane = createLaneState(targetId, hint);
  laneStates.set(targetId, lane);
  lanesEl.append(lane.element);
  renderSpiceMenu();
  renderFilterPills();
  subscribeLaneToLiveBus(lane);
}

const laneTemplate =
  '<div class="lane-top" data-lane-drag-handle tabindex="0" title="Drag lane to reorder; drop on a lane edge to fuse">' +
  '  <span class="lane-pip-stack">' +
  '    <span class="agent-status-pip" data-agent-status-pip aria-label="agent status"></span>' +
  '    <span class="lane-lights" data-lane-lights hidden></span>' +
  "  </span>" +
  '  <div class="lane-mode-rail" data-lane-mode-rail role="tablist" aria-label="Lane views">' +
  "  </div>" +
  '  <button class="icon-button" type="button" data-close-lane title="Close lane" aria-label="Close lane">×</button>' +
  "</div>" +
  '<div class="lane-view-stack" data-lane-view-stack>' +
  '  <section class="lane-view-panel" data-lane-view-panel="compose" role="tabpanel">' +
  '    <form class="lane-composer">' +
  '      <div class="composer-shards" data-composer-shards></div>' +
  '      <div class="composer-controls">' +
  '        <label class="stack-slider"><input type="range" min="0" max="2" step="1" value="1" data-speech aria-label="Speech mode"><span data-speech-label>Speak</span></label>' +
  '        <label class="stack-slider"><input type="range" min="0" max="2" step="1" value="1" data-lifetime aria-label="Agent lifetime"><span data-lifetime-label>Steer</span></label>' +
  '        <button class="primary submit-action" type="submit" data-submit>Steer</button>' +
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

const laneViewGlyphs = { compose: "✎", filters: "❒", metrics: "▦", info: "ⓘ" };
let composerBandMenuDismissHandler = null;

function createLaneState(targetId, hint = null) {
  const target = targetById.get(targetId) || {};
  const element = document.createElement("section");
  element.className = "lane";
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
      '<span class="lane-mode-glyph" aria-hidden="true"></span>' +
      '<span class="lane-mode-word"></span>' +
      '<span class="lane-mode-badge" data-lane-view-badge hidden></span>';
    button.querySelector(".lane-mode-glyph").textContent = laneViewGlyphs[view];
    button.querySelector(".lane-mode-word").textContent = view;
    rail.append(button);
  }
  const lane = {
    targetId,
    element,
    closed: false,
    laneId: "lane:" + targetId,
    agentName: target.agentName || "",
    branchName: target.branch || target.displayName || targetId,
    targetThreadId: target.threadId || "",
    activeThreadId: target.threadId || "",
    ...laneStreamState(target),
    speechMode: hint ? hint.speechMode : defaultSpeechMode,
    lifetime: target.lifetime || defaultAgentLifetime,
    selectedView: hint ? hint.selectedView : defaultLaneViewMode,
    taskFilters: uniqueStringList(target.taskFilters || []),
    laneFilterVersion: target.laneFilterVersion || "",
    taskFilterInventory: target.taskFilterInventory || null,
    laneMetrics: target.laneMetrics || {},
    laneInfo: target.laneInfo || { summaryRows: [], members: [] },
    privateTaskCount: Math.max(0, Number(target.privateTaskCount) || 0),
    teamId: target.teamId || "",
    teamRevision: target.teamRevision || 0,
    configRevision: target.configRevision || 0,
    groupTopology: null,
    backendPendingInboxCount: 0,
    optimisticPendingInboxCount: 0,
    pendingSubmissionCount: 0,
    sendAwaitingBackendCount: 0,
    refreshInFlight: false,
    liveBusSubscribed: false,
    serverReachable: true,
    serverCloseRequested: false,
    filterPickerOpen: false,
    filterPickerQuery: "",
    filterPickerPendingAssignments: new Set(),
    filterPickerOverlayEl: null,
    filterPickerOverlayPositionHandler: null,
    filterPickerOverlayDismissHandler: null,
    selectedFilterRemovals: new Set(),
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
  wireLaneShell(lane);
  syncComposerShards(lane, [lane]);
  syncLaneEffectiveControls(lane);
  renderLaneChrome(lane, targetPayloadShim(target));
  return lane;
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
    statusTransientTimer: null,
    latestPayload: null,
    ackContextByKey: new Map(),
    missingAckContextKeys: new Set(),
    recentSentAckKeys: [],
    spokenMessageKeys: new Set(),
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
    closeButtonEl: element.querySelector("[data-close-lane]"),
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
  lane.closeButtonEl.addEventListener("click", () => closeLane(lane));
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
  lane.selectedView = laneViewMode(view);
  persistLaneHints();
  renderLaneViewShell(lane);
  expandLanePane(laneGroupHost(lane));
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
  const next = Math.max(0, Math.min(maxHeight, collapsePx));
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
  lane.viewStackEl.style.setProperty("--lane-view-position", String(position));
  for (const button of lane.element.querySelectorAll(
    "[data-lane-view-button]",
  )) {
    const view = button.dataset.laneViewButton;
    const active = view === selectedView;
    button.classList.toggle("lane-mode-button--active", active);
    button.setAttribute("aria-selected", active ? "true" : "false");
    button.tabIndex = active ? 0 : -1;
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
  const driving = laneEffectiveLifetime(lane) === "Drive";
  badge.classList.toggle(
    "lane-mode-badge--inactive",
    view === "filters" && !driving,
  );
}

function laneComposeBadgeCount(lane) {
  const members = laneGroupMemberLanes(laneGroupHost(lane));
  return members.reduce(
    (total, member) => total + lanePendingDisplayCount(member),
    0,
  );
}

// ---- composer shards ---------------------------------------------------------------

// A fused host owns one composer with one shard per member target; the shard
// is the concrete send address. Standalone lanes have exactly one shard.
function syncComposerShards(lane, members) {
  const wanted = members.length ? members : [lane];
  const wantedTargetIds = wanted.map((member) => member.targetId);
  const liveTargetIds = new Set(wantedTargetIds);
  pruneComposerQuoteDrafts(lane, wantedTargetIds);
  pruneComposerAttachments(lane, wantedTargetIds);
  for (const targetId of lane.shardTextareas.keys()) {
    if (!liveTargetIds.has(targetId)) lane.shardTextareas.delete(targetId);
  }
  const shards = wanted.map((member) => {
    let shard = composerShardElementForTarget(lane, member.targetId);
    if (!shard) shard = createComposerShardElement(member.targetId);
    syncComposerShard(lane, shard, member);
    return shard;
  });
  syncComposerShardOrder(lane.shardsEl, shards);
  syncLanePaneMetrics(lane);
}

function composerShardElementForTarget(lane, targetId) {
  for (const child of lane.shardsEl.children) {
    if (child.dataset && child.dataset.shardTargetId === targetId)
      return child;
  }
  return null;
}

function createComposerShardElement(targetId) {
  const shard = document.createElement("div");
  shard.className = "composer-shard";
  shard.dataset.shardTargetId = targetId;
  const quoteStack = document.createElement("div");
  quoteStack.className = "composer-quote-stack";
  quoteStack.dataset.composerQuoteStackTargetId = targetId;
  const primary = document.createElement("div");
  primary.className = "composer-band composer-band--primary";
  primary.dataset.composerPrimaryTargetId = targetId;
  shard.append(quoteStack, primary);
  return shard;
}

function syncComposerShard(lane, shard, member) {
  shard.className = "composer-shard";
  shard.dataset.shardTargetId = member.targetId;
  const quoteStack = composerShardQuoteStack(shard, member.targetId);
  syncComposerQuoteStack(lane, quoteStack, member.targetId);
  const primary = composerShardPrimaryBand(shard, member.targetId);
  primary.className = "composer-band composer-band--primary";
  primary.dataset.composerPrimaryTargetId = member.targetId;
  const header = composerPrimaryBandHeader(lane, member);
  const previousHeader = primary.querySelector(".composer-band-header--primary");
  if (previousHeader) previousHeader.replaceWith(header);
  else primary.prepend(header);
  syncComposerBandMenuState(primary);
  let textarea = primary.querySelector("textarea");
  syncComposerAttachmentStrip(primary, lane, member.targetId, textarea);
  if (!textarea) {
    textarea = createComposerPrimaryTextarea(lane, member.targetId);
    primary.append(textarea);
  }
  textarea.placeholder = laneComposePlaceholder(member);
  lane.shardTextareas.set(member.targetId, textarea);
}

function composerShardQuoteStack(shard, targetId) {
  let quoteStack = shard.querySelector("[data-composer-quote-stack-target-id]");
  if (!quoteStack) {
    quoteStack = document.createElement("div");
    quoteStack.className = "composer-quote-stack";
    shard.prepend(quoteStack);
  }
  quoteStack.dataset.composerQuoteStackTargetId = targetId;
  return quoteStack;
}

function composerShardPrimaryBand(shard, targetId) {
  let primary = shard.querySelector("[data-composer-primary-target-id]");
  if (!primary) {
    primary = document.createElement("div");
    shard.append(primary);
  }
  primary.dataset.composerPrimaryTargetId = targetId;
  return primary;
}

function composerPrimaryBandHeader(lane, member) {
  const label = laneMemberTargetLabel(member);
  const latest = latestComposerMessage(member);
  const header = composerBandHeader({
    className: "composer-band-header--primary",
    title: composerPrimaryHeaderTitle(latest),
    beforeMenu: composerPrimaryHeaderBeforeMenu(latest),
    trailingControl: composerBandMenuTrigger(
      "Composer actions for " + label,
      "Composer actions for " + label,
      composerPrimaryMenuActions(lane, member, label),
    ),
  });
  header.title = "Drag composer to move this agent to another lane";
  if (typeof wireComposerMoveDrag === "function")
    wireComposerMoveDrag(lane, header, member.targetId);
  return header;
}

function composerPrimaryMenuActions(lane, member, label) {
  const leave = composerBandMenuAction(
    "Leave all teams",
    "Remove " + label + " from all teams",
  );
  leave.onClick = () => removeComposerAgentFromTeam(lane, member.targetId);

  const create = composerBandMenuAction(
    "Create new team",
    "Move only " + label + " to a new team",
  );
  create.disabled = laneGroupMemberLanes(laneGroupHost(lane)).length < 2;
  create.onClick = () => splitComposerAgentFromTeam(lane, member.targetId);

  return [leave, create];
}

function composerBandMenuAction(label, detail) {
  const action = {};
  action.label = label;
  action.detail = detail;
  return action;
}

function composerPrimaryHeaderTitle(latest) {
  return latest ? composerQuotePreview(latest) : "No assistant messages yet";
}

function composerPrimaryHeaderBeforeMenu(latest) {
  return [
    latest
      ? composerPrimaryLatestMessageLink(latest)
      : composerPrimaryLatestMessageNote(),
  ];
}

function composerPrimaryLatestMessageLink(latest) {
  const time = document.createElement("a");
  time.href = "#" + messageDomId(latest.key);
  time.title = "Jump to latest message";
  time.className = "composer-quote-time composer-latest-time";
  time.dataset.relativeTimestamp = latest.timestamp || "";
  time.dataset.relativeFallback = "message";
  setRelativeTimeText(time);
  return time;
}

function composerPrimaryLatestMessageNote() {
  const note = document.createElement("span");
  note.className = "composer-quote-time composer-latest-time composer-latest-time--empty";
  note.textContent = "no messages";
  note.title = "No latest message";
  return note;
}

function latestComposerMessage(member) {
  return member.knownMessages.find(isComposerLatestMessage);
}

function isComposerLatestMessage(item) {
  return item.kind === "assistant" || item.kind === "final";
}

function createComposerPrimaryTextarea(lane, targetId) {
  const textarea = document.createElement("textarea");
  textarea.rows = 3;
  textarea.addEventListener("focus", () => expandLanePane(lane));
  textarea.addEventListener("input", () => expandLanePane(lane));
  wireComposerAttachmentIngress(textarea, lane, targetId);
  textarea.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      submitLaneForm(lane, event, targetId);
    }
  });
  return textarea;
}

function syncComposerShardOrder(container, shards) {
  const wanted = new Set(shards);
  for (const child of [...container.children]) {
    if (!wanted.has(child)) child.remove();
  }
  let cursor = container.firstElementChild;
  for (const shard of shards) {
    if (shard === cursor) {
      cursor = cursor.nextElementSibling;
      continue;
    }
    container.insertBefore(shard, cursor);
  }
}

function laneComposePlaceholder(member) {
  const label = laneMemberTargetLabel(member);
  const status = laneComposePlaceholderStatus(member);
  return [label, status].filter(Boolean).join("\n");
}

function laneComposePlaceholderStatus(member) {
  const parts = [];
  const pending = lanePendingDisplayCount(member);
  parts.push(pending + " pending");
  const status = (member.lastRenderedStatusLine || {}).agentProcessStatus || "";
  if (status) parts.push(status);
  return parts.join(", ");
}

function syncComposerPlaceholders(lane) {
  for (const [targetId, textarea] of lane.shardTextareas) {
    const member = laneStates.get(targetId) || lane;
    textarea.placeholder = laneComposePlaceholder(member);
  }
}

function laneComposerDraftText(lane) {
  const host = laneGroupHost(lane);
  let text = "";
  for (const textarea of host.shardTextareas.values()) text += textarea.value;
  for (const attachments of host.shardAttachments.values()) {
    if (attachments.length) text += " attachment";
  }
  for (const drafts of host.quoteDrafts.values()) {
    for (const draft of drafts) text += (draft.quoteText || "") + (draft.text || "");
  }
  return text;
}

function laneComposerTargetDraftText(lane, targetId) {
  const host = laneGroupHost(lane);
  let text = host.shardTextareas.get(targetId)?.value || "";
  if (composerAttachmentDraftsForTarget(host, targetId).length) text += " attachment";
  for (const draft of composerQuoteDraftsForTarget(host, targetId)) {
    text += (draft.quoteText || "") + (draft.text || "");
  }
  return text;
}

function resetLaneComposerDraft(lane, targetId) {
  const host = laneGroupHost(lane);
  const textarea = host.shardTextareas.get(targetId);
  if (textarea) textarea.value = "";
  if (host.shardAttachments.delete(targetId)) renderComposerAttachmentStrips(host);
  if (host.quoteDrafts.delete(targetId)) renderComposerQuoteBands(host);
}

function composerAttachmentStrip(lane, targetId) {
  const wrap = document.createElement("div");
  wrap.className = "composer-attachments";
  wrap.dataset.composerAttachmentsTargetId = targetId;
  fillComposerAttachmentStrip(wrap, lane, targetId);
  return wrap;
}

function syncComposerAttachmentStrip(parent, lane, targetId, beforeNode) {
  let wrap = parent.querySelector(
    "[data-composer-attachments-target-id]",
  );
  if (!wrap) {
    wrap = composerAttachmentStrip(lane, targetId);
  }
  wrap.dataset.composerAttachmentsTargetId = targetId;
  fillComposerAttachmentStrip(wrap, lane, targetId);
  const body = parent.querySelector(".composer-band-body");
  if (body) {
    if (wrap.parentElement !== body) body.append(wrap);
    fillComposerAttachmentStrip(wrap, lane, targetId);
    return;
  }
  if (!wrap.parentElement) parent.insertBefore(wrap, beforeNode || null);
  else if (beforeNode && wrap.nextElementSibling !== beforeNode)
    parent.insertBefore(wrap, beforeNode);
}

function fillComposerAttachmentStrip(wrap, lane, targetId) {
  const attachments = composerAttachmentDraftsForTarget(lane, targetId);
  wrap.hidden = attachments.length === 0;
  wrap
    .closest(".composer-band-body")
    ?.classList.toggle("composer-band-body--attachments", attachments.length > 0);
  wrap
    .closest(".composer-band-header")
    ?.classList.toggle("composer-band-header--attachments", attachments.length > 0);
  if (!attachments.length) {
    wrap.replaceChildren();
    return;
  }
  const list = document.createElement("div");
  list.className = "composer-attachment-list";
  for (const attachment of attachments) {
    const chip = document.createElement("span");
    chip.className = "composer-attachment-chip";
    chip.title = attachment.name;
    const img = document.createElement("img");
    img.src = attachment.dataUrl;
    img.alt = attachment.name;
    const label = document.createElement("span");
    label.className = "composer-attachment-name";
    label.textContent = attachment.name || "image";
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "composer-attachment-remove";
    remove.title = "Remove image";
    remove.setAttribute("aria-label", "Remove image");
    remove.textContent = "×";
    remove.addEventListener("click", () =>
      removeComposerAttachment(lane, targetId, attachment.id),
    );
    chip.append(img, label, remove);
    list.append(chip);
  }
  wrap.replaceChildren(list);
}

function composerAttachmentDraftsForTarget(lane, targetId) {
  return lane.shardAttachments.get(targetId) || [];
}

function renderComposerAttachmentStrips(lane) {
  for (const wrap of lane.element.querySelectorAll(
    "[data-composer-attachments-target-id]",
  )) {
    fillComposerAttachmentStrip(
      wrap,
      lane,
      wrap.dataset.composerAttachmentsTargetId || "",
    );
  }
  syncLanePaneMetrics(lane);
}

function wireComposerAttachmentIngress(textarea, lane, targetId) {
  textarea.addEventListener("paste", (event) =>
    handleComposerAttachmentPaste(lane, targetId, event),
  );
  textarea.addEventListener("dragenter", (event) =>
    handleComposerAttachmentDrag(textarea, event),
  );
  textarea.addEventListener("dragover", (event) =>
    handleComposerAttachmentDrag(textarea, event),
  );
  textarea.addEventListener("dragleave", () =>
    textarea.closest(".composer-band")?.classList.remove("composer-band--drop-ready"),
  );
  textarea.addEventListener("drop", (event) =>
    handleComposerAttachmentDrop(textarea, lane, targetId, event),
  );
}

function composerImageFilesFromTransfer(transfer) {
  return [...(transfer?.files || [])].filter((file) =>
    String(file.type || "").startsWith("image/"),
  );
}

function composerTransferHasImage(transfer) {
  return composerImageFilesFromTransfer(transfer).length > 0 ||
    [...(transfer?.items || [])].some((item) =>
      String(item.type || "").startsWith("image/"),
    );
}

function handleComposerAttachmentPaste(lane, targetId, event) {
  const files = composerImageFilesFromTransfer(event.clipboardData);
  if (!files.length) return;
  event.preventDefault();
  addComposerAttachmentFiles(lane, targetId, files);
}

function handleComposerAttachmentDrag(textarea, event) {
  if (!composerTransferHasImage(event.dataTransfer)) return;
  event.preventDefault();
  event.dataTransfer.dropEffect = "copy";
  textarea.closest(".composer-band")?.classList.add("composer-band--drop-ready");
}

function handleComposerAttachmentDrop(textarea, lane, targetId, event) {
  const files = composerImageFilesFromTransfer(event.dataTransfer);
  textarea.closest(".composer-band")?.classList.remove("composer-band--drop-ready");
  if (!files.length) return;
  event.preventDefault();
  if (files.length) addComposerAttachmentFiles(lane, targetId, files);
}

function addComposerAttachmentFiles(lane, targetId, files) {
  const sourceLane = laneGroupHost(lane);
  for (const file of files) {
    if (!String(file.type || "").startsWith("image/")) continue;
    const current = composerAttachmentDraftsForTarget(lane, targetId);
    if (current.length >= composerAttachmentMaxItems) {
      setLaneTransientStatus(sourceLane, "maximum images attached");
      return;
    }
    if (file.size > composerAttachmentMaxBytes) {
      setLaneTransientStatus(sourceLane, "image is over 8MB");
      continue;
    }
    const reader = new FileReader();
    reader.addEventListener("load", () => {
      const dataUrl = String(reader.result || "");
      if (!dataUrl) return;
      const drafts = composerAttachmentDraftsForTarget(lane, targetId).slice();
      if (drafts.length >= composerAttachmentMaxItems) return;
      drafts.push({
        id: "attachment-" + Date.now() + "-" + drafts.length,
        name: file.name || "pasted-image.png",
        contentType: file.type || "image/png",
        size: file.size || 0,
        dataUrl,
      });
      lane.shardAttachments.set(targetId, drafts);
      renderComposerAttachmentStrips(lane);
      expandLanePane(sourceLane);
    });
    reader.readAsDataURL(file);
  }
}

function removeComposerAttachment(lane, targetId, attachmentId) {
  const retained = composerAttachmentDraftsForTarget(lane, targetId).filter(
    (attachment) => attachment.id !== attachmentId,
  );
  if (retained.length) lane.shardAttachments.set(targetId, retained);
  else lane.shardAttachments.delete(targetId);
  renderComposerAttachmentStrips(lane);
}

function laneComposerAttachmentPayloads(lane, targetId) {
  return composerAttachmentDraftsForTarget(lane, targetId).map((attachment) => ({
    name: attachment.name,
    contentType: attachment.contentType,
    size: attachment.size,
    dataUrl: attachment.dataUrl,
  }));
}

function quoteMessageIntoComposer(lane, item) {
  const host = laneGroupHost(lane);
  const producer = item.producerTargetId || host.targetId;
  const targetId = host.shardTextareas.has(producer)
    ? producer
    : host.shardTextareas.keys().next().value;
  if (!targetId) return;
  const textarea =
    host.shardTextareas.get(targetId) ||
    host.shardTextareas.values().next().value;
  if (!textarea) return;
  const draftId = addComposerQuoteDraft(host, targetId, item);
  setLaneSelectedView(host, "compose");
  if (draftId) revealComposerQuoteDraft(host, draftId);
  else textarea.focus();
}

function addComposerQuoteDraft(lane, targetId, item) {
  const drafts = lane.quoteDrafts.get(targetId) || [];
  const messageKey = String(item.key || item.index || item.timestamp || "");
  if (messageKey && drafts.some((draft) => draft.messageKey === messageKey)) {
    renderComposerQuoteBands(lane);
    return drafts.find((draft) => draft.messageKey === messageKey)?.id || "";
  }
  const draft = {
    id: "quote-" + ++lane.nextQuoteDraftId,
    messageKey,
    href: messageKey ? "#" + messageDomId(messageKey) : "",
    timestamp: item.timestamp || "",
    preview: composerQuotePreview(item),
    quoteText: messageCopyText(lane, item) || item.display_text || item.text || "",
    text: "",
  };
  drafts.push(draft);
  drafts.sort((left, right) => {
    const leftTime = Date.parse(left.timestamp || "") || 0;
    const rightTime = Date.parse(right.timestamp || "") || 0;
    return rightTime - leftTime;
  });
  lane.quoteDrafts.set(targetId, drafts);
  renderComposerQuoteBands(lane);
  return draft.id;
}

function composerQuotePreview(item) {
  return String(item.preview || item.display_text || item.text || "assistant message")
    .replace(/\s+/g, " ")
    .trim();
}

function renderComposerQuoteBands(lane) {
  for (const stack of lane.element.querySelectorAll(
    "[data-composer-quote-stack-target-id]",
  )) {
    syncComposerQuoteStack(
      lane,
      stack,
      stack.dataset.composerQuoteStackTargetId || "",
    );
  }
  syncLanePaneMetrics(lane);
}

function syncComposerQuoteStack(lane, stack, targetId) {
  const member = laneStates.get(targetId) || lane;
  const bands = composerQuoteDraftsForTarget(lane, targetId).map((draft) => {
    let band = composerQuoteBandElementForDraft(stack, draft.id);
    if (!band) band = composerQuoteBand(lane, targetId, member, draft);
    else syncComposerQuoteBand(band, lane, targetId, member, draft);
    return band;
  });
  syncComposerQuoteBandOrder(stack, bands);
}

function composerQuoteBandElementForDraft(stack, draftId) {
  for (const child of stack.children) {
    if (child.dataset && child.dataset.composerQuoteBandDraftId === draftId)
      return child;
  }
  return null;
}

function syncComposerQuoteBandOrder(stack, bands) {
  const wanted = new Set(bands);
  for (const child of [...stack.children]) {
    if (!wanted.has(child)) child.remove();
  }
  let cursor = stack.firstElementChild;
  for (const band of bands) {
    if (band === cursor) {
      cursor = cursor.nextElementSibling;
      continue;
    }
    stack.insertBefore(band, cursor);
  }
}

function revealComposerQuoteDraft(lane, draftId) {
  const textarea = [...lane.element.querySelectorAll("[data-quote-draft-id]")]
    .find((element) => element.dataset.quoteDraftId === draftId);
  if (!(textarea instanceof HTMLTextAreaElement)) return;
  const band = textarea.closest(".composer-band");
  const shard = textarea.closest(".composer-shard");
  if (band instanceof HTMLElement && shard instanceof HTMLElement) {
    shard.scrollTop = Math.max(0, band.offsetTop - shard.offsetTop);
  }
  textarea.focus({ preventScroll: true });
}

function composerQuoteDraftsForTarget(lane, targetId) {
  return lane.quoteDrafts.get(targetId) || [];
}

function composerBandHeader({
  className,
  title,
  beforeMenu = [],
  trailingControl = null,
}) {
  const header = document.createElement("div");
  header.className = "composer-band-header " + className;
  const body = document.createElement("div");
  body.className = "composer-band-body";
  const label = document.createElement("span");
  label.className = "composer-band-title";
  label.textContent = title;
  body.append(label);
  header.append(...beforeMenu, body);
  if (trailingControl) header.append(trailingControl);
  return header;
}

function composerBandMenuTrigger(menuTitle, menuLabel, menuActions) {
  const trigger = document.createElement("button");
  trigger.type = "button";
  trigger.className = "composer-band-menu-button";
  trigger.title = menuTitle;
  trigger.setAttribute("aria-label", menuLabel);
  trigger.setAttribute("aria-haspopup", "menu");
  trigger.setAttribute("aria-expanded", "false");
  trigger.replaceChildren(composerBandMenuIcon());
  trigger.addEventListener("click", (event) => {
    event.stopPropagation();
    toggleComposerBandMenu(trigger, menuActions || []);
  });
  return trigger;
}

function composerBandMenuIcon() {
  const icon = document.createElement("span");
  icon.className = "composer-band-menu-icon";
  icon.setAttribute("aria-hidden", "true");
  icon.style.background =
    "linear-gradient(currentColor, currentColor) 0 0 / 100% 1.5px no-repeat, " +
    "linear-gradient(currentColor, currentColor) 0 50% / 100% 1.5px no-repeat, " +
    "linear-gradient(currentColor, currentColor) 0 100% / 100% 1.5px no-repeat";
  icon.style.display = "block";
  icon.style.height = "8px";
  icon.style.width = "11px";
  return icon;
}

function composerBandCloseButton(closeTitle, closeLabel, onClose) {
  const close = document.createElement("button");
  close.type = "button";
  close.className = "composer-band-close-button";
  close.title = closeTitle;
  close.setAttribute("aria-label", closeLabel || closeTitle);
  close.textContent = "×";
  close.addEventListener("click", (event) => {
    event.stopPropagation();
    onClose();
  });
  return close;
}

function toggleComposerBandMenu(trigger, actions) {
  const band = trigger.closest(".composer-band");
  if (!band) return;
  const open = trigger.getAttribute("aria-expanded") === "true";
  closeComposerBandMenusExcept(band);
  closeComposerBandMenu(band);
  if (open) return;
  const menu = document.createElement("div");
  menu.className = "composer-band-menu";
  menu.setAttribute("role", "menu");
  for (const action of actions) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "composer-band-menu-action spice-menu-action";
    button.setAttribute("role", "menuitem");
    button.disabled = Boolean(action.disabled);
    if (action.detail) button.title = action.detail;
    button.innerHTML =
      '<span class="spice-menu-action-label"></span>' +
      '<span class="spice-menu-action-detail"></span>';
    button.querySelector(".spice-menu-action-label").textContent = action.label;
    button.querySelector(".spice-menu-action-detail").textContent =
      action.detail || "";
    button.addEventListener("click", () => {
      closeComposerBandMenu(band);
      action.onClick();
    });
    menu.append(button);
  }
  const textarea = band.querySelector("textarea");
  band.insertBefore(menu, textarea || null);
  band.classList.add("composer-band--menu-open");
  trigger.setAttribute("aria-expanded", "true");
  syncComposerBandMenuDismissHandler();
}

function closeComposerBandMenu(band) {
  band.querySelector(".composer-band-menu")?.remove();
  band.classList.remove("composer-band--menu-open");
  const trigger = band.querySelector(".composer-band-menu-button");
  if (trigger) trigger.setAttribute("aria-expanded", "false");
  syncComposerBandMenuDismissHandler();
}

function closeComposerBandMenusExcept(exceptBand) {
  for (const band of document.querySelectorAll(".composer-band--menu-open")) {
    if (band !== exceptBand) closeComposerBandMenu(band);
  }
  syncComposerBandMenuDismissHandler();
}

function syncComposerBandMenuDismissHandler() {
  const hasOpenMenu = document.querySelector(".composer-band--menu-open");
  if (hasOpenMenu && !composerBandMenuDismissHandler) {
    composerBandMenuDismissHandler = dismissComposerBandMenusOnPointerDown;
    document.addEventListener("pointerdown", composerBandMenuDismissHandler, true);
  } else if (!hasOpenMenu && composerBandMenuDismissHandler) {
    document.removeEventListener(
      "pointerdown",
      composerBandMenuDismissHandler,
      true,
    );
    composerBandMenuDismissHandler = null;
  }
}

function dismissComposerBandMenusOnPointerDown(event) {
  const target = event.target;
  if (!(target instanceof Node)) return;
  for (const band of document.querySelectorAll(".composer-band--menu-open")) {
    const menu = band.querySelector(".composer-band-menu");
    const trigger = band.querySelector(".composer-band-menu-button");
    if (menu?.contains(target) || trigger?.contains(target)) continue;
    closeComposerBandMenu(band);
  }
  syncComposerBandMenuDismissHandler();
}

function syncComposerBandMenuState(band) {
  const open = [...band.children].some((child) =>
    child.classList?.contains("composer-band-menu"),
  );
  band.classList.toggle("composer-band--menu-open", open);
  const trigger = band.querySelector(".composer-band-menu-button");
  if (trigger) trigger.setAttribute("aria-expanded", open ? "true" : "false");
  syncComposerBandMenuDismissHandler();
}

function composerQuoteBand(lane, targetId, member, draft) {
  const band = document.createElement("div");
  syncComposerQuoteBand(band, lane, targetId, member, draft);
  return band;
}

function syncComposerQuoteBand(band, lane, targetId, member, draft) {
  band.className = "composer-band composer-band--quote";
  band.title = draft.quoteText || draft.preview;
  band.dataset.composerQuoteBandDraftId = draft.id;
  const header = composerQuoteBandHeader(lane, targetId, draft);
  const previousHeader = band.querySelector(".composer-band-header--quote");
  if (previousHeader) previousHeader.replaceWith(header);
  else band.prepend(header);
  syncComposerBandMenuState(band);
  let textarea = band.querySelector("textarea");
  syncComposerAttachmentStrip(band, lane, targetId, textarea);
  if (!textarea) {
    textarea = createComposerQuoteTextarea(lane, targetId, draft);
    band.append(textarea);
  } else {
    textarea.dataset.quoteDraftId = draft.id;
    if (document.activeElement !== textarea && textarea.value !== (draft.text || ""))
      textarea.value = draft.text || "";
  }
  textarea.placeholder = laneComposePlaceholder(member);
}

function composerQuoteBandHeader(lane, targetId, draft) {
  let time;
  if (draft.href) {
    const anchor = document.createElement("a");
    anchor.href = draft.href;
    anchor.title = "Jump to quoted message";
    time = anchor;
  } else {
    time = document.createElement("span");
  }
  time.className = "composer-quote-time";
  time.dataset.relativeTimestamp = draft.timestamp || "";
  time.dataset.relativeFallback = "quote";
  setRelativeTimeText(time);
  const header = composerBandHeader({
    className: "composer-band-header--quote",
    title: draft.preview || "quoted message",
    beforeMenu: [time],
    trailingControl: composerBandCloseButton(
      "Remove quote",
      "Remove quoted composer",
      () => removeComposerQuoteDraft(lane, targetId, draft.id),
    ),
  });
  return header;
}

function createComposerQuoteTextarea(lane, targetId, draft) {
  const textarea = document.createElement("textarea");
  textarea.rows = 2;
  textarea.value = draft.text || "";
  textarea.dataset.quoteDraftId = draft.id;
  textarea.addEventListener("focus", () => expandLanePane(lane));
  textarea.addEventListener("input", () => {
    draft.text = textarea.value;
    expandLanePane(lane);
  });
  wireComposerAttachmentIngress(textarea, lane, targetId);
  textarea.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      submitLaneForm(lane, event, targetId);
    }
  });
  return textarea;
}

function removeComposerQuoteDraft(lane, targetId, draftId) {
  const drafts = composerQuoteDraftsForTarget(lane, targetId).filter(
    (draft) => draft.id !== draftId,
  );
  if (drafts.length) lane.quoteDrafts.set(targetId, drafts);
  else lane.quoteDrafts.delete(targetId);
  renderComposerQuoteBands(lane);
}

function pruneComposerQuoteDrafts(lane, targetIds) {
  const liveTargets = new Set(targetIds);
  for (const targetId of lane.quoteDrafts.keys()) {
    if (!liveTargets.has(targetId)) lane.quoteDrafts.delete(targetId);
  }
}

function pruneComposerAttachments(lane, targetIds) {
  const liveTargets = new Set(targetIds);
  for (const targetId of lane.shardAttachments.keys()) {
    if (!liveTargets.has(targetId)) lane.shardAttachments.delete(targetId);
  }
}

function removeComposerAgentFromTeam(lane, targetId) {
  const host = laneGroupHost(lane);
  const member = laneStates.get(targetId);
  if (!member) return;
  if (laneComposerTargetDraftText(host, targetId).trim()) {
    if (!window.confirm(unsafeDraftWarningText())) return;
  }
  const teamId = member.teamId || host.teamId;
  if (!teamId) return;
  member.serverCloseRequested = true;
  requestTeamCommand(
    teamCommandPayload("removeAgentFromTeam", {
      teamId,
      agentId: laneTeamAgentId(member),
      agentAliases: laneTeamAgentAliases(member),
    }),
  ).catch(() => {
    member.serverCloseRequested = false;
    setLaneTransientStatus(host, "remove agent from team failed");
  });
}

function laneComposerSubmissionText(lane, targetId, draftText) {
  const quotes = composerQuoteDraftsForTarget(lane, targetId)
    .map((draft) => quoteDraftSubmissionText(draft))
    .filter((part) => part.trim());
  const body = String(draftText || "").trim();
  return [body, ...quotes].filter((part) => part.trim()).join("\n\n");
}

function quoteDraftSubmissionText(draft) {
  const quote = markdownBlockQuote(draft.quoteText);
  const body = String(draft.text || "").trim();
  return [quote, body].filter((part) => part.trim()).join("\n\n");
}
